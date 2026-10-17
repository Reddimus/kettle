#[test]
fn animation_graphics_frame_and_composition_replies_preserve_core_refresh() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=401,q=2,f=24,s=1,v=1,C=1;AAAA\x1b\\");
        assert!(ordinary_reply_bytes(&harness).is_empty());
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=f,i=401,f=24,s=1,v=1;/wAA\x1b\\");
        if synchronized {
            assert!(ordinary_reply_bytes(&harness).is_empty());
            assert!(harness.anims.lock().unwrap().get(&401).is_none());
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(ordinary_reply_bytes(&harness), ["\x1b_Gi=401,r=2;OK\x1b\\"]);
        assert_eq!(
            harness.anims.lock().unwrap().get(&401).unwrap().imgs[1]
                .rgba
                .as_slice(),
            &[255, 0, 0, 255]
        );
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=c,i=401,r=1,c=2,w=1,h=1,C=1\x1b\\");
        if synchronized {
            assert!(ordinary_reply_bytes(&harness).is_empty());
            assert_eq!(
                harness.anims.lock().unwrap().get(&401).unwrap().imgs[1]
                    .rgba
                    .as_slice(),
                &[255, 0, 0, 255]
            );
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(ordinary_reply_bytes(&harness), ["\x1b_Gi=401;OK\x1b\\"]);
        let animations = harness.anims.lock().unwrap();
        let animation = animations.get(&401).unwrap();
        assert_eq!(animation.imgs.len(), 2);
        assert_eq!(animation.imgs[0].rgba.as_slice(), &[0, 0, 0, 255]);
        assert_eq!(animation.imgs[1].rgba.as_slice(), &[0, 0, 0, 255]);
    }
}

use super::fuzzing::{SyncGraphicsHarness, registries};
use super::{
    DeferredGraphicsJournal, GraphicsActionContext, GraphicsEvent, GraphicsEventBatch,
    GraphicsRegistries, GraphicsScroll, GraphicsScrollDirection, ImageHistoryPruner, Images,
    PTY_PUMP_QUEUE_DEPTH, Placement, PtyGeometry, RelEntry, SharedTerm, SyncFlushContext,
    SyncGraphicsContext, SyncGraphicsDispatch, TermSize, VersionedPtyGeometry, VirtualEntry,
    advance_terminal_bytes_with_commit_hook, apply_graphics_batch, apply_graphics_event,
    apply_sync_dispatch, chunk_needs_graphics_gate, clear_reflowed_regular_placements,
    commit_local_geometry, finish_deferred_sync, receive_pty_chunk, recompute_kitty_placements,
    scroll_regular_placements,
};
use crate::event::{OutputWakeGate, TermEvent};
use crate::{EventProxy, ImageData, Waker};
use alacritty_terminal::Term;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{Processor, SYNC_MARKER_CAPACITY};
use kettle_vt::{Chunk, Extractor, PlacementParams};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn image(channel: u8) -> ImageData {
    ImageData::new(1, 1, vec![channel, 0, 0, 255]).expect("test pixel")
}

fn placement(id: u32) -> Placement {
    Placement {
        abs_line: id as u64,
        col: 0,
        cell_cols: 1,
        cell_rows: 1,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 1.0,
        display_rows: 1.0,
        img: image(id as u8),
        source_rect: None,
        source_crop: None,
        id: Some(id),
        placement_id: 1,
        kitty_params: Some(PlacementParams::default()),
        z: 0,
    }
}

fn virtual_entry(id: u32) -> VirtualEntry {
    VirtualEntry {
        img: image(id as u8),
        placement_id: 1,
        cols: 1,
        rows: 1,
        z: 0,
    }
}

fn relative_entry(id: u32) -> RelEntry {
    RelEntry {
        img: image(id as u8),
        parent_img: 1,
        parent_placement: 1,
        h: 0,
        v: 0,
        z: 0,
        params: PlacementParams::default(),
    }
}

fn ordinary_reply_bytes(harness: &SyncGraphicsHarness) -> Vec<String> {
    harness
        .replies
        .try_iter()
        .filter_map(|event| match event {
            TermEvent::PtyWrite(reply) => Some(reply),
            _ => None,
        })
        .collect()
}

#[test]
fn ordinary_graphics_no_cursor_movement_is_an_accepted_placement() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=T,i=381,p=9,f=24,s=1,v=1,C=1;AAAA\x1b\\");
        if synchronized {
            assert!(ordinary_reply_bytes(&harness).is_empty());
            assert!(harness.active_ids().is_empty());
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(ordinary_reply_bytes(&harness), ["\x1b_Gi=381,p=9;OK\x1b\\"]);
        assert_eq!(harness.active_ids(), [381]);
        assert_eq!(
            harness.term.lock().unwrap().grid().cursor.point,
            Point::new(Line(0), Column(0))
        );
    }
}

#[test]
fn ordinary_graphics_invalid_crop_is_not_acknowledged_as_placed() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=t,i=382,q=2,f=24,s=1,v=1;AAAA\x1b\\");
        assert!(ordinary_reply_bytes(&harness).is_empty());
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=p,i=382,p=10,q=1,x=1,C=1\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026l");
        }
        let responses = ordinary_reply_bytes(&harness);
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with("\x1b_Gi=382,p=10;EINVAL:"));
        assert!(responses[0].ends_with("\x1b\\"));
        assert!(harness.active_ids().is_empty());
    }
}

#[test]
fn named_placement_changes_form_only_after_admission() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=7,p=1,q=2,f=32,s=1,v=1,C=1;AQID/w==\x1b\\");
        harness.feed(b"\x1b_Ga=T,i=8,p=2,q=2,f=32,s=1,v=1,C=1;CQgH/w==\x1b\\");
        for (command, form) in [
            ("a=p,i=7,p=1,P=8,Q=2,H=1,C=1", 2),
            ("a=p,i=7,p=1,U=1,c=1,r=1", 1),
            ("a=p,i=7,p=1,P=8,Q=2,H=1,C=1", 2),
            ("a=p,i=7,p=1,C=1", 0),
            ("a=p,i=7,p=1,U=1,c=1,r=1", 1),
            ("a=p,i=7,p=1,C=1", 0),
        ] {
            if synchronized {
                harness.feed(b"\x1b[?2026h");
            }
            harness.feed(format!("\x1b_G{command}\x1b\\").as_bytes());
            if synchronized {
                harness.feed(b"\x1b[?2026l");
            }
            assert_eq!(ordinary_reply_bytes(&harness), ["\x1b_Gi=7,p=1;OK\x1b\\"]);
            assert_eq!(harness.active_ids().contains(&7), form == 0);
            assert_eq!(
                harness.virtuals.lock().unwrap().contains_key(&(7, 1)),
                form == 1
            );
            assert_eq!(
                harness.relatives.lock().unwrap().contains_key(&(7, 1)),
                form == 2
            );
            if synchronized {
                harness.feed(b"\x1b[?2026h");
            }
            harness.feed(b"\x1b_Ga=p,i=7,p=1,P=8,Q=99,q=1,C=1\x1b\\");
            if synchronized {
                harness.feed(b"\x1b[?2026l");
            }
            let replies = ordinary_reply_bytes(&harness);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].starts_with("\x1b_Gi=7,p=1;ENOPARENT:"));
            assert_eq!(harness.active_ids().contains(&7), form == 0);
            assert_eq!(
                harness.virtuals.lock().unwrap().contains_key(&(7, 1)),
                form == 1
            );
            assert_eq!(
                harness.relatives.lock().unwrap().contains_key(&(7, 1)),
                form == 2
            );
        }
    }
}

#[test]
fn ordinary_graphics_rejected_crop_preserves_an_existing_physical_placement() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=392,p=18,q=2,f=24,s=1,v=1,c=2,r=1,C=1;AAAA\x1b\\");
        assert!(ordinary_reply_bytes(&harness).is_empty());
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=p,i=392,p=18,q=1,x=1,c=9,r=3,C=1\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026l");
        }
        let replies = ordinary_reply_bytes(&harness);
        assert_eq!(replies.len(), 1);
        assert!(replies[0].starts_with("\x1b_Gi=392,p=18;EINVAL:"));
        assert!(replies[0].ends_with("\x1b\\"));
        let images = harness.images.lock().unwrap();
        assert_eq!(images.len(), 1);
        let previous = &images[0];
        assert_eq!((previous.id, previous.placement_id), (Some(392), 18));
        let params = previous.kitty_params.expect("original geometry");
        assert_eq!((params.source_x, params.columns, params.rows), (0, 2, 1));
    }
}

#[test]
fn ordinary_graphics_virtual_acknowledgement_follows_registration() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b[?2026h\x1b_Ga=T,i=383,p=11,U=1,c=2,r=3,f=24,s=1,v=1;AAAA\x1b\\");
    assert!(ordinary_reply_bytes(&harness).is_empty());
    assert!(harness.virtuals.lock().unwrap().is_empty());
    harness.feed(b"\x1b[?2026l");
    assert_eq!(
        ordinary_reply_bytes(&harness),
        ["\x1b_Gi=383,p=11;OK\x1b\\"]
    );
    let virtuals = harness.virtuals.lock().unwrap();
    let placement = virtuals
        .get(&(383, 11))
        .expect("accepted virtual placement");
    assert_eq!((placement.cols, placement.rows), (2, 3));
    assert!(harness.active_ids().is_empty());
}

#[test]
fn ordinary_graphics_missing_explicit_parent_does_not_acknowledge_success() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=384,p=12,q=2,f=24,s=1,v=1,C=1;AAAA\x1b\\");
        harness.feed(b"\x1b_Ga=t,i=385,q=2,f=24,s=1,v=1;AAAA\x1b\\");
        assert!(ordinary_reply_bytes(&harness).is_empty());
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=p,i=385,p=13,P=384,Q=99,q=1\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026l");
        }
        let replies = ordinary_reply_bytes(&harness);
        assert_eq!(replies.len(), 1);
        assert!(replies[0].starts_with("\x1b_Gi=385,p=13;ENOPARENT:"));
        assert!(replies[0].ends_with("\x1b\\"));
        assert_eq!(harness.active_ids(), [384]);
        assert!(!harness.relatives.lock().unwrap().contains_key(&(385, 13)));
    }
}

#[test]
fn ordinary_graphics_combined_transmission_keeps_a_relative_destination() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b_Ga=T,i=386,p=14,q=2,f=24,s=1,v=1,C=1;AAAA\x1b\\");
    harness.feed(b"\x1b_Ga=T,i=387,p=15,P=386,Q=14,H=1,V=1,f=24,s=1,v=1;AQID\x1b\\");
    assert_eq!(
        ordinary_reply_bytes(&harness),
        ["\x1b_Gi=387,p=15;OK\x1b\\"]
    );
    assert_eq!(harness.active_ids(), [386]);
    let relatives = harness.relatives.lock().unwrap();
    let relative = relatives
        .get(&(387, 15))
        .expect("accepted relative destination");
    assert_eq!(
        (
            relative.parent_img,
            relative.parent_placement,
            relative.h,
            relative.v
        ),
        (386, 14, 1, 1)
    );
}

#[test]
fn ordinary_graphics_cyclic_relative_update_preserves_its_previous_parent() {
    for synchronized in [false, true] {
        for parent_placement in [0, 1] {
            let mut harness = SyncGraphicsHarness::new();
            harness.feed(b"\x1b_Ga=T,i=1,p=1,q=2,f=24,s=1,v=1,C=1;AAAA\x1b\\");
            harness.feed(b"\x1b_Ga=T,i=2,p=1,P=1,Q=1,q=2,f=24,s=1,v=1;AAAA\x1b\\");
            harness.feed(b"\x1b_Ga=T,i=3,p=1,P=2,Q=1,q=2,f=24,s=1,v=1;AAAA\x1b\\");
            assert_eq!(harness.relatives.lock().unwrap().len(), 2);
            assert!(ordinary_reply_bytes(&harness).is_empty());
            if synchronized {
                harness.feed(b"\x1b[?2026h");
            }
            harness.feed(
                format!("\x1b_Ga=p,i=2,p=1,P=3,Q={parent_placement},H=2,q=1\x1b\\").as_bytes(),
            );
            if synchronized {
                assert!(ordinary_reply_bytes(&harness).is_empty());
                harness.feed(b"\x1b[?2026l");
            }
            let replies = ordinary_reply_bytes(&harness);
            assert_eq!(replies.len(), 1);
            assert!(replies[0].starts_with("\x1b_Gi=2,p=1;EINVAL:"));
            let relatives = harness.relatives.lock().unwrap();
            let previous = relatives.get(&(2, 1)).unwrap();
            assert_eq!(
                (previous.parent_img, previous.parent_placement, previous.h),
                (1, 1, 0)
            );
            assert_eq!(relatives.get(&(3, 1)).unwrap().parent_img, 2);
            drop(relatives);
            harness.feed(b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");
            assert!(harness.relatives.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn ordinary_graphics_rejected_relative_update_preserves_its_previous_parent() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b_Ga=T,i=388,p=16,q=2,f=24,s=1,v=1,C=1;AAAA\x1b\\");
    harness.feed(b"\x1b_Ga=t,i=389,q=2,f=24,s=1,v=1;AQID\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=389,p=17,P=388,Q=16,H=1,q=2\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=389,p=17,P=388,Q=99,H=2,q=1\x1b\\");
    let replies = ordinary_reply_bytes(&harness);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].starts_with("\x1b_Gi=389,p=17;ENOPARENT:"));
    let relatives = harness.relatives.lock().unwrap();
    let previous = relatives
        .get(&(389, 17))
        .expect("previous accepted placement");
    assert_eq!(
        (previous.parent_img, previous.parent_placement, previous.h),
        (388, 16, 1)
    );
    drop(relatives);
    harness.feed(b"\x1b_Ga=d,d=i,i=388,p=16\x1b\\");
    assert!(harness.relatives.lock().unwrap().is_empty());
}

#[test]
fn kitty_query_echoes_id_before_device_attributes_even_during_sync() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
        if synchronized {
            let immediate: Vec<_> = harness
                .replies
                .try_iter()
                .filter_map(|event| {
                    if let TermEvent::PtyWrite(reply) = event {
                        Some(reply)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(immediate, ["\x1b_Gi=31;OK\x1b\\"]);
            harness.feed(b"\x1b[?2026l");
            assert_eq!(
                harness
                    .replies
                    .try_iter()
                    .filter_map(|event| {
                        if let TermEvent::PtyWrite(reply) = event {
                            Some(reply)
                        } else {
                            None
                        }
                    })
                    .collect::<String>(),
                crate::event::PRIMARY_DA_REPLY
            );
        } else {
            let replies: String = harness
                .replies
                .try_iter()
                .filter_map(|event| {
                    if let TermEvent::PtyWrite(reply) = event {
                        Some(reply)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                replies,
                format!("\x1b_Gi=31;OK\x1b\\{}", crate::event::PRIMARY_DA_REPLY)
            );
        }
        assert!(harness.active_ids().is_empty());
        harness.feed(b"\x1b_Ga=p,i=31\x1b\\");
        assert!(
            harness.active_ids().is_empty(),
            "query pixels must not be stored"
        );
    }
}

#[test]
fn chunked_kitty_queries_reply_once_in_wire_order_for_every_read_split() {
    let wire = b"\x1b[?2026h\x1b_Ga=q,i=77,f=32,s=1,v=1,m=1;AQID\x1b\\\x1b_Gm=0;BA==\x1b\\\x1b_Ga=q,i=78,f=24,s=1,v=1;%%%%\x1b\\\x1b[c\x1b[?2026l";
    for split in 0..=wire.len() {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(&wire[..split]);
        harness.feed(&wire[split..]);
        let replies: String = harness
            .replies
            .try_iter()
            .filter_map(|event| {
                if let TermEvent::PtyWrite(reply) = event {
                    Some(reply)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            replies,
            format!(
                "\x1b_Gi=77;OK\x1b\\\x1b_Gi=78;EINVAL:Invalid image data\x1b\\{}",
                crate::event::PRIMARY_DA_REPLY
            ),
            "read split {split}"
        );
        assert!(harness.active_ids().is_empty());
    }
}

/// Two synchronized graphics frames arriving in one PTY read.
///
/// kettle-core's chunk handler re-enters `feed_with` on the same
/// `Extractor` to replay a deferred frame. `dispatch_escape_follower` must
/// deliver the flushed pass-through bytes before it sets `mode` to `Apc`.
/// Otherwise the inner call sees an open string, cancels it, and returns
/// with `mode == Pass`, so the outer loop resumes in the wrong mode and
/// paints the rest of the second frame onto the grid as text.
///
/// The other tests here end their buffer at the ESU or feed it separately,
/// so the flush happens with `mode == Pass` and never exercises this order.
#[test]
fn a_second_synchronized_frame_in_one_read_is_not_painted_onto_the_grid() {
    let mut harness = SyncGraphicsHarness::new();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b[?2026h");
    bytes.extend_from_slice(&kitty_image(1, 1, 1, 1));
    bytes.extend_from_slice(b"\x1b[?2026l");
    bytes.extend_from_slice(&kitty_image(2, 1, 1, 1));
    harness.feed(&bytes);

    let term = harness.term.lock().unwrap();
    let text = crate::term::screen_text_of(&term, 0).text;
    drop(term);
    let placed = harness.images.lock().unwrap().len();

    // Assert on the whole grid, not on a substring. The payload wraps at
    // the harness's 8 columns, so `AQIDBA==` never appears contiguously and
    // a `contains` check passes while the screen is full of it.
    assert_eq!(
        text.trim(),
        "",
        "graphics command text was painted onto the grid: {text:?}"
    );
    assert_eq!(placed, 2, "the second frame was dropped instead of placed");
}

fn kitty_image(id: u32, placement: u32, columns: u32, rows: u32) -> Vec<u8> {
    format!("\x1b_Ga=T,i={id},p={placement},f=32,s=1,v=1,c={columns},r={rows};AQIDBA==\x1b\\")
        .into_bytes()
}

/// A placement asking for billions of rows or columns moves the cursor to
/// the same cell inside a synchronized update as outside one: the grid's
/// last row and column, not a row its line arithmetic wrapped to.
#[test]
fn kitty_cursor_movement_is_bounded_in_both_placement_paths() {
    for (columns, rows) in [(1, u32::MAX), (1, 1 << 31), (u32::MAX, u32::MAX)] {
        let mut cursors = Vec::new();
        for synchronized in [false, true] {
            let mut harness = SyncGraphicsHarness::new();
            harness.feed(b"\x1b[2;1H");
            if synchronized {
                harness.feed(b"\x1b[?2026h");
            }
            harness.feed(&kitty_image(1, 1, columns, rows));
            if synchronized {
                harness.feed(b"\x1b[?2026l");
            }
            cursors.push(harness.term.lock().unwrap().grid().cursor.point);
        }
        assert_eq!(cursors[0], cursors[1], "c={columns}, r={rows}");
        assert_eq!(cursors[1].line, Line(3), "c={columns}, r={rows}");
    }
}

#[test]
fn count_pressure_evicts_oldest_unplaced_root_before_displayed_images() {
    let mut harness = SyncGraphicsHarness::new();
    let cap = u32::try_from(kettle_vt::GraphicsLimits::default().placements).unwrap();
    for id in 1..=cap {
        harness.feed(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1;AQID/w==\x1b\\").as_bytes());
    }
    harness.feed(b"\x1b_Ga=p,i=1,p=1,c=1,r=1,C=1\x1b\\");
    assert_eq!(harness.images.lock().unwrap().len(), 1);
    harness.feed(
        format!(
            "\x1b_Ga=T,i={},p=2,f=32,s=1,v=1,c=1,r=1,C=1;BQYH/w==\x1b\\",
            cap + 1
        )
        .as_bytes(),
    );
    {
        let placements = harness.images.lock().unwrap();
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].id, Some(1));
        assert_eq!(placements[1].id, Some(cap + 1));
        assert_eq!(placements[1].img.rgba.as_slice(), &[5, 6, 7, 255]);
    }
    harness.feed(b"\x1b_Ga=p,i=2,p=3,c=1,r=1,C=1\x1b\\");
    assert_eq!(
        harness.images.lock().unwrap().len(),
        2,
        "oldest unplaced root was evicted"
    );
    harness.feed(b"\x1b_Ga=p,i=3,p=4,c=1,r=1,C=1\x1b\\");
    assert_eq!(
        harness.images.lock().unwrap().len(),
        3,
        "younger unplaced root remains addressable"
    );
}

#[test]
fn active_count_pressure_preserves_same_id_on_the_inactive_screen() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=t,i=1,f=32,s=1,v=1;CQgH/w==\x1b\\\x1b[?47h");
        let cap = u32::try_from(kettle_vt::GraphicsLimits::default().placements).unwrap();
        for id in 1..=cap {
            harness.feed(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1;AQID/w==\x1b\\").as_bytes());
        }
        harness.feed(b"\x1b_Ga=p,i=1,p=1,c=1,r=1,C=1\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(
            format!(
                "\x1b_Ga=T,i={},p=2,f=32,s=1,v=1,c=1,r=1,C=1;BQYH/w==\x1b\\",
                cap + 1
            )
            .as_bytes(),
        );
        if synchronized {
            assert_eq!(harness.active_ids(), [1]);
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(harness.active_ids(), [1, cap + 1]);
        harness.feed(b"\x1b[?47l\x1b_Ga=p,i=1,p=3,c=1,r=1,C=1\x1b\\");
        let placements = harness.images.lock().unwrap();
        assert_eq!(
            placements.len(),
            1,
            "active slots cannot be reclaimed from the other screen"
        );
        assert_eq!(placements[0].id, Some(1));
        assert_eq!(placements[0].img.rgba.as_slice(), &[9, 8, 7, 255]);
    }
}

#[test]
fn count_eviction_keeps_a_retired_snapshot_alive_without_hiding_the_new_id() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        let cap = u32::try_from(kettle_vt::GraphicsLimits::default().placements).unwrap();
        for id in 1..=cap {
            harness.feed(
                format!("\x1b_Ga=T,i={id},p=1,f=32,s=1,v=1,c=1,r=1,C=1;AQID/w==\x1b\\").as_bytes(),
            );
        }
        let snapshot = harness.images.lock().unwrap()[0].img.clone();
        let retired = Arc::downgrade(&snapshot.rgba);
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(
            format!(
                "\x1b_Ga=T,i={},p=2,f=32,s=1,v=1,c=1,r=1,C=1;BQYH/w==\x1b\\",
                cap + 1
            )
            .as_bytes(),
        );
        if synchronized {
            assert_eq!(harness.active_ids(), (1..=cap).collect::<Vec<_>>());
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(harness.active_ids(), (2..=cap + 1).collect::<Vec<_>>());
        assert_eq!(snapshot.rgba.as_slice(), &[1, 2, 3, 255]);
        assert!(retired.upgrade().is_some());
        drop(snapshot);
        assert!(retired.upgrade().is_none());
    }
}

#[test]
fn quota_eviction_releases_visible_virtual_relative_and_animation_owners() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        let cap = u32::try_from(kettle_vt::GraphicsLimits::default().placements).unwrap();
        for id in 1..=cap {
            harness.feed(
                format!("\x1b_Ga=T,i={id},p=1,f=32,s=1,v=1,c=1,r=1,C=1;AQID/w==\x1b\\").as_bytes(),
            );
        }
        harness.feed(b"\x1b_Ga=p,i=1,p=2,U=1,c=1,r=1\x1b\\\x1b_Ga=p,i=1,p=3,P=1,Q=1,c=1,r=1\x1b\\\x1b_Ga=f,i=1,f=32,s=1,v=1,z=37;CQgH/w==\x1b\\");
        let root = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
        let frame = Arc::downgrade(&harness.anims.lock().unwrap()[&1].imgs[1].rgba);
        assert_eq!(harness.images.lock().unwrap().len(), cap as usize);
        assert!(harness.virtuals.lock().unwrap().contains_key(&(1, 2)));
        assert!(harness.relatives.lock().unwrap().contains_key(&(1, 3)));
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(
            format!(
                "\x1b_Ga=T,i={},p=4,f=32,s=1,v=1,c=1,r=1,C=1;BQYH/w==\x1b\\",
                cap + 1
            )
            .as_bytes(),
        );
        if synchronized {
            assert!(root.upgrade().is_some());
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(harness.active_ids(), (2..=cap + 1).collect::<Vec<_>>());
        assert!(harness.virtuals.lock().unwrap().is_empty());
        assert!(harness.relatives.lock().unwrap().is_empty());
        assert!(!harness.anims.lock().unwrap().contains_key(&1));
        assert!(root.upgrade().is_none());
        assert!(frame.upgrade().is_none());
        assert_eq!(
            harness
                .images
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .img
                .rgba
                .as_slice(),
            &[5, 6, 7, 255]
        );
    }
}

#[cfg(unix)]
#[test]
fn quota_eviction_retains_new_ids_through_a_real_pty() {
    use super::Terminal;
    use alacritty_terminal::vte::ansi::CursorShape;
    use std::time::Duration;

    for synchronized in [false, true] {
        let cap = u32::try_from(kettle_vt::GraphicsLimits::default().placements).unwrap();
        let mut wire = String::new();
        for id in 1..=cap {
            wire.push_str(&format!("\x1b_Ga=t,i={id},f=32,s=1,v=1;AQID/w==\x1b\\"));
        }
        // Fill before the synchronized frame: its existing journal has
        // 256 entries, so only the pressure/placement operations belong in it.
        if synchronized {
            wire.push_str("\x1b[?2026h");
        }
        wire.push_str("\x1b_Ga=p,i=1,p=1,c=1,r=1,C=1\x1b\\");
        wire.push_str(&format!(
            "\x1b_Ga=T,i={},p=2,f=32,s=1,v=1,c=1,r=1,C=1;BQYH/w==\x1b\\",
            cap + 1
        ));
        wire.push_str("\x1b_Ga=p,i=2,p=3,c=1,r=1,C=1\x1b\\\x1b_Ga=p,i=3,p=4,c=1,r=1,C=1\x1b\\");
        if synchronized {
            wire.push_str("\x1b[?2026l");
        }
        wire.push_str("\x1b[5;1HQUOTA_PLACEMENTS_READY");
        let escaped = wire.replace('\\', "\\\\").replace('\x1b', "\\033");
        let script = format!("printf '{escaped}'; read finish");
        let (tx, _rx) = crossbeam_channel::unbounded();
        let terminal = Terminal::new(
            &["/bin/sh".into(), "-c".into(), script],
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            Arc::new(|| {}),
        )
        .expect("native Kitty quota PTY");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if terminal.term.lock().is_ok_and(|term| {
                super::screen_text_of(&term, 0)
                    .text
                    .contains("QUOTA_PLACEMENTS_READY")
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "quota result did not reach the terminal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let actual: Vec<_> = terminal
            .images
            .lock()
            .unwrap()
            .iter()
            .map(|p| (p.id, p.placement_id, p.img.rgba.as_slice().to_vec()))
            .collect();
        assert_eq!(
            actual,
            [
                (Some(1), 1, vec![1, 2, 3, 255]),
                (Some(cap + 1), 2, vec![5, 6, 7, 255]),
                (Some(3), 4, vec![1, 2, 3, 255]),
            ],
            "synchronized={synchronized}"
        );
        terminal.write(b"continue\n");
    }
}

#[test]
fn first_retransmission_chunk_releases_all_active_placement_and_animation_owners() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=24,p=1,f=32,s=1,v=1,c=2,r=2;AQID/w==\x1b\\");
        harness.feed(b"\x1b_Ga=p,i=24,p=2,U=1,c=2,r=2\x1b\\");
        harness.feed(b"\x1b_Ga=p,i=24,p=3,P=24,Q=1,H=1,V=1\x1b\\");
        harness.feed(b"\x1b_Ga=f,i=24,f=32,s=1,v=1,z=37;AQID/w==\x1b\\");
        let root = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
        let frame = Arc::downgrade(&harness.anims.lock().unwrap()[&24].imgs[1].rgba);
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=t,i=24,f=32,s=1,v=1,m=1;BQYH\x1b\\");
        if synchronized {
            assert_eq!(harness.images.lock().unwrap().len(), 1);
            assert!(root.upgrade().is_some());
            harness.feed(b"\x1b[?2026l");
        }
        assert!(harness.images.lock().unwrap().is_empty());
        assert!(harness.virtuals.lock().unwrap().is_empty());
        assert!(harness.relatives.lock().unwrap().is_empty());
        assert!(harness.anims.lock().unwrap().is_empty());
        assert!(root.upgrade().is_none());
        assert!(frame.upgrade().is_none());
        harness.feed(b"\x1b_Ga=p,i=24\x1b\\");
        assert!(harness.images.lock().unwrap().is_empty());
        harness.feed(b"\x1b_Gm=0;/w==\x1b\\");
        assert!(harness.images.lock().unwrap().is_empty());
        harness.feed(b"\x1b_Ga=p,i=24,p=4,c=1,r=1\x1b\\");
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            &[5, 6, 7, 255]
        );
        assert!(harness.virtuals.lock().unwrap().is_empty());
        assert!(harness.relatives.lock().unwrap().is_empty());
        assert!(harness.anims.lock().unwrap().is_empty());
    }
}

#[test]
fn relative_q_spatial_deletion_uses_the_selected_parent_placement() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=t,i=51,f=32,s=1,v=1;AQID/w==\x1b\\\x1b[1;1H\x1b_Ga=p,i=51,p=1,c=1,r=1,C=1\x1b\\\x1b[3;4H\x1b_Ga=p,i=51,p=2,c=1,r=1,C=1\x1b\\\x1b_Ga=t,i=52,f=32,s=1,v=1;CQgH/w==\x1b\\\x1b_Ga=p,i=52,p=5,P=51,Q=2,H=1,V=0,c=1,r=1\x1b\\");
        assert!(harness.relatives.lock().unwrap().contains_key(&(52, 5)));
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=d,d=p,x=5,y=3\x1b\\");
        if synchronized {
            assert!(harness.relatives.lock().unwrap().contains_key(&(52, 5)));
            harness.feed(b"\x1b[?2026l");
        }
        assert!(
            harness.relatives.lock().unwrap().is_empty(),
            "synchronized={synchronized}"
        );
        assert_eq!(harness.images.lock().unwrap().len(), 2);
    }
}

#[test]
fn zero_image_number_retransmission_retires_pixels_and_animation() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=7,p=1,f=32,s=1,v=1,C=1;AQID/w==\x1b\\");
        harness.feed(b"\x1b_Ga=f,i=7,f=32,s=1,v=1;CQgH/w==\x1b\\");
        harness.feed(b"\x1b_Ga=a,i=7,s=3\x1b\\");
        let old = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
        assert!(harness.anims.lock().unwrap().contains_key(&7));
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=t,i=7,I=0,f=32,s=1,v=1;BQYH/w==\x1b\\");
        if synchronized {
            assert_eq!(harness.active_ids(), [7]);
            harness.feed(b"\x1b[?2026l");
        }
        assert!(harness.active_ids().is_empty());
        assert!(!harness.anims.lock().unwrap().contains_key(&7));
        assert!(old.upgrade().is_none());
        harness.feed(b"\x1b_Ga=p,i=7,p=2,C=1\x1b\\");
        assert_eq!(harness.active_ids(), [7]);
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            &[5, 6, 7, 255]
        );
    }
}

#[test]
fn retransmission_keeps_same_numeric_id_on_the_other_screen() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b_Ga=T,i=25,p=1,f=32,s=1,v=1;AQID/w==\x1b\\");
    let primary = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
    harness.feed(b"\x1b[?47h\x1b_Ga=T,i=25,p=1,f=32,s=1,v=1;CQgH/w==\x1b\\");
    let alternate = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
    harness.feed(b"\x1b[?47l\x1b_Ga=t,i=25,f=32,s=1,v=1,m=1;BQYH\x1b\\");
    assert!(harness.images.lock().unwrap().is_empty());
    assert!(primary.upgrade().is_none());
    assert!(alternate.upgrade().is_some());
    harness.feed(b"\x1b[?47h");
    assert_eq!(
        harness.images.lock().unwrap()[0].img.rgba.as_slice(),
        &[9, 8, 7, 255]
    );
    harness.feed(b"\x1b[?47l\x1b_Gm=0;/w==\x1b\\\x1b_Ga=p,i=25,p=2\x1b\\");
    assert_eq!(
        harness.images.lock().unwrap()[0].img.rgba.as_slice(),
        &[5, 6, 7, 255]
    );
    assert!(alternate.upgrade().is_some());
}

#[test]
fn retransmission_invalidates_relative_children_without_deleting_independent_placements() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b_Ga=T,i=31,p=1,f=32,s=1,v=1;AQID/w==\x1b\\");
    harness.feed(b"\x1b_Ga=t,i=32,f=32,s=1,v=1;CQgH/w==\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=32,p=2,P=31,Q=1,H=1,V=1\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=32,p=3\x1b\\");
    let parent = Arc::downgrade(&harness.images.lock().unwrap()[0].img.rgba);
    let child = Arc::downgrade(&harness.relatives.lock().unwrap()[&(32, 2)].img.rgba);
    harness.feed(b"\x1b_Ga=t,i=31,f=32,s=1,v=1,m=1;BQYH\x1b\\");
    assert_eq!(harness.images.lock().unwrap().len(), 1);
    assert_eq!(harness.images.lock().unwrap()[0].id, Some(32));
    assert!(harness.relatives.lock().unwrap().is_empty());
    assert!(parent.upgrade().is_none());
    assert!(child.upgrade().is_some());
    harness.feed(b"\x1b_Ga=p,i=32,p=4\x1b\\");
    assert_eq!(
        harness.images.lock().unwrap()[0].img.rgba.as_slice(),
        &[9, 8, 7, 255]
    );
}

#[test]
fn frame_composition_offsets_refresh_all_placement_kinds() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=2,v=2,c=2,r=2;/wAA//8AAP//AAD//wAA/w==\x1b\\");
        harness.feed(b"\x1b_Ga=p,i=1,p=2,U=1,c=2,r=2\x1b\\");
        harness.feed(b"\x1b_Ga=p,i=1,p=3,P=1,Q=1,H=1,V=1\x1b\\");
        harness.feed(b"\x1b_Ga=f,i=1,f=32,s=2,v=2;AAD//wD/AP///wD/gACA/w==\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=c,i=1,r=2,c=1,w=1,h=1,X=1,Y=1,x=0,y=0,C=1\x1b\\");
        if synchronized {
            assert_eq!(
                harness.images.lock().unwrap()[0].img.rgba.as_slice(),
                &[255, 0, 0, 255].repeat(4)
            );
            harness.feed(b"\x1b[?2026l");
        }
        let expected = [
            128, 0, 128, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            expected
        );
        assert_eq!(
            harness.virtuals.lock().unwrap()[&(1, 2)]
                .img
                .rgba
                .as_slice(),
            expected
        );
        assert_eq!(
            harness.relatives.lock().unwrap()[&(1, 3)]
                .img
                .rgba
                .as_slice(),
            expected
        );
    }
}

#[test]
fn root_frame_edit_refreshes_all_existing_placement_kinds() {
    assert_root_frame_edit_refreshes_all_existing_placement_kinds(false);
}

#[test]
fn synchronized_root_frame_edit_refreshes_all_existing_placement_kinds() {
    assert_root_frame_edit_refreshes_all_existing_placement_kinds(true);
}

fn assert_root_frame_edit_refreshes_all_existing_placement_kinds(synchronized: bool) {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,c=2,r=2;AQID/w==\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=1,p=2,U=1,c=2,r=2\x1b\\");
    harness.feed(b"\x1b_Ga=p,i=1,p=3,P=1,Q=1,H=1,V=1\x1b\\");
    let (old_pixels, old_geometry) = {
        let placements = harness.images.lock().unwrap();
        assert_eq!(placements.len(), 1);
        let p = &placements[0];
        (
            Arc::downgrade(&p.img.rgba),
            (p.abs_line, p.col, p.display_cols, p.display_rows),
        )
    };
    assert_eq!(harness.virtuals.lock().unwrap().len(), 1);
    assert_eq!(harness.relatives.lock().unwrap().len(), 1);
    if synchronized {
        harness.feed(b"\x1b[?2026h");
    }
    harness.feed(b"\x1b_Ga=f,i=1,r=1,X=1,f=32,s=1,v=1;BQYH/w==\x1b\\");
    if synchronized {
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            &[1, 2, 3, 255]
        );
        harness.feed(b"\x1b[?2026l");
    }
    let placements = harness.images.lock().unwrap();
    let p = &placements[0];
    assert_eq!(p.img.rgba.as_slice(), &[5, 6, 7, 255]);
    assert_eq!(
        (p.abs_line, p.col, p.display_cols, p.display_rows),
        old_geometry
    );
    assert_eq!(
        harness.virtuals.lock().unwrap()[&(1, 2)]
            .img
            .rgba
            .as_slice(),
        &[5, 6, 7, 255]
    );
    assert_eq!(
        harness.relatives.lock().unwrap()[&(1, 3)]
            .img
            .rgba
            .as_slice(),
        &[5, 6, 7, 255]
    );
    assert!(harness.anims.lock().unwrap().is_empty());
    assert!(old_pixels.upgrade().is_none());
}

#[test]
fn root_frame_edit_preserves_other_screen_with_the_same_image_id() {
    for synchronized in [false, true] {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        let primary = harness.images.lock().unwrap()[0].img.rgba.clone();
        harness.feed(b"\x1b[?47h");
        harness.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1;CAkK/w==\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026h");
        }
        harness.feed(b"\x1b_Ga=f,i=1,r=1,X=1,f=32,s=1,v=1;BQYH/w==\x1b\\");
        if synchronized {
            harness.feed(b"\x1b[?2026l");
        }
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            &[5, 6, 7, 255]
        );
        assert_eq!(
            harness.inactive.lock().unwrap().placements[0]
                .img
                .rgba
                .as_slice(),
            &[1, 2, 3, 255]
        );
        harness.feed(b"\x1b[?47l");
        assert!(Arc::ptr_eq(
            &harness.images.lock().unwrap()[0].img.rgba,
            &primary
        ));
        harness.feed(b"\x1b[?47h");
        assert_eq!(
            harness.images.lock().unwrap()[0].img.rgba.as_slice(),
            &[5, 6, 7, 255]
        );
    }
}

#[test]
fn root_frame_edit_preserves_appended_frame_selection_and_playback_clock() {
    for synchronized in [false, true] {
        for running in [false, true] {
            let mut harness = SyncGraphicsHarness::new();
            harness.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1;AQID/w==\x1b\\");
            harness.feed(b"\x1b_Ga=f,i=1,f=32,s=1,v=1,z=40;CAkK/w==\x1b\\");
            let control = if running {
                b"\x1b_Ga=a,i=1,c=2,s=3,v=5\x1b\\"
            } else {
                b"\x1b_Ga=a,i=1,c=2,s=1,v=5\x1b\\"
            };
            harness.feed(control);
            let (started, state, selected_pixels) = {
                let animations = harness.anims.lock().unwrap();
                let animation = &animations[&1];
                (
                    animation.started,
                    animation.state,
                    animation.imgs[1].rgba.clone(),
                )
            };
            if synchronized {
                harness.feed(b"\x1b[?2026h");
            }
            harness.feed(b"\x1b_Ga=f,i=1,r=1,X=1,f=32,s=1,v=1;BQYH/w==\x1b\\");
            if synchronized {
                harness.feed(b"\x1b[?2026l");
            }
            let animations = harness.anims.lock().unwrap();
            let animation = &animations[&1];
            assert_eq!(animation.started, started);
            assert_eq!(animation.state.current, state.current);
            assert_eq!(animation.state.running, state.running);
            assert_eq!(animation.state.loops, state.loops);
            assert_eq!(animation.imgs[0].rgba.as_slice(), &[5, 6, 7, 255]);
            assert!(Arc::ptr_eq(&animation.imgs[1].rgba, &selected_pixels));
            assert_eq!(
                kettle_vt::kitty::current_frame(&animation.gaps, &animation.state, 0),
                1
            );
            assert_eq!(
                harness.images.lock().unwrap()[0].img.rgba.as_slice(),
                &[5, 6, 7, 255]
            );
        }
    }
}

#[test]
fn terminal_delete_cascades_through_the_full_relative_budget() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(&kitty_image(1, 1, 1, 1));
    let cap = kettle_vt::GraphicsLimits::default().placements as u32;
    for placement in 2..=cap + 1 {
        harness
            .feed(format!("\x1b_Ga=p,i=1,p={placement},P=1,Q={}\x1b\\", placement - 1).as_bytes());
    }
    assert_eq!(harness.relatives.lock().unwrap().len(), cap as usize);

    harness.feed(b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");

    assert!(harness.relatives.lock().unwrap().is_empty());
}

fn placement_at(id: u32, abs_line: u64, rows: usize) -> Placement {
    let mut placement = placement(id);
    placement.abs_line = abs_line;
    placement.cell_rows = rows;
    placement.display_rows = rows as f32;
    placement
}

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

#[test]
fn plain_scroll_and_resize_share_one_graphics_order() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let term: SharedTerm = Arc::new(Mutex::new(Term::new(
        TermConfig::default(),
        &TermSize {
            columns: 8,
            screen_lines: 4,
        },
        proxy.clone(),
    )));
    let mut processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[4;1H");
        assert!(term.take_graphics_events().events.is_empty());
    }

    let (images, virtuals, anims, relatives, inactive) = registries();
    images.lock().unwrap().push(placement_at(1, 0, 1));
    let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
        geometry: PtyGeometry::new(8, 4, 80, 40),
        generation: 0,
    }));
    let graphics_gate = Arc::new(Mutex::new(()));
    let order = Arc::new(Mutex::new(Vec::new()));
    let (text_committed_tx, text_committed_rx) = std::sync::mpsc::channel();
    let (resize_committed_tx, resize_committed_rx) = std::sync::mpsc::channel();
    let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel();

    let reader_term = term.clone();
    let reader_images = images.clone();
    let reader_virtuals = virtuals.clone();
    let reader_anims = anims.clone();
    let reader_relatives = relatives.clone();
    let reader_inactive = inactive.clone();
    let reader_geometry = geometry.clone();
    let reader_gate = graphics_gate.clone();
    let reader_order = order.clone();
    let reader_proxy = proxy.clone();
    let reader = std::thread::spawn(move || {
        let chunk = Chunk::Pass(b"\n".to_vec());
        let mut deferred = DeferredGraphicsJournal::new();
        let graphics_related = chunk_needs_graphics_gate(&chunk);
        let _graphics_guard = graphics_related.then(|| {
            reader_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        });
        let Chunk::Pass(bytes) = chunk else {
            unreachable!("fixture creates a pass chunk")
        };
        let mut extractor = Extractor::new();
        let mut active_alternate = false;
        let mut context = SyncGraphicsContext {
            active_alternate: &mut active_alternate,
            deferred: &mut deferred,
            registries: GraphicsRegistries {
                inactive: &reader_inactive,
                images: &reader_images,
                virtuals: &reader_virtuals,
                anims: &reader_anims,
                relatives: &reader_relatives,
            },
            actions: GraphicsActionContext {
                inactive: &reader_inactive,
                images: &reader_images,
                virtuals: &reader_virtuals,
                anims: &reader_anims,
                relatives: &reader_relatives,
                geometry: &reader_geometry,
                proxy: &reader_proxy,
            },
            extractor: &mut extractor,
        };
        advance_terminal_bytes_with_commit_hook(
            &mut processor,
            &reader_term,
            &bytes,
            &mut context,
            |batch| {
                let saw_scroll = matches!(batch.events.as_slice(), [GraphicsEvent::Scroll { .. }]);
                reader_order.lock().unwrap().push("text-scroll");
                text_committed_tx
                    .send((graphics_related, saw_scroll))
                    .unwrap();
                if !graphics_related {
                    resize_committed_rx.recv().unwrap();
                }
            },
        );
        reader_order.lock().unwrap().push("graphics-scroll");
        reader_done_tx.send(()).unwrap();
    });

    let (graphics_related, saw_scroll) = text_committed_rx.recv().unwrap();
    assert!(
        saw_scroll,
        "plain LF must emit a real graphics scroll event"
    );
    {
        let _graphics_guard = graphics_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resized = PtyGeometry::new(8, 4, 160, 40);
        commit_local_geometry(&term, &geometry, resized, None);
        recompute_kitty_placements(&mut images.lock().unwrap(), resized);
        order.lock().unwrap().push("graphics-resize");
    }
    if !graphics_related {
        resize_committed_tx.send(()).unwrap();
    }
    reader_done_rx.recv().unwrap();
    reader.join().unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        ["text-scroll", "graphics-scroll", "graphics-resize"],
        "a plain-byte scroll committed outside the graphics gate, allowing \
             resize to split its text and graphics mutations"
    );
}

#[test]
fn authoritative_journal_preserves_mixed_control_order() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let mut term = Term::new(
        TermConfig::default(),
        &TermSize {
            columns: 4,
            screen_lines: 4,
        },
        proxy,
    );
    let mut processor: Processor = Processor::new();
    processor.advance(
        &mut term,
        b"\x1b[2J\x1b[?47h\x1b[?47l\x1b[?1049h\x1b[?1049l\x1bc",
    );
    assert_eq!(
        term.take_graphics_events().events,
        vec![
            GraphicsEvent::EraseDisplay,
            GraphicsEvent::EnterAlternate {
                mode: 47,
                clear: false,
            },
            GraphicsEvent::LeaveAlternate {
                mode: 47,
                clear: false,
            },
            GraphicsEvent::EnterAlternate {
                mode: 1049,
                clear: true,
            },
            GraphicsEvent::LeaveAlternate {
                mode: 1049,
                clear: false,
            },
            GraphicsEvent::Reset,
        ]
    );
}

#[test]
fn synchronized_images_follow_enter_and_leave_wire_order() {
    let mut harness = SyncGraphicsHarness::new();
    let mut first = b"\x1b[?2026h".to_vec();
    first.extend(kitty_image(1, 1, 1, 1));
    first.extend_from_slice(b"\x1b[?1049h");
    first.extend(kitty_image(2, 1, 1, 1));
    first.extend_from_slice(b"\x1b[?2026l");
    harness.feed(&first);

    assert!(harness.active_alternate);
    assert_eq!(harness.active_ids(), vec![2]);
    assert_eq!(harness.inactive_ids(), vec![1]);

    let mut second = b"\x1b[?2026h".to_vec();
    second.extend(kitty_image(3, 1, 1, 1));
    second.extend_from_slice(b"\x1b[?1049l");
    second.extend(kitty_image(4, 1, 1, 1));
    second.extend_from_slice(b"\x1b[?2026l");
    harness.feed(&second);

    assert!(!harness.active_alternate);
    assert_eq!(harness.active_ids(), vec![1, 4]);
    assert_eq!(harness.inactive_ids(), vec![2, 3]);
}

#[test]
fn synchronized_image_cursor_movement_precedes_later_text_and_stays_invisible_mid_update() {
    let mut harness = SyncGraphicsHarness::new();
    let mut update = b"\x1b[?2026h\x1b[2;2H".to_vec();
    update.extend(kitty_image(9, 1, 2, 2));
    update.extend_from_slice(b"X");
    harness.feed(&update);

    assert!(
        harness.images.lock().unwrap().is_empty(),
        "graphics must remain invisible until the synchronized update commits"
    );
    assert_eq!(
        harness.term.lock().unwrap().grid()[Point::new(Line(2), Column(3))].c,
        ' ',
        "text must remain buffered with the image"
    );

    harness.feed(b"\x1b[?2026l");
    let placements = harness.images.lock().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(
        (
            placements[0].col,
            placements[0].cell_cols,
            placements[0].cell_rows
        ),
        (1, 2, 2)
    );
    drop(placements);
    let term = harness.term.lock().unwrap();
    assert_eq!(term.grid()[Point::new(Line(2), Column(3))].c, 'X');
    assert_eq!(term.grid().cursor.point, Point::new(Line(2), Column(4)));
}

#[test]
fn synchronized_graphics_replay_before_forced_eof_publication() {
    let mut harness = SyncGraphicsHarness::new();
    let mut update = b"\x1b[?2026h\x1b[2;2H".to_vec();
    update.extend(kitty_image(10, 1, 2, 1));
    harness.feed(&update);
    assert!(harness.images.lock().unwrap().is_empty());

    let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
    tx.send(None).unwrap();
    let generation = AtomicU64::new(0);
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_callback = wakes.clone();
    let waker: Waker = Arc::new(move || {
        wakes_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    let output_wake = OutputWakeGate::new(waker);
    let graphics_gate = Mutex::new(());
    let mut image_pruner = ImageHistoryPruner::default();

    let SyncGraphicsHarness {
        term,
        processor,
        extractor,
        deferred,
        active_alternate,
        images,
        virtuals,
        anims,
        relatives,
        inactive,
        geometry,
        replies: _,
        proxy,
        ..
    } = &mut harness;
    let mut on_graphics = |dispatch: SyncGraphicsDispatch<'_>| {
        assert_eq!(
            generation.load(Ordering::Acquire),
            0,
            "deferred graphics must replay before output publication"
        );
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            0,
            "deferred graphics must replay before the redraw wake"
        );
        let finishes_sync = matches!(&dispatch, SyncGraphicsDispatch::Batch(_));
        let mut context = SyncGraphicsContext {
            active_alternate,
            deferred,
            registries: GraphicsRegistries {
                inactive,
                images,
                virtuals,
                anims,
                relatives,
            },
            actions: GraphicsActionContext {
                inactive,
                images,
                virtuals,
                anims,
                relatives,
                geometry,
                proxy,
            },
            extractor,
        };
        apply_sync_dispatch(dispatch, &mut context);
        if finishes_sync {
            finish_deferred_sync(&mut context);
        }
    };
    let mut sync_flush = SyncFlushContext {
        term,
        images,
        graphics_gate: &graphics_gate,
        image_pruner: &mut image_pruner,
        on_graphics: &mut on_graphics,
        out_gen: &generation,
        output_wake: &output_wake,
    };

    assert!(receive_pty_chunk(processor, &rx, &mut sync_flush).is_none());
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    let placements = images.lock().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!((placements[0].id, placements[0].col), (Some(10), 1));
}

#[test]
fn synchronized_graphics_overflow_fails_closed_and_resets_decoder_state() {
    let mut harness = SyncGraphicsHarness::new();
    harness.feed(b"\x1b[?2026h");
    let mut images = Vec::new();
    for id in 1..=SYNC_MARKER_CAPACITY as u32 + 1 {
        images.extend(kitty_image(id, 1, 1, 1));
    }
    harness.feed(&images);

    assert!(harness.deferred.overflowed);
    assert!(harness.deferred.entries.is_empty());
    assert!(harness.images.lock().unwrap().is_empty());

    harness.feed(b"\x1b[?2026l");
    assert!(!harness.deferred.overflowed);
    assert!(harness.deferred.entries.is_empty());
    assert!(harness.images.lock().unwrap().is_empty());

    harness.feed(b"\x1b_Ga=p,i=1,p=2\x1b\\");
    assert!(
        harness.images.lock().unwrap().is_empty(),
        "overflow recovery must clear deferred kitty image data"
    );
}

#[test]
fn mode_47_preserves_primary_and_alternate_graphics() {
    let (images, virtuals, anims, relatives, inactive) = registries();
    images.lock().unwrap().push(placement(1));
    virtuals.lock().unwrap().insert((1, 1), virtual_entry(1));
    relatives.lock().unwrap().insert((2, 1), relative_entry(2));
    let mut extractor = Extractor::new();
    let mut alternate = false;

    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(alternate);
    assert!(images.lock().unwrap().is_empty());
    assert!(virtuals.lock().unwrap().is_empty());
    assert_eq!(inactive.lock().unwrap().placements.len(), 1);

    images.lock().unwrap().push(placement(9));
    virtuals.lock().unwrap().insert((9, 1), virtual_entry(9));
    apply_graphics_event(
        GraphicsEvent::LeaveAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(!alternate);
    assert_eq!(images.lock().unwrap()[0].id, Some(1));
    assert!(virtuals.lock().unwrap().contains_key(&(1, 1)));
    assert!(!virtuals.lock().unwrap().contains_key(&(9, 1)));
    assert_eq!(inactive.lock().unwrap().placements[0].id, Some(9));

    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert_eq!(images.lock().unwrap()[0].id, Some(9));
    assert!(virtuals.lock().unwrap().contains_key(&(9, 1)));
    assert_eq!(inactive.lock().unwrap().placements[0].id, Some(1));
}

#[test]
fn mode_1047_clears_on_exit_and_1049_clears_on_entry() {
    let (images, virtuals, anims, relatives, inactive) = registries();
    let mut extractor = Extractor::new();
    let mut alternate = false;

    // Seed a persistent alternate buffer through mode 47.
    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    images.lock().unwrap().push(placement(7));
    apply_graphics_event(
        GraphicsEvent::LeaveAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );

    // Mode 1047 preserves alternate contents on entry, then clears them
    // before returning to the primary buffer.
    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 1047,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert_eq!(images.lock().unwrap()[0].id, Some(7));
    apply_graphics_event(
        GraphicsEvent::LeaveAlternate {
            mode: 1047,
            clear: true,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(inactive.lock().unwrap().placements.is_empty());

    // Re-seed the parked alternate buffer. Mode 1049 clears it before
    // entry, but preserves new contents when returning to primary.
    inactive.lock().unwrap().placements.push(placement(8));
    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 1049,
            clear: true,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(images.lock().unwrap().is_empty());
    images.lock().unwrap().push(placement(9));
    apply_graphics_event(
        GraphicsEvent::LeaveAlternate {
            mode: 1049,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert_eq!(inactive.lock().unwrap().placements[0].id, Some(9));
}

#[test]
fn ed2_is_buffer_local_and_ris_clears_both_graphics_buffers() {
    let (images, virtuals, anims, relatives, inactive) = registries();
    images.lock().unwrap().push(placement(1));
    let mut extractor = Extractor::new();
    let mut alternate = false;
    apply_graphics_event(
        GraphicsEvent::EnterAlternate {
            mode: 47,
            clear: false,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    images.lock().unwrap().push(placement(9));
    apply_graphics_event(
        GraphicsEvent::EraseDisplay,
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(images.lock().unwrap().is_empty());
    assert_eq!(inactive.lock().unwrap().placements[0].id, Some(1));

    apply_graphics_event(
        GraphicsEvent::Reset,
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );
    assert!(!alternate);
    assert!(images.lock().unwrap().is_empty());
    assert!(inactive.lock().unwrap().placements.is_empty());
}

#[test]
fn partial_scroll_up_moves_wholly_contained_images_and_crops_at_top_margin() {
    let images: Images = Arc::new(Mutex::new(vec![
        placement_at(1, 103, 2),
        placement_at(2, 100, 1),
        placement_at(3, 101, 2),
        placement_at(4, 104, 1),
        placement_at(5, 102, 1),
    ]));

    assert!(scroll_regular_placements(
        &images,
        GraphicsScroll {
            direction: GraphicsScrollDirection::Up,
            top: 2,
            bottom: 6,
            lines: 2,
            old_screen_top: 100,
            new_screen_top: 100,
            screen_lines: 8,
        },
    ));

    let images = images.lock().unwrap();
    let find = |id| {
        images
            .iter()
            .find(|placement| placement.id == Some(id))
            .unwrap()
    };
    let clipped = find(1);
    assert_eq!(clipped.abs_line, 102);
    close(clipped.display_rows, 1.0);
    let crop = clipped.source_crop.expect("top crop");
    close(crop.top, 0.5);
    close(crop.bottom, 1.0);
    assert_eq!(
        clipped.kitty_params,
        Some(PlacementParams::default()),
        "partial scrolling must retain Kitty intent for a later DPI recompute"
    );
    assert_eq!(find(2).abs_line, 100, "image above the page stays fixed");
    assert_eq!(
        find(3).abs_line,
        101,
        "image crossing a margin must not scroll"
    );
    assert_eq!(find(4).abs_line, 102);
    assert!(
        images.iter().all(|placement| placement.id != Some(5)),
        "an image moved entirely above the margin is discarded"
    );
}

#[test]
fn partial_scroll_down_crops_at_bottom_margin_and_composes_source_ranges() {
    let mut first = placement_at(1, 103, 2);
    first.source_crop = Some(crate::ImageSourceCrop {
        top: 0.2,
        bottom: 1.0,
    });
    let images: Images = Arc::new(Mutex::new(vec![first, placement_at(2, 104, 1)]));

    assert!(scroll_regular_placements(
        &images,
        GraphicsScroll {
            direction: GraphicsScrollDirection::Down,
            top: 2,
            bottom: 6,
            lines: 2,
            old_screen_top: 100,
            new_screen_top: 100,
            screen_lines: 8,
        },
    ));

    let images = images.lock().unwrap();
    assert_eq!(images.len(), 1);
    let clipped = &images[0];
    assert_eq!(clipped.abs_line, 105);
    close(clipped.display_rows, 1.0);
    let crop = clipped.source_crop.expect("bottom crop");
    close(crop.top, 0.2);
    close(crop.bottom, 0.6);
}

#[test]
fn top_anchored_scroll_preserves_history_and_reanchors_fixed_rows() {
    let images: Images = Arc::new(Mutex::new(vec![
        placement_at(1, 10, 1),
        placement_at(2, 11, 1),
        placement_at(3, 15, 1),
    ]));

    assert!(scroll_regular_placements(
        &images,
        GraphicsScroll {
            direction: GraphicsScrollDirection::Up,
            top: 0,
            bottom: 4,
            lines: 1,
            old_screen_top: 10,
            new_screen_top: 11,
            screen_lines: 6,
        },
    ));

    let images = images.lock().unwrap();
    let find = |id| {
        images
            .iter()
            .find(|placement| placement.id == Some(id))
            .unwrap()
    };
    assert_eq!(
        find(1).abs_line,
        10,
        "top-row image follows text into retained history"
    );
    assert_eq!(find(2).abs_line, 11, "scrolled image keeps its document id");
    assert_eq!(
        find(3).abs_line,
        16,
        "row below the partial region stays at the same viewport position"
    );
}

#[test]
fn coalesced_top_scroll_uses_full_screen_delta_for_graphics_anchors() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let mut term = Term::new(
        TermConfig::default(),
        &TermSize {
            columns: 4,
            screen_lines: 4,
        },
        proxy,
    );
    let mut processor: Processor = Processor::new();
    processor.advance(&mut term, b"\x1b[1;2r\x1b[2H\n\n\n\n\n\n\n\n\n\n");
    let batch = term.take_graphics_events();
    assert_eq!(
        batch.events,
        vec![GraphicsEvent::Scroll {
            direction: GraphicsScrollDirection::Up,
            top: 0,
            bottom: 2,
            lines: 2,
            old_screen_top: 0,
            new_screen_top: 10,
            screen_lines: 4,
        }]
    );

    let (images, virtuals, anims, relatives, inactive) = registries();
    images.lock().unwrap().extend([
        placement_at(1, 0, 1),
        placement_at(2, 1, 1),
        placement_at(3, 3, 1),
    ]);
    let mut extractor = Extractor::new();
    let mut alternate = false;
    apply_graphics_batch(
        batch,
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );

    let images = images.lock().unwrap();
    let find = |id| {
        images
            .iter()
            .find(|placement| placement.id == Some(id))
            .unwrap()
    };
    assert_eq!(
        find(1).abs_line,
        0,
        "first scrolled row retains its document id in history"
    );
    assert_eq!(
        find(2).abs_line,
        1,
        "second scrolled row retains its document id in history"
    );
    assert_eq!(
        find(3).abs_line,
        13,
        "fixed row below the region stays at the same viewport position"
    );
}

#[test]
fn journal_overflow_clears_both_buffers_and_resynchronizes_active_screen() {
    let (images, virtuals, anims, relatives, inactive) = registries();
    images.lock().unwrap().push(placement(1));
    inactive.lock().unwrap().placements.push(placement(2));
    let mut extractor = Extractor::new();
    let mut alternate = false;

    apply_graphics_batch(
        GraphicsEventBatch {
            events: Vec::new(),
            overflowed: true,
            alternate_screen: true,
        },
        &mut alternate,
        GraphicsRegistries {
            inactive: &inactive,
            images: &images,
            virtuals: &virtuals,
            anims: &anims,
            relatives: &relatives,
        },
        &mut extractor,
    );

    assert!(alternate);
    assert!(images.lock().unwrap().is_empty());
    assert!(inactive.lock().unwrap().placements.is_empty());
}

#[test]
fn column_reflow_clears_regular_anchors_but_preserves_virtual_data() {
    let (images, virtuals, _anims, relatives, inactive) = registries();
    images.lock().unwrap().push(placement(1));
    virtuals.lock().unwrap().insert((1, 1), virtual_entry(1));
    relatives.lock().unwrap().insert((2, 1), relative_entry(2));
    {
        let mut saved = inactive.lock().unwrap();
        saved.placements.push(placement(3));
        saved.virtuals.insert((3, 1), virtual_entry(3));
        saved.relatives.insert((4, 1), relative_entry(4));
    }

    clear_reflowed_regular_placements(&images, &relatives, &inactive);

    assert!(images.lock().unwrap().is_empty());
    assert!(relatives.lock().unwrap().is_empty());
    assert!(virtuals.lock().unwrap().contains_key(&(1, 1)));
    let saved = inactive.lock().unwrap();
    assert!(saved.placements.is_empty());
    assert!(saved.relatives.is_empty());
    assert!(saved.virtuals.contains_key(&(3, 1)));
}

#[test]
fn history_pruning_cache_distinguishes_equal_origins_in_each_buffer() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, Arc::new(|| {}));
    let term: SharedTerm = Arc::new(Mutex::new(Term::new(
        TermConfig::default(),
        &TermSize {
            columns: 4,
            screen_lines: 2,
        },
        proxy,
    )));
    let mut processor: Processor = Processor::new();
    {
        let mut term = term.lock().unwrap();
        processor.advance(&mut *term, b"\x1b[?1049h0\r\n1\r\n2\r\n3");
    }
    let origin = term.lock().unwrap().grid().history_origin();
    assert!(origin > 0, "alternate grid discarded at least one row");

    let images: Images = Arc::new(Mutex::new(vec![placement(0)]));
    let mut pruner = ImageHistoryPruner {
        // A primary-buffer cache entry with the same numeric origin must
        // not suppress pruning after a screen transition.
        last_key: Some((false, origin)),
    };
    pruner.prune_if_changed(&term, &images);

    assert!(images.lock().unwrap().is_empty());
    assert_eq!(pruner.last_key, Some((true, origin)));
}
