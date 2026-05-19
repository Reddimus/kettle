//! iTerm2 inline image protocol: `OSC 1337 ; File=<args> : <base64> ST`.

use base64::Engine;

use crate::image::ImageData;

/// Decode an `OSC 1337` body (the bytes after `OSC` and before `ST`, i.e.
/// starting with `1337;File=`).
pub fn decode(body: &str) -> Option<ImageData> {
    let rest = body.strip_prefix("1337;File=")?;
    let (_args, b64) = rest.split_once(':')?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    ImageData::from_encoded(&bytes)
}
