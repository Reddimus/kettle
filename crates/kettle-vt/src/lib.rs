//! kettle-vt: image-protocol support layered in front of the VT engine.
//!
//! Sixel, the kitty graphics protocol and iTerm2 inline images are extracted
//! from the PTY stream by [`Extractor`], decoded to RGBA [`ImageData`], and
//! handed to the renderer for GPU compositing.

pub mod extract;
pub mod image;
pub mod iterm;
pub mod kitty;
pub mod sixel;

pub use extract::{Chunk, Extractor, PromptKind};
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
    fn osc7_and_osc133_are_consumed() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"x\x1b]7;file://host/tmp/work%20dir\x1b\\y\x1b]133;A\x1b\\z");
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
    fn sequence_split_across_feeds() {
        let png = base64_png();
        let seq = format!("\x1b]1337;File=inline=1:{png}\x07");
        let bytes = seq.as_bytes();
        let mut e = Extractor::new();
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
        input.extend(format!("\x1b]1337;File=:{png}\x07").bytes());

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
