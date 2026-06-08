//! iTerm2 inline image protocol: `OSC 1337 ; File=<args> : <base64> ST`.

use base64::Engine;

use crate::image::ImageData;

/// Decode an `OSC 1337` body (the bytes after `OSC` and before `ST`, i.e.
/// starting with `1337;File=`).
pub fn decode(body: &str) -> Option<ImageData> {
    let rest = body.strip_prefix("1337;File=")?;
    let (_args, b64) = rest.split_once(':')?;
    // Cycle 916 (file-by-file audit): STANDARD base64 rejects embedded whitespace
    // and `.trim()` only strips the ends, so a line-wrapped OSC-1337 body (raw
    // newlines aren't ST, so they reach the decoder) silently failed. Strip all
    // ASCII whitespace first.
    let cleaned: Vec<u8> = b64.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .ok()?;
    ImageData::from_encoded(&bytes)
}

#[cfg(test)]
mod tests {
    use super::decode;
    use base64::Engine;

    /// Encode a solid `w`×`h` RGBA PNG (the wire format an iTerm2 client
    /// sends), matching the helper used in `image.rs`'s decoder tests.
    fn png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
            .expect("encode test PNG");
        buf
    }

    #[test]
    fn decodes_a_well_formed_osc1337_body() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(png(3, 2));
        // `File=` args (size=…;inline=1) are ignored by the decoder; only
        // the base64 after the `:` matters.
        let body = format!("1337;File=size=99;inline=1:{b64}");
        let img = decode(&body).expect("valid OSC 1337 body should decode");
        assert_eq!((img.width, img.height), (3, 2));
    }

    #[test]
    fn rejects_malformed_bodies_without_panicking() {
        // Wrong OSC number / missing `File=` prefix.
        assert!(decode("1337;Foo=:abcd").is_none());
        assert!(decode("9;hello").is_none());
        // Missing the `:` that separates args from the payload.
        assert!(decode("1337;File=inline=1").is_none());
        // Present separator but the payload is not valid base64.
        assert!(decode("1337;File=inline=1:@@@not-base64@@@").is_none());
        // Valid base64 that is not a valid image → graceful None.
        let junk = base64::engine::general_purpose::STANDARD.encode(b"not an image");
        assert!(decode(&format!("1337;File=inline=1:{junk}")).is_none());
    }
}
