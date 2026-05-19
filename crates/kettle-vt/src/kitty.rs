//! Kitty graphics protocol decoder (common path: RGB/RGBA/PNG transmit, zlib
//! optional, chunked payloads). Spec: `kitty/docs/graphics-protocol.rst`.
//!
//! Advanced features (placements by id, deletion, Unicode placeholders,
//! animation) are intentionally out of scope for now — see ROADMAP.

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;

use crate::image::ImageData;

#[derive(Default)]
struct Acc {
    control: String,
    payload: String,
}

/// Reassembles chunked kitty transmissions until the final (`m=0`) chunk.
#[derive(Default)]
pub struct KittyState {
    in_flight: HashMap<u32, Acc>,
}

impl KittyState {
    /// Feed one APC `G` body (between `ESC _ G` and `ESC \`). Returns a decoded
    /// image once the last chunk of a transmission arrives.
    pub fn feed(&mut self, body: &str) -> Option<ImageData> {
        let (control, payload) = match body.split_once(';') {
            Some((c, p)) => (c, p),
            None => (body, ""),
        };
        let kv = parse_control(control);
        let id = kv.get("i").and_then(|v| v.parse().ok()).unwrap_or(0u32);
        let more = kv.get("m").map(|v| v == "1").unwrap_or(false);

        let acc = self.in_flight.entry(id).or_default();
        if acc.control.is_empty() {
            acc.control = control.to_string();
        }
        acc.payload.push_str(payload.trim());

        if more {
            return None;
        }
        let Acc { control, payload } = self.in_flight.remove(&id).unwrap_or_default();
        decode(&control, &payload)
    }
}

fn parse_control(s: &str) -> HashMap<String, String> {
    s.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn decode(control: &str, b64: &str) -> Option<ImageData> {
    let kv = parse_control(control);
    let action = kv.get("a").map(|s| s.as_str()).unwrap_or("t");
    if !matches!(action, "t" | "T") {
        return None; // placement/delete/query: unsupported for now
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let raw = if kv.get("o").map(|s| s == "z").unwrap_or(false) {
        let mut d = flate2::read::ZlibDecoder::new(&raw[..]);
        let mut out = Vec::new();
        d.read_to_end(&mut out).ok()?;
        out
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
