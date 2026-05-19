//! Kitty graphics protocol decoder. Common transmission path (RGB/RGBA/PNG,
//! zlib optional, chunked) plus the advanced ops kettle supports:
//!
//! - `a=t` transmit only (store, don't display) — later shown with `a=p`
//! - `a=T` transmit and display
//! - `a=p` put a previously transmitted image (by `i=` id) at the cursor
//! - `a=d` delete images (all, or by `i=` id)
//! - `z=`  z-index ordering between images
//! - `U=1` *virtual placement*: the image is stored and a rows×cols virtual
//!   placement registered, but nothing is drawn at the cursor — it is shown
//!   later via Unicode placeholder text (see [`crate::placeholder`])
//!
//! Spec: `kitty/docs/graphics-protocol.rst`. Animation and relative
//! placements remain out of scope (see ROADMAP).

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;

use crate::image::{ImageData, Placed};

#[derive(Default)]
struct Acc {
    control: String,
    payload: String,
}

/// A `U=1` virtual placement: the image is fit into a `cols`×`rows`
/// rectangle and displayed later via Unicode placeholder cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualPlacement {
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
}

/// What a kitty APC resolved to.
pub enum KittyOut {
    None,
    Place(Placed),
    Delete {
        all: bool,
        id: Option<u32>,
    },
    /// A virtual placement was (re)registered for image `id`; nothing is
    /// drawn now — the renderer composites it where placeholder cells appear.
    Virtual {
        id: u32,
    },
}

/// Reassembles chunked transmissions and remembers transmitted images so
/// they can be placed later by id.
#[derive(Default)]
pub struct KittyState {
    in_flight: HashMap<u32, Acc>,
    store: HashMap<u32, ImageData>,
    virtual_placements: HashMap<u32, VirtualPlacement>,
}

impl KittyState {
    /// Feed one APC `G` body (between `ESC _ G` and `ESC \`).
    pub fn feed(&mut self, body: &str) -> KittyOut {
        let (control, payload) = body.split_once(';').unwrap_or((body, ""));
        let kv = parse_control(control);
        let id = kv.get("i").and_then(|v| v.parse().ok()).unwrap_or(0u32);
        let action = kv.get("a").map(|s| s.as_str()).unwrap_or("t");
        let z = kv.get("z").and_then(|v| v.parse().ok()).unwrap_or(0i32);

        let virt = kv.get("U").map(|v| v == "1").unwrap_or(false);
        let dim = |k: &str| kv.get(k).and_then(|v| v.parse::<u32>().ok());

        // Control-only ops are never chunked.
        if action == "d" {
            self.in_flight.clear();
            let target = kv.get("d").map(|s| s.as_str()).unwrap_or("a");
            return match target {
                "i" | "I" => {
                    self.store.remove(&id);
                    self.virtual_placements.remove(&id);
                    KittyOut::Delete {
                        all: false,
                        id: Some(id),
                    }
                }
                _ => {
                    self.store.clear();
                    self.virtual_placements.clear();
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
            // `a=p,U=1` registers a virtual placement (shown later via
            // placeholder text); plain `a=p` puts the image at the cursor.
            if virt {
                self.virtual_placements.insert(
                    id,
                    VirtualPlacement {
                        cols: dim("c").unwrap_or(0),
                        rows: dim("r").unwrap_or(0),
                        z,
                    },
                );
                return KittyOut::Virtual { id };
            }
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
        // `U=1` (possibly combined with `a=T`): store + register a virtual
        // placement, but draw nothing at the cursor.
        if first.get("U").map(|v| v == "1").unwrap_or(false) {
            let fz = first.get("z").and_then(|v| v.parse().ok()).unwrap_or(z);
            self.virtual_placements.insert(
                id,
                VirtualPlacement {
                    cols: first.get("c").and_then(|v| v.parse().ok()).unwrap_or(0),
                    rows: first.get("r").and_then(|v| v.parse().ok()).unwrap_or(0),
                    z: fz,
                },
            );
            return KittyOut::Virtual { id };
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

    /// A stored image by id (for compositing Unicode-placeholder cells).
    pub fn image(&self, id: u32) -> Option<&ImageData> {
        self.store.get(&id)
    }

    /// The registered virtual placement for an image id, if any.
    pub fn virtual_placement(&self, id: u32) -> Option<&VirtualPlacement> {
        self.virtual_placements.get(&id)
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

#[cfg(test)]
mod tests {
    use super::*;

    // One opaque RGBA pixel (f=32,s=1,v=1): bytes [1,2,3,4].
    const PX: &str = "AQIDBA==";

    #[test]
    fn transmit_and_display_virtual_placement() {
        let mut k = KittyState::default();
        let out = k.feed(&format!("a=T,U=1,i=7,c=2,r=3,f=32,s=1,v=1;{PX}"));
        assert!(
            matches!(out, KittyOut::Virtual { id: 7 }),
            "a=T,U=1 must register a virtual placement, not draw at cursor"
        );
        assert!(
            k.image(7).is_some(),
            "image still stored for later compositing"
        );
        assert_eq!(
            k.virtual_placement(7).copied(),
            Some(VirtualPlacement {
                cols: 2,
                rows: 3,
                z: 0
            })
        );
    }

    #[test]
    fn transmit_then_put_virtual() {
        let mut k = KittyState::default();
        // a=t stores only (no placement).
        assert!(matches!(
            k.feed(&format!("a=t,i=8,f=32,s=1,v=1;{PX}")),
            KittyOut::None
        ));
        // a=p,U=1 registers the virtual placement by id.
        let out = k.feed("a=p,U=1,i=8,c=4,r=1,z=5");
        assert!(matches!(out, KittyOut::Virtual { id: 8 }));
        assert_eq!(
            k.virtual_placement(8).copied(),
            Some(VirtualPlacement {
                cols: 4,
                rows: 1,
                z: 5
            })
        );
        // Plain a=p (no U) still draws at the cursor.
        assert!(matches!(
            k.feed("a=p,i=8"),
            KittyOut::Place(p) if p.id == Some(8)
        ));
    }

    #[test]
    fn delete_clears_virtual_placement() {
        let mut k = KittyState::default();
        k.feed(&format!("a=T,U=1,i=9,c=1,r=1,f=32,s=1,v=1;{PX}"));
        assert!(k.virtual_placement(9).is_some());
        k.feed("a=d,d=i,i=9");
        assert!(
            k.virtual_placement(9).is_none(),
            "delete-by-id drops the virtual placement too"
        );
    }
}
