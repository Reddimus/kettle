//! Kitty graphics protocol decoder. Common transmission path (RGB/RGBA/PNG,
//! zlib optional, chunked) plus the advanced ops kettle supports:
//!
//! - `a=t` transmit only (store, don't display) — later shown with `a=p`
//! - `a=T` transmit and display
//! - `a=p` put a previously transmitted image (by `i=` id) at the cursor
//! - `a=d` delete images (all, or by `i=` id)
//! - `z=`  z-index ordering between images
//!
//! Spec: `kitty/docs/graphics-protocol.rst`. Animation, Unicode placeholders
//! and relative placements remain out of scope (see ROADMAP).

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;

use crate::image::{ImageData, Placed};

#[derive(Default)]
struct Acc {
    control: String,
    payload: String,
}

/// What a kitty APC resolved to.
pub enum KittyOut {
    None,
    Place(Placed),
    Delete { all: bool, id: Option<u32> },
}

/// Reassembles chunked transmissions and remembers transmitted images so
/// they can be placed later by id.
#[derive(Default)]
pub struct KittyState {
    in_flight: HashMap<u32, Acc>,
    store: HashMap<u32, ImageData>,
}

impl KittyState {
    /// Feed one APC `G` body (between `ESC _ G` and `ESC \`).
    pub fn feed(&mut self, body: &str) -> KittyOut {
        let (control, payload) = body.split_once(';').unwrap_or((body, ""));
        let kv = parse_control(control);
        let id = kv.get("i").and_then(|v| v.parse().ok()).unwrap_or(0u32);
        let action = kv.get("a").map(|s| s.as_str()).unwrap_or("t");
        let z = kv.get("z").and_then(|v| v.parse().ok()).unwrap_or(0i32);

        // Control-only ops are never chunked.
        if action == "d" {
            self.in_flight.clear();
            let target = kv.get("d").map(|s| s.as_str()).unwrap_or("a");
            return match target {
                "i" | "I" => {
                    self.store.remove(&id);
                    KittyOut::Delete {
                        all: false,
                        id: Some(id),
                    }
                }
                _ => {
                    self.store.clear();
                    KittyOut::Delete {
                        all: true,
                        id: None,
                    }
                }
            };
        }
        if action == "q" {
            return KittyOut::None; // capability query — nothing to render
        }
        if action == "p" {
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
        let acc = self.in_flight.entry(id).or_default();
        if acc.control.is_empty() {
            acc.control = control.to_string();
        }
        acc.payload.push_str(payload.trim());
        if more {
            return KittyOut::None;
        }
        let Acc { control, payload } = self.in_flight.remove(&id).unwrap_or_default();
        let first = parse_control(&control);
        let Some(img) = decode(&control, &payload) else {
            return KittyOut::None;
        };
        if id != 0 {
            self.store.insert(id, img.clone());
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
}

fn parse_control(s: &str) -> HashMap<String, String> {
    s.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn decode(control: &str, b64: &str) -> Option<ImageData> {
    let kv = parse_control(control);
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
