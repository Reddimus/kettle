use std::time::{Duration, Instant};

use super::book::{Choice, IDLE_RELEASE, MAX_BUFFERS, SlotBook};
use super::damage::{DamageHistory, HISTORY_FRAMES, MAX_DAMAGE_RECTS, Rect, normalize};
use super::soft::SoftAges;
use super::{MAX_BUFFER_BYTES, MAX_BUFFER_SIDE, PixelFormat, buffer_extent, check_extent};

const W: u32 = 100;
const H: u32 = 50;
const OPAQUE: PixelFormat = PixelFormat::Opaque;

fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect::new(x, y, w, h)
}

// --- damage normalisation ---

#[test]
fn normalize_clips_to_the_buffer_and_drops_empty_rects() {
    let out = normalize(
        &[
            r(90, 40, 30, 30),
            r(10, 10, 0, 5),
            r(200, 0, 5, 5),
            r(0, 0, 5, 0),
        ],
        W,
        H,
    );
    assert_eq!(out, vec![r(90, 40, 10, 10)]);
}

#[test]
fn normalize_survives_rects_at_the_u32_edge() {
    let out = normalize(
        &[
            r(u32::MAX - 1, 0, u32::MAX, 10),
            r(5, 5, u32::MAX, u32::MAX),
        ],
        W,
        H,
    );
    assert_eq!(out, vec![r(5, 5, 95, 45)]);
}

#[test]
fn normalize_drops_contained_rects() {
    let out = normalize(&[r(0, 0, 50, 50), r(10, 10, 5, 5), r(20, 20, 30, 30)], W, H);
    assert_eq!(out, vec![r(0, 0, 50, 50)]);
}

#[test]
fn normalize_merges_adjacent_rows_of_one_span() {
    // A terminal's row damage: three full-width rows, touching.
    let out = normalize(&[r(0, 20, W, 10), r(0, 0, W, 10), r(0, 10, W, 10)], W, H);
    assert_eq!(out, vec![r(0, 0, W, 30)]);
}

#[test]
fn normalize_merges_side_by_side_rects_of_one_row() {
    let out = normalize(&[r(0, 5, 10, 8), r(10, 5, 20, 8)], W, H);
    assert_eq!(out, vec![r(0, 5, 30, 8)]);
}

#[test]
fn normalize_keeps_disjoint_rects_apart() {
    let out = normalize(&[r(60, 30, 5, 5), r(0, 0, 5, 5)], W, H);
    assert_eq!(out, vec![r(0, 0, 5, 5), r(60, 30, 5, 5)]);
}

#[test]
fn normalize_collapses_a_long_list_to_its_bounds() {
    let many: Vec<Rect> = (0..(MAX_DAMAGE_RECTS as u32 + 4))
        .map(|i| r(i * 5, (i % 2) * 20, 2, 2))
        .collect();
    let out = normalize(&many, W, H);
    assert_eq!(out, vec![r(0, 0, 97, 22)]);
}

// --- damage accumulation by buffer age ---

#[test]
fn age_zero_repaints_the_whole_buffer() {
    let mut history = DamageHistory::default();
    history.push(vec![r(0, 0, 5, 5)]);
    assert_eq!(
        history.repaint_region(0, &[r(1, 1, 1, 1)], W, H),
        vec![r(0, 0, W, H)]
    );
}

#[test]
fn age_one_repaints_only_this_frames_damage() {
    let mut history = DamageHistory::default();
    history.push(vec![r(0, 0, 5, 5)]);
    assert_eq!(
        history.repaint_region(1, &[r(40, 40, 2, 2)], W, H),
        vec![r(40, 40, 2, 2)]
    );
}

#[test]
fn age_two_adds_the_previous_frames_damage() {
    // Double buffering: the back buffer missed the last frame.
    let mut history = DamageHistory::default();
    history.push(vec![r(0, 0, 5, 5)]);
    history.push(vec![r(20, 20, 5, 5)]);
    let out = history.repaint_region(2, &[r(40, 40, 2, 2)], W, H);
    assert_eq!(out, vec![r(20, 20, 5, 5), r(40, 40, 2, 2)]);
}

#[test]
fn age_three_adds_the_two_previous_frames() {
    let mut history = DamageHistory::default();
    history.push(vec![r(0, 0, 5, 5)]);
    history.push(vec![r(20, 20, 5, 5)]);
    history.push(vec![r(60, 0, 5, 5)]);
    let out = history.repaint_region(3, &[], W, H);
    assert_eq!(out, vec![r(60, 0, 5, 5), r(20, 20, 5, 5)]);
}

#[test]
fn an_age_older_than_the_history_repaints_in_full() {
    let mut history = DamageHistory::default();
    for i in 0..HISTORY_FRAMES as u32 + 2 {
        history.push(vec![r(i, 0, 1, 1)]);
    }
    let covered = HISTORY_FRAMES as u8 + 1;
    assert_ne!(
        history.repaint_region(covered, &[], W, H),
        vec![r(0, 0, W, H)]
    );
    assert_eq!(
        history.repaint_region(covered + 1, &[], W, H),
        vec![r(0, 0, W, H)]
    );
}

#[test]
fn a_cleared_history_repaints_old_buffers_in_full() {
    let mut history = DamageHistory::default();
    history.push(vec![r(0, 0, 5, 5)]);
    history.clear();
    assert_eq!(history.repaint_region(2, &[], W, H), vec![r(0, 0, W, H)]);
    // Age 1 needs no history: only this frame's damage.
    assert_eq!(
        history.repaint_region(1, &[r(3, 3, 1, 1)], W, H),
        vec![r(3, 3, 1, 1)]
    );
}

/// Drive a book and a history together for a sequence of frames, drawing
/// only the repaint region, and check every buffer ends up exact.
#[test]
fn drawing_only_the_repaint_region_keeps_every_buffer_exact() {
    let mut book: SlotBook<Vec<u32>> = SlotBook::default();
    let mut history = DamageHistory::default();
    let mut truth = vec![0u32; (W * H) as usize];
    let now = Instant::now();
    // The compositor holds the newest buffer until the next commit (so the
    // presenter alternates two buffers) and, every fourth frame, a frame
    // longer (so a third appears).
    let mut held: Vec<u64> = Vec::new();
    for frame in 1..40u32 {
        let x = (frame * 7) % (W - 10);
        let y = (frame * 3) % (H - 10);
        let damage = vec![r(x, y, 10, 10), r(0, frame % H, W, 1)];
        for rect in &damage {
            for py in rect.y..rect.y + rect.height {
                for px in rect.x..rect.x + rect.width {
                    truth[(py * W + px) as usize] = frame;
                }
            }
        }
        let id = match book.choose(W, H, OPAQUE) {
            Choice::Reuse(id) => id,
            Choice::Allocate => book.insert(W, H, OPAQUE, now, vec![0xdead; (W * H) as usize]),
            other => panic!("frame {frame}: unexpected {other:?}"),
        };
        let age = book.get(id).unwrap().age;
        let repaint = history.repaint_region(age, &normalize(&damage, W, H), W, H);
        let slot = book.get_mut(id).unwrap();
        for rect in &repaint {
            for py in rect.y..rect.y + rect.height {
                for px in rect.x..rect.x + rect.width {
                    let i = (py * W + px) as usize;
                    slot.backing[i] = truth[i];
                }
            }
        }
        assert_eq!(slot.backing, truth, "frame {frame} age {age}");
        history.push(normalize(&damage, W, H));
        book.drew(id, now, HISTORY_FRAMES);
        book.committed(id);
        let keep = if frame % 4 == 0 { 2 } else { 1 };
        held.push(id);
        while held.len() > keep {
            book.released(held.remove(0));
        }
    }
    assert!(book.len() <= MAX_BUFFERS);
    assert!(book.len() >= 2, "the held buffer forced a second one");
}

// --- buffer bookkeeping ---

#[test]
fn a_promptly_released_buffer_is_reused_with_age_one() {
    let mut book: SlotBook<()> = SlotBook::default();
    let now = Instant::now();
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Allocate);
    let a = book.insert(W, H, OPAQUE, now, ());
    book.drew(a, now, HISTORY_FRAMES);
    book.committed(a);
    book.released(a);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Reuse(a));
    assert_eq!(book.get(a).unwrap().age, 1);
    assert_eq!(book.len(), 1);
}

#[test]
fn a_held_front_buffer_allocates_a_second_then_a_third_never_blocking() {
    let mut book: SlotBook<()> = SlotBook::default();
    let now = Instant::now();
    let a = book.insert(W, H, OPAQUE, now, ());
    book.drew(a, now, HISTORY_FRAMES);
    book.committed(a);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Allocate);
    let b = book.insert(W, H, OPAQUE, now, ());
    book.drew(b, now, HISTORY_FRAMES);
    book.committed(b);
    assert_eq!(book.get(a).unwrap().age, 2);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Allocate);
    let c = book.insert(W, H, OPAQUE, now, ());
    book.drew(c, now, HISTORY_FRAMES);
    book.committed(c);
    assert_eq!(book.len(), MAX_BUFFERS);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Busy);
    book.released(a);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Reuse(a));
    assert_eq!(book.get(a).unwrap().age, 3);
}

#[test]
fn the_newest_free_buffer_is_preferred() {
    let mut book: SlotBook<()> = SlotBook::default();
    let now = Instant::now();
    let a = book.insert(W, H, OPAQUE, now, ());
    let b = book.insert(W, H, OPAQUE, now, ());
    book.drew(a, now, HISTORY_FRAMES);
    book.drew(b, now, HISTORY_FRAMES);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Reuse(b));
    // An unknown age loses to any known one.
    book.abandoned(b);
    assert_eq!(book.choose(W, H, OPAQUE), Choice::Reuse(a));
}

#[test]
fn ages_past_the_history_become_unknown() {
    let mut book: SlotBook<()> = SlotBook::default();
    let now = Instant::now();
    let old = book.insert(W, H, OPAQUE, now, ());
    let new = book.insert(W, H, OPAQUE, now, ());
    book.drew(old, now, HISTORY_FRAMES);
    for _ in 0..HISTORY_FRAMES {
        book.drew(new, now, HISTORY_FRAMES);
    }
    assert_eq!(book.get(old).unwrap().age, HISTORY_FRAMES as u8 + 1);
    book.drew(new, now, HISTORY_FRAMES);
    assert_eq!(book.get(old).unwrap().age, 0);
}

#[test]
fn a_resize_or_format_change_replaces_free_buffers_of_the_old_shape() {
    let mut book: SlotBook<()> = SlotBook::default();
    let now = Instant::now();
    let a = book.insert(W, H, OPAQUE, now, ());
    book.drew(a, now, HISTORY_FRAMES);
    book.invalidate();
    assert_eq!(book.get(a).unwrap().age, 0);
    assert_eq!(book.choose(W + 1, H, OPAQUE), Choice::Replace(a));
    assert_eq!(
        book.choose(W, H, PixelFormat::Premultiplied),
        Choice::Replace(a)
    );
    assert_eq!(book.stale(W + 1, H, OPAQUE), vec![a]);
    // A busy buffer of the old shape is never replaced or listed stale.
    book.committed(a);
    assert_eq!(book.choose(W + 1, H, OPAQUE), Choice::Allocate);
    assert!(book.stale(W + 1, H, OPAQUE).is_empty());
}

#[test]
fn idle_release_keeps_the_newest_buffer_only() {
    let mut book: SlotBook<()> = SlotBook::default();
    let t0 = Instant::now();
    let a = book.insert(W, H, OPAQUE, t0, ());
    let b = book.insert(W, H, OPAQUE, t0, ());
    book.drew(a, t0, HISTORY_FRAMES);
    book.drew(b, t0, HISTORY_FRAMES);
    assert!(book.idle_spares(t0 + Duration::from_millis(10)).is_empty());
    assert_eq!(book.idle_deadline(), Some(t0 + IDLE_RELEASE));
    assert_eq!(book.idle_spares(t0 + IDLE_RELEASE), vec![a]);
    assert_eq!(book.spares(true), vec![a]);
    book.remove(a);
    assert_eq!(book.len(), 1);
    assert_eq!(book.idle_deadline(), None);
    assert_eq!(book.newest(), Some(b));
}

#[test]
fn a_busy_spare_is_never_released() {
    let mut book: SlotBook<()> = SlotBook::default();
    let t0 = Instant::now();
    let a = book.insert(W, H, OPAQUE, t0, ());
    let b = book.insert(W, H, OPAQUE, t0, ());
    book.drew(a, t0, HISTORY_FRAMES);
    book.committed(a);
    book.drew(b, t0, HISTORY_FRAMES);
    assert!(book.idle_spares(t0 + IDLE_RELEASE * 2).is_empty());
    assert!(book.spares(false).contains(&b));
    assert!(!book.spares(false).contains(&a));
}

#[test]
fn occlusion_releases_the_newest_buffer_too() {
    let mut book: SlotBook<()> = SlotBook::default();
    let t0 = Instant::now();
    let a = book.insert(W, H, OPAQUE, t0, ());
    book.drew(a, t0, HISTORY_FRAMES);
    assert!(book.spares(true).is_empty());
    assert_eq!(book.spares(false), vec![a]);
}

// --- sizes ---

#[test]
fn integer_scales_round_the_buffer_up_to_a_multiple() {
    assert_eq!(buffer_extent(1600, 1000, 1.0), (1600, 1000));
    assert_eq!(buffer_extent(1600, 1000, 2.0), (1600, 1000));
    assert_eq!(buffer_extent(1601, 999, 2.0), (1602, 1000));
    assert_eq!(buffer_extent(100, 100, 3.0), (102, 102));
    // Fractional scales go through a viewport: any size is valid.
    assert_eq!(buffer_extent(1601, 999, 1.5), (1601, 999));
    assert_eq!(buffer_extent(0, 0, 1.0), (1, 1));
    assert_eq!(buffer_extent(7, 7, f64::NAN), (7, 7));
}

#[test]
fn oversized_buffers_are_refused() {
    assert!(check_extent(7680, 4320).is_ok());
    assert!(check_extent(MAX_BUFFER_SIDE + 1, 1).is_err());
    let side = ((MAX_BUFFER_BYTES / 4) as f64).sqrt() as u32 + 1;
    assert!(check_extent(side, side).is_err());
}

// --- softbuffer age correction ---

#[test]
fn softbuffer_ages_from_before_a_resize_are_unknown() {
    // softbuffer's Wayland backend reports age 2 for the second buffer
    // after a resize although the resize reallocated it.
    let mut ages = SoftAges::default();
    ages.reset();
    assert_eq!(ages.age(0), 0);
    assert!(
        ages.presented(),
        "the first frame after a reset damages all"
    );
    assert_eq!(ages.age(2), 0, "the other buffer predates the resize");
    assert!(!ages.presented());
    assert_eq!(ages.age(2), 2);
    assert_eq!(ages.age(1), 1);
}

#[test]
fn a_dropped_softbuffer_frame_makes_the_next_age_unknown() {
    let mut ages = SoftAges::default();
    ages.reset();
    for _ in 0..3 {
        ages.presented();
    }
    assert_eq!(ages.age(1), 1);
    ages.abandoned();
    assert_eq!(ages.age(1), 0);
    ages.presented();
    assert_eq!(ages.age(1), 1);
}
