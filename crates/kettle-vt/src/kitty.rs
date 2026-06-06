//! Kitty graphics protocol decoder. Common transmission path (RGB/RGBA/PNG,
//! zlib optional, chunked) plus the advanced ops kettle supports:
//!
//! - `a=t` transmit only (store, don't display) — later shown with `a=p`
//! - `a=T` transmit and display
//! - `a=p` put a previously transmitted image (by `i=` id) at the cursor
//! - `a=d` delete images (all, or by `i=` id)
//! - `z=`  z-index ordering between images
//! - `U=1` *virtual placement*: the image is stored and a rows×cols virtual
//!   placement registered, but nothing is drawn at the cursor — it is shown
//!   later via Unicode placeholder text (see [`crate::placeholder`])
//!
//! - `a=f` transmit animation frames (full or partial-rect over a
//!   previous-frame / `Y=` color / transparent canvas; `r=` edits a frame
//!   in place); `a=a` control (current frame / run-stop / loop / gap);
//!   `a=c` copy a rectangle between frames; `a=d,d=f` frame delete
//! - `a=p,P=,Q=` *relative placement*: recorded with its `H/V` cell offset
//!   and parent; the on-screen position is resolved from the parent at
//!   render time (a later cycle). Parent deletion cascades to relatives.
//!
//! Spec: `kitty/docs/graphics-protocol.rst`.

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;

use crate::image::{ImageData, Placed};

/// Cycle 578: per-slot accumulator cap for kitty `m=1` chunked
/// transmissions. A hostile PTY emitter can chain continuation
/// chunks indefinitely; without a cap, the in-flight `String`
/// grows until the host OOMs. 384 MiB covers the largest realistic
/// payload (8192² × 4 RGBA bytes = 256 MiB base64-encoded at the
/// 4/3 expansion ≈ 342 MiB) with margin, and stays well below any
/// realistic host RAM. Pairs with the cycle-576 256-MiB decoded-
/// image cap in `ImageData::from_encoded`.
const MAX_KITTY_PAYLOAD_BYTES: usize = 384 * 1024 * 1024;

/// Cycle 579: cap on concurrent in-flight kitty image transmissions
/// keyed by `i=`. Without it, a hostile PTY emitter can send 100 000+
/// distinct `i=` values, each with one small `m=1` chunk that never
/// receives its terminating `m=0`, and grow the `in_flight` HashMap
/// without bound. 32 sits well above any realistic client (kitty,
/// ueberzug, and chafa typically interleave one or two transmissions);
/// past that, new ids are refused until the existing slots complete or
/// are evicted by the per-slot cap above.
const MAX_IN_FLIGHT_SLOTS: usize = 32;

/// Cycle 764: global cap on the *sum* of all in-flight transmission payloads
/// (every `in_flight` slot plus the animation `frame_in_flight` slot). The
/// per-slot `MAX_KITTY_PAYLOAD_BYTES` (384 MiB) is sized for one legitimate
/// 8192²-pixel image, but on its own `MAX_IN_FLIGHT_SLOTS` (32) × 384 MiB ≈
/// 12 GiB could be accumulated by a hostile emitter chaining many large partial
/// transmissions. 1 GiB total comfortably allows a couple of concurrent
/// max-size images (or many small ones) while bounding the worst case to a
/// fraction of host RAM. On breach the offending slot is dropped.
const MAX_TOTAL_IN_FLIGHT_BYTES: usize = 1024 * 1024 * 1024;

/// Cycle 580: cap on per-image animation frames. Each successful
/// `a=f` frame transmission appends a `Frame` (carrying an `ImageData`
/// Arc) to `frames[id]`; without a cap, an attacker can chain
/// 100 000+ frame transmissions for one id and grow the Vec
/// unboundedly. 256 sits well above any realistic animation (`.gif`
/// files top out around 200 frames; kitty's animation protocol
/// imposes no spec-level bound but real content stays small). Past
/// the cap, additional frame pushes are silently dropped — the
/// existing animation continues to play with the frames already
/// captured.
const MAX_FRAMES_PER_IMAGE: usize = 256;

/// Cycle 581: cap on the number of completed (`store`) images kept
/// around for later placement. Each entry holds an `ImageData` Arc
/// whose payload can be up to MAX_IMAGE_DIM² × 4 = 256 MiB (cycle
/// 576), so 1000 distinct successful transmissions = up to 256 GB
/// resident. 64 sits well above any realistic terminal usage (a
/// terminal showing icons + a couple animations rarely transmits
/// more than a dozen images; even chafa's slideshow mode resets
/// each frame). Past the cap, new `a=T` completions are dropped:
/// the image data is decoded (work was done) but not added to
/// `store`, so it can be drawn at-cursor but can't be replaced
/// later via `a=p,i=...`.
const MAX_STORED_IMAGES: usize = 64;

#[derive(Default)]
struct Acc {
    control: String,
    payload: String,
}

/// A `U=1` virtual placement: the image is fit into a `cols`×`rows`
/// rectangle and displayed later via Unicode placeholder cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualPlacement {
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
}

/// A *relative placement* (`a=p,P=,Q=`): this placement is positioned
/// `(h, v)` cells from the top-left of its parent placement (positive = right
/// / down). Most useful with Unicode placeholders — the real image tracks a
/// placeholder that moves with the text. Render-time position resolution is
/// a later cycle; this records the relation. kitty
/// `graphics-protocol.rst:682`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativePlacement {
    pub parent_img: u32,
    pub parent_placement: u32,
    pub h: i32,
    pub v: i32,
}

/// One animation frame of an image (`a=f`). `img` is the *fully composed*
/// frame (partial-rect transmissions are already blended onto their canvas).
/// `gap_ms`: `0` = unset, `> 0` = display this many ms, `< 0` = *gapless*
/// (skipped on playback, kept only as base data).
#[derive(Debug, Clone)]
pub struct Frame {
    pub img: ImageData,
    pub gap_ms: i32,
}

/// Per-image animation control state (`a=a`), set by the client and read by
/// the renderer's playback loop (a later cycle). `current` is 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationState {
    pub current: u32,
    /// `false` = stopped (`s=1`); `true` = running (`s=2|3`).
    pub running: bool,
    /// `s=2`: wait for more frames at the end instead of looping.
    pub loading: bool,
    /// Loop count: `0` = infinite, `n` = play `n` times (kitty `v`,
    /// normalized: `v=1`→infinite→0 here, `v=n`→`n-1`).
    pub loops: u32,
    /// Gap (ms) of the *root* frame (frame 1 = the base image); only
    /// settable via `a=a,r=1,z=` since the root has no gap by default.
    pub root_gap: i32,
}

impl Default for AnimationState {
    fn default() -> Self {
        AnimationState {
            current: 1,
            running: false,
            loading: false,
            loops: 0,
            root_gap: 0,
        }
    }
}

/// The 0-based frame index to display *now*.
///
/// `gaps[i]` is frame `i+1`'s gap in milliseconds: `> 0` dwell that long,
/// `<= 0` *gapless* (kept as base data but never dwelt on, so skipped for
/// display — kitty `graphics-protocol.rst:909`). `gaps[0]` is the root /
/// base-image frame. Pure and deterministic so the renderer's clock is the
/// only non-testable part.
///
/// - Stopped (`s=1`): the explicitly selected `current` frame, clamped.
/// - Running: time is mapped over the displayable frames. `loops == 0`
///   (kitty `v=1`) loops forever; a finite count stops on the last
///   displayable frame after that many full passes. `loading` (`s=2`)
///   never loops — it holds on the last frame waiting for more frames.
/// - No displayable frame ⇒ hold on `current`.
pub fn current_frame(gaps: &[i32], st: &AnimationState, elapsed_ms: u128) -> usize {
    if gaps.is_empty() {
        return 0;
    }
    let clamp_current = || (st.current.max(1) as usize - 1).min(gaps.len() - 1);
    // Cycle 849 (audit): two cheap passes over `gaps` instead of collecting a
    // `Vec<(usize, u128)>` of the displayable frames. This runs from
    // `Terminal::placements()` on every paint of a playing animation, so a
    // running GIF allocated + freed a Vec per frame. Pass 1 accumulates the
    // total dwell + the last displayable (`g > 0`) index; the modulo walk below
    // re-filters `gaps` directly. A frame is displayable when its gap is `> 0`
    // (kitty `graphics-protocol.rst:909`).
    let mut total: u128 = 0;
    let mut last_shown: Option<usize> = None;
    for (i, &g) in gaps.iter().enumerate() {
        if g > 0 {
            total += g as u128;
            last_shown = Some(i);
        }
    }
    if !st.running {
        return clamp_current();
    }
    // No displayable frame ⇒ hold on `current`. Past this `let`, `last_shown` is
    // set and `total >= 1` (every counted frame contributed `g >= 1`).
    let Some(last_shown) = last_shown else {
        return clamp_current();
    };
    // Finite, non-loading loop count: freeze on the last shown frame once all
    // passes have elapsed.
    if !st.loading && st.loops > 0 && elapsed_ms >= total * st.loops as u128 {
        return last_shown;
    }
    // Loading mode never loops: hold the last shown frame at/after the end.
    if st.loading && elapsed_ms >= total {
        return last_shown;
    }
    let mut t = elapsed_ms % total;
    for (idx, &g) in gaps.iter().enumerate() {
        if g <= 0 {
            continue;
        }
        let g = g as u128;
        if t < g {
            return idx;
        }
        t -= g;
    }
    last_shown
}

/// What a kitty APC resolved to.
pub enum KittyOut {
    None,
    Place(Placed),
    Delete {
        all: bool,
        id: Option<u32>,
    },
    /// A virtual placement was (re)registered for image `id`; nothing is
    /// drawn now — the renderer composites it where placeholder cells appear.
    Virtual {
        id: u32,
    },
    /// An animation frame was transmitted or the animation control state
    /// changed for image `id`; nothing is drawn at the cursor — the caller
    /// snapshots `frames`/`animation` and runs the playback clock.
    Animate {
        id: u32,
    },
    /// A relative placement was registered for `(id, placement)`; nothing is
    /// drawn at the cursor — its position is derived from the parent
    /// placement at render time.
    Relative {
        id: u32,
        placement: u32,
    },
}

/// Reassembles chunked transmissions and remembers transmitted images so
/// they can be placed later by id.
#[derive(Default)]
pub struct KittyState {
    in_flight: HashMap<u32, Acc>,
    store: HashMap<u32, ImageData>,
    virtual_placements: HashMap<u32, VirtualPlacement>,
    /// The single in-flight `a=f` frame transmission (`id`, accumulator).
    /// Continuation chunks omit `i=`, so a slot — not an id map — is right;
    /// the protocol only allows one transmission in flight at a time.
    frame_in_flight: Option<(u32, Acc)>,
    /// Animation frames appended after the root image, per image id.
    frames: HashMap<u32, Vec<Frame>>,
    /// Animation control state per image id (`a=a`).
    anim: HashMap<u32, AnimationState>,
    /// Relative placements, keyed by `(image id, placement id)`.
    rel: HashMap<(u32, u32), RelativePlacement>,
}

impl KittyState {
    /// Feed one APC `G` body (between `ESC _ G` and `ESC \`).
    pub fn feed(&mut self, body: &str) -> KittyOut {
        let (control, payload) = body.split_once(';').unwrap_or((body, ""));
        let kv = parse_control(control);
        let id = kv.get("i").and_then(|v| v.parse().ok()).unwrap_or(0u32);
        // Continuation chunks carry only `m` (no `a`); route them to the
        // frame accumulator if a frame — and only a frame — is in flight.
        let action = match kv.get("a") {
            Some(a) => a.as_str(),
            // Continuation chunks carry only `m`; route to the frame
            // accumulator when a frame — and only a frame — is in flight.
            None if self.frame_in_flight.is_some() && !self.in_flight.contains_key(&id) => "f",
            None => "t",
        };
        let z = kv.get("z").and_then(|v| v.parse().ok()).unwrap_or(0i32);

        let virt = kv.get("U").map(|v| v == "1").unwrap_or(false);
        let dim = |k: &str| kv.get(k).and_then(|v| v.parse::<u32>().ok());

        // Control-only ops are never chunked.
        if action == "d" {
            self.in_flight.clear();
            self.frame_in_flight = None;
            let target = kv.get("d").map(|s| s.as_str()).unwrap_or("a");
            // `d=f|F`: delete only the animation frames/state, keep the image.
            if target.eq_ignore_ascii_case("f") {
                if id != 0 {
                    self.frames.remove(&id);
                    self.anim.remove(&id);
                } else {
                    self.frames.clear();
                    self.anim.clear();
                }
                // Surface an (now-empty) snapshot so the caller drops it.
                return KittyOut::Animate { id };
            }
            return match target {
                "i" | "I" => {
                    self.store.remove(&id);
                    self.virtual_placements.remove(&id);
                    self.frames.remove(&id);
                    self.anim.remove(&id);
                    // A placement group dies with its parent: drop this
                    // image's relatives and any placement parented to it.
                    self.rel
                        .retain(|&(img, _), r| img != id && r.parent_img != id);
                    KittyOut::Delete {
                        all: false,
                        id: Some(id),
                    }
                }
                _ => {
                    self.store.clear();
                    self.virtual_placements.clear();
                    self.frames.clear();
                    self.anim.clear();
                    self.rel.clear();
                    KittyOut::Delete {
                        all: true,
                        id: None,
                    }
                }
            };
        }
        if action == "a" {
            // Animation control. Record state for the renderer playback loop.
            // Cycle 582: gate the entry on the saturation cap. Updates to an
            // already-tracked id are always allowed (no growth); a brand-new
            // id past saturation is a no-op for animation control so an
            // attacker can't grow `anim` indefinitely by sending `a=a,i=N`
            // for distinct N without ever transmitting an image.
            if !self.anim.contains_key(&id) && self.anim.len() >= MAX_STORED_IMAGES {
                return KittyOut::None;
            }
            let st = self.anim.entry(id).or_default();
            if let Some(c) = dim("c") {
                st.current = c.max(1);
            }
            match kv.get("s").and_then(|v| v.parse::<u32>().ok()) {
                Some(1) => {
                    st.running = false;
                    st.loading = false;
                    st.loops = 0; // stopping resets the loop counter
                }
                Some(2) => {
                    st.running = true;
                    st.loading = true;
                }
                Some(3) => {
                    st.running = true;
                    st.loading = false;
                }
                _ => {}
            }
            if let Some(v) = kv.get("v").and_then(|v| v.parse::<u32>().ok())
                && v != 0
            {
                st.loops = if v == 1 { 0 } else { v - 1 };
            }
            // `r` + `z`: set the gap of an existing 1-based frame. `r=1` is
            // the root frame (base image); `r>=2` is `frames[r-2]`.
            if z != 0
                && let Some(r) = dim("r")
            {
                if r <= 1 {
                    // Cycle 582: already inside the `action == "a"` arm so the
                    // saturation gate above protects this `entry` from growth
                    // (we only get here if id was admitted). Safe to keep.
                    self.anim.entry(id).or_default().root_gap = z;
                } else if let Some(fr) = self
                    .frames
                    .get_mut(&id)
                    .and_then(|f| f.get_mut(r as usize - 2))
                {
                    fr.gap_ms = z;
                }
            }
            return KittyOut::Animate { id };
        }
        if action == "c" {
            // Frame composition: copy a rectangle from source frame `r`
            // onto destination frame `c` (both 1-based; 1 = root/base).
            let src = dim("r").and_then(|n| self.frame_image(id, n));
            let dn = dim("c").unwrap_or(0);
            let Some(src) = src else {
                return KittyOut::None;
            };
            let w = dim("w").unwrap_or(src.width);
            let h = dim("h").unwrap_or(src.height);
            let replace = kv.get("C").map(|v| v == "1").unwrap_or(false);
            let (dx, dy) = (dim("X").unwrap_or(0), dim("Y").unwrap_or(0));
            let (sx, sy) = (dim("x").unwrap_or(0), dim("y").unwrap_or(0));
            if let Some(patch) = src.crop(sx, sy, w, h) {
                if dn <= 1 {
                    if let Some(b) = self.store.get_mut(&id) {
                        b.compose(&patch, dx, dy, replace);
                    }
                } else if let Some(fr) = self
                    .frames
                    .get_mut(&id)
                    .and_then(|f| f.get_mut(dn as usize - 2))
                {
                    fr.img.compose(&patch, dx, dy, replace);
                }
            }
            return KittyOut::Animate { id };
        }
        if action == "f" {
            // Transmit animation frame data (chunked like an image). The
            // first chunk carries `i=`/control; continuations carry only
            // `m`, so the id + control come from the in-flight slot.
            let more = kv.get("m").map(|v| v == "1").unwrap_or(false);
            let exceeded = {
                let slot = self
                    .frame_in_flight
                    .get_or_insert_with(|| (id, Acc::default()));
                if slot.1.control.is_empty() {
                    slot.0 = id;
                    slot.1.control = control.to_string();
                }
                slot.1.payload.push_str(payload.trim());
                slot.1.payload.len() > MAX_KITTY_PAYLOAD_BYTES
            };
            // Cycle 578: defense against an attacker chaining `m=1`
            // continuation chunks indefinitely. Drop the slot once it
            // crosses the per-slot cap. Cycle 764: also enforce the global
            // cap (this frame slot + every in_flight slot) so concurrent
            // image + animation transmissions can't sum past the ceiling.
            if exceeded || self.in_flight_bytes() > MAX_TOTAL_IN_FLIGHT_BYTES {
                self.frame_in_flight = None;
                return KittyOut::None;
            }
            if more {
                return KittyOut::None;
            }
            // `take()` is safe because the `get_or_insert_with(...)` above
            // guarantees `frame_in_flight` is `Some` by this point — we
            // either matched an existing slot or just inserted one. Using
            // `expect` documents that invariant so a future refactor that
            // breaks it fails with a pinpointed message.
            let (fid, Acc { control, payload }) = self
                .frame_in_flight
                .take()
                .expect("frame_in_flight is Some after get_or_insert_with");
            if let Some(patch) = decode(&control, &payload) {
                let fc = parse_control(&control);
                let g = |k: &str| fc.get(k).and_then(|v| v.parse::<u32>().ok());
                let gap = fc.get("z").and_then(|v| v.parse().ok()).unwrap_or(0i32);
                let (x, y) = (g("x").unwrap_or(0), g("y").unwrap_or(0));
                let replace = fc.get("X").map(|v| v == "1").unwrap_or(false);
                let edit = g("r");
                let bg_frame = g("c");
                let bg_color = g("Y");
                // Base-image dimensions size the canvas (fallback: patch).
                let (bw, bh) = self
                    .store
                    .get(&fid)
                    .map(|b| (b.width, b.height))
                    .unwrap_or((patch.width, patch.height));
                let partial = x != 0
                    || y != 0
                    || bg_frame.is_some()
                    || bg_color.is_some()
                    || edit.is_some()
                    || patch.width != bw
                    || patch.height != bh;
                let frame_img = if !partial {
                    patch
                } else {
                    let mut canvas = edit
                        .and_then(|r| self.frame_image(fid, r))
                        .or_else(|| bg_frame.and_then(|n| self.frame_image(fid, n)))
                        .or_else(|| {
                            bg_color.and_then(|c| {
                                ImageData::solid(
                                    bw,
                                    bh,
                                    [(c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8],
                                )
                            })
                        })
                        .or_else(|| ImageData::solid(bw, bh, [0, 0, 0, 0]))
                        .unwrap_or_else(|| patch.clone());
                    canvas.compose(&patch, x, y, replace);
                    canvas
                };
                // `r` (>=2) edits an existing frame in place; else append.
                if let Some(r) = edit
                    && r >= 2
                    && let Some(fr) = self
                        .frames
                        .get_mut(&fid)
                        .and_then(|f| f.get_mut(r as usize - 2))
                {
                    fr.img = frame_img;
                    if gap != 0 {
                        fr.gap_ms = gap;
                    }
                } else {
                    // Cycle 580: cap per-image frame count. Beyond
                    // MAX_FRAMES_PER_IMAGE, drop the push silently —
                    // the animation already has plenty to play, and
                    // refusing growth bounds the per-id memory ceiling.
                    // Cycle 582: also cap the frames-map slot count so a
                    // hostile emitter can't grow the map keyset itself
                    // by firing `a=f,i=N` for many distinct N. Same shape
                    // as the `anim` / `store` / `virtual_placements`
                    // saturation gates.
                    if self.frames.contains_key(&fid) || self.frames.len() < MAX_STORED_IMAGES {
                        let frames = self.frames.entry(fid).or_default();
                        if frames.len() < MAX_FRAMES_PER_IMAGE {
                            frames.push(Frame {
                                img: frame_img,
                                gap_ms: gap,
                            });
                        }
                    }
                }
            }
            return KittyOut::Animate { id: fid };
        }
        if action == "q" {
            return KittyOut::None; // capability query — nothing to render
        }
        if action == "p" {
            // `a=p,U=1` registers a virtual placement (shown later via
            // placeholder text); plain `a=p` puts the image at the cursor.
            if virt {
                // Cycle 582: saturation gate. Updates to an already-tracked
                // id are always allowed (no growth); brand-new ids past the
                // cap are dropped so an attacker can't grow the placement
                // map by firing `a=p,U=1,i=N` for many distinct N.
                if !self.virtual_placements.contains_key(&id)
                    && self.virtual_placements.len() >= MAX_STORED_IMAGES
                {
                    return KittyOut::None;
                }
                self.virtual_placements.insert(
                    id,
                    VirtualPlacement {
                        cols: dim("c").unwrap_or(0),
                        rows: dim("r").unwrap_or(0),
                        z,
                    },
                );
                return KittyOut::Virtual { id };
            }
            // `P=` (parent image id) ⇒ a relative placement: recorded and
            // positioned from the parent at render time, not at the cursor.
            if let Some(parent_img) = dim("P") {
                let geti = |k: &str| kv.get(k).and_then(|v| v.parse::<i32>().ok());
                let placement = dim("p").unwrap_or(0);
                let key = (id, placement);
                // Cycle 582: saturation gate on the (id, placement) key
                // space. `rel` keys are pairs; cap at MAX_STORED_IMAGES²
                // would be huge — use the same flat MAX_STORED_IMAGES so
                // total entries stay bounded with `store`.
                if !self.rel.contains_key(&key) && self.rel.len() >= MAX_STORED_IMAGES {
                    return KittyOut::None;
                }
                self.rel.insert(
                    key,
                    RelativePlacement {
                        parent_img,
                        parent_placement: dim("Q").unwrap_or(0),
                        h: geti("H").unwrap_or(0),
                        v: geti("V").unwrap_or(0),
                    },
                );
                return KittyOut::Relative { id, placement };
            }
            return match self.store.get(&id) {
                Some(img) => KittyOut::Place(Placed {
                    img: img.clone(),
                    id: Some(id),
                    z,
                }),
                None => KittyOut::None,
            };
        }

        // Transmit (optionally + display): only the *first* chunk carries the
        // full control; continuation chunks carry just `m` (and maybe `q`).
        let more = kv.get("m").map(|v| v == "1").unwrap_or(false);
        // Cycle 579: refuse new transmissions once the in-flight map
        // is saturated. A continuation chunk for an existing slot is
        // always allowed — only brand-new ids count against the cap.
        if !self.in_flight.contains_key(&id) && self.in_flight.len() >= MAX_IN_FLIGHT_SLOTS {
            return KittyOut::None;
        }
        let exceeded = {
            let acc = self.in_flight.entry(id).or_default();
            if acc.control.is_empty() {
                acc.control = control.to_string();
            }
            acc.payload.push_str(payload.trim());
            acc.payload.len() > MAX_KITTY_PAYLOAD_BYTES
        };
        // Cycle 578: per-slot cap. Cycle 764: also enforce the global cap
        // across all slots so concurrent large transmissions can't sum past
        // MAX_TOTAL_IN_FLIGHT_BYTES. Either breach drops this slot.
        if exceeded || self.in_flight_bytes() > MAX_TOTAL_IN_FLIGHT_BYTES {
            self.in_flight.remove(&id);
            return KittyOut::None;
        }
        if more {
            return KittyOut::None;
        }
        let Acc { control, payload } = self.in_flight.remove(&id).unwrap_or_default();
        let first = parse_control(&control);
        let Some(img) = decode(&control, &payload) else {
            return KittyOut::None;
        };
        if id != 0 {
            // Cycle 581: cap stored-image count. An update to an
            // already-present id is always allowed (replaces in
            // place — no growth); a brand-new id past saturation is
            // refused so an attacker can't grow `store` indefinitely
            // by completing distinct `i=` transmissions.
            if self.store.contains_key(&id) || self.store.len() < MAX_STORED_IMAGES {
                self.store.insert(id, img.clone());
            }
        }
        // `U=1` (possibly combined with `a=T`): store + register a virtual
        // placement, but draw nothing at the cursor.
        if first.get("U").map(|v| v == "1").unwrap_or(false) {
            let fz = first.get("z").and_then(|v| v.parse().ok()).unwrap_or(z);
            // Cycle 582: saturation gate (same shape as the standalone
            // `a=p,U=1` path above). The store-side gate above doesn't
            // imply this one — store and virtual_placements are independent
            // maps, and `U=1` on an existing-id update would silently grow
            // virtual_placements without it.
            if !self.virtual_placements.contains_key(&id)
                && self.virtual_placements.len() >= MAX_STORED_IMAGES
            {
                return KittyOut::None;
            }
            self.virtual_placements.insert(
                id,
                VirtualPlacement {
                    cols: first.get("c").and_then(|v| v.parse().ok()).unwrap_or(0),
                    rows: first.get("r").and_then(|v| v.parse().ok()).unwrap_or(0),
                    z: fz,
                },
            );
            return KittyOut::Virtual { id };
        }
        // `T` displays now; bare `t` only stores.
        if first.get("a").map(|s| s.as_str()).unwrap_or("t") == "T" {
            let fz = first.get("z").and_then(|v| v.parse().ok()).unwrap_or(z);
            KittyOut::Place(Placed {
                img,
                id: (id != 0).then_some(id),
                z: fz,
            })
        } else {
            KittyOut::None
        }
    }

    /// A stored image by id (for compositing Unicode-placeholder cells).
    pub fn image(&self, id: u32) -> Option<&ImageData> {
        self.store.get(&id)
    }

    /// The registered virtual placement for an image id, if any.
    pub fn virtual_placement(&self, id: u32) -> Option<&VirtualPlacement> {
        self.virtual_placements.get(&id)
    }

    /// Animation frames appended to an image (root frame is the base image
    /// itself and is not included here), in transmit order.
    pub fn frames(&self, id: u32) -> &[Frame] {
        self.frames.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Animation control state for an image id, if the client set any.
    pub fn animation(&self, id: u32) -> Option<&AnimationState> {
        self.anim.get(&id)
    }

    /// The relative-placement relation for `(image id, placement id)`.
    pub fn relative_placement(&self, id: u32, placement: u32) -> Option<&RelativePlacement> {
        self.rel.get(&(id, placement))
    }

    /// Cycle 764: total bytes currently buffered across every in-flight
    /// transmission — all `in_flight` slots plus the animation `frame_in_flight`
    /// slot. Used to enforce `MAX_TOTAL_IN_FLIGHT_BYTES`. O(slots) ≤ 32, called
    /// once per chunk, so trivially cheap.
    fn in_flight_bytes(&self) -> usize {
        self.in_flight
            .values()
            .map(|a| a.payload.len())
            .sum::<usize>()
            + self
                .frame_in_flight
                .as_ref()
                .map_or(0, |(_, a)| a.payload.len())
    }

    /// Test-only accessor for the cycle-579 in-flight slot cap drift guard.
    #[cfg(test)]
    fn in_flight_len_for_test(&self) -> usize {
        self.in_flight.len()
    }

    /// Test-only accessor for the cycle-581 store slot cap drift guard.
    #[cfg(test)]
    fn store_len_for_test(&self) -> usize {
        self.store.len()
    }

    /// Test-only accessor for the cycle-582 anim slot cap drift guard.
    /// `anim` is the most acute remaining per-id HashMap because an
    /// attacker can grow it with `a=a,i=N` for arbitrary N without ever
    /// transmitting a real image.
    #[cfg(test)]
    fn anim_len_for_test(&self) -> usize {
        self.anim.len()
    }

    /// A clone of a 1-based frame's pixels: `n <= 1` is the root/base image,
    /// `n >= 2` is `frames[n-2]` (used as a composition background).
    fn frame_image(&self, id: u32, n: u32) -> Option<ImageData> {
        if n <= 1 {
            self.store.get(&id).cloned()
        } else {
            self.frames
                .get(&id)
                .and_then(|f| f.get(n as usize - 2))
                .map(|fr| fr.img.clone())
        }
    }
}

fn parse_control(s: &str) -> HashMap<String, String> {
    s.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Inflate a zlib (`o=z`) kitty payload, never allocating more than `cap`
/// bytes. A decompression bomb — a tiny compressed stream that inflates to
/// gigabytes — returns `None` instead of OOMing/aborting the process. Cycle
/// 814 (audit): `.take(cap + 1)` bounds the read; reading past `cap` proves the
/// stream is over-budget, so we reject it rather than silently truncate.
fn inflate_bounded(compressed: &[u8], cap: u64) -> Option<Vec<u8>> {
    let mut d = flate2::read::ZlibDecoder::new(compressed).take(cap + 1);
    let mut out = Vec::new();
    d.read_to_end(&mut out).ok()?;
    if out.len() as u64 > cap {
        return None;
    }
    Some(out)
}

fn decode(control: &str, b64: &str) -> Option<ImageData> {
    let kv = parse_control(control);
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let raw = if kv.get("o").map(|s| s == "z").unwrap_or(false) {
        inflate_bounded(&raw, crate::image::MAX_IMAGE_BYTES)?
    } else {
        raw
    };
    match kv.get("f").map(|s| s.as_str()).unwrap_or("32") {
        "100" => ImageData::from_encoded(&raw),
        "32" => {
            let w: u32 = kv.get("s")?.parse().ok()?;
            let h: u32 = kv.get("v")?.parse().ok()?;
            ImageData::new(w, h, raw)
        }
        "24" => {
            let w: u32 = kv.get("s")?.parse().ok()?;
            let h: u32 = kv.get("v")?.parse().ok()?;
            let mut rgba = Vec::with_capacity(raw.len() / 3 * 4);
            for px in raw.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            ImageData::new(w, h, rgba)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One opaque RGBA pixel (f=32,s=1,v=1): bytes [1,2,3,4].
    const PX: &str = "AQIDBA==";

    /// Cycle 814 (audit) drift guard for the kitty `o=z` decompression-bomb
    /// defense. A few dozen bytes of zlib inflate to 64 KiB of zeros; under a
    /// generous cap it decodes, under a tiny cap it's rejected (None) WITHOUT
    /// allocating the full output. This pins the `.take(cap+1)` bound so a
    /// future refactor can't re-introduce the unbounded `read_to_end`.
    #[test]
    fn inflate_bounded_rejects_a_decompression_bomb() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&vec![0u8; 64 * 1024]).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(
            compressed.len() < 1024,
            "zero-run should compress tiny (got {})",
            compressed.len()
        );
        // Generous cap → inflates to the full 64 KiB.
        let ok = super::inflate_bounded(&compressed, 64 * 1024).expect("fits under cap");
        assert_eq!(ok.len(), 64 * 1024);
        // Tiny cap (1 KiB) → rejected without materializing the 64 KiB output.
        assert!(super::inflate_bounded(&compressed, 1024).is_none());
    }

    #[test]
    fn transmit_and_display_virtual_placement() {
        let mut k = KittyState::default();
        let out = k.feed(&format!("a=T,U=1,i=7,c=2,r=3,f=32,s=1,v=1;{PX}"));
        assert!(
            matches!(out, KittyOut::Virtual { id: 7 }),
            "a=T,U=1 must register a virtual placement, not draw at cursor"
        );
        assert!(
            k.image(7).is_some(),
            "image still stored for later compositing"
        );
        assert_eq!(
            k.virtual_placement(7).copied(),
            Some(VirtualPlacement {
                cols: 2,
                rows: 3,
                z: 0
            })
        );
    }

    #[test]
    fn transmit_then_put_virtual() {
        let mut k = KittyState::default();
        // a=t stores only (no placement).
        assert!(matches!(
            k.feed(&format!("a=t,i=8,f=32,s=1,v=1;{PX}")),
            KittyOut::None
        ));
        // a=p,U=1 registers the virtual placement by id.
        let out = k.feed("a=p,U=1,i=8,c=4,r=1,z=5");
        assert!(matches!(out, KittyOut::Virtual { id: 8 }));
        assert_eq!(
            k.virtual_placement(8).copied(),
            Some(VirtualPlacement {
                cols: 4,
                rows: 1,
                z: 5
            })
        );
        // Plain a=p (no U) still draws at the cursor.
        assert!(matches!(
            k.feed("a=p,i=8"),
            KittyOut::Place(p) if p.id == Some(8)
        ));
    }

    #[test]
    fn delete_clears_virtual_placement() {
        let mut k = KittyState::default();
        k.feed(&format!("a=T,U=1,i=9,c=1,r=1,f=32,s=1,v=1;{PX}"));
        assert!(k.virtual_placement(9).is_some());
        k.feed("a=d,d=i,i=9");
        assert!(
            k.virtual_placement(9).is_none(),
            "delete-by-id drops the virtual placement too"
        );
    }

    #[test]
    fn animation_frames_transmit_and_control() {
        let mut k = KittyState::default();
        // Root image (id 3) shown.
        assert!(matches!(
            k.feed(&format!("a=T,i=3,f=32,s=1,v=1;{PX}")),
            KittyOut::Place(_)
        ));
        // Two frames with gaps; neither draws at the cursor (Animate, not
        // Place/None).
        assert!(matches!(
            k.feed(&format!("a=f,i=3,f=32,s=1,v=1,z=40;{PX}")),
            KittyOut::Animate { id: 3 }
        ));
        k.feed(&format!("a=f,i=3,f=32,s=1,v=1,z=-1;{PX}"));
        let fr = k.frames(3);
        assert_eq!(fr.len(), 2);
        assert_eq!(fr[0].gap_ms, 40);
        assert_eq!(fr[1].gap_ms, -1, "z<0 ⇒ gapless");
        assert!(k.frames(99).is_empty());

        // Control: make frame 2 current, run looping 5 times (v=5 ⇒ 4).
        k.feed("a=a,i=3,c=2,s=3,v=5");
        let a = k.animation(3).copied().unwrap();
        assert_eq!(a.current, 2);
        assert!(a.running && !a.loading);
        assert_eq!(a.loops, 4);

        // r>=2 sets a frame's gap; r=1 sets the root frame's gap.
        k.feed("a=a,i=3,r=2,z=48");
        assert_eq!(k.frames(3)[0].gap_ms, 48, "r=2 → frames[0]");
        k.feed("a=a,i=3,r=1,z=70");
        assert_eq!(k.animation(3).unwrap().root_gap, 70, "r=1 → root frame gap");

        // Stop resets running + loop counter.
        k.feed("a=a,i=3,s=1");
        let a = k.animation(3).copied().unwrap();
        assert!(!a.running);
        assert_eq!(a.loops, 0);

        // d=f deletes frames/anim but keeps the image.
        k.feed("a=d,d=f,i=3");
        assert!(k.frames(3).is_empty());
        assert!(k.animation(3).is_none());
        assert!(k.image(3).is_some(), "d=f keeps the base image");
    }

    #[test]
    fn playback_timing_maps_elapsed_to_frame() {
        let run = |loops, loading| AnimationState {
            current: 1,
            running: true,
            loading,
            loops,
            ..AnimationState::default()
        };
        // Stopped → the selected current frame, clamped.
        let stop = AnimationState {
            current: 3,
            running: false,
            loading: false,
            loops: 0,
            ..AnimationState::default()
        };
        assert_eq!(current_frame(&[10, 10, 10, 10], &stop, 9_999), 2);
        assert_eq!(current_frame(&[], &stop, 0), 0);
        let stop_oob = AnimationState {
            current: 99,
            ..stop
        };
        assert_eq!(current_frame(&[10, 10], &stop_oob, 0), 1, "clamped");

        // Infinite loop over [100,200,300] (total 600).
        let g = [100, 200, 300];
        let inf = run(0, false);
        assert_eq!(current_frame(&g, &inf, 0), 0);
        assert_eq!(current_frame(&g, &inf, 150), 1);
        assert_eq!(current_frame(&g, &inf, 350), 2);
        assert_eq!(current_frame(&g, &inf, 650), 0, "wraps (650 % 600 = 50)");

        // Gapless frame (g<=0) is never displayed, only skipped over.
        let gl = [100, -1, 200];
        assert_eq!(current_frame(&gl, &inf, 50), 0);
        assert_eq!(current_frame(&gl, &inf, 150), 2, "frame 2 is gapless");
        assert_eq!(current_frame(&gl, &inf, 350), 0, "300ms shown cycle");
        // All gapless ⇒ hold on current.
        assert_eq!(current_frame(&[0, -1], &run(0, false), 1234), 0);

        // Finite loop count: freeze on the last shown frame after N passes.
        let g2 = [100, 200]; // total 300
        let fin = run(2, false);
        assert_eq!(current_frame(&g2, &fin, 0), 0);
        assert_eq!(current_frame(&g2, &fin, 250), 1);
        assert_eq!(current_frame(&g2, &fin, 600), 1, "2 passes done → freeze");
        assert_eq!(current_frame(&g2, &fin, 500), 1, "500 % 300 = 200 → f2");

        // Loading mode never loops: holds the last frame at/after the end.
        let load = run(0, true);
        assert_eq!(current_frame(&g2, &load, 50), 0);
        assert_eq!(current_frame(&g2, &load, 250), 1);
        assert_eq!(current_frame(&g2, &load, 900), 1, "loading waits at end");

        // Cycle 849: the freeze / loading-hold must land on the last
        // *displayable* frame, skipping a trailing gapless one — the two-pass
        // rewrite tracks the last `g > 0` index, so guard that directly.
        let g3 = [100, 200, -1]; // displayable: idx 0,1; total 300
        assert_eq!(
            current_frame(&g3, &run(1, false), 999),
            1,
            "finite-loop freeze skips a trailing gapless frame"
        );
        assert_eq!(
            current_frame(&g3, &run(0, true), 999),
            1,
            "loading-hold skips a trailing gapless frame"
        );
    }

    #[test]
    fn relative_placement_recorded_and_lifetime() {
        let mut k = KittyState::default();
        // Parent image 1 (placement 1) and child image 2.
        k.feed(&format!("a=T,i=1,f=32,s=1,v=1;{PX}"));
        k.feed(&format!("a=t,i=2,f=32,s=1,v=1;{PX}"));
        // Relative placement: image 2 / placement 7, parent = img 1 / p 1,
        // offset 3 right, -2 up. Drawn nowhere now (Relative, not Place).
        assert!(matches!(
            k.feed("a=p,i=2,p=7,P=1,Q=1,H=3,V=-2"),
            KittyOut::Relative {
                id: 2,
                placement: 7
            }
        ));
        assert_eq!(
            k.relative_placement(2, 7).copied(),
            Some(RelativePlacement {
                parent_img: 1,
                parent_placement: 1,
                h: 3,
                v: -2,
            })
        );
        // Deleting the parent image cascades: the relative is dropped.
        k.feed("a=d,d=i,i=1");
        assert!(
            k.relative_placement(2, 7).is_none(),
            "relative dies with its parent"
        );

        // A relative whose own image is deleted also goes away.
        k.feed("a=p,i=2,p=8,P=9,Q=1");
        assert!(k.relative_placement(2, 8).is_some());
        k.feed("a=d,d=i,i=2");
        assert!(k.relative_placement(2, 8).is_none());
    }

    #[test]
    fn partial_rect_frame_and_compose() {
        use base64::Engine;
        let b = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
        let mut k = KittyState::default();
        // Base image: 2×1, pixels black then white.
        let base = b(&[0, 0, 0, 255, 255, 255, 255, 255]);
        k.feed(&format!("a=T,i=1,f=32,s=2,v=1;{base}"));
        // Partial frame: replace just pixel (1,0) with red over the base
        // (c=1 = root background, x=1, X=1 replace), 1×1 patch.
        let red = b(&[255, 0, 0, 255]);
        k.feed(&format!("a=f,i=1,c=1,x=1,X=1,f=32,s=1,v=1,z=30;{red}"));
        let fr = k.frames(1);
        assert_eq!(fr.len(), 1);
        assert_eq!(fr[0].gap_ms, 30);
        // Frame = base with (1,0) → red; (0,0) still black.
        assert_eq!(&fr[0].img.rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&fr[0].img.rgba[4..8], &[255, 0, 0, 255]);
        assert_eq!((fr[0].img.width, fr[0].img.height), (2, 1));

        // a=c: copy frame-2's pixel (0,0) onto the root at (0,0), replace.
        k.feed("a=c,i=1,r=2,c=1,w=1,h=1,C=1");
        assert_eq!(
            &k.image(1).unwrap().rgba[0..4],
            &[0, 0, 0, 255],
            "frame2(0,0) is black → root(0,0) stays black"
        );
        // Copy frame-2's red pixel (1,0) onto root (0,0).
        k.feed("a=c,i=1,r=2,c=1,w=1,h=1,x=1,C=1");
        assert_eq!(
            &k.image(1).unwrap().rgba[0..4],
            &[255, 0, 0, 255],
            "root(0,0) now red from frame2(1,0)"
        );

        // Editing an existing frame in place (r=2) updates its gap too.
        k.feed(&format!("a=f,i=1,r=2,x=0,X=1,f=32,s=1,v=1,z=99;{red}"));
        assert_eq!(k.frames(1).len(), 1, "r=2 edits, does not append");
        assert_eq!(k.frames(1)[0].gap_ms, 99);
        assert_eq!(&k.frames(1)[0].img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn chunked_frame_transmission() {
        let mut k = KittyState::default();
        k.feed(&format!("a=T,i=4,f=32,s=1,v=1;{PX}"));
        // Frame split across chunks (m=1 continuation).
        assert!(matches!(
            k.feed("a=f,i=4,f=32,s=1,v=1,m=1;AQID"),
            KittyOut::None
        ));
        assert!(k.frames(4).is_empty(), "incomplete frame not stored yet");
        k.feed("m=0;BA==");
        assert_eq!(k.frames(4).len(), 1, "frame completes on final chunk");
    }

    /// Cycle 578: pins the per-slot chunked-transmission accumulator
    /// cap. The behavioral drift guard (`chunked_transmission_caps_*`
    /// below) actually exercises the cap; this test is the cheap
    /// always-on snapshot that the constant hasn't drifted from its
    /// documented sizing (8192² × 4 RGBA × base64 4/3 ≈ 342 MiB plus
    /// ~12% margin = 384 MiB).
    #[test]
    fn kitty_payload_cap_fits_8k_rgba_base64_with_margin() {
        // 8192² × 4 RGBA = 256 MiB. base64 expansion = 4/3.
        let realistic_max_base64 = 8192usize * 8192 * 4 * 4 / 3;
        assert!(
            super::MAX_KITTY_PAYLOAD_BYTES > realistic_max_base64,
            "cap {} must fit a legitimate 8192² RGBA base64 payload ({})",
            super::MAX_KITTY_PAYLOAD_BYTES,
            realistic_max_base64
        );
        // Sanity floor (>= 1 MiB) and ceiling (<= 1 GiB). const-block
        // form so clippy's `assertions_on_constants` lint is satisfied
        // — these are compile-time invariants of the constant itself.
        const _: () = assert!((1 << 20) < super::MAX_KITTY_PAYLOAD_BYTES);
        const _: () = assert!(super::MAX_KITTY_PAYLOAD_BYTES <= 1 << 30);
        // Cycle 764: the global in-flight cap must allow at least one legitimate
        // max-size slot (else valid large images would always be refused) and
        // stay well below the naive slots × per-slot worst case (~12 GiB).
        const _: () = assert!(super::MAX_TOTAL_IN_FLIGHT_BYTES >= super::MAX_KITTY_PAYLOAD_BYTES);
        const _: () = assert!(
            super::MAX_TOTAL_IN_FLIGHT_BYTES
                < super::MAX_IN_FLIGHT_SLOTS * super::MAX_KITTY_PAYLOAD_BYTES
        );
    }

    /// Cycle 582 drift guard: `a=a,i=N` for many distinct N must not
    /// grow the `anim` HashMap past `MAX_STORED_IMAGES`. This is the
    /// most acute remaining per-id surface because animation control
    /// doesn't require a prior transmission — every `a=a` admits a
    /// new id by default. Updates to already-tracked ids still work.
    #[test]
    fn kitty_anim_slot_cap_holds_against_distinct_id_flood() {
        let mut k = KittyState::default();
        // Fill anim with MAX_STORED_IMAGES distinct ids via `a=a`.
        for id in 1..=super::MAX_STORED_IMAGES as u32 {
            k.feed(&format!("a=a,i={id},s=2"));
        }
        assert_eq!(k.anim_len_for_test(), super::MAX_STORED_IMAGES);
        // Distinct id past the cap is refused (no growth).
        let overflow = super::MAX_STORED_IMAGES as u32 + 1;
        k.feed(&format!("a=a,i={overflow},s=2"));
        assert_eq!(
            k.anim_len_for_test(),
            super::MAX_STORED_IMAGES,
            "anim id {overflow} past saturation must be refused"
        );
        // Update to an existing tracked id is still accepted.
        k.feed("a=a,i=1,s=1");
        assert_eq!(
            k.anim_len_for_test(),
            super::MAX_STORED_IMAGES,
            "update to existing id must not grow the map"
        );
    }

    /// Cycle 581 drift guard: completing more than `MAX_STORED_IMAGES`
    /// distinct `a=T` transmissions must not grow `store` past the
    /// cap. An update to an already-stored id is still accepted
    /// (replaces in place; no growth).
    #[test]
    fn kitty_stored_images_cap_holds_against_distinct_id_flood() {
        let mut k = KittyState::default();
        // Fill the store with MAX_STORED_IMAGES distinct ids.
        for id in 1..=super::MAX_STORED_IMAGES as u32 {
            k.feed(&format!("a=T,i={id},f=32,s=1,v=1;{PX}"));
        }
        assert_eq!(k.store_len_for_test(), super::MAX_STORED_IMAGES);
        // One more distinct id: refused; map size unchanged.
        let overflow = super::MAX_STORED_IMAGES as u32 + 1;
        k.feed(&format!("a=T,i={overflow},f=32,s=1,v=1;{PX}"));
        assert_eq!(
            k.store_len_for_test(),
            super::MAX_STORED_IMAGES,
            "distinct id {overflow} past saturation must be refused"
        );
        // Update to an existing id: accepted (replaces in place);
        // map size still unchanged.
        k.feed("a=T,i=1,f=32,s=1,v=1;AQIDBA==");
        assert_eq!(
            k.store_len_for_test(),
            super::MAX_STORED_IMAGES,
            "update to existing id must replace in place (no growth)"
        );
    }

    /// Cycle 580 drift guard: chaining more than `MAX_FRAMES_PER_IMAGE`
    /// frame transmissions for one image must not grow the `frames[id]`
    /// Vec past the cap. Verifies the silent-drop behavior so a hostile
    /// PTY emitter can't OOM kettle by spamming `a=f` frames at one id.
    #[test]
    fn kitty_frames_per_image_cap_holds_against_flood() {
        let mut k = KittyState::default();
        // Establish the base image (`a=T` transmit).
        k.feed(&format!("a=T,i=7,f=32,s=1,v=1;{PX}"));
        // Spam more than the cap's worth of frames; each is a tiny
        // 1×1 RGBA frame so the test allocation stays modest.
        for _ in 0..(super::MAX_FRAMES_PER_IMAGE + 16) {
            k.feed(&format!("a=f,i=7,f=32,s=1,v=1;{PX}"));
        }
        assert_eq!(
            k.frames(7).len(),
            super::MAX_FRAMES_PER_IMAGE,
            "frames Vec for one id must clamp at MAX_FRAMES_PER_IMAGE"
        );
    }

    /// Cycle 579 drift guard: a hostile PTY emitter that fires
    /// `MAX_IN_FLIGHT_SLOTS + 1` distinct `i=` values (each with a
    /// single `m=1` chunk that never receives its `m=0` terminator)
    /// must not grow the `in_flight` HashMap past the cap. Brand-new
    /// ids past the saturation point are refused; continuation chunks
    /// for already-tracked ids still work.
    #[test]
    fn kitty_in_flight_slot_cap_refuses_new_ids_past_saturation() {
        let mut k = KittyState::default();
        // Fill MAX_IN_FLIGHT_SLOTS distinct ids, each with an `m=1`
        // chunk so the slot is held open.
        for id in 1..=super::MAX_IN_FLIGHT_SLOTS as u32 {
            k.feed(&format!("a=T,i={id},f=32,s=1,v=1,m=1;AQID"));
        }
        assert_eq!(
            k.in_flight_len_for_test(),
            super::MAX_IN_FLIGHT_SLOTS,
            "first MAX_IN_FLIGHT_SLOTS distinct ids should all be tracked"
        );
        // One more distinct id is refused without growing the map.
        let overflow_id = super::MAX_IN_FLIGHT_SLOTS as u32 + 1;
        k.feed(&format!("a=T,i={overflow_id},f=32,s=1,v=1,m=1;AQID"));
        assert_eq!(
            k.in_flight_len_for_test(),
            super::MAX_IN_FLIGHT_SLOTS,
            "id {overflow_id} past the saturation point must be refused"
        );
        // A continuation chunk for an already-tracked id still works.
        // (Verify by feeding the final m=0 chunk for id=1 and checking
        // the slot is removed — completed.)
        k.feed("i=1,m=0;BA==");
        assert_eq!(
            k.in_flight_len_for_test(),
            super::MAX_IN_FLIGHT_SLOTS - 1,
            "completing id=1 should free a slot"
        );
    }

    /// Cycle 578 behavioral drift guard: a single chunk whose payload
    /// exceeds `MAX_KITTY_PAYLOAD_BYTES` must drop the in-flight slot
    /// rather than continue accumulating. Allocates ~384 MiB, so it's
    /// `#[ignore]` by default; run via
    /// `cargo test -p kettle-vt -- --ignored kitty_chunk_payload_cap`.
    #[test]
    #[ignore = "allocates ~384 MiB; opt-in via --ignored"]
    fn kitty_chunk_payload_cap_drops_oversize_in_flight() {
        let mut k = KittyState::default();
        // First chunk already exceeds the cap. The implementation
        // pushes the payload then checks length, so this is the
        // worst-case single-shot path.
        let oversize = "A".repeat(super::MAX_KITTY_PAYLOAD_BYTES + 1);
        let out = k.feed(&format!("a=T,i=99,f=32,s=1,v=1,m=1;{oversize}"));
        assert!(matches!(out, KittyOut::None));
        // A normal final chunk for the same id must NOT reassemble the
        // dropped payload — the slot was cleared by the cap.
        let out2 = k.feed("m=0;AAAA");
        assert!(matches!(out2, KittyOut::None));
    }
}
