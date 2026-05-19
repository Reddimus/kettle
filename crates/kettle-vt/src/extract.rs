//! Pulls image escape sequences (Sixel DCS, kitty APC `G`, iTerm2 `OSC 1337`)
//! out of the PTY byte stream *before* it reaches the VT parser, which has no
//! image support. Everything else passes through byte-for-byte so the terminal
//! engine still sees correct cursor/scroll behavior.

use crate::image::ImageData;
use crate::kitty::KittyState;
use crate::{iterm, sixel};

const MAX_SEQ: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum Chunk {
    /// Bytes to forward to the terminal engine unchanged.
    Pass(Vec<u8>),
    /// A decoded image to place at the current cursor position.
    Image(ImageData),
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
                            self.finish_seq(&mut out);
                            continue;
                        } else {
                            self.seq.push(0x1b);
                            self.seq.push(b);
                        }
                    } else if b == 0x1b {
                        self.st_pending = true;
                    } else if (b == 0x07 && self.mode == Mode::Osc) || b == 0x9c {
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
        let img = match mode {
            Mode::Dcs => {
                // Sixel: params then 'q' then data.
                if let Some(qpos) = seq.iter().position(|&c| c == b'q') {
                    sixel::decode(&seq[qpos + 1..])
                } else {
                    None
                }
            }
            Mode::Apc => {
                if seq.first() == Some(&b'G') {
                    let body = String::from_utf8_lossy(&seq[1..]).into_owned();
                    self.kitty.feed(&body)
                } else {
                    None
                }
            }
            Mode::Osc => {
                let body = String::from_utf8_lossy(&seq).into_owned();
                if body.starts_with("1337;File=") {
                    iterm::decode(&body)
                } else {
                    None
                }
            }
            Mode::Pass => None,
        };

        match img {
            Some(data) => out.push(Chunk::Image(data)),
            None => {
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
                v.push(0x1b);
                v.push(b'\\');
                out.push(Chunk::Pass(v));
            }
        }
    }
}
