//! iTerm2 inline image protocol: `OSC 1337 ; File=<args> : <base64> ST`.

use base64::Engine;

use crate::graphics_limits::GraphicsBudget;
use crate::image::ImageData;

/// Decode an `OSC 1337` body (the bytes after `OSC` and before `ST`, i.e.
/// starting with `1337;File=`), returning the image **only** when it is meant
/// to be displayed inline.
///
/// The `File=` args are `;`-separated `key=value` pairs. iTerm2's `inline` key
/// governs display: `inline=1` draws the payload in the terminal grid, while
/// `inline=0` (or an absent `inline`) is a plain file *transfer* (a download),
/// which must NOT be rendered as an image. We therefore parse the args and
/// return `None` for any non-inline transfer — the bytes are simply consumed,
/// matching iTerm2's default-to-download behavior.
pub fn decode(body: &str) -> Option<ImageData> {
    decode_with_budget(body, &GraphicsBudget::default())
}

pub(crate) fn decode_with_budget(body: &str, budget: &GraphicsBudget) -> Option<ImageData> {
    if body.len() > budget.limits().sequence_bytes {
        return None;
    }
    let rest = body.strip_prefix("1337;File=")?;
    let (args, b64) = rest.split_once(':')?;
    // Only inline=1 is displayed; absent/0/other → file download, not an image.
    let inline = args.split(';').any(|kv| {
        kv.split_once('=')
            .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case("inline") && v.trim() == "1")
    });
    if !inline {
        return None;
    }
    // STANDARD base64 rejects embedded whitespace and `.trim()` only strips
    // the ends, so a line-wrapped OSC-1337 body (raw newlines aren't ST, so
    // they reach the decoder) silently failed. Strip all ASCII whitespace
    // first.
    let _cleaned_reservation = budget.reserve_transient_cpu(b64.len().max(1))?;
    let mut cleaned = Vec::new();
    cleaned.try_reserve_exact(b64.len()).ok()?;
    cleaned.extend(b64.bytes().filter(|b| !b.is_ascii_whitespace()));
    let decoded_cap = cleaned
        .len()
        .checked_add(3)?
        .checked_div(4)?
        .checked_mul(3)?
        .max(1);
    let _decoded_reservation = budget.reserve_transient_cpu(decoded_cap)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .ok()?;
    ImageData::from_encoded_with_budget(&bytes, budget)
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
        // `inline=1` marks it for display; the size= arg is informational. Only
        // an inline image decodes; the base64 after the `:` is the payload.
        let body = format!("1337;File=size=99;inline=1:{b64}");
        let img = decode(&body).expect("valid OSC 1337 body should decode");
        assert_eq!((img.width, img.height), (3, 2));
    }

    #[test]
    fn non_inline_file_transfer_is_not_rendered() {
        // inline=0 or an absent `inline` key is a plain file download (iTerm2's
        // default), NOT an image to draw — decode must return None so the bytes
        // are consumed rather than splatted onto the grid as a bogus image.
        let b64 = base64::engine::general_purpose::STANDARD.encode(png(3, 2));
        // Explicit inline=0.
        assert!(
            decode(&format!("1337;File=name=Zg==;size=10;inline=0:{b64}")).is_none(),
            "inline=0 must not render inline"
        );
        // Absent inline key → defaults to download.
        assert!(
            decode(&format!("1337;File=name=Zg==;size=10:{b64}")).is_none(),
            "absent inline must not render inline"
        );
        // Empty args (no inline) → still a download.
        assert!(
            decode(&format!("1337;File=:{b64}")).is_none(),
            "no args means no inline display"
        );
        // inline=1 still renders (case-insensitive key, surrounding-space tolerant).
        let img = decode(&format!("1337;File=inline=1;size=10:{b64}"))
            .expect("inline=1 should still render");
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
