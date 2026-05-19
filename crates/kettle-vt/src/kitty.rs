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
//! - `a=f` transmit animation frames; `a=a` animation control (current
//!   frame / run-stop / loop count / per-frame gap); `a=d,d=f` frame delete
//!
//! Spec: `kitty/docs/graphics-protocol.rst`. Frame compositing (`a=c`),
//! partial-rect frames, playback timing and relative placements remain out
//! of scope for now (see ROADMAP).

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

/// One animation frame of an image (`a=f`). `gap_ms`: `0` = unset,
/// `> 0` = display this many ms, `< 0` = *gapless* (skipped on playback,
/// kept only as base data). Partial-rect frames (`x,y` offsets) and
/// frame-composition (`a=c`) are not modelled yet — see ROADMAP.
#[derive(Debug, Clone)]
pub struct Frame {
    pub img: ImageData,
    pub gap_ms: i32,
}

/// Per-image animation control state (`a=a`), set by the client and read by
/// the renderer's playback loop (a later cycle). `current` is 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationState {
    pub current: u32,
    /// `false` = stopped (`s=1`); `true` = running (`s=2|3`).
    pub running: bool,
    /// `s=2`: wait for more frames at the end instead of looping.
    pub loading: bool,
    /// Loop count: `0` = infinite, `n` = play `n` times (kitty `v`,
    /// normalized: `v=1`→infinite→0 here, `v=n`→`n-1`).
    pub loops: u32,
    /// Gap (ms) of the *root* frame (frame 1 = the base image); only
    /// settable via `a=a,r=1,z=` since the root has no gap by default.
    pub root_gap: i32,
}

impl Default for AnimationState {
    fn default() -> Self {
        AnimationState {
            current: 1,
            running: false,
            loading: false,
            loops: 0,
            root_gap: 0,
        }
    }
}

/// The 0-based frame index to display *now*.
///
/// `gaps[i]` is frame `i+1`'s gap in milliseconds: `> 0` dwell that long,
/// `<= 0` *gapless* (kept as base data but never dwelt on, so skipped for
/// display — kitty `graphics-protocol.rst:909`). `gaps[0]` is the root /
/// base-image frame. Pure and deterministic so the renderer's clock is the
/// only non-testable part.
///
/// - Stopped (`s=1`): the explicitly selected `current` frame, clamped.
/// - Running: time is mapped over the displayable frames. `loops == 0`
///   (kitty `v=1`) loops forever; a finite count stops on the last
///   displayable frame after that many full passes. `loading` (`s=2`)
///   never loops — it holds on the last frame waiting for more frames.
/// - No displayable frame ⇒ hold on `current`.
pub fn current_frame(gaps: &[i32], st: &AnimationState, elapsed_ms: u128) -> usize {
    if gaps.is_empty() {
        return 0;
    }
    let clamp_current = || (st.current.max(1) as usize - 1).min(gaps.len() - 1);
    let shown: Vec<(usize, u128)> = gaps
        .iter()
        .enumerate()
        .filter(|&(_, &g)| g > 0)
        .map(|(i, &g)| (i, g as u128))
        .collect();
    if !st.running || shown.is_empty() {
        return clamp_current();
    }
    let total: u128 = shown.iter().map(|&(_, g)| g).sum();
    if total == 0 {
        return clamp_current();
    }
    // Finite, non-loading loop count: freeze on the last shown frame once
    // all passes have elapsed.
    if !st.loading && st.loops > 0 && elapsed_ms >= total * st.loops as u128 {
        return shown.last().unwrap().0;
    }
    // Loading mode never loops: hold the last shown frame at/after the end.
    if st.loading && elapsed_ms >= total {
        return shown.last().unwrap().0;
    }
    let mut t = elapsed_ms % total;
    for &(idx, g) in &shown {
        if t < g {
            return idx;
        }
        t -= g;
    }
    shown.last().unwrap().0
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
    /// An animation frame was transmitted or the animation control state
    /// changed for image `id`; nothing is drawn at the cursor — the caller
    /// snapshots `frames`/`animation` and runs the playback clock.
    Animate {
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
    /// The single in-flight `a=f` frame transmission (`id`, accumulator).
    /// Continuation chunks omit `i=`, so a slot — not an id map — is right;
    /// the protocol only allows one transmission in flight at a time.
    frame_in_flight: Option<(u32, Acc)>,
    /// Animation frames appended after the root image, per image id.
    frames: HashMap<u32, Vec<Frame>>,
    /// Animation control state per image id (`a=a`).
    anim: HashMap<u32, AnimationState>,
}

impl KittyState {
    /// Feed one APC `G` body (between `ESC _ G` and `ESC \`).
    pub fn feed(&mut self, body: &str) -> KittyOut {
        let (control, payload) = body.split_once(';').unwrap_or((body, ""));
        let kv = parse_control(control);
        let id = kv.get("i").and_then(|v| v.parse().ok()).unwrap_or(0u32);
        // Continuation chunks carry only `m` (no `a`); route them to the
        // frame accumulator if a frame — and only a frame — is in flight.
        let action = match kv.get("a") {
            Some(a) => a.as_str(),
            // Continuation chunks carry only `m`; route to the frame
            // accumulator when a frame — and only a frame — is in flight.
            None if self.frame_in_flight.is_some() && !self.in_flight.contains_key(&id) => "f",
            None => "t",
        };
        let z = kv.get("z").and_then(|v| v.parse().ok()).unwrap_or(0i32);

        let virt = kv.get("U").map(|v| v == "1").unwrap_or(false);
        let dim = |k: &str| kv.get(k).and_then(|v| v.parse::<u32>().ok());

        // Control-only ops are never chunked.
        if action == "d" {
            self.in_flight.clear();
            self.frame_in_flight = None;
            let target = kv.get("d").map(|s| s.as_str()).unwrap_or("a");
            // `d=f|F`: delete only the animation frames/state, keep the image.
            if target.eq_ignore_ascii_case("f") {
                if id != 0 {
                    self.frames.remove(&id);
                    self.anim.remove(&id);
                } else {
                    self.frames.clear();
                    self.anim.clear();
                }
                // Surface an (now-empty) snapshot so the caller drops it.
                return KittyOut::Animate { id };
            }
            return match target {
                "i" | "I" => {
                    self.store.remove(&id);
                    self.virtual_placements.remove(&id);
                    self.frames.remove(&id);
                    self.anim.remove(&id);
                    KittyOut::Delete {
                        all: false,
                        id: Some(id),
                    }
                }
                _ => {
                    self.store.clear();
                    self.virtual_placements.clear();
                    self.frames.clear();
                    self.anim.clear();
                    KittyOut::Delete {
                        all: true,
                        id: None,
                    }
                }
            };
        }
        if action == "a" {
            // Animation control. Record state for the renderer playback loop.
            let st = self.anim.entry(id).or_default();
            if let Some(c) = dim("c") {
                st.current = c.max(1);
            }
            match kv.get("s").and_then(|v| v.parse::<u32>().ok()) {
                Some(1) => {
                    st.running = false;
                    st.loading = false;
                    st.loops = 0; // stopping resets the loop counter
                }
                Some(2) => {
                    st.running = true;
                    st.loading = true;
                }
                Some(3) => {
                    st.running = true;
                    st.loading = false;
                }
                _ => {}
            }
            if let Some(v) = kv.get("v").and_then(|v| v.parse::<u32>().ok())
                && v != 0
            {
                st.loops = if v == 1 { 0 } else { v - 1 };
            }
            // `r` + `z`: set the gap of an existing 1-based frame. `r=1` is
            // the root frame (base image); `r>=2` is `frames[r-2]`.
            if z != 0
                && let Some(r) = dim("r")
            {
                if r <= 1 {
                    self.anim.entry(id).or_default().root_gap = z;
                } else if let Some(fr) = self
                    .frames
                    .get_mut(&id)
                    .and_then(|f| f.get_mut(r as usize - 2))
                {
                    fr.gap_ms = z;
                }
            }
            return KittyOut::Animate { id };
        }
        if action == "c" {
            // Frame composition (`a=c`): not modelled yet (see ROADMAP).
            return KittyOut::None;
        }
        if action == "f" {
            // Transmit animation frame data (chunked like an image). The
            // first chunk carries `i=`/control; continuations carry only
            // `m`, so the id + control come from the in-flight slot.
            let more = kv.get("m").map(|v| v == "1").unwrap_or(false);
            let slot = self
                .frame_in_flight
                .get_or_insert_with(|| (id, Acc::default()));
            if slot.1.control.is_empty() {
                slot.0 = id;
                slot.1.control = control.to_string();
            }
            slot.1.payload.push_str(payload.trim());
            if more {
                return KittyOut::None;
            }
            let (fid, Acc { control, payload }) = self.frame_in_flight.take().unwrap();
            if let Some(img) = decode(&control, &payload) {
                let gap = parse_control(&control)
                    .get("z")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0i32);
                self.frames
                    .entry(fid)
                    .or_default()
                    .push(Frame { img, gap_ms: gap });
            }
            return KittyOut::Animate { id: fid };
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

    /// Animation frames appended to an image (root frame is the base image
    /// itself and is not included here), in transmit order.
    pub fn frames(&self, id: u32) -> &[Frame] {
        self.frames.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Animation control state for an image id, if the client set any.
    pub fn animation(&self, id: u32) -> Option<&AnimationState> {
        self.anim.get(&id)
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

    #[test]
    fn animation_frames_transmit_and_control() {
        let mut k = KittyState::default();
        // Root image (id 3) shown.
        assert!(matches!(
            k.feed(&format!("a=T,i=3,f=32,s=1,v=1;{PX}")),
            KittyOut::Place(_)
        ));
        // Two frames with gaps; neither draws at the cursor (Animate, not
        // Place/None).
        assert!(matches!(
            k.feed(&format!("a=f,i=3,f=32,s=1,v=1,z=40;{PX}")),
            KittyOut::Animate { id: 3 }
        ));
        k.feed(&format!("a=f,i=3,f=32,s=1,v=1,z=-1;{PX}"));
        let fr = k.frames(3);
        assert_eq!(fr.len(), 2);
        assert_eq!(fr[0].gap_ms, 40);
        assert_eq!(fr[1].gap_ms, -1, "z<0 ⇒ gapless");
        assert!(k.frames(99).is_empty());

        // Control: make frame 2 current, run looping 5 times (v=5 ⇒ 4).
        k.feed("a=a,i=3,c=2,s=3,v=5");
        let a = k.animation(3).copied().unwrap();
        assert_eq!(a.current, 2);
        assert!(a.running && !a.loading);
        assert_eq!(a.loops, 4);

        // r>=2 sets a frame's gap; r=1 sets the root frame's gap.
        k.feed("a=a,i=3,r=2,z=48");
        assert_eq!(k.frames(3)[0].gap_ms, 48, "r=2 → frames[0]");
        k.feed("a=a,i=3,r=1,z=70");
        assert_eq!(k.animation(3).unwrap().root_gap, 70, "r=1 → root frame gap");

        // Stop resets running + loop counter.
        k.feed("a=a,i=3,s=1");
        let a = k.animation(3).copied().unwrap();
        assert!(!a.running);
        assert_eq!(a.loops, 0);

        // d=f deletes frames/anim but keeps the image.
        k.feed("a=d,d=f,i=3");
        assert!(k.frames(3).is_empty());
        assert!(k.animation(3).is_none());
        assert!(k.image(3).is_some(), "d=f keeps the base image");
    }

    #[test]
    fn playback_timing_maps_elapsed_to_frame() {
        let run = |loops, loading| AnimationState {
            current: 1,
            running: true,
            loading,
            loops,
            ..AnimationState::default()
        };
        // Stopped → the selected current frame, clamped.
        let stop = AnimationState {
            current: 3,
            running: false,
            loading: false,
            loops: 0,
            ..AnimationState::default()
        };
        assert_eq!(current_frame(&[10, 10, 10, 10], &stop, 9_999), 2);
        assert_eq!(current_frame(&[], &stop, 0), 0);
        let stop_oob = AnimationState {
            current: 99,
            ..stop
        };
        assert_eq!(current_frame(&[10, 10], &stop_oob, 0), 1, "clamped");

        // Infinite loop over [100,200,300] (total 600).
        let g = [100, 200, 300];
        let inf = run(0, false);
        assert_eq!(current_frame(&g, &inf, 0), 0);
        assert_eq!(current_frame(&g, &inf, 150), 1);
        assert_eq!(current_frame(&g, &inf, 350), 2);
        assert_eq!(current_frame(&g, &inf, 650), 0, "wraps (650 % 600 = 50)");

        // Gapless frame (g<=0) is never displayed, only skipped over.
        let gl = [100, -1, 200];
        assert_eq!(current_frame(&gl, &inf, 50), 0);
        assert_eq!(current_frame(&gl, &inf, 150), 2, "frame 2 is gapless");
        assert_eq!(current_frame(&gl, &inf, 350), 0, "300ms shown cycle");
        // All gapless ⇒ hold on current.
        assert_eq!(current_frame(&[0, -1], &run(0, false), 1234), 0);

        // Finite loop count: freeze on the last shown frame after N passes.
        let g2 = [100, 200]; // total 300
        let fin = run(2, false);
        assert_eq!(current_frame(&g2, &fin, 0), 0);
        assert_eq!(current_frame(&g2, &fin, 250), 1);
        assert_eq!(current_frame(&g2, &fin, 600), 1, "2 passes done → freeze");
        assert_eq!(current_frame(&g2, &fin, 500), 1, "500 % 300 = 200 → f2");

        // Loading mode never loops: holds the last frame at/after the end.
        let load = run(0, true);
        assert_eq!(current_frame(&g2, &load, 50), 0);
        assert_eq!(current_frame(&g2, &load, 250), 1);
        assert_eq!(current_frame(&g2, &load, 900), 1, "loading waits at end");
    }

    #[test]
    fn chunked_frame_transmission() {
        let mut k = KittyState::default();
        k.feed(&format!("a=T,i=4,f=32,s=1,v=1;{PX}"));
        // Frame split across chunks (m=1 continuation).
        assert!(matches!(
            k.feed("a=f,i=4,f=32,s=1,v=1,m=1;AQID"),
            KittyOut::None
        ));
        assert!(k.frames(4).is_empty(), "incomplete frame not stored yet");
        k.feed("m=0;BA==");
        assert_eq!(k.frames(4).len(), 1, "frame completes on final chunk");
    }
}
