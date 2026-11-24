//! Damage tracking for CPU windows.
//!
//! A CPU window frame is drawn into a presenter buffer that may still hold
//! an older frame (its *age*). Only what changed since then has to be drawn
//! again. This module finds what changed from the recorded draw list alone:
//!
//! - **Signature pass.** Before a window frame is drawn, the scene pass runs
//!   once into [`TileHashes`] instead of the rasterizer (see
//!   [`CpuPass::signing`](super::cpu::CpuPass::signing)). Every instance of
//!   every draw is hashed with everything its pipeline reads (the instance's
//!   bytes, the pipeline, the screen size, the scissor, the identity and
//!   generation of the textures it samples, and the rasterizer's own choice
//!   of path), and folded, in draw order, into each [`TILE`] x [`TILE`]
//!   tile its footprint touches. The footprint is
//!   [`rasterize`](crate::cpu_raster::rasterize)'s, the exact pixels every CPU
//!   pipeline covers.
//! - **Damage.** The tiles whose hash differs from the previous drawn frame's
//!   are the frame's damage. A pixel's value is a function of the ordered
//!   draws whose footprint covers it, so a tile whose ordered list of
//!   covering draws is unchanged holds unchanged pixels.
//! - **Repaint.** The presenter widens the damage by the buffer's age
//!   (`repaint_region`). The raster pass then clears and draws only those
//!   rectangles, made disjoint ([`disjoint`]), with every draw's scissor
//!   intersected with each of them, and the presenter is handed this frame's
//!   damage.
//!
//! The frame is a full repaint when no previous frame can be compared (the
//! first frame, a resize, a buffer the presenter rounded past the scene) and
//! when `KETTLE_CPU_FULL_REPAINT=1` is set, which is the switch for
//! diagnosing a damage bug.
//!
//! The hash is a seeded 64-bit multiply-xor fold, keyed per process. Two
//! different tile contents colliding would leave that tile stale until it
//! changes again; the per-process key keeps terminal output from aiming at
//! a collision.

use std::sync::{Arc, OnceLock, Weak};

use kettle_core::PixelBuffer;

use super::cpu::CpuDraw;
use crate::cpu_raster::{PixelRect, Viewport, rasterize};

/// The side of a damage tile in pixels. A terminal cell is about 8x18 at
/// scale 1 and 16x36 at scale 2, so a typed character damages one to four
/// tiles; a 3840x2160 frame has 8,160.
pub(crate) const TILE: u32 = 32;

/// The most rectangles one frame's damage is reduced to. The presenter
/// collapses more than `cpu_present::MAX_DAMAGE_RECTS` (16) to one bounding
/// box; reducing here first keeps far-apart changes (a cursor and a clock)
/// apart.
pub(crate) const MAX_RECTS: usize = 12;

/// The environment switch that makes every CPU window frame a full repaint.
pub(crate) const FULL_REPAINT_ENV: &str = "KETTLE_CPU_FULL_REPAINT";

/// Whether `KETTLE_CPU_FULL_REPAINT` asks for full repaints. Read once.
pub(crate) fn full_repaint_forced() -> bool {
    static FORCED: OnceLock<bool> = OnceLock::new();
    *FORCED.get_or_init(|| {
        let forced = std::env::var_os(FULL_REPAINT_ENV)
            .is_some_and(|value| !value.is_empty() && value != "0");
        if forced {
            log::info!("renderer: {FULL_REPAINT_ENV} is set; CPU frames repaint in full");
        }
        forced
    })
}

const K0: u64 = 0x9e37_79b9_7f4a_7c15;
const K1: u64 = 0xbf58_476d_1ce4_e5b9;

#[inline(always)]
fn mix(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(K0).rotate_left(29)
}

/// splitmix64's finalizer: every input bit reaches every output bit.
#[inline(always)]
fn finish(mut h: u64) -> u64 {
    h ^= h >> 30;
    h = h.wrapping_mul(K1);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// Fold `words` (an instance's bytes as `u32`s) into `h`, two at a time.
#[inline(always)]
fn mix_words(mut h: u64, words: &[u32]) -> u64 {
    let (pairs, rest) = words.as_chunks::<2>();
    for [a, b] in pairs {
        h = mix(h, u64::from(*a) | (u64::from(*b) << 32));
    }
    for w in rest {
        h = mix(h, u64::from(*w));
    }
    h
}

/// A fresh per-process key.
fn process_seed() -> u64 {
    use std::hash::BuildHasher;
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        std::collections::hash_map::RandomState::new().hash_one(0x6b65_7474_6c65_u64)
    })
}

/// One frame's signature: a hash per tile of everything drawn over it.
#[derive(Debug)]
pub(crate) struct TileHashes {
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    hashes: Vec<u64>,
    /// Weak pins of the images the frame samples, so their allocations, whose
    /// addresses the hashes hold, cannot be reused for other pixels while
    /// this signature can still be compared. A weak pin also makes an edit
    /// of the pixels move them to a new allocation, as the GPU image cache's
    /// pins do (`ImageData::compose`).
    pins: Vec<Weak<PixelBuffer>>,
    /// Draws whose plan was not the default path (an image drawn from the
    /// rasterizer's cache).
    planned: u32,
    seed: u64,
}

impl TileHashes {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            cols: 0,
            rows: 0,
            hashes: Vec::new(),
            pins: Vec::new(),
            planned: 0,
            seed: process_seed(),
        }
    }

    /// Start a `width` x `height` frame.
    pub(crate) fn begin(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.cols = width.div_ceil(TILE);
        self.rows = height.div_ceil(TILE);
        self.hashes.clear();
        self.hashes
            .resize(self.cols as usize * self.rows as usize, self.seed);
        self.pins.clear();
        self.planned = 0;
    }

    /// How many draws of the frame the rasterizer planned off its default
    /// path ([`super::cpu::CpuRaster::plan`] returned non-zero).
    pub(crate) fn planned(&self) -> u32 {
        self.planned
    }

    pub(crate) fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Fold `item` into every tile of the pixels `cols` x `rows`.
    #[inline]
    fn fold(&mut self, cols: std::ops::Range<u32>, rows: std::ops::Range<u32>, item: u64) {
        if cols.is_empty() || rows.is_empty() {
            return;
        }
        let tx0 = cols.start / TILE;
        let tx1 = ((cols.end - 1) / TILE).min(self.cols.saturating_sub(1));
        let ty0 = rows.start / TILE;
        let ty1 = ((rows.end - 1) / TILE).min(self.rows.saturating_sub(1));
        for ty in ty0..=ty1 {
            let row = ty as usize * self.cols as usize;
            for tile in &mut self.hashes[row + tx0 as usize..=row + tx1 as usize] {
                *tile = mix(*tile, item);
            }
        }
    }

    /// Fold one instance at `pos`/`size` with `words`, under `state`. The
    /// pixels it covers, after its scissor, are hashed with it: the same
    /// instance clipped differently draws different pixels even when it
    /// touches the same tiles.
    #[inline]
    fn instance(&mut self, area: &Area, pos: [f32; 2], size: [f32; 2], state: u64, words: &[u32]) {
        let Some(r) = rasterize(&area.viewport, area.screen, pos, size, area.scissor) else {
            return;
        };
        let mut h = mix_words(state, words);
        h = mix(h, u64::from(r.cols.start) | (u64::from(r.cols.end) << 32));
        h = mix(h, u64::from(r.rows.start) | (u64::from(r.rows.end) << 32));
        self.fold(r.cols, r.rows, finish(h));
    }

    fn pin(&mut self, pixels: &Arc<PixelBuffer>) {
        if !self
            .pins
            .last()
            .is_some_and(|last| std::ptr::eq(last.as_ptr(), Arc::as_ptr(pixels)))
        {
            self.pins.push(Arc::downgrade(pixels));
        }
    }

    /// Record `draw` under `scissor` on a `screen`-sized scene. `plan` is
    /// the rasterizer's path for it ([`super::cpu::CpuRaster::plan`]).
    pub(crate) fn draw(
        &mut self,
        screen: [f32; 2],
        scissor: PixelRect,
        plan: u64,
        draw: &CpuDraw<'_>,
    ) {
        let area = Area {
            viewport: Viewport {
                x: 0.0,
                y: 0.0,
                w: self.width as f32,
                h: self.height as f32,
            },
            screen,
            scissor,
        };
        let mut state = mix(self.seed, draw.pipeline() as u64);
        state = mix(
            state,
            u64::from(screen[0].to_bits()) | (u64::from(screen[1].to_bits()) << 32),
        );
        state = mix(state, u64::from(scissor.x) | (u64::from(scissor.y) << 32));
        state = mix(state, u64::from(scissor.w) | (u64::from(scissor.h) << 32));
        state = mix(state, plan);
        self.planned += u32::from(plan != 0);
        match *draw {
            CpuDraw::Quads { replace, instances } => {
                let state = mix(state, u64::from(replace));
                for q in instances {
                    self.instance(
                        &area,
                        q.pos,
                        q.size,
                        state,
                        bytemuck::cast_slice(std::slice::from_ref(q)),
                    );
                }
            }
            CpuDraw::Glyphs {
                instances,
                color_atlas,
                mask_atlas,
                atlas_epoch,
            } => {
                let mut state = mix(state, atlas_epoch);
                for view in [color_atlas, mask_atlas] {
                    state = mix(
                        state,
                        u64::from(view.width()) | (u64::from(view.height()) << 32),
                    );
                    state = mix(state, view.format() as u64);
                }
                for g in instances {
                    self.instance(
                        &area,
                        g.pos,
                        g.size,
                        state,
                        bytemuck::cast_slice(std::slice::from_ref(g)),
                    );
                }
            }
            CpuDraw::Images {
                instances,
                image,
                repeat,
            } => {
                self.pin(&image.rgba);
                let mut state = mix(state, Arc::as_ptr(&image.rgba) as usize as u64);
                state = mix(
                    state,
                    u64::from(image.width) | (u64::from(image.height) << 32),
                );
                state = mix(state, u64::from(repeat));
                for i in instances {
                    self.instance(
                        &area,
                        i.pos,
                        i.size,
                        state,
                        bytemuck::cast_slice(std::slice::from_ref(i)),
                    );
                }
            }
            CpuDraw::RoundedSurfaces { instances } => {
                for s in instances {
                    self.instance(
                        &area,
                        s.pos,
                        s.size,
                        state,
                        bytemuck::cast_slice(std::slice::from_ref(s)),
                    );
                }
            }
            CpuDraw::Outlines { instances } => {
                for o in instances {
                    self.instance(
                        &area,
                        o.pos,
                        o.size,
                        state,
                        bytemuck::cast_slice(std::slice::from_ref(o)),
                    );
                }
            }
            CpuDraw::Starfield { uniforms } => {
                // Every pixel under the scissor is shaded.
                let item = finish(mix_words(
                    state,
                    bytemuck::cast_slice(std::slice::from_ref(uniforms)),
                ));
                let cols = scissor.x..scissor.x.saturating_add(scissor.w).min(self.width);
                let rows = scissor.y..scissor.y.saturating_add(scissor.h).min(self.height);
                self.fold(cols, rows, item);
            }
            CpuDraw::Text { glyphs, .. } => {
                for glyph in glyphs {
                    if glyph.dim[0] == 0 || glyph.dim[1] == 0 {
                        continue;
                    }
                    let c = glyph.color;
                    let words = [
                        glyph.pos[0] as u32,
                        glyph.pos[1] as u32,
                        u32::from(glyph.dim[0]) | (u32::from(glyph.dim[1]) << 16),
                        u32::from(glyph.uv[0]) | (u32::from(glyph.uv[1]) << 16),
                        u32::from_le_bytes([c.r(), c.g(), c.b(), c.a()]),
                        glyph.raster.id() as u32,
                        (glyph.raster.id() >> 32) as u32,
                    ];
                    let pos = [glyph.pos[0] as f32, glyph.pos[1] as f32];
                    let size = [f32::from(glyph.dim[0]), f32::from(glyph.dim[1])];
                    self.instance(&area, pos, size, state, &words);
                }
            }
        }
    }
}

/// Where a draw lands: the scene's viewport and size, and its scissor.
struct Area {
    viewport: Viewport,
    screen: [f32; 2],
    scissor: PixelRect,
}

/// A CPU window's damage state: the signature of the frame last drawn into
/// a presenter buffer, and the one being built.
#[derive(Debug)]
pub(crate) struct DamageTracker {
    previous: Option<TileHashes>,
    next: TileHashes,
    /// Every frame repaints in full (`KETTLE_CPU_FULL_REPAINT`, or a test).
    pub(crate) force_full: bool,
    /// The last drawn frame's damage and repainted area, for tests and
    /// measurements.
    pub(crate) last: DamageStats,
}

/// What the last frame drawn into a buffer damaged and repainted.
#[derive(Clone, Debug, Default)]
pub(crate) struct DamageStats {
    /// This frame's damage, as handed to the presenter.
    pub(crate) damage: Vec<PixelRect>,
    /// The rectangles cleared and drawn (the damage widened by the buffer's
    /// age), disjoint.
    pub(crate) repainted: Vec<PixelRect>,
    /// Whether the whole buffer was repainted.
    pub(crate) full: bool,
    /// Draws planned off the rasterizer's default path (cached images).
    pub(crate) planned: u32,
    /// The signature pass's time, and the clear and raster pass's.
    pub(crate) sign: std::time::Duration,
    pub(crate) raster: std::time::Duration,
}

impl DamageStats {
    #[cfg(test)]
    pub(crate) fn repainted_pixels(&self) -> u64 {
        self.repainted
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h))
            .sum()
    }
}

impl Default for DamageTracker {
    fn default() -> Self {
        Self::new(full_repaint_forced())
    }
}

impl DamageTracker {
    pub(crate) fn new(force_full: bool) -> Self {
        Self {
            previous: None,
            next: TileHashes::new(),
            force_full,
            last: DamageStats::default(),
        }
    }

    /// The signature to build for a `width` x `height` frame, emptied.
    pub(crate) fn begin(&mut self, width: u32, height: u32) -> &mut TileHashes {
        self.next.begin(width, height);
        &mut self.next
    }

    /// The frame being built since [`Self::begin`].
    pub(crate) fn next(&self) -> &TileHashes {
        &self.next
    }

    /// The damage of the frame built since [`Self::begin`] against the last
    /// drawn one; `None` when it must be repainted in full.
    pub(crate) fn damage(&self) -> Option<Vec<PixelRect>> {
        let previous = self.previous.as_ref()?;
        if self.force_full || previous.size() != self.next.size() {
            return None;
        }
        Some(changed_rects(previous, &self.next))
    }

    /// The frame built since [`Self::begin`] was drawn into a buffer: it is
    /// what the next frame is compared with.
    pub(crate) fn commit(&mut self) {
        match &mut self.previous {
            Some(previous) => std::mem::swap(previous, &mut self.next),
            None => self.previous = Some(std::mem::replace(&mut self.next, TileHashes::new())),
        }
        self.next.pins.clear();
    }

    /// Forget the last drawn frame: the next one is a full repaint.
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.next.pins.clear();
    }
}

/// The tiles that differ between two same-size signatures, as rectangles
/// clipped to the frame: runs along each tile row, merged down columns, and
/// reduced to at most [`MAX_RECTS`].
fn changed_rects(a: &TileHashes, b: &TileHashes) -> Vec<PixelRect> {
    let (cols, width, height) = (b.cols as usize, b.width, b.height);
    let mut rects: Vec<PixelRect> = Vec::new();
    for (ty, (ra, rb)) in a.hashes.chunks(cols).zip(b.hashes.chunks(cols)).enumerate() {
        let mut tx = 0;
        while tx < cols {
            if ra[tx] == rb[tx] {
                tx += 1;
                continue;
            }
            let start = tx;
            while tx < cols && ra[tx] != rb[tx] {
                tx += 1;
            }
            let x = start as u32 * TILE;
            let y = ty as u32 * TILE;
            let w = (tx as u32 * TILE).min(width) - x;
            let h = (y + TILE).min(height) - y;
            // Merge with the run directly above when it spans the same
            // columns: the rectangle grows down.
            if let Some(above) = rects
                .iter_mut()
                .rev()
                .find(|r| r.x == x && r.w == w && r.y + r.h == y)
            {
                above.h += h;
            } else {
                rects.push(PixelRect::new(x, y, w, h));
            }
            if rects.len() > COALESCE_LIMIT {
                // Scattered damage becomes its bounding box anyway.
                return vec![PixelRect::new(0, 0, width, height)];
            }
        }
    }
    coalesce(rects, MAX_RECTS)
}

fn area(r: &PixelRect) -> u64 {
    u64::from(r.w) * u64::from(r.h)
}

fn bounds(a: &PixelRect, b: &PixelRect) -> PixelRect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.w).max(b.x + b.w);
    let y1 = (a.y + a.h).max(b.y + b.h);
    PixelRect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Past this many rectangles damage becomes its bounding box.
const COALESCE_LIMIT: usize = 64;

/// Reduce `rects` to at most `max`, each time merging the pair whose
/// bounding box adds the least area. Past [`COALESCE_LIMIT`] rectangles the
/// list becomes its bounding box, which bounds the cost; damage that
/// scattered is close to a full repaint anyway.
pub(crate) fn coalesce(mut rects: Vec<PixelRect>, max: usize) -> Vec<PixelRect> {
    let max = max.max(1);
    if rects.is_empty() {
        return rects;
    }
    if rects.len() > COALESCE_LIMIT {
        let all = rects[1..].iter().fold(rects[0], |acc, r| bounds(&acc, r));
        return vec![all];
    }
    while rects.len() > max {
        let mut best = (u64::MAX, 0, 1);
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let merged = area(&bounds(&rects[i], &rects[j]));
                let cost = merged.saturating_sub(area(&rects[i]) + area(&rects[j]));
                if cost < best.0 {
                    best = (cost, i, j);
                }
            }
        }
        let (_, i, j) = best;
        rects[i] = bounds(&rects[i], &rects[j]);
        rects.swap_remove(j);
    }
    rects
}

/// The same pixels as `rects`, as rectangles that do not overlap, so a
/// translucent draw clipped to each of them blends every pixel once.
/// Horizontal bands between the rectangles' edges, each band's covered
/// spans merged, and bands with the same spans merged down.
pub(crate) fn disjoint(rects: &[PixelRect]) -> Vec<PixelRect> {
    let rects: Vec<PixelRect> = rects
        .iter()
        .copied()
        .filter(|r| r.w > 0 && r.h > 0)
        .collect();
    if rects.len() <= 1 {
        return rects;
    }
    let mut edges: Vec<u32> = rects.iter().flat_map(|r| [r.y, r.y + r.h]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut out: Vec<PixelRect> = Vec::new();
    // The spans of the band just emitted, and where its rectangles start in
    // `out`, to merge a band with identical spans into them.
    let mut previous: Vec<(u32, u32)> = Vec::new();
    let mut previous_at = 0;
    let mut previous_end = 0;
    for pair in edges.windows(2) {
        let (y0, y1) = (pair[0], pair[1]);
        let mut spans: Vec<(u32, u32)> = rects
            .iter()
            .filter(|r| r.y <= y0 && r.y + r.h >= y1)
            .map(|r| (r.x, r.x + r.w))
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for (x0, x1) in spans {
            match merged.last_mut() {
                Some(last) if x0 <= last.1 => last.1 = last.1.max(x1),
                _ => merged.push((x0, x1)),
            }
        }
        if merged.is_empty() {
            previous.clear();
            continue;
        }
        if merged == previous && previous_end == y0 {
            for r in &mut out[previous_at..] {
                r.h += y1 - y0;
            }
        } else {
            previous_at = out.len();
            out.extend(
                merged
                    .iter()
                    .map(|&(x0, x1)| PixelRect::new(x0, y0, x1 - x0, y1 - y0)),
            );
            previous = merged;
        }
        previous_end = y1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quad::QuadInstance;

    fn covered(rects: &[PixelRect], w: u32, h: u32) -> Vec<u8> {
        let mut grid = vec![0u8; (w * h) as usize];
        for r in rects {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    grid[(y * w + x) as usize] += 1;
                }
            }
        }
        grid
    }

    #[test]
    fn disjoint_rectangles_cover_the_same_pixels_once() {
        let rects = [
            PixelRect::new(0, 0, 10, 10),
            PixelRect::new(5, 5, 10, 10),
            PixelRect::new(30, 2, 4, 30),
            PixelRect::new(30, 2, 4, 30),
            PixelRect::new(0, 20, 40, 3),
        ];
        let out = disjoint(&rects);
        let before = covered(&rects, 40, 40);
        let after = covered(&out, 40, 40);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(*a, u8::from(*b > 0), "{out:?}");
        }
        // Rows of the same span stay one rectangle.
        assert_eq!(
            disjoint(&[PixelRect::new(0, 0, 8, 4), PixelRect::new(0, 4, 8, 4)]),
            [PixelRect::new(0, 0, 8, 8)]
        );
    }

    #[test]
    fn coalescing_keeps_far_apart_damage_apart_and_covers_everything() {
        let mut rects: Vec<PixelRect> = (0..20).map(|i| PixelRect::new(i * 40, 0, 8, 8)).collect();
        rects.push(PixelRect::new(0, 900, 8, 8));
        let out = coalesce(rects.clone(), 4);
        assert_eq!(out.len(), 4);
        for r in &rects {
            assert!(
                out.iter().any(|o| o.intersect(*r) == Some(*r)),
                "{r:?} not covered by {out:?}"
            );
        }
        assert!(
            out.iter().any(|o| o.y == 900),
            "the far rectangle is not merged with the row: {out:?}"
        );
        let many: Vec<PixelRect> = (0..100).map(|i| PixelRect::new(i, i, 1, 1)).collect();
        assert_eq!(coalesce(many, 12), [PixelRect::new(0, 0, 100, 100)]);
    }

    fn quad(pos: [f32; 2], size: [f32; 2], color: [f32; 4]) -> QuadInstance {
        QuadInstance { pos, size, color }
    }

    fn sign(tracker: &mut DamageTracker, quads: &[QuadInstance]) -> Option<Vec<PixelRect>> {
        let tiles = tracker.begin(200, 100);
        tiles.draw(
            [200.0, 100.0],
            PixelRect::new(0, 0, 200, 100),
            0,
            &CpuDraw::Quads {
                replace: false,
                instances: quads,
            },
        );
        let damage = tracker.damage();
        tracker.commit();
        damage
    }

    #[test]
    fn a_changed_instance_damages_only_the_tiles_it_covers() {
        let mut tracker = DamageTracker::new(false);
        let base = [
            quad([0.0, 0.0], [200.0, 100.0], [0.1, 0.1, 0.1, 1.0]),
            quad([40.0, 40.0], [8.0, 18.0], [1.0, 1.0, 1.0, 1.0]),
        ];
        assert_eq!(sign(&mut tracker, &base), None, "the first frame is full");
        assert_eq!(sign(&mut tracker, &base), Some(vec![]), "nothing changed");
        let mut moved = base;
        moved[1].pos = [72.0, 40.0];
        // The old cell (tile 1,1) and the new one (tile 2,1).
        assert_eq!(
            sign(&mut tracker, &moved),
            Some(vec![PixelRect::new(32, 32, 64, 32)])
        );
        // The order of two overlapping draws matters where they overlap,
        // and only there.
        let swapped = [moved[1], moved[0]];
        assert_eq!(
            sign(&mut tracker, &swapped),
            Some(vec![PixelRect::new(64, 32, 32, 32)])
        );
        let mut forced = DamageTracker::new(true);
        sign(&mut forced, &base);
        assert_eq!(sign(&mut forced, &base), None, "forced full repaints");
    }

    #[test]
    fn a_different_size_or_a_reset_repaints_in_full() {
        let mut tracker = DamageTracker::new(false);
        let base = [quad([0.0, 0.0], [10.0, 10.0], [1.0; 4])];
        sign(&mut tracker, &base);
        tracker.begin(201, 100);
        assert_eq!(tracker.damage(), None);
        tracker.reset();
        assert_eq!(sign(&mut tracker, &base), None);
    }

    #[test]
    fn the_last_tile_row_and_column_are_clipped_to_the_frame() {
        let mut tracker = DamageTracker::new(false);
        let tiles = tracker.begin(70, 40);
        tiles.draw(
            [70.0, 40.0],
            PixelRect::new(0, 0, 70, 40),
            0,
            &CpuDraw::Quads {
                replace: true,
                instances: &[quad([0.0, 0.0], [70.0, 40.0], [0.0; 4])],
            },
        );
        tracker.commit();
        let tiles = tracker.begin(70, 40);
        tiles.draw(
            [70.0, 40.0],
            PixelRect::new(0, 0, 70, 40),
            0,
            &CpuDraw::Quads {
                replace: true,
                instances: &[quad([0.0, 0.0], [70.0, 40.0], [1.0; 4])],
            },
        );
        assert_eq!(tracker.damage(), Some(vec![PixelRect::new(0, 0, 70, 40)]));
    }
}
