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
        let seq = std::mem::take(&mut self.seq);
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
        }

        let result = match mode {
            Mode::Dcs => {
                // Sixel: params then 'q' then data.
                if let Some(qpos) = seq.iter().position(|&c| c == b'q') {
                    sixel::decode(&seq[qpos + 1..])
                        .map(|i| R::Img(Placed::plain(i)))
                        .unwrap_or(R::None)
                } else {
                    R::None
                }
            }
            Mode::Apc => {
                if seq.first() == Some(&b'G') {
                    let body = String::from_utf8_lossy(&seq[1..]).into_owned();
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
                        // Relative placements are recorded in KittyState;
                        // their on-screen position is derived from the
                        // parent at render time (a later cycle), so nothing
                        // is emitted at the cursor now.
                        KittyOut::Relative { .. } | KittyOut::None => R::None,
                    }
                } else {
                    R::None
                }
            }
            Mode::Osc => {
                let body = String::from_utf8_lossy(&seq).into_owned();
                if body.starts_with("1337;File=") {
                    iterm::decode(&body)
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
    let mut out = String::with_capacity(path.len());
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(c) = u8::from_str_radix(&path[i + 1..i + 3], 16)
        {
            out.push(c as char);
            i += 3;
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    Some(out)
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
