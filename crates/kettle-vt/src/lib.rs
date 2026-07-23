//! kettle-vt: image-protocol + shell-integration extractor that sits in
//! front of the VT engine.
//!
//! Sixel (DCS), the kitty graphics protocol (APC `G`), and iTerm2 inline
//! images (OSC 1337) are pulled out of the PTY byte stream by
//! [`Extractor`], decoded to RGBA [`ImageData`], and handed to the
//! renderer for GPU compositing. Everything else passes through
//! byte-for-byte (BEL vs ST terminator preserved) so the engine still
//! sees a correct VT stream.
//!
//! The extractor also handles two non-image protocols whose semantics
//! belong upstream of the engine:
//! - **OSC 7** (cwd report) — `Chunk::Cwd(path)` for the UI's cwd
//!   tracker; powers session restore + new-tab/new-split inheriting the
//!   focused pane's directory.
//! - **OSC 133** (FinalTerm shell integration) — `Chunk::Prompt(kind)`
//!   for the jump-to-prompt navigation (Ctrl+Up / Ctrl+Down).
//! - **OSC 9/777 notifications** — `Chunk::Notification { title, body }`
//!   for desktop notification dispatch.
//!
//! Modules (all `pub`):
//! - [`extract`] — the state-machine `Extractor` itself; main entry
//!   point [`Extractor::feed`].
//! - [`sixel`] — Sixel DCS parser → `ImageData`.
//! - [`kitty`] — kitty graphics protocol (a=t/T/p/a/c/d/f, chunked
//!   payloads, animation frames + control, Unicode placeholder cells,
//!   relative placements).
//! - [`iterm`] — OSC 1337 inline-image decoder.
//! - [`image`] — `ImageData` (RGBA pixel buffer + dimensions) and
//!   `Placed` placement geometry shared by all three protocols.
//! - [`placeholder`] — Unicode-placeholder (`U+10EEEE` + zero-width
//!   diacritics) decode path for kitty `U=1` virtual placements.

pub mod extract;
pub mod graphics_limits;
pub mod image;
pub mod iterm;
pub mod kitty;
pub mod placeholder;
pub mod sixel;

pub use extract::{Chunk, Extractor, Progress, PromptKind};
pub use graphics_limits::{GraphicsBudget, GraphicsLimits, GraphicsReservation};
pub use image::{ImageData, Placed};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text_through() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"hello world");
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            Chunk::Pass(b) => assert_eq!(b, b"hello world"),
            _ => panic!("expected pass"),
        }
    }

    #[test]
    fn extracts_iterm_image() {
        // 1x1 red PNG.
        let png = base64_png();
        let seq = format!("\x1b]1337;File=inline=1:{png}\x07");
        let mut e = Extractor::new();
        let chunks = e.feed(seq.as_bytes());
        assert!(
            chunks.iter().any(|c| matches!(c, Chunk::Image(_))),
            "expected an image chunk, got {chunks:?}"
        );
    }

    #[test]
    fn osc1_icon_name_rewrites_to_osc2_window_title() {
        // vim / tmux / ranger / mc emit OSC 1 (icon name) to set their
        // "short" tab title. VTE/alacritty silently drop OSC 1 (their
        // dispatch only matches "0" and "2"), so the title disappeared.
        // kitty / iTerm2 / Gnome Terminal / Konsole treat OSC 1 the
        // same as OSC 2; we rewrite the leading byte so VTE picks it
        // up downstream and `TermEvent::Title` actually fires.
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]1;short label\x07");
        // Only a Pass chunk — no consumed handler. The bytes are
        // forwarded with OSC `2` so the VT engine sets the title.
        let forwarded: Vec<u8> = chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(forwarded, b"\x1b]2;short label\x07");

        // ST-terminated form is rewritten too (vim uses `\e\\` more
        // often than `\a`).
        let mut e2 = Extractor::new();
        let chunks2 = e2.feed(b"\x1b]1;vim - file.rs\x1b\\");
        let forwarded2: Vec<u8> = chunks2
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(forwarded2, b"\x1b]2;vim - file.rs\x1b\\");

        // OSC 0 / OSC 2 are untouched (we don't want to double-rewrite
        // or accidentally munge a real OSC 2). The previous test
        // (`non_image_osc_passes_through`) already pins OSC 0; pin OSC
        // 2 here too for symmetry.
        let mut e3 = Extractor::new();
        let chunks3 = e3.feed(b"\x1b]2;real title\x07");
        let forwarded3: Vec<u8> = chunks3
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(forwarded3, b"\x1b]2;real title\x07");
    }

    #[test]
    fn non_image_osc_passes_through() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]0;my title\x07");
        assert!(chunks.iter().all(|c| matches!(c, Chunk::Pass(_))));
        // The original bytes must survive intact for the VT engine.
        let mut out = Vec::new();
        for c in chunks {
            if let Chunk::Pass(b) = c {
                out.extend(b);
            }
        }
        assert_eq!(out, b"\x1b]0;my title\x07");
    }

    #[test]
    fn osc7_percent_decodes_utf8_paths_correctly() {
        // Shells (zsh `print -P %d`, bash via `printf`) percent-encode
        // each *UTF-8 byte* of a non-ASCII filename individually, so a
        // path ending in `café` arrives as `caf%C3%A9` — two encoded
        // bytes that together form U+00E9 (é). The old parser pushed
        // each decoded byte as a `char`, which gave the Latin-1 garbage
        // `cafÃ©` and broke prompt-tracking on any non-ASCII directory.
        // `localhost` host: hostname-neutral so the decode coverage is
        // deterministic on every machine (v2.20.0 validates real hostnames
        // against this machine's name — that policy has its own
        // injected-host test in extract.rs).
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]7;file://localhost/home/u/caf%C3%A9\x1b\\");
        let cwd = chunks.iter().find_map(|c| match c {
            Chunk::Cwd(p) => Some(p.clone()),
            _ => None,
        });
        assert_eq!(cwd.as_deref(), Some("/home/u/café"));

        // Mixed: space + UTF-8 + plain. Combines the three cases that
        // were each broken in isolation.
        let mut e2 = Extractor::new();
        let chunks2 = e2.feed(b"\x1b]7;file:///tmp/work%20dir/caf%C3%A9/x\x07");
        let cwd2 = chunks2.iter().find_map(|c| match c {
            Chunk::Cwd(p) => Some(p.clone()),
            _ => None,
        });
        assert_eq!(cwd2.as_deref(), Some("/tmp/work dir/café/x"));
    }

    #[test]
    fn osc7_percent_followed_by_multibyte_char_does_not_panic() {
        // Regression: the percent-decoder sliced the &str (`&path[i+1..i+3]`)
        // when it saw `%`. A `%` immediately followed by a multibyte UTF-8
        // char (here `€`, 3 bytes) made the slice land on a non-char-boundary
        // and panic — a hard crash under panic=abort, triggerable by any
        // program writing an OSC 7 report to the PTY. The fix slices the
        // *bytes* and validates via from_utf8, so the stray `%` is kept
        // literally and decoding continues.
        // `localhost` (hostname-neutral): a REJECTED host short-circuits
        // before the decoder runs, which would silently skip the
        // panic-regression path this test exists for.
        let mut e = Extractor::new();
        let chunks = e.feed("\x1b]7;file://localhost/p/%€x\x1b\\".as_bytes());
        let cwd = chunks.iter().find_map(|c| match c {
            Chunk::Cwd(p) => Some(p.clone()),
            _ => None,
        });
        // No panic; the malformed `%` survives as a literal and the rest of
        // the path is intact.
        assert_eq!(cwd.as_deref(), Some("/p/%€x"));

        // `%` with one trailing byte then EOF, and `%` followed by a
        // non-hex multibyte char, also must not panic.
        let mut e2 = Extractor::new();
        let _ = e2.feed("\x1b]7;file://localhost/a%é\x1b\\".as_bytes());
        let mut e3 = Extractor::new();
        let _ = e3.feed(b"\x1b]7;file://localhost/trailing%\x1b\\");
    }

    #[test]
    fn osc7_and_osc133_are_consumed() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"x\x1b]7;file://localhost/tmp/work%20dir\x1b\\y\x1b]133;A\x1b\\z");
        let cwd = chunks.iter().find_map(|c| match c {
            Chunk::Cwd(p) => Some(p.clone()),
            _ => None,
        });
        assert_eq!(cwd.as_deref(), Some("/tmp/work dir"));
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, Chunk::Prompt(extract::PromptKind::PromptStart)))
        );
        // Surrounding text still passes through.
        let passed: Vec<u8> = chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(passed, b"xyz");
    }

    fn progress_chunks(chunks: &[Chunk]) -> Vec<Progress> {
        chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Progress(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    fn notification_chunks(chunks: &[Chunk]) -> Vec<(String, String)> {
        chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Notification { title, body } => Some((title.clone(), body.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn osc9_4_progress_is_parsed_and_consumed() {
        // state 1 with pct, BEL-terminated; surrounding text passes through.
        let mut e = Extractor::new();
        let chunks = e.feed(b"a\x1b]9;4;1;42\x07b");
        assert_eq!(progress_chunks(&chunks), vec![Progress::Normal(42)]);
        let passed: Vec<u8> = chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(passed, b"ab");

        // All states, ESC\-terminated. state 0/3 carry no pct.
        let mut e = Extractor::new();
        let chunks =
            e.feed(b"\x1b]9;4;0\x1b\\\x1b]9;4;2;7\x1b\\\x1b]9;4;3\x1b\\\x1b]9;4;4;90\x1b\\");
        assert_eq!(
            progress_chunks(&chunks),
            vec![
                Progress::Clear,
                Progress::Error(7),
                Progress::Indeterminate,
                Progress::Warning(90),
            ]
        );

        // Over-range pct clamps to 100.
        let mut e = Extractor::new();
        assert_eq!(
            progress_chunks(&e.feed(b"\x1b]9;4;1;250\x07")),
            vec![Progress::Normal(100)]
        );

        // An unknown state is dropped (no Progress chunk, not forwarded raw).
        let mut e = Extractor::new();
        assert!(progress_chunks(&e.feed(b"\x1b]9;4;9;5\x07")).is_empty());

        // A non-9;4 OSC 9 is a notification, not a progress chunk.
        let mut e = Extractor::new();
        assert!(progress_chunks(&e.feed(b"\x1b]9;hello\x07")).is_empty());
    }

    #[test]
    fn osc9_and_osc777_notifications_are_parsed_and_consumed() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"a\x1b]9;Build finished\x07b");
        assert_eq!(
            notification_chunks(&chunks),
            vec![("Build finished".to_string(), String::new())]
        );
        assert_eq!(
            chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Pass(b) => Some(b.clone()),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>(),
            b"ab"
        );

        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]777;notify;Build done;cargo test passed\x1b\\");
        assert_eq!(
            notification_chunks(&chunks),
            vec![("Build done".to_string(), "cargo test passed".to_string())]
        );

        // Control characters are cleaned before the UI sees the fields.
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]777;notify;Bad\rTitle;line1\nline2\x07");
        assert_eq!(
            notification_chunks(&chunks),
            vec![("Bad Title".to_string(), "line1\nline2".to_string())]
        );

        // Unknown OSC 777 commands are not ours; preserve them byte-for-byte.
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]777;unknown;payload\x07");
        assert!(notification_chunks(&chunks).is_empty());
        assert_eq!(
            chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Pass(b) => Some(b.clone()),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>(),
            b"\x1b]777;unknown;payload\x07"
        );

        // Oversized fields are dropped instead of allocating/dispatching a
        // huge desktop notification.
        let mut e = Extractor::new();
        let huge = format!("\x1b]9;{}\x07", "x".repeat(9 << 10));
        assert!(notification_chunks(&e.feed(huge.as_bytes())).is_empty());
    }

    #[test]
    fn sixel_decodes_a_white_column() {
        // color 0 = white (RGB 100%), then `~` = all six pixels in the band.
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1bP0;0;0q#0;2;100;100;100~\x1b\\");
        let img = chunks.iter().find_map(|c| match c {
            Chunk::Image(d) => Some(d.img.clone()),
            _ => None,
        });
        let img = img.expect("sixel image");
        assert_eq!((img.width, img.height), (1, 6));
        assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn kitty_rgba_and_chunking() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([9u8, 8, 7, 255]);
        let (h1, h2) = b64.split_at(b64.len() / 2);
        let mut e = Extractor::new();
        // First chunk (m=1) then final chunk (m=0) must reassemble.
        let mut chunks = e.feed(format!("\x1b_Gf=32,s=1,v=1,a=T,m=1;{h1}\x1b\\").as_bytes());
        chunks.extend(e.feed(format!("\x1b_Gm=0;{h2}\x1b\\").as_bytes()));
        let img = chunks.iter().find_map(|c| match c {
            Chunk::Image(d) => Some(d.img.clone()),
            _ => None,
        });
        let img = img.expect("kitty image");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(&img.rgba[..], &[9, 8, 7, 255]);
    }

    #[test]
    fn kitty_transmit_then_place_by_id_and_delete() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 255]);
        let mut e = Extractor::new();
        // a=t: transmit only (id 7) — no image shown yet.
        let c1 = e.feed(format!("\x1b_Gf=32,s=1,v=1,i=7,a=t;{b64}\x1b\\").as_bytes());
        assert!(!c1.iter().any(|c| matches!(c, Chunk::Image(_))));
        // a=p: place stored image 7 with a z-index.
        let c2 = e.feed(b"\x1b_Ga=p,i=7,z=5\x1b\\");
        let placed = c2.iter().find_map(|c| match c {
            Chunk::Image(p) => Some(p.clone()),
            _ => None,
        });
        let p = placed.expect("placed by id");
        assert_eq!(p.id, Some(7));
        assert_eq!(p.z, 5);
        assert_eq!(&p.img.rgba[..], &[1, 2, 3, 255]);
        // a=d,d=i: delete image 7.
        let c3 = e.feed(b"\x1b_Ga=d,d=i,i=7\x1b\\");
        assert!(c3.iter().any(|c| matches!(
            c,
            Chunk::DeleteImages {
                all: false,
                id: Some(7)
            }
        )));
        // After deletion it can no longer be placed.
        let c4 = e.feed(b"\x1b_Ga=p,i=7\x1b\\");
        assert!(!c4.iter().any(|c| matches!(c, Chunk::Image(_))));
    }

    #[test]
    fn kitty_relative_placement_surfaces_child_and_parent() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([3u8, 3, 3, 255]);
        let mut e = Extractor::new();
        // Parent image 1 and child image 2 stored (a=t = no cursor draw).
        e.feed(format!("\x1b_Gf=32,s=1,v=1,i=1,a=t;{b64}\x1b\\").as_bytes());
        e.feed(format!("\x1b_Gf=32,s=1,v=1,i=2,a=t;{b64}\x1b\\").as_bytes());
        let cs = e.feed(b"\x1b_Ga=p,i=2,p=7,P=1,Q=1,H=2,V=-1\x1b\\");
        assert!(
            !cs.iter().any(|c| matches!(c, Chunk::Image(_))),
            "relative placement must not draw at the cursor"
        );
        let rp = cs
            .iter()
            .find_map(|c| match c {
                Chunk::RelativePlacement {
                    id,
                    placement,
                    parent_img,
                    parent_placement,
                    h,
                    v,
                    img,
                } => Some((
                    *id,
                    *placement,
                    *parent_img,
                    *parent_placement,
                    *h,
                    *v,
                    img.clone(),
                )),
                _ => None,
            })
            .expect("a RelativePlacement chunk");
        assert_eq!((rp.0, rp.1, rp.2, rp.3, rp.4, rp.5), (2, 7, 1, 1, 2, -1));
        assert_eq!((rp.6.width, rp.6.height), (1, 1));
    }

    #[test]
    fn kitty_animation_snapshot_surfaces_sequence() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([7u8, 7, 7, 255]);
        let mut e = Extractor::new();
        // Base image (root frame) for id 2.
        e.feed(format!("\x1b_Gf=32,s=1,v=1,i=2,a=T;{b64}\x1b\\").as_bytes());
        // One animation frame with a 40ms gap.
        e.feed(format!("\x1b_Ga=f,i=2,f=32,s=1,v=1,z=40;{b64}\x1b\\").as_bytes());
        // Control: run looping, root gap 60ms via r=1.
        let cs = e.feed(b"\x1b_Ga=a,i=2,s=3,r=1,z=60\x1b\\");
        let snap = cs
            .iter()
            .rev()
            .find_map(|c| match c {
                Chunk::Animation {
                    id,
                    imgs,
                    gaps,
                    state,
                } => Some((*id, imgs.len(), gaps.clone(), *state)),
                _ => None,
            })
            .expect("an Animation snapshot");
        assert_eq!(snap.0, 2);
        assert_eq!(snap.1, 2, "root + 1 frame");
        assert_eq!(snap.2, vec![60, 40], "root gap then frame gap");
        assert!(snap.3.running && !snap.3.loading);
    }

    #[test]
    fn kitty_virtual_placement_surfaces_image_not_at_cursor() {
        use base64::Engine;
        // 2×1 RGBA image (8 bytes).
        let b64 =
            base64::engine::general_purpose::STANDARD.encode([10u8, 20, 30, 255, 40, 50, 60, 255]);
        let mut e = Extractor::new();
        // a=T,U=1: transmit + virtual placement → VirtualImage, no Image.
        let c = e.feed(format!("\x1b_Ga=T,U=1,i=5,c=2,r=1,f=32,s=2,v=1;{b64}\x1b\\").as_bytes());
        assert!(
            !c.iter().any(|c| matches!(c, Chunk::Image(_))),
            "U=1 must not place at the cursor"
        );
        let v = c
            .iter()
            .find_map(|c| match c {
                Chunk::VirtualImage {
                    id,
                    img,
                    cols,
                    rows,
                    z,
                } => Some((*id, img.clone(), *cols, *rows, *z)),
                _ => None,
            })
            .expect("a VirtualImage chunk");
        assert_eq!((v.0, v.2, v.3, v.4), (5, 2, 1, 0));
        assert_eq!((v.1.width, v.1.height), (2, 1));
        // Deleting the id reaps the virtual image too.
        let d = e.feed(b"\x1b_Ga=d,d=i,i=5\x1b\\");
        assert!(d.iter().any(|c| matches!(
            c,
            Chunk::DeleteImages {
                all: false,
                id: Some(5)
            }
        )));
    }

    #[test]
    fn sequence_split_across_feeds() {
        let png = base64_png();
        let seq = format!("\x1b]1337;File=inline=1:{png}\x07");
        let bytes = seq.as_bytes();
        // This test owns an isolated account because it verifies streaming
        // state, not contention in the process-wide graphics budget.
        let mut e = Extractor::isolated();
        let mut images = 0;
        for b in bytes {
            for c in e.feed(&[*b]) {
                if matches!(c, Chunk::Image(_)) {
                    images += 1;
                }
            }
        }
        assert_eq!(images, 1, "image must survive byte-by-byte delivery");
    }

    #[test]
    fn large_output_is_linear_and_intact() {
        // ~8 MiB of plain text with image sequences interleaved must pass
        // through correctly and quickly.
        let mut input = Vec::new();
        for _ in 0..200_000 {
            input.extend_from_slice(b"the quick brown fox 0123456789\n");
        }
        let plain_len = input.len();
        let png = base64_png();
        input.extend(format!("\x1b]1337;File=inline=1:{png}\x07").bytes());

        let t = std::time::Instant::now();
        let mut e = Extractor::new();
        let mut passed = 0usize;
        let mut images = 0usize;
        for ch in e.feed(&input) {
            match ch {
                Chunk::Pass(b) => passed += b.len(),
                Chunk::Image(_) => images += 1,
                _ => {}
            }
        }
        assert_eq!(passed, plain_len);
        assert_eq!(images, 1);
        assert!(
            t.elapsed().as_secs() < 5,
            "8MiB extraction took too long: {:?}",
            t.elapsed()
        );
    }

    fn base64_png() -> String {
        use base64::Engine;
        let img = image::ImageData::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        let rgba = ::image::RgbaImage::from_raw(1, 1, img.rgba.as_ref().clone()).unwrap();
        ::image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut buf, ::image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
    }
}
