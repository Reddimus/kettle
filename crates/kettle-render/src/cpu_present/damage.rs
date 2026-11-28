//! Damage rectangles and the per-frame history that buffer age reads.

use std::collections::VecDeque;

/// A rectangle of buffer pixels: physical pixels, origin at the top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The whole of a `width` x `height` buffer.
    pub const fn full(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn right(&self) -> u64 {
        self.x as u64 + self.width as u64
    }

    pub const fn bottom(&self) -> u64 {
        self.y as u64 + self.height as u64
    }

    pub fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// This rectangle clipped to a `width` x `height` buffer, or `None` when
    /// nothing of it is inside.
    pub fn clipped(&self, width: u32, height: u32) -> Option<Rect> {
        if self.x >= width || self.y >= height || self.is_empty() {
            return None;
        }
        let right = self.right().min(u64::from(width)) as u32;
        let bottom = self.bottom().min(u64::from(height)) as u32;
        Some(Rect::new(self.x, self.y, right - self.x, bottom - self.y))
    }

    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The smallest rectangle holding both.
    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(
            x,
            y,
            (right - u64::from(x)) as u32,
            (bottom - u64::from(y)) as u32,
        )
    }

    /// Two rectangles whose union is exactly their two areas: the same
    /// columns touching or overlapping vertically, or the same rows
    /// touching or overlapping horizontally.
    fn merges_exactly_with(&self, other: &Rect) -> bool {
        let same_columns = self.x == other.x && self.width == other.width;
        let same_rows = self.y == other.y && self.height == other.height;
        (same_columns && u64::from(self.y) <= other.bottom() && u64::from(other.y) <= self.bottom())
            || (same_rows
                && u64::from(self.x) <= other.right()
                && u64::from(other.x) <= self.right())
    }
}

/// More rectangles than this collapse to their bounding box. Compositors
/// union damage anyway; a long list costs protocol traffic and per-rectangle
/// clipping in the renderer.
pub const MAX_DAMAGE_RECTS: usize = 16;

/// Damage clipped to the buffer, with empty and contained rectangles dropped
/// and exactly mergeable neighbours (adjacent rows of the same span, as a
/// terminal's row damage is) merged. Past [`MAX_DAMAGE_RECTS`] it becomes one
/// bounding box.
pub fn normalize(damage: &[Rect], width: u32, height: u32) -> Vec<Rect> {
    let mut out: Vec<Rect> = damage
        .iter()
        .filter_map(|rect| rect.clipped(width, height))
        .collect();
    // Merge to a fixed point; the lists are short, so quadratic is fine, but
    // keep a bound so a pathological list cannot spin.
    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds < 8 {
        changed = false;
        rounds += 1;
        let mut i = 0;
        while i < out.len() {
            let mut j = i + 1;
            while j < out.len() {
                let (a, b) = (out[i], out[j]);
                if a.contains(&b) {
                    out.swap_remove(j);
                    changed = true;
                } else if b.contains(&a) || a.merges_exactly_with(&b) {
                    out[i] = a.union(&b);
                    out.swap_remove(j);
                    changed = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    if out.len() > MAX_DAMAGE_RECTS {
        let bounds = out[1..].iter().fold(out[0], |acc, r| acc.union(r));
        out.clear();
        out.push(bounds);
    }
    out.sort_by_key(|r| (r.y, r.x));
    out
}

/// How many past frames' damage is kept. A buffer older than this is
/// repainted in full; with at most three buffers in flight an age above
/// four only happens after an idle buffer was skipped for a long time.
pub const HISTORY_FRAMES: usize = 4;

/// The damage of the last few drawn frames, newest last, all in the current
/// buffer size. A resize or a format change clears it.
#[derive(Debug, Default)]
pub struct DamageHistory {
    frames: VecDeque<Vec<Rect>>,
}

impl DamageHistory {
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// Record the (already normalized) damage of a frame just drawn.
    pub fn push(&mut self, damage: Vec<Rect>) {
        if self.frames.len() == HISTORY_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(damage);
    }

    /// What a buffer of `age` must repaint so that, after the renderer also
    /// draws `current`, it holds the newest frame.
    ///
    /// Age 0 means the contents are unknown. Age `n` means the buffer holds
    /// the frame drawn `n` frames ago, so the damage of the `n - 1` frames
    /// since then is missing from it, plus this frame's.
    pub fn repaint_region(&self, age: u8, current: &[Rect], width: u32, height: u32) -> Vec<Rect> {
        let missing = usize::from(age).saturating_sub(1);
        if age == 0 || missing > self.frames.len() {
            return normalize(&[Rect::full(width, height)], width, height);
        }
        let mut all: Vec<Rect> = current.to_vec();
        for frame in self.frames.iter().rev().take(missing) {
            all.extend_from_slice(frame);
        }
        normalize(&all, width, height)
    }
}

/// Regions the window system lost and asked to be drawn again (X11
/// `Expose`) while the retained buffer still holds them. The frame has not
/// changed there, so the renderer repaints nothing; the presenter sends
/// them again with its next present, whatever that frame's damage.
#[derive(Debug, Default)]
pub struct Exposures {
    rects: Vec<Rect>,
}

impl Exposures {
    /// More than this collapse to their bounding box until they are sent.
    const MAX: usize = 64;

    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        if self.rects.len() >= Self::MAX {
            let bounds = self.rects.iter().fold(rect, |acc, r| acc.union(r));
            self.rects.clear();
            self.rects.push(bounds);
            return;
        }
        self.rects.push(rect);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// What a present of a `width` x `height` buffer sends: `current`, the
    /// frame's own (normalized) damage, and every region exposed since the
    /// last present, clipped to the buffer. The exposures are spent.
    pub fn upload(&mut self, current: &[Rect], width: u32, height: u32) -> Vec<Rect> {
        if self.rects.is_empty() {
            return current.to_vec();
        }
        let mut all = current.to_vec();
        all.append(&mut self.rects);
        normalize(&all, width, height)
    }
}

#[cfg(test)]
mod exposure_tests {
    use super::*;

    #[test]
    fn exposed_regions_go_out_with_the_next_present_once() {
        let mut exposed = Exposures::default();
        assert_eq!(exposed.upload(&[], 100, 50), Vec::<Rect>::new());
        exposed.add(Rect::new(10, 10, 20, 8));
        // Partly outside the buffer: clipped.
        exposed.add(Rect::new(90, 40, 30, 30));
        exposed.add(Rect::new(0, 0, 0, 5));
        let current = [Rect::new(0, 0, 4, 4)];
        let sent = exposed.upload(&current, 100, 50);
        assert_eq!(
            sent,
            [
                Rect::new(0, 0, 4, 4),
                Rect::new(10, 10, 20, 8),
                Rect::new(90, 40, 10, 10)
            ]
        );
        assert!(exposed.is_empty());
        assert_eq!(exposed.upload(&current, 100, 50), current);
    }

    #[test]
    fn a_storm_of_exposures_stays_bounded_and_covers_them_all() {
        let mut exposed = Exposures::default();
        for i in 0..1000 {
            exposed.add(Rect::new(i % 97, i % 41, 3, 2));
        }
        assert!(exposed.rects.len() <= Exposures::MAX);
        let sent = exposed.upload(&[], 200, 100);
        for i in 0..1000 {
            let r = Rect::new(i % 97, i % 41, 3, 2);
            assert!(sent.iter().any(|s| s.contains(&r)), "{r:?} not sent");
        }
    }
}
