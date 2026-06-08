//! Pulls image escape sequences (Sixel DCS, kitty APC `G`, iTerm2 `OSC 1337`)
//! out of the PTY byte stream *before* it reaches the VT parser, which has no
//! image support. Everything else passes through byte-for-byte so the terminal
//! engine still sees correct cursor/scroll behavior.

use crate::image::{ImageData, Placed};
use crate::kitty::{KittyOut, KittyState};
use crate::{iterm, sixel};

const MAX_SEQ: usize = 64 * 1024 * 1024;

/// OSC 133 shell-integration marks (FinalTerm / iTerm2 / kitty convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `A` — start of a fresh prompt.
    PromptStart,
    /// `B` — end of prompt / start of user input.
    CommandStart,
    /// `C` — command began executing (output starts).
    OutputStart,
    /// `D` — command finished (optional exit code).
    CommandEnd(Option<i32>),
}

#[derive(Debug)]
pub enum Chunk {
    /// Bytes to forward to the terminal engine unchanged.
    Pass(Vec<u8>),
    /// A decoded image to place at the current cursor position.
    Image(Placed),
    /// kitty `a=d`: delete images (all, or by image id).
    DeleteImages { all: bool, id: Option<u32> },
    /// kitty `U=1` virtual placement: store the image + its `cols`×`rows`
    /// box by id; it is drawn later wherever `U+10EEEE` placeholder cells
    /// reference this id (not at the cursor).
    VirtualImage {
        id: u32,
        img: ImageData,
        cols: u32,
        rows: u32,
        z: i32,
    },
    /// kitty relative placement: child image `id`/`placement` is positioned
    /// `(h, v)` cells from its parent placement's origin (resolved against
    /// the parent's on-screen position at draw time).
    RelativePlacement {
        id: u32,
        placement: u32,
        img: ImageData,
        parent_img: u32,
        parent_placement: u32,
        h: i32,
        v: i32,
    },
    /// kitty animation snapshot for image `id`: the full display sequence
    /// (`imgs[0]` = base/root frame) with each frame's gap in ms and the
    /// current animation control state. Emitted whenever a frame or the
    /// control state changes; an empty/▒single-image non-running snapshot
    /// means the animation was cleared.
    Animation {
        id: u32,
        imgs: Vec<ImageData>,
        gaps: Vec<i32>,
        state: crate::kitty::AnimationState,
    },
    /// A shell-integration mark at the current cursor line.
    Prompt(PromptKind),
    /// Working-directory report (OSC 7), absolute path.
    Cwd(String),
    /// OSC 9;4 progress report — drives the OS taskbar/dock progress
    /// indicator (Windows Terminal's behavior).
    Progress(Progress),
}

/// OSC 9;4 taskbar-progress state. PowerShell 7 `Write-Progress` (with
/// `$PSStyle.Progress.UseOSCIndicator = $true`), `winget`, and many CLIs
/// emit `ESC ] 9 ; 4 ; <state> ; <pct> ST` so the terminal can drive the OS
/// taskbar/dock progress indicator. Maps onto Win32
/// `ITaskbarList3::SetProgressState` / `SetProgressValue` (the ConEmu/
/// Windows Terminal convention).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Progress {
    /// state 0 — clear the indicator.
    Clear,
    /// state 1 — normal progress, 0..=100%.
    Normal(u8),
    /// state 2 — error, 0..=100% (red).
    Error(u8),
    /// state 3 — indeterminate (marquee; no value).
    Indeterminate,
    /// state 4 — paused / warning, 0..=100% (yellow).
    Warning(u8),
}

#[derive(PartialEq)]
enum Mode {
    Pass,
    Dcs,
    Apc,
    Osc,
}

pub struct Extractor {
    mode: Mode,
    pass: Vec<u8>,
    seq: Vec<u8>,
    esc_pending: bool,
    st_pending: bool,
    /// The terminator that ended the current sequence was a BEL (`0x07`),
    /// not `ESC \`; preserved so pass-through bytes echo exactly.
    term_bel: bool,
    kitty: KittyState,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    pub fn new() -> Self {
        Extractor {
            mode: Mode::Pass,
            pass: Vec::with_capacity(8192),
            seq: Vec::new(),
            esc_pending: false,
            st_pending: false,
            term_bel: false,
            kitty: KittyState::default(),
        }
    }

    pub fn feed(&mut self, input: &[u8]) -> Vec<Chunk> {
        let mut out: Vec<Chunk> = Vec::new();
        for &b in input {
            match self.mode {
                Mode::Pass => {
                    if self.esc_pending {
                        self.esc_pending = false;
                        match b {
                            b'P' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Dcs;
                                self.seq.clear();
                            }
                            b'_' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Apc;
                                self.seq.clear();
                            }
                            b']' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Osc;
                                self.seq.clear();
                            }
                            _ => {
                                self.pass.push(0x1b);
                                self.pass.push(b);
                            }
                        }
                    } else if b == 0x1b {
                        self.esc_pending = true;
                    } else {
                        self.pass.push(b);
                    }
                }
                Mode::Dcs | Mode::Apc | Mode::Osc => {
                    if self.st_pending {
                        self.st_pending = false;
                        if b == b'\\' {
                            self.term_bel = false;
                            self.finish_seq(&mut out);
                            continue;
                        } else {
                            self.seq.push(0x1b);
                            self.seq.push(b);
                        }
                    } else if b == 0x1b {
                        self.st_pending = true;
                    } else if (b == 0x07 && self.mode == Mode::Osc) || b == 0x9c {
                        self.term_bel = b == 0x07;
                        self.finish_seq(&mut out);
                    } else {
                        self.seq.push(b);
                        if self.seq.len() > MAX_SEQ {
                            // Give up: forward verbatim so we never hang.
                            self.bail(&mut out);
                        }
                    }
                }
            }
        }
        self.flush_pass(&mut out);
        out
    }

    fn flush_pass(&mut self, out: &mut Vec<Chunk>) {
        if !self.pass.is_empty() {
            out.push(Chunk::Pass(std::mem::take(&mut self.pass)));
        }
    }

    fn bail(&mut self, out: &mut Vec<Chunk>) {
        let mut v = Vec::with_capacity(self.seq.len() + 2);
        v.push(0x1b);
        v.push(match self.mode {
            Mode::Dcs => b'P',
            Mode::Apc => b'_',
            _ => b']',
        });
        v.extend_from_slice(&self.seq);
        out.push(Chunk::Pass(v));
        self.seq.clear();
        self.mode = Mode::Pass;
    }

    fn finish_seq(&mut self, out: &mut Vec<Chunk>) {
        let mut seq = std::mem::take(&mut self.seq);
        let mode = std::mem::replace(&mut self.mode, Mode::Pass);

        // OSC 133 shell-integration marks are consumed (not forwarded).
        if mode == Mode::Osc && seq.starts_with(b"133;") {
            if let Some(kind) = parse_prompt(&seq[4..]) {
                out.push(Chunk::Prompt(kind));
            }
            return;
        }
        // OSC 7 cwd report (`7;file://host/abs/path`).
        if mode == Mode::Osc && seq.starts_with(b"7;") {
            if let Some(path) = parse_osc7(&String::from_utf8_lossy(&seq[2..])) {
                out.push(Chunk::Cwd(path));
            }
            return;
        }
        // OSC 9;4 progress report (ConEmu / Windows Terminal taskbar
        // progress). pwsh 7 `Write-Progress` + `winget` emit it; the VT
        // engine ignores it, so consume it here and surface a Progress chunk
        // the UI maps onto the OS taskbar indicator. Other OSC 9 sequences
        // (e.g. iTerm2's `OSC 9;<msg>` notification) are NOT matched here and
        // fall through to the default handling unchanged.
        if mode == Mode::Osc && seq.starts_with(b"9;4;") {
            if let Some(p) = parse_osc9_4(&seq) {
                out.push(Chunk::Progress(p));
            }
            return;
        }
        // OSC 1 (icon name) — VTE/alacritty drop it entirely (their
        // dispatch table only matches "0" and "2"), but vim / tmux /
        // ranger / mc emit it to set the *short* title intended for
        // tabs and iconified-window labels. xterm's distinction
        // between icon name and window title isn't useful in modern
        // tabbed terminals, so kitty / iTerm2 / Gnome Terminal /
        // Konsole all treat OSC 1 the same as OSC 2. Rewrite the
        // first byte of the payload from `1` to `2` so VTE picks it
        // up; everything downstream then behaves identically.
        if mode == Mode::Osc && seq.starts_with(b"1;") {
            seq[0] = b'2';
        }

        enum R {
            None,
            Img(Placed),
            Del {
                all: bool,
                id: Option<u32>,
            },
            Virtual {
                id: u32,
                img: ImageData,
                cols: u32,
                rows: u32,
                z: i32,
            },
            Anim {
                id: u32,
                imgs: Vec<ImageData>,
                gaps: Vec<i32>,
                state: crate::kitty::AnimationState,
            },
            Rel {
                id: u32,
                placement: u32,
                img: ImageData,
                parent_img: u32,
                parent_placement: u32,
                h: i32,
                v: i32,
            },
        }

        let result = match mode {
            Mode::Dcs => {
                // Sixel: params then 'q' then data.
                // Cycle 916 (file-by-file audit): a Sixel DCS is `P1;P2;P3 q
                // <data>` — the bytes before `q` are only digits / `;`. Requiring
                // that prefix shape stops other DCS strings whose body contains a
                // `q` (DECRQSS `$q…`, XTGETTCAP `+q…`) from being swallowed as
                // tiny spurious images; those now forward verbatim (R::None).
                match seq
                    .iter()
                    .position(|&c| c == b'q')
                    .filter(|&q| seq[..q].iter().all(|&c| c.is_ascii_digit() || c == b';'))
                {
                    Some(qpos) => sixel::decode(&seq[qpos + 1..])
                        .map(|i| R::Img(Placed::plain(i)))
                        .unwrap_or(R::None),
                    None => R::None,
                }
            }
            Mode::Apc => {
                if seq.first() == Some(&b'G') {
                    // Borrow when the APC payload is valid UTF-8 (the common
                    // case — kitty graphics keys are ASCII); only the lossy
                    // fallback allocates (cycle 844, audit).
                    let body = String::from_utf8_lossy(&seq[1..]);
                    match self.kitty.feed(&body) {
                        KittyOut::Place(p) => R::Img(p),
                        KittyOut::Delete { all, id } => R::Del { all, id },
                        // Virtual placements draw nothing at the cursor; the
                        // stored image + box are surfaced so the renderer can
                        // composite them where placeholder cells appear.
                        KittyOut::Virtual { id } => {
                            match (self.kitty.image(id), self.kitty.virtual_placement(id)) {
                                (Some(img), Some(vp)) => R::Virtual {
                                    id,
                                    img: img.clone(),
                                    cols: vp.cols,
                                    rows: vp.rows,
                                    z: vp.z,
                                },
                                _ => R::None,
                            }
                        }
                        // Snapshot the full display sequence: base/root
                        // image first, then each transmitted frame, with the
                        // root gap from the control state.
                        KittyOut::Animate { id } => match self.kitty.image(id) {
                            Some(base) => {
                                let st = self.kitty.animation(id).copied().unwrap_or_default();
                                let mut imgs = vec![base.clone()];
                                let mut gaps = vec![st.root_gap];
                                for f in self.kitty.frames(id) {
                                    imgs.push(f.img.clone());
                                    gaps.push(f.gap_ms);
                                }
                                R::Anim {
                                    id,
                                    imgs,
                                    gaps,
                                    state: st,
                                }
                            }
                            None => R::None,
                        },
                        // Relative placement: surface the child image + its
                        // parent reference; the renderer resolves the
                        // on-screen position from the parent placement.
                        KittyOut::Relative { id, placement } => {
                            match (
                                self.kitty.image(id),
                                self.kitty.relative_placement(id, placement),
                            ) {
                                (Some(img), Some(rp)) => R::Rel {
                                    id,
                                    placement,
                                    img: img.clone(),
                                    parent_img: rp.parent_img,
                                    parent_placement: rp.parent_placement,
                                    h: rp.h,
                                    v: rp.v,
                                },
                                _ => R::None,
                            }
                        }
                        KittyOut::None => R::None,
                    }
                } else {
                    R::None
                }
            }
            Mode::Osc => {
                // Test the iTerm prefix on the raw bytes (byte-exact for an
                // ASCII prefix) and only allocate the owned String on a match.
                // Every other OSC — titles, colors, OSC 8 hyperlinks, OSC 52,
                // OSC 104 — reaches this branch and would otherwise heap-alloc a
                // full String just to fail `starts_with` (cycle 844, audit).
                if seq.starts_with(b"1337;File=") {
                    iterm::decode(&String::from_utf8_lossy(&seq))
                        .map(|i| R::Img(Placed::plain(i)))
                        .unwrap_or(R::None)
                } else {
                    R::None
                }
            }
            Mode::Pass => R::None,
        };

        match result {
            R::Img(data) => out.push(Chunk::Image(data)),
            R::Del { all, id } => out.push(Chunk::DeleteImages { all, id }),
            R::Virtual {
                id,
                img,
                cols,
                rows,
                z,
            } => out.push(Chunk::VirtualImage {
                id,
                img,
                cols,
                rows,
                z,
            }),
            R::Anim {
                id,
                imgs,
                gaps,
                state,
            } => out.push(Chunk::Animation {
                id,
                imgs,
                gaps,
                state,
            }),
            R::Rel {
                id,
                placement,
                img,
                parent_img,
                parent_placement,
                h,
                v,
            } => out.push(Chunk::RelativePlacement {
                id,
                placement,
                img,
                parent_img,
                parent_placement,
                h,
                v,
            }),
            R::None => {
                // Not an image (or unsupported): forward verbatim, terminator
                // included, so the VT engine handles it.
                let mut v = Vec::with_capacity(seq.len() + 4);
                v.push(0x1b);
                v.push(match mode {
                    Mode::Dcs => b'P',
                    Mode::Apc => b'_',
                    Mode::Osc => b']',
                    Mode::Pass => b' ',
                });
                v.extend_from_slice(&seq);
                if self.term_bel && mode == Mode::Osc {
                    v.push(0x07);
                } else {
                    v.push(0x1b);
                    v.push(b'\\');
                }
                out.push(Chunk::Pass(v));
            }
        }
    }
}

fn parse_osc7(s: &str) -> Option<String> {
    // `file://host/path` — keep the path; percent-decode the common cases.
    let rest = s.strip_prefix("file://").unwrap_or(s);
    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => rest,
    };
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // Decode into a *byte* buffer first: shells percent-encode each UTF-8
    // byte of a non-ASCII path individually (zsh's `print -P %d` emits
    // `%C3%A9` for `é`), so we need to reassemble the bytes before
    // interpreting them as UTF-8. The old code pushed each decoded byte as
    // a `char`, which gave `Ã©` instead of `é` and corrupted every cwd
    // outside the ASCII range.
    let mut bytes = Vec::with_capacity(path.len());
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            // Slice the *bytes* (never the &str): a `%` immediately
            // followed by a multibyte UTF-8 char would otherwise make
            // `&path[i+1..i+3]` land on a non-char-boundary and panic
            // (a hard crash under panic=abort) before from_str_radix
            // could reject it. from_utf8 rejects a mid-char byte pair,
            // so the `%` falls through and is pushed as a literal byte.
            && let Ok(hex) = std::str::from_utf8(&b[i + 1..i + 3])
            && let Ok(c) = u8::from_str_radix(hex, 16)
        {
            bytes.push(c);
            i += 3;
            continue;
        }
        bytes.push(b[i]);
        i += 1;
    }
    // Lossy → invalid byte sequences become U+FFFD instead of dropping the
    // whole report; a partly-corrupted path is more useful than no path.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Parse an OSC 9;4 body (`9;4;<state>[;<pct>]`) into a [`Progress`].
/// Tolerant of a missing/over-range pct (clamped to 100) and surrounding
/// whitespace; an unknown state yields `None` so the sequence is simply
/// dropped rather than mis-rendered.
fn parse_osc9_4(seq: &[u8]) -> Option<Progress> {
    let rest = std::str::from_utf8(seq).ok()?.strip_prefix("9;4;")?;
    let mut parts = rest.split(';');
    let state: u8 = parts.next()?.trim().parse().ok()?;
    let pct = parts
        .next()
        .and_then(|p| p.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(100) as u8;
    Some(match state {
        0 => Progress::Clear,
        1 => Progress::Normal(pct),
        2 => Progress::Error(pct),
        3 => Progress::Indeterminate,
        4 => Progress::Warning(pct),
        _ => return None,
    })
}

fn parse_prompt(rest: &[u8]) -> Option<PromptKind> {
    match rest.first()? {
        b'A' => Some(PromptKind::PromptStart),
        b'B' => Some(PromptKind::CommandStart),
        b'C' => Some(PromptKind::OutputStart),
        b'D' => {
            let s = String::from_utf8_lossy(rest);
            let code = s
                .split(';')
                .nth(1)
                .and_then(|c| c.trim().parse::<i32>().ok());
            Some(PromptKind::CommandEnd(code))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunk, Extractor, PromptKind};
    use base64::Engine;

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
    fn plain_bytes_pass_through_unchanged() {
        let mut ex = Extractor::new();
        let out = ex.feed(b"hello world");
        assert_eq!(out.len(), 1);
        match &out[0] {
            Chunk::Pass(b) => assert_eq!(b, b"hello world"),
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[test]
    fn esc_split_across_feeds_still_passes_verbatim() {
        // A lone ESC at the end of one chunk must not be misread as the
        // start of a sequence and dropped — `ESC` + `M` (reverse index) is
        // a plain VT control the engine handles, so it round-trips.
        let mut ex = Extractor::new();
        let mut bytes = Vec::new();
        for c in ex.feed(b"a\x1b") {
            if let Chunk::Pass(b) = c {
                bytes.extend_from_slice(&b);
            }
        }
        for c in ex.feed(b"Mb") {
            if let Chunk::Pass(b) = c {
                bytes.extend_from_slice(&b);
            }
        }
        assert_eq!(bytes, b"a\x1bMb");
    }

    #[test]
    fn osc133_prompt_mark_is_consumed_and_surfaced() {
        // `ESC ] 133 ; A BEL` — a prompt-start mark. It is consumed (not
        // forwarded to the VT engine) and surfaced as a Prompt chunk; the
        // trailing text passes through.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]133;A\x07$ ");
        assert!(
            matches!(out.first(), Some(Chunk::Prompt(PromptKind::PromptStart))),
            "first chunk should be PromptStart, got {out:?}"
        );
        let passed: Vec<u8> = out
            .iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(passed, b"$ ");
    }

    #[test]
    fn iterm2_inline_image_is_extracted() {
        // Full OSC 1337 round-trip: a base64 PNG between `ESC ]` and `BEL`
        // is pulled out as an Image chunk before the VT engine sees it.
        let b64 = base64::engine::general_purpose::STANDARD.encode(png(2, 2));
        let seq = format!("\x1b]1337;File=inline=1:{b64}\x07");
        let mut ex = Extractor::new();
        let out = ex.feed(seq.as_bytes());
        let img = out.iter().find_map(|c| match c {
            Chunk::Image(p) => Some(&p.img),
            _ => None,
        });
        let img = img.expect("an Image chunk should be emitted");
        assert_eq!((img.width, img.height), (2, 2));
    }
}
