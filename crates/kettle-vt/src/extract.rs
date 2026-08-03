//! Pulls image escape sequences (Sixel DCS, kitty APC `G`, iTerm2 `OSC 1337`)
//! out of the PTY byte stream *before* it reaches the VT parser, which has no
//! image support. Everything else passes through byte-for-byte so the terminal
//! engine still sees correct cursor/scroll behavior.

use crate::image::{ImageData, Placed, PlacementParams};
use crate::kitty::{Delete, KittyOut, KittyState, PlacementKey};
use crate::{GraphicsBudget, GraphicsReservation};
use crate::{iterm, sixel};

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

/// Graphics-protocol storage follows the terminal's two screen buffers.
///
/// Kitty image ids, virtual placements, animations, and partial uploads are
/// isolated between the primary and alternate screens. This is deliberately
/// independent of the text parser: [`Extractor::enter_alternate_screen`] and
/// [`Extractor::leave_alternate_screen`] are called by the terminal core after
/// it observes the corresponding buffer transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphicsScreen {
    Primary,
    Alternate,
}

#[derive(Debug)]
pub enum Chunk {
    /// Bytes to forward to the terminal engine unchanged.
    Pass(Vec<u8>),
    /// A decoded image to place at the current cursor position.
    Image(Placed),
    /// Kitty `a=d`: delete placements selected by id, number, cursor/cell,
    /// range, column, row, or z-index.
    DeleteImages(Delete),
    /// kitty `U=1` virtual placement: store the image + its `cols`×`rows`
    /// box by id; it is drawn later wherever `U+10EEEE` placeholder cells
    /// reference this id (not at the cursor).
    VirtualImage {
        id: u32,
        placement: u32,
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
        z: i32,
        params: PlacementParams,
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
    /// A complete graphics control string held for an out-of-band DEC 2026
    /// marker. It has not mutated any graphics-protocol store yet.
    DeferredGraphics(DeferredGraphics),
    /// A shell-integration mark at the current cursor line.
    Prompt(PromptKind),
    /// Working-directory report (OSC 7), absolute path.
    Cwd(String),
    /// OSC 9;4 progress report — drives the OS taskbar/dock progress
    /// indicator (Windows Terminal's behavior).
    Progress(Progress),
    /// Protocol desktop notification (`OSC 9 ; message` or
    /// `OSC 777 ; notify ; title ; body`).
    Notification { title: String, body: String },
}

/// One process-budgeted graphics control string deferred by DEC 2026.
pub struct DeferredGraphics {
    bytes: Vec<u8>,
    _reservation: GraphicsReservation,
}

impl DeferredGraphics {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for DeferredGraphics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredGraphics")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
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

const MAX_NOTIFY_FIELD_BYTES: usize = 8 << 10;

/// After abandoning an over-budget control string, consume at most one PTY
/// read-sized window looking for its real terminator before returning to
/// ground state. This bounds desynchronization when a producer never emits a
/// terminator without immediately exposing the rejected payload downstream.
const MAX_SEQ_RESYNC_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Pass,
    Dcs,
    Apc,
    Osc,
}

fn is_graphics_sequence(mode: Mode, seq: &[u8]) -> bool {
    match mode {
        Mode::Dcs => seq.iter().position(|&byte| byte == b'q').is_some_and(|q| {
            seq[..q]
                .iter()
                .all(|byte| byte.is_ascii_digit() || *byte == b';')
        }),
        Mode::Apc => seq.first() == Some(&b'G'),
        Mode::Osc => seq.starts_with(b"1337;File="),
        Mode::Pass => false,
    }
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
    kitty_primary: KittyState,
    kitty_alternate: KittyState,
    graphics_screen: GraphicsScreen,
    defer_graphics: bool,
    budget: GraphicsBudget,
    seq_reservation: Option<GraphicsReservation>,
    discarding_seq: bool,
    discard_remaining: usize,
    /// Rolling tail (≤3 bytes) of the active control string's payload, kept
    /// even while discarding, so a raw `0x9c` can be classified as either a
    /// standalone C1 ST or a UTF-8 continuation byte of an in-progress
    /// multi-byte character (`E2 9C xx` covers the whole U+2700 block —
    /// ✢ ✳ ✶ ✻ ✽ — which Claude Code puts in OSC 0 titles).
    seq_tail: [u8; 3],
    seq_tail_len: u8,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    pub fn new() -> Self {
        Self::with_budget(GraphicsBudget::default())
    }

    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        let budget = GraphicsBudget::isolated(crate::GraphicsLimits::default())
            .expect("default graphics limits are valid");
        Self::with_budget(budget)
    }

    fn with_budget(budget: GraphicsBudget) -> Self {
        Extractor {
            mode: Mode::Pass,
            pass: Vec::with_capacity(8192),
            seq: Vec::new(),
            esc_pending: false,
            st_pending: false,
            term_bel: false,
            kitty_primary: KittyState::new(budget.clone()),
            kitty_alternate: KittyState::new(budget.clone()),
            graphics_screen: GraphicsScreen::Primary,
            defer_graphics: false,
            budget,
            seq_reservation: None,
            discarding_seq: false,
            discard_remaining: 0,
            seq_tail: [0; 3],
            seq_tail_len: 0,
        }
    }

    /// Complete a kitty deletion after the terminal core has resolved spatial
    /// placement selectors and uppercase data-lifetime semantics.
    pub fn apply_kitty_delete_result(&mut self, removed: &[PlacementKey], freed_image_ids: &[u32]) {
        self.kitty_mut()
            .apply_delete_result(removed, freed_image_ids);
    }

    /// Enter the alternate screen, optionally clearing its stored graphics.
    ///
    /// DECSET 47 and 1047 preserve the alternate buffer on entry. DECSET 1049
    /// clears it before switching, and Kitty requires images to follow that
    /// text-buffer boundary while preserving primary-screen graphics.
    pub fn enter_alternate_screen(&mut self, clear: bool) {
        if clear {
            self.kitty_alternate = KittyState::new(self.budget.clone());
        }
        self.graphics_screen = GraphicsScreen::Alternate;
    }

    /// Return to the primary screen, optionally clearing alternate graphics.
    ///
    /// DECRST 47 and 1049 preserve the alternate buffer. DECRST 1047 clears it
    /// before returning to the primary buffer, while 1049 defers its clear to
    /// the next entry so its contents remain available for selection.
    pub fn leave_alternate_screen(&mut self, clear: bool) {
        if clear {
            self.kitty_alternate = KittyState::new(self.budget.clone());
        }
        self.graphics_screen = GraphicsScreen::Primary;
    }

    /// Clear every graphics-protocol object in the active screen.
    ///
    /// This is the cache-clearing behavior required for ED 2. Primary and
    /// alternate stores remain isolated.
    pub fn clear_active_graphics(&mut self) {
        let replacement = KittyState::new(self.budget.clone());
        match self.graphics_screen {
            GraphicsScreen::Primary => self.kitty_primary = replacement,
            GraphicsScreen::Alternate => self.kitty_alternate = replacement,
        }
    }

    /// Reset graphics in both screen buffers, as required by RIS.
    pub fn reset_all_graphics(&mut self) {
        self.kitty_primary = KittyState::new(self.budget.clone());
        self.kitty_alternate = KittyState::new(self.budget.clone());
        self.graphics_screen = GraphicsScreen::Primary;
    }

    /// Drop placement relations whose document-row anchors cannot survive a
    /// column reflow, while retaining transmitted image data and Unicode
    /// virtual-placement prototypes.
    pub fn clear_reflowed_regular_placements(&mut self) {
        self.kitty_primary.clear_relative_placements();
        self.kitty_alternate.clear_relative_placements();
    }

    /// Defer complete graphics strings without parsing or mutating their
    /// buffer-local stores.
    pub fn set_graphics_deferred(&mut self, deferred: bool) {
        self.defer_graphics = deferred;
    }

    fn kitty(&self) -> &KittyState {
        match self.graphics_screen {
            GraphicsScreen::Primary => &self.kitty_primary,
            GraphicsScreen::Alternate => &self.kitty_alternate,
        }
    }

    fn kitty_mut(&mut self) -> &mut KittyState {
        match self.graphics_screen {
            GraphicsScreen::Primary => &mut self.kitty_primary,
            GraphicsScreen::Alternate => &mut self.kitty_alternate,
        }
    }

    /// v2.20.0 P3 (perf): the PTY front stage. Plain bytes (the overwhelming
    /// majority of real output) used to walk a per-byte state machine —
    /// `match` + bounds-checked `Vec::push` for every single byte of a 64KiB
    /// read. Now each loop iteration `memchr`-scans (SIMD) to the next byte
    /// that can change state — ESC in pass-through; ESC / raw ST (and BEL for
    /// OSC) inside a sequence — and bulk-copies the run before it with
    /// `extend_from_slice`. That also collapses the old doubling-ladder
    /// reallocs (a taken `pass` buffer re-grew 0→64KiB in ~17 steps under
    /// flood) into a single exact `reserve` per run. State semantics are
    /// byte-identical to the old loop, including ESC / ESC-\ split across
    /// `feed` calls.
    pub fn feed(&mut self, input: &[u8]) -> Vec<Chunk> {
        let mut collected = Vec::new();
        self.feed_with(input, |_, chunk| collected.push(chunk));
        collected
    }

    /// Feed bytes and handle each completed chunk before parsing the next
    /// control sequence.
    ///
    /// This streaming form is required when a handler feeds state back into
    /// the extractor. In particular, kitty uppercase deletion depends on the
    /// terminal core's placement registry; applying that result before the
    /// next APC preserves wire order when delete and re-place/re-transmit
    /// commands arrive in one PTY read.
    pub fn feed_with<F>(&mut self, input: &[u8], mut handle: F)
    where
        F: FnMut(&mut Self, Chunk),
    {
        // `finish_seq` and `flush_pass` each emit at most one chunk. Reusing a
        // tiny staging Vec keeps their established, heavily-tested parsing
        // paths intact while still handing chunks off at sequence boundaries.
        let mut out: Vec<Chunk> = Vec::with_capacity(1);
        let mut i = 0usize;
        while i < input.len() {
            match self.mode {
                Mode::Pass => {
                    if self.esc_pending {
                        let b = input[i];
                        i += 1;
                        self.esc_pending = false;
                        match b {
                            b'P' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Dcs;
                                self.begin_seq();
                            }
                            b'_' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Apc;
                                self.begin_seq();
                            }
                            b']' => {
                                self.flush_pass(&mut out);
                                self.mode = Mode::Osc;
                                self.begin_seq();
                            }
                            _ => {
                                self.pass.push(0x1b);
                                self.pass.push(b);
                            }
                        }
                    } else {
                        // Bulk path: everything up to the next ESC is plain.
                        match memchr::memchr(0x1b, &input[i..]) {
                            Some(off) => {
                                self.pass.extend_from_slice(&input[i..i + off]);
                                self.esc_pending = true;
                                i += off + 1;
                            }
                            None => {
                                self.pass.extend_from_slice(&input[i..]);
                                break;
                            }
                        }
                    }
                }
                Mode::Dcs | Mode::Apc | Mode::Osc => {
                    if self.st_pending {
                        let b = input[i];
                        self.st_pending = false;
                        if b == b'\\' {
                            i += 1;
                            self.term_bel = false;
                            self.finish_seq(&mut out);
                            self.handle_pending_chunks(&mut out, &mut handle);
                            continue;
                        }
                        if self.discarding_seq {
                            // Account for the ESC consumed on the preceding
                            // iteration first. If it is the final quarantined
                            // byte, leave `b` untouched for Pass mode.
                            let consumed = self.consume_discard_bytes(1);
                            debug_assert_eq!(consumed, 1);
                            if self.mode == Mode::Pass {
                                continue;
                            }
                            i += 1;
                            let consumed = self.consume_discard_bytes(1);
                            debug_assert_eq!(consumed, 1);
                            continue;
                        }
                        i += 1;
                        // The ESC was consumed by the preceding feed/loop
                        // iteration. The recovery window is always at least
                        // two bytes, so a failed append consumes this complete
                        // pair and never has to replay only half an escape.
                        let consumed = self.consume_seq_bytes(&[0x1b, b]);
                        debug_assert_eq!(consumed, 2);
                    } else {
                        // Bulk path: sequence bytes run to the next ESC, raw
                        // ST (0x9c), or — OSC only — BEL terminator. A BEL
                        // inside a DCS/APC body is payload, exactly as the
                        // old per-byte arm treated it.
                        let hay = &input[i..];
                        let terminator = if self.mode == Mode::Osc {
                            memchr::memchr3(0x1b, 0x9c, 0x07, hay)
                        } else {
                            memchr::memchr2(0x1b, 0x9c, hay)
                        };
                        // CAN/SUB cancel the string wherever they appear, so
                        // they are stop bytes too — scanned separately because
                        // `memchr` tops out at three needles.
                        let cancel = memchr::memchr2(0x18, 0x1a, hay);
                        let stop = match (terminator, cancel) {
                            (Some(t), Some(c)) => Some(t.min(c)),
                            (t, c) => t.or(c),
                        };
                        match stop {
                            Some(off) => {
                                let consumed = self.consume_seq_bytes(&hay[..off]);
                                i += consumed;
                                if consumed < off {
                                    // Bounded discard recovery landed inside
                                    // this run. Reprocess the remainder in
                                    // Pass mode instead of swallowing the
                                    // entire caller-provided buffer.
                                    continue;
                                }
                                if self.mode == Mode::Pass {
                                    // Recovery landed exactly before the stop
                                    // byte. Let Pass mode interpret it instead
                                    // of finishing an already-abandoned
                                    // sequence with an empty accumulator.
                                    continue;
                                }
                                let b = hay[off];
                                if b == 0x9c && self.seq_expects_utf8_continuation() {
                                    // A raw 0x9c that continues an in-progress
                                    // UTF-8 character is payload, not an 8-bit
                                    // ST — matching the downstream VT engine,
                                    // xterm, and Windows Terminal. Cutting here
                                    // leaked the rest of the string to the grid
                                    // as text (stray "C" / stale-row bugs).
                                    let consumed = self.consume_seq_bytes(&hay[off..off + 1]);
                                    i += consumed;
                                    // If the discard-recovery boundary landed on
                                    // this byte the mode is already Pass; either
                                    // way the outer loop re-dispatches correctly.
                                    continue;
                                }
                                i += 1;
                                if b == 0x18 || b == 0x1a {
                                    self.cancel_seq();
                                } else if b == 0x1b {
                                    self.st_pending = true;
                                } else {
                                    self.term_bel = b == 0x07;
                                    self.finish_seq(&mut out);
                                }
                            }
                            None => {
                                let consumed = self.consume_seq_bytes(hay);
                                i += consumed;
                            }
                        }
                    }
                }
            }
            self.handle_pending_chunks(&mut out, &mut handle);
        }
        self.flush_pass(&mut out);
        self.handle_pending_chunks(&mut out, &mut handle);
    }

    fn handle_pending_chunks<F>(&mut self, out: &mut Vec<Chunk>, handle: &mut F)
    where
        F: FnMut(&mut Self, Chunk),
    {
        for chunk in out.drain(..) {
            handle(self, chunk);
        }
    }

    fn flush_pass(&mut self, out: &mut Vec<Chunk>) {
        if !self.pass.is_empty() {
            out.push(Chunk::Pass(std::mem::take(&mut self.pass)));
        }
    }

    fn begin_seq(&mut self) {
        self.seq.clear();
        self.seq_reservation = None;
        self.discarding_seq = false;
        self.discard_remaining = 0;
        self.seq_tail_len = 0;
    }

    fn append_seq(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let Some(new_len) = self.seq.len().checked_add(bytes.len()) else {
            self.bail(0);
            return false;
        };
        if new_len > self.budget.limits().sequence_bytes {
            self.bail(0);
            return false;
        }
        if let Some(r) = self.seq_reservation.as_mut() {
            if !r.try_grow_to(new_len) {
                self.bail(0);
                return false;
            }
        } else {
            let Some(r) = self.budget.reserve_transient_cpu(new_len) else {
                self.bail(0);
                return false;
            };
            self.seq_reservation = Some(r);
        }
        if self.seq.try_reserve_exact(bytes.len()).is_err() {
            self.bail(0);
            return false;
        }
        self.seq.extend_from_slice(bytes);
        true
    }

    /// Consume bytes that belong to the active control string. Once the
    /// configured sequence limit is crossed, bytes up to that boundary plus a
    /// bounded recovery window are dropped. If the recovery boundary lands in
    /// this slice, the unconsumed suffix is handled again in Pass mode.
    fn consume_seq_bytes(&mut self, bytes: &[u8]) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        let consumed = 'consumed: {
            if !self.discarding_seq {
                let room = self
                    .budget
                    .limits()
                    .sequence_bytes
                    .saturating_sub(self.seq.len());
                if bytes.len() > room {
                    // `append_seq` is intentionally all-or-nothing. Account for
                    // the prefix that still fit so recovery starts at the actual
                    // configured limit, not at the start of this bulk slice.
                    self.bail(room);
                } else if self.append_seq(bytes) {
                    break 'consumed bytes.len();
                }
            }
            self.consume_discard_bytes(bytes.len())
        };
        self.note_seq_tail(&bytes[..consumed]);
        consumed
    }

    /// Remember the last ≤3 payload bytes actually consumed (stored or
    /// discarded) so `seq_expects_utf8_continuation` can classify a raw
    /// `0x9c` stop byte without re-walking the accumulator.
    fn note_seq_tail(&mut self, consumed: &[u8]) {
        if consumed.is_empty() {
            return;
        }
        if consumed.len() >= 3 {
            self.seq_tail
                .copy_from_slice(&consumed[consumed.len() - 3..]);
            self.seq_tail_len = 3;
        } else {
            let keep = (3 - consumed.len()).min(self.seq_tail_len as usize);
            let start = self.seq_tail_len as usize - keep;
            self.seq_tail
                .copy_within(start..self.seq_tail_len as usize, 0);
            self.seq_tail[keep..keep + consumed.len()].copy_from_slice(consumed);
            self.seq_tail_len = (keep + consumed.len()) as u8;
        }
    }

    /// True when the payload consumed so far ends with an incomplete UTF-8
    /// scalar, i.e. the next byte is expected to be a continuation byte. A
    /// raw `0x9c` in that position is character data (✳ = `E2 9C B3`,
    /// 💜 = `F0 9F 92 9C`, 末 = `E6 9C AB`, …), not a C1 ST.
    fn seq_expects_utf8_continuation(&self) -> bool {
        let tail = &self.seq_tail[..self.seq_tail_len as usize];
        let mut cont = 0usize;
        for &b in tail.iter().rev() {
            if (0x80..=0xBF).contains(&b) {
                cont += 1;
            } else {
                break;
            }
        }
        if cont >= tail.len() {
            // Every known byte is a continuation: any lead byte is outside
            // the 3-byte window, so the character is already complete (or
            // the payload is malformed) — treat the 0x9c as a real ST.
            return false;
        }
        let needed = match tail[tail.len() - 1 - cont] {
            0xC2..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF4 => 3,
            _ => return false,
        };
        cont < needed
    }

    fn consume_discard_bytes(&mut self, available: usize) -> usize {
        let consumed = available.min(self.discard_remaining);
        self.discard_remaining -= consumed;
        if self.discard_remaining == 0 {
            self.reset_discard();
        }
        consumed
    }

    /// Drop an over-budget graphics/control string. The downstream VT engine
    /// never saw its introducer, so forwarding a second full copy is both
    /// unnecessary and would defeat the allocation limit.
    fn bail(&mut self, bytes_before_limit: usize) {
        // Release the backing allocation while its reservation is still held;
        // `clear` alone would retain up to 16 MiB without an active lease.
        self.seq = Vec::new();
        self.seq_reservation = None;
        self.discarding_seq = true;
        let recovery = self
            .budget
            .limits()
            .sequence_bytes
            .clamp(2, MAX_SEQ_RESYNC_BYTES);
        self.discard_remaining = bytes_before_limit.saturating_add(recovery);
    }

    fn reset_discard(&mut self) {
        self.seq = Vec::new();
        self.seq_reservation = None;
        self.discarding_seq = false;
        self.discard_remaining = 0;
        self.st_pending = false;
        self.term_bel = false;
        self.mode = Mode::Pass;
    }

    /// CAN (0x18) / SUB (0x1a) abandon the control string in progress.
    ///
    /// DEC defines both as immediate cancellation of DCS/OSC/APC/PM/SOS, and
    /// every real terminal implements it. Treating them as payload meant the
    /// extractor stayed in the string state waiting for a terminator that was
    /// never coming: a single stray `0x18` inside an OSC swallowed the rest of
    /// the stream, so the pane simply stopped updating and looked frozen.
    ///
    /// The accumulated bytes are dropped rather than emitted — a cancelled
    /// string was never a command, and printing its half-finished payload to
    /// the grid is how the "stray text after a truncated title" class of bug
    /// happens.
    fn cancel_seq(&mut self) {
        if self.discarding_seq {
            self.reset_discard();
            return;
        }
        self.seq.clear();
        let _seq_reservation = self.seq_reservation.take();
        self.mode = Mode::Pass;
        self.st_pending = false;
        self.term_bel = false;
    }

    fn finish_seq(&mut self, out: &mut Vec<Chunk>) {
        if self.discarding_seq {
            self.reset_discard();
            return;
        }
        let mut seq = std::mem::take(&mut self.seq);
        let _seq_reservation = self.seq_reservation.take();
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
        // the UI maps onto the OS taskbar indicator.
        if mode == Mode::Osc && seq.starts_with(b"9;4;") {
            if let Some(p) = parse_osc9_4(&seq) {
                out.push(Chunk::Progress(p));
            }
            return;
        }
        // OSC 9;9;<path> — ConEmu "set working directory" (the Windows
        // convention Windows Terminal also honors). The payload is a PLAIN
        // filesystem path (often double-quoted), NOT a file:// URI like OSC 7.
        // MUST precede the OSC 9 notification handler below, which strips the
        // `9;` prefix and would otherwise swallow `9;9;C:\path` as a bogus
        // notification. Surfaces the same Chunk::Cwd as OSC 7 (last-writer-wins;
        // both are shell-volunteered truth).
        if mode == Mode::Osc && seq.starts_with(b"9;9;") {
            if let Some(path) = parse_osc9_9(&seq[4..]) {
                out.push(Chunk::Cwd(path));
            }
            return;
        }
        // OSC 9;<message> (iTerm2-style) and OSC 777;notify;<title>;<body>
        // (ConEmu-style) are terminal-to-desktop notification requests. Consume
        // only recognized notify shapes; leave unrelated OSC 777 commands
        // untouched for downstream compatibility.
        if mode == Mode::Osc
            && let Some((title, body)) = parse_protocol_notification(&seq)
        {
            out.push(Chunk::Notification { title, body });
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

        if self.defer_graphics && is_graphics_sequence(mode, &seq) {
            let terminator_len = if self.term_bel && mode == Mode::Osc {
                1
            } else {
                2
            };
            let Some(raw_len) = seq
                .len()
                .checked_add(2)
                .and_then(|len| len.checked_add(terminator_len))
            else {
                return;
            };
            let mut reservation = match _seq_reservation {
                Some(reservation) => reservation,
                None => {
                    let Some(reservation) = self.budget.reserve_transient_cpu(raw_len) else {
                        return;
                    };
                    reservation
                }
            };
            if !reservation.try_grow_to(raw_len)
                || seq.try_reserve_exact(2 + terminator_len).is_err()
            {
                return;
            }
            let payload_len = seq.len();
            seq.resize(raw_len, 0);
            seq.copy_within(0..payload_len, 2);
            seq[0] = 0x1b;
            seq[1] = match mode {
                Mode::Dcs => b'P',
                Mode::Apc => b'_',
                Mode::Osc => b']',
                Mode::Pass => return,
            };
            if terminator_len == 1 {
                seq[raw_len - 1] = 0x07;
            } else {
                seq[raw_len - 2] = 0x1b;
                seq[raw_len - 1] = b'\\';
            }
            out.push(Chunk::DeferredGraphics(DeferredGraphics {
                bytes: seq,
                _reservation: reservation,
            }));
            return;
        }

        enum R {
            None,
            Img(Placed),
            Del(Delete),
            Virtual {
                id: u32,
                placement: u32,
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
                z: i32,
                params: PlacementParams,
            },
        }

        let result = match mode {
            Mode::Dcs => {
                // Sixel: params then 'q' then data.
                // A Sixel DCS is `P1;P2;P3 q
                // <data>` — the bytes before `q` are only digits / `;`. Requiring
                // that prefix shape stops other DCS strings whose body contains a
                // `q` (DECRQSS `$q…`, XTGETTCAP `+q…`) from being swallowed as
                // tiny spurious images; those now forward verbatim (R::None).
                match seq
                    .iter()
                    .position(|&c| c == b'q')
                    .filter(|&q| seq[..q].iter().all(|&c| c.is_ascii_digit() || c == b';'))
                {
                    Some(qpos) => sixel::decode_with_budget(&seq[qpos + 1..], &self.budget)
                        .map(|i| R::Img(Placed::plain(i)))
                        .unwrap_or(R::None),
                    None => R::None,
                }
            }
            Mode::Apc => {
                if seq.first() == Some(&b'G') {
                    // Kitty control/base64 is ASCII. Reject invalid UTF-8
                    // without allocating a second sequence-sized lossy copy.
                    let Some(body) = std::str::from_utf8(&seq[1..]).ok() else {
                        return;
                    };
                    match self.kitty_mut().feed(body) {
                        KittyOut::Place(p) => R::Img(p),
                        KittyOut::Delete(delete) => R::Del(delete),
                        // Virtual placements draw nothing at the cursor; the
                        // stored image + box are surfaced so the renderer can
                        // composite them where placeholder cells appear.
                        KittyOut::Virtual { id, placement } => {
                            match (
                                self.kitty().image(id),
                                self.kitty().virtual_placement(id, placement),
                            ) {
                                (Some(img), Some(vp)) => R::Virtual {
                                    id,
                                    placement,
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
                        KittyOut::Animate { id } => match self.kitty().image(id) {
                            Some(base) => {
                                let st = self.kitty().animation(id).copied().unwrap_or_default();
                                let mut imgs = vec![base.clone()];
                                let mut gaps = vec![st.root_gap];
                                for f in self.kitty().frames(id) {
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
                                self.kitty().image(id),
                                self.kitty().relative_placement(id, placement),
                            ) {
                                (Some(img), Some(rp)) => R::Rel {
                                    id,
                                    placement,
                                    img: img.clone(),
                                    parent_img: rp.parent_img,
                                    parent_placement: rp.parent_placement,
                                    h: rp.h,
                                    v: rp.v,
                                    z: rp.z,
                                    params: rp.params,
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
                // full String just to fail `starts_with`.
                if seq.starts_with(b"1337;File=") {
                    std::str::from_utf8(&seq)
                        .ok()
                        .and_then(|body| iterm::decode_with_budget(body, &self.budget))
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
            R::Del(delete) => out.push(Chunk::DeleteImages(delete)),
            R::Virtual {
                id,
                placement,
                img,
                cols,
                rows,
                z,
            } => out.push(Chunk::VirtualImage {
                id,
                placement,
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
                z,
                params,
            } => out.push(Chunk::RelativePlacement {
                id,
                placement,
                img,
                parent_img,
                parent_placement,
                h,
                v,
                z,
                params,
            }),
            R::None => {
                // Not an image (or unsupported): forward verbatim, terminator
                // included, so the VT engine handles it.
                let Some(pass_len) = seq.len().checked_add(4) else {
                    return;
                };
                let Some(_pass_reservation) = self.budget.reserve_transient_cpu(pass_len) else {
                    return;
                };
                let mut v = Vec::new();
                if v.try_reserve_exact(pass_len).is_err() {
                    return;
                }
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
    let local = local_hostname();
    parse_osc7_with_host(s, local.as_deref())
}

/// v2.29.0: parse an OSC 9;9 working-directory payload (everything after the
/// `9;9;`). ConEmu's "set working directory" convention — a PLAIN filesystem
/// path (e.g. `C:\Users\me\proj` or `/home/me/proj`), frequently wrapped in
/// double quotes — NOT a `file://` URI like OSC 7, so it is taken verbatim
/// (after unquoting) rather than URL-decoded. Returns the unquoted path, or
/// `None` if empty. Because Windows Terminal honors this sequence, any
/// oh-my-posh / starship / custom prompt a user already configured for WT
/// reports its cwd to kettle for free.
fn parse_osc9_9(payload: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(payload);
    let trimmed = s.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The machine's hostname for OSC 7 validation. Asks the OS
/// (gethostname(2) / GetComputerNameExW — review fix: the env vars alone
/// fail OPEN on Linux/macOS, where interactive bash does not export
/// `HOSTNAME`), falling back to `COMPUTERNAME`/`HOSTNAME`. Cached: one OS
/// call per process, not one per OSC 7 report. `None` means "unknown" —
/// validation then only rejects nothing (an unknown local name must not
/// break every report that carries a host).
fn local_hostname() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            gethostname::gethostname()
                .into_string()
                .ok()
                .filter(|h| !h.trim().is_empty())
                .or_else(|| {
                    std::env::var("COMPUTERNAME")
                        .or_else(|_| std::env::var("HOSTNAME"))
                        .ok()
                        .filter(|h| !h.trim().is_empty())
                })
        })
        .clone()
}

/// v2.20.0 (Ghostty parity): parse an OSC 7 body, accepting BOTH schemes —
/// `file://host/path` (percent-encoded) and kitty's `kitty-shell-cwd://host/path`
/// (raw bytes, NOT percent-encoded — kitty invented the scheme precisely so
/// shells don't have to URL-encode) — and validating the hostname: a report
/// whose host is non-empty, not `localhost`, and not THIS machine is dropped.
/// An ssh session's shell integration reports the REMOTE host's cwd; treating
/// `/home/user` from another machine as a local directory breaks new-tab
/// cwd inheritance and `OpenCwdInFileManager`. (Ghostty applies the same
/// check in its stream handler.) `local_host = None` skips the rejection for
/// named hosts only when the local name is unknowable.
fn parse_osc7_with_host(s: &str, local_host: Option<&str>) -> Option<String> {
    // Split scheme; kitty-shell-cwd paths are used VERBATIM (no decode).
    let (rest, percent_encoded) = if let Some(r) = s.strip_prefix("kitty-shell-cwd://") {
        (r, false)
    } else {
        (s.strip_prefix("file://").unwrap_or(s), true)
    };
    // No path component at all (e.g. `file://localhost` with no trailing
    // slash) is invalid: an OSC 7 cwd starts with `/` (or `/C:/...`).
    let path_start = rest.find('/')?;
    let (host, path) = (&rest[..path_start], &rest[path_start..]);
    let host = host.trim();
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        match local_host {
            Some(local) if host.eq_ignore_ascii_case(local.trim()) => {}
            // A FQDN report from this machine ("host.lan" vs "host"): accept
            // when the first label matches.
            Some(local)
                if host
                    .split('.')
                    .next()
                    .is_some_and(|l| l.eq_ignore_ascii_case(local.trim())) => {}
            Some(_) => return None, // someone else's cwd (ssh) — reject
            None => {}              // local name unknown — accept
        }
    }
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    if !percent_encoded {
        return Some(normalize_drive_path(path.to_string()));
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
            // Require BOTH escape bytes to be ASCII hex digits. `u8::from_str_radix`
            // also accepts a leading sign (`+5`/`-5`), so `%+5`/`%-5` would
            // otherwise mis-decode to a byte instead of being passed through as
            // the literal text they are — guard the digits explicitly first.
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
            // Slice the *bytes* (never the &str): a `%` immediately
            // followed by a multibyte UTF-8 char would otherwise make
            // `&path[i+1..i+3]` land on a non-char-boundary and panic
            // (a hard crash under panic=abort) before from_str_radix
            // could reject it. from_utf8 rejects a mid-char byte pair,
            // so the `%` falls through and is pushed as a literal byte.
            // (Both bytes are now known ASCII hex, so from_utf8 / from_str_radix
            // cannot fail here, but keep the chained form for robustness.)
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
    Some(normalize_drive_path(
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

/// v2.20.0: a Windows drive path travels in URL form as `/C:/Users/x`
/// (leading slash before the drive letter — the WT / Ghostty convention).
/// Strip that slash so the reported cwd is a usable Windows path
/// (`C:/Users/x`; Windows APIs accept forward slashes). Unix paths are
/// untouched.
fn normalize_drive_path(path: String) -> String {
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path[1..].to_string()
    } else {
        path
    }
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

fn parse_protocol_notification(seq: &[u8]) -> Option<(String, String)> {
    if let Some(rest) = seq.strip_prefix(b"9;") {
        // ConEmu/Windows-Terminal OSC 9 is a structured command namespace
        // (`9;1` progress, `9;2`, `9;3`, `9;4` taskbar progress, …), NOT an
        // iTerm2 free-text notification. The 9;4 / 9;9 carve-outs above catch
        // the two kettle understands; forward the rest downstream rather than
        // firing a spurious notification with a numeric/garbled title. Two
        // structured shapes:
        //   * `<digits>;…`  — a subcommand id followed by `;` and a payload.
        //   * all-digit/whitespace (e.g. a bare `ESC]9;4 ST`) — a subcommand
        //     id with no payload.
        // iTerm2 free text (`9;build finished`, `9;100% done`) has its digits
        // NOT immediately followed by `;`, so it still notifies.
        let lead_digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let structured = (lead_digits > 0 && rest.get(lead_digits) == Some(&b';'))
            || (!rest.is_empty()
                && rest
                    .iter()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_whitespace()));
        if structured {
            return None;
        }
        let title = clean_notify_field(rest, false)?;
        return Some((title, String::new()));
    }

    let rest = seq.strip_prefix(b"777;")?;
    let mut parts = rest.splitn(3, |&b| b == b';');
    if !parts.next()?.eq_ignore_ascii_case(b"notify") {
        return None;
    }
    let title = clean_notify_field(parts.next().unwrap_or_default(), false)
        .unwrap_or_else(|| "kettle".to_string());
    let body = clean_notify_field(parts.next().unwrap_or_default(), true).unwrap_or_default();
    Some((title, body))
}

fn clean_notify_field(bytes: &[u8], allow_newline: bool) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_NOTIFY_FIELD_BYTES {
        return None;
    }
    let mut out = String::with_capacity(bytes.len().min(MAX_NOTIFY_FIELD_BYTES));
    for ch in String::from_utf8_lossy(bytes).chars() {
        if ch == '\n' && allow_newline {
            out.push('\n');
        } else if ch == '\t' || ch == '\r' || ch == '\n' || ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
    /// CAN/SUB must cancel a control string, or the terminal freezes.
    ///
    /// DEC defines `0x18` and `0x1a` as immediate cancellation of any
    /// DCS/OSC/APC string. Treating them as payload left the extractor waiting
    /// for a terminator that never arrived, so ONE stray byte swallowed the
    /// entire rest of the stream — the pane stopped updating and looked hung.
    /// Untrusted program output can contain that byte.
    #[test]
    fn can_and_sub_cancel_a_control_string_instead_of_wedging_it() {
        for cancel in [0x18_u8, 0x1a] {
            for intro in [&b"]2;"[..], &b"P"[..], &b"_"[..]] {
                let mut ex = Extractor::default();
                let mut input = intro.to_vec();
                input.extend_from_slice(b"payload");
                input.push(cancel);
                input.extend_from_slice(
                    b"after
",
                );

                let chunks = ex.feed(&input);
                let mut plain = Vec::new();
                for c in &chunks {
                    if let Chunk::Pass(b) = c {
                        plain.extend_from_slice(b);
                    }
                }
                assert_eq!(
                    String::from_utf8_lossy(&plain),
                    "after
",
                    "cancel {cancel:#04x} after {intro:?}: output following the                      cancellation must reach the terminal, and the abandoned                      payload must not"
                );

                // And the extractor is back in Pass mode — a later feed is not
                // still being eaten by the abandoned string.
                let more = ex.feed(
                    b"later
",
                );
                let mut plain2 = Vec::new();
                for c in &more {
                    if let Chunk::Pass(b) = c {
                        plain2.extend_from_slice(b);
                    }
                }
                assert_eq!(
                    String::from_utf8_lossy(&plain2),
                    "later
",
                    "the next chunk must not be swallowed either"
                );
            }
        }
    }

    /// The cancellation must not change how a properly terminated string
    /// behaves — BEL and ST still complete their sequence.
    #[test]
    fn a_properly_terminated_control_string_is_unaffected_by_cancel_support() {
        let mut ex = Extractor::default();
        let chunks = ex.feed(
            b"]2;titlevisible
",
        );
        let mut plain = Vec::new();
        for c in &chunks {
            if let Chunk::Pass(b) = c {
                plain.extend_from_slice(b);
            }
        }
        assert!(
            String::from_utf8_lossy(&plain).ends_with(
                "visible
"
            ),
            "a BEL-terminated OSC still passes its trailing text through"
        );
    }

    use super::{Chunk, Extractor, PromptKind};
    use crate::kitty::PlacementKey;
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
    fn streaming_feed_applies_delete_before_later_apcs_in_the_same_read() {
        let mut ex = Extractor::isolated();
        ex.feed(b"\x1b_Ga=t,i=7,f=32,s=1,v=1;AQIDBA==\x1b\\");

        let input = concat!(
            "\x1b_Ga=d,d=I,i=7,p=9\x1b\\",
            "\x1b_Ga=p,i=7,p=10\x1b\\",
            "\x1b_Ga=t,i=7,f=32,s=1,v=1;AQIDBA==\x1b\\",
            "\x1b_Ga=p,i=7,p=11\x1b\\",
        );
        let mut chunks = Vec::new();
        ex.feed_with(input.as_bytes(), |extractor, chunk| {
            if matches!(chunk, Chunk::DeleteImages(_)) {
                extractor.apply_kitty_delete_result(
                    &[PlacementKey {
                        image_id: 7,
                        placement_id: 9,
                    }],
                    &[7],
                );
            }
            chunks.push(chunk);
        });

        let placements = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                Chunk::Image(placed) => Some(placed.placement_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            placements,
            vec![11],
            "the old data must be unavailable immediately, while data transmitted after deletion survives"
        );
    }

    #[test]
    fn deferred_graphics_preserve_wire_bytes_without_mutating_kitty_state() {
        const TRANSMIT: &[u8] = b"\x1b_Ga=t,i=7,f=32,s=1,v=1;AQIDBA==\x1b\\";
        const PUT: &[u8] = b"\x1b_Ga=p,i=7,p=9\x1b\\";

        let mut ex = Extractor::isolated();
        ex.set_graphics_deferred(true);
        let deferred = ex.feed(TRANSMIT);
        assert_eq!(deferred.len(), 1);
        let raw = match &deferred[0] {
            Chunk::DeferredGraphics(graphics) => graphics.as_bytes(),
            other => panic!("expected deferred kitty APC, got {other:?}"),
        };
        assert_eq!(raw, TRANSMIT);

        ex.set_graphics_deferred(false);
        assert!(
            !ex.feed(PUT)
                .iter()
                .any(|chunk| matches!(chunk, Chunk::Image(_))),
            "deferring the transmit must not populate the active kitty store"
        );

        ex.feed(raw);
        assert!(
            ex.feed(PUT)
                .iter()
                .any(|chunk| matches!(chunk, Chunk::Image(placed) if placed.id == Some(7) && placed.placement_id == 9)),
            "replaying the deferred bytes must mutate kitty state at that exact point"
        );
    }

    #[test]
    fn all_supported_graphics_controls_defer_with_exact_terminators() {
        for raw in [
            &b"\x1bPq~\x1b\\"[..],
            &b"\x1b_Ga=d,d=A\x1b\\"[..],
            &b"\x1b]1337;File=inline=1:AAAA\x07"[..],
        ] {
            let mut ex = Extractor::isolated();
            ex.set_graphics_deferred(true);
            let chunks = ex.feed(raw);
            assert_eq!(chunks.len(), 1, "unexpected chunks for {raw:?}");
            match &chunks[0] {
                Chunk::DeferredGraphics(graphics) => assert_eq!(graphics.as_bytes(), raw),
                other => panic!("expected deferred graphics for {raw:?}, got {other:?}"),
            }
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

    /// v2.29.0: OSC 9;9 (ConEmu "set working directory") is consumed as a Cwd
    /// chunk — a PLAIN path (often quoted), NOT a file:// URI. CRITICAL: it must
    /// be handled by the dedicated 9;9 branch and NOT swallowed by the OSC 9
    /// notification handler, while a bare `OSC 9;<msg>` still notifies and 9;4
    /// still reports progress.
    #[test]
    fn osc9_9_sets_cwd_without_colliding_with_osc9_notification() {
        let mut ex = Extractor::new();
        // Plain Windows path.
        let out = ex.feed(b"\x1b]9;9;C:\\Users\\me\\proj\x07");
        assert!(
            matches!(out.first(), Some(Chunk::Cwd(p)) if p == "C:\\Users\\me\\proj"),
            "9;9 should emit Cwd, got {out:?}"
        );
        // Quoted path (some prompts wrap it in double quotes).
        let out = ex.feed(b"\x1b]9;9;\"C:\\path with space\"\x07");
        assert!(
            matches!(out.first(), Some(Chunk::Cwd(p)) if p == "C:\\path with space"),
            "quoted 9;9 should unquote, got {out:?}"
        );
        // Forward-slash / Unix path accepted verbatim (no file:// decode).
        let out = ex.feed(b"\x1b]9;9;/home/me/proj\x07");
        assert!(
            matches!(out.first(), Some(Chunk::Cwd(p)) if p == "/home/me/proj"),
            "unix 9;9 should emit Cwd, got {out:?}"
        );
        // REGRESSION: a bare OSC 9;<message> is STILL a notification, not a cwd.
        let out = ex.feed(b"\x1b]9;build finished\x07");
        assert!(
            out.iter().any(|c| matches!(c, Chunk::Notification { .. }))
                && !out.iter().any(|c| matches!(c, Chunk::Cwd(_))),
            "bare OSC 9 must still notify and NOT be a cwd, got {out:?}"
        );
        // OSC 9;4 progress still works (its handler precedes the 9;9 branch).
        let out = ex.feed(b"\x1b]9;4;1;50\x07");
        assert!(
            out.iter().any(|c| matches!(c, Chunk::Progress(_))),
            "9;4 should still be progress, got {out:?}"
        );
    }

    /// v2.20.0 P3 regression guards for the memchr bulk path: byte-exact
    /// state semantics at every boundary the old per-byte loop handled.
    fn passed(out: &[Chunk]) -> Vec<u8> {
        out.iter()
            .filter_map(|c| match c {
                Chunk::Pass(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// v2.20.0 (Ghostty parity): OSC 7 accepts the kitty-shell-cwd scheme
    /// (raw path, no percent-decode) and validates the hostname — a remote
    /// host's cwd (ssh shell integration) must be DROPPED, not adopted.
    #[test]
    fn osc7_kitty_scheme_and_hostname_validation() {
        use super::parse_osc7_with_host;
        // kitty scheme: path verbatim, no decode (a literal `%20` stays).
        assert_eq!(
            parse_osc7_with_host("kitty-shell-cwd://myhost/home/u/dir%20x", Some("myhost")),
            Some("/home/u/dir%20x".to_string())
        );
        // file scheme still percent-decodes.
        assert_eq!(
            parse_osc7_with_host("file://myhost/home/u/dir%20x", Some("myhost")),
            Some("/home/u/dir x".to_string())
        );
        // Empty host + localhost are always local.
        assert_eq!(
            parse_osc7_with_host("file:///tmp", Some("myhost")),
            Some("/tmp".to_string())
        );
        assert_eq!(
            parse_osc7_with_host("file://localhost/tmp", Some("myhost")),
            Some("/tmp".to_string())
        );
        // Case-insensitive + FQDN-first-label matches count as local.
        assert_eq!(
            parse_osc7_with_host("file://MyHost/tmp", Some("myhost")),
            Some("/tmp".to_string())
        );
        assert_eq!(
            parse_osc7_with_host("file://myhost.lan/tmp", Some("myhost")),
            Some("/tmp".to_string())
        );
        // ANOTHER machine's report is rejected (the ssh case).
        assert_eq!(
            parse_osc7_with_host("file://buildbox/home/u", Some("myhost")),
            None
        );
        // A host with NO path component (no slash) is not a usable cwd — reject
        // rather than emit a bogus relative cwd like "localhost" (audit v2.25.0).
        assert_eq!(
            parse_osc7_with_host("file://localhost", Some("myhost")),
            None
        );
        assert_eq!(parse_osc7_with_host("file://myhost", Some("myhost")), None);
        assert_eq!(
            parse_osc7_with_host("kitty-shell-cwd://buildbox/home/u", Some("myhost")),
            None
        );
        // Unknown local name: named hosts are accepted (can't validate).
        assert_eq!(
            parse_osc7_with_host("file://buildbox/home/u", None),
            Some("/home/u".to_string())
        );
        // Windows drive paths arrive URL-form (`/C:/…`) and normalize to a
        // usable path; the drive colon may be percent-encoded by strict
        // encoders ([uri]::EscapeDataString in the pwsh snippet).
        assert_eq!(
            parse_osc7_with_host("file://myhost/C:/Users/k/dir", Some("myhost")),
            Some("C:/Users/k/dir".to_string())
        );
        assert_eq!(
            parse_osc7_with_host("file://myhost/C%3A/Users/k/dir%20x", Some("myhost")),
            Some("C:/Users/k/dir x".to_string())
        );
        // A plain unix root path is untouched by drive normalization.
        assert_eq!(
            parse_osc7_with_host("file:///c", Some("myhost")),
            Some("/c".to_string())
        );
    }

    #[test]
    fn bel_inside_dcs_is_payload_not_terminator() {
        // BEL terminates OSC only; inside a DCS body it is payload. The
        // non-sixel DCS is forwarded verbatim (ESC \ terminated), BEL intact.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1bPnot-sixel\x07body\x1b\\after");
        assert_eq!(passed(&out), b"\x1bPnot-sixel\x07body\x1b\\after");
    }

    #[test]
    fn raw_st_terminates_a_sequence() {
        // 0x9c (raw C1 ST) ends an OSC; the forwarded copy is re-terminated
        // with ESC \ (term_bel = false), exactly as the per-byte loop did.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]2;title\x9cafter");
        assert_eq!(passed(&out), b"\x1b]2;title\x1b\\after");
    }

    #[test]
    fn utf8_continuation_0x9c_inside_osc_is_payload_not_st() {
        // ✳ (U+2733) = E2 9C B3 — every U+2700-block glyph Claude Code puts
        // in its OSC 0 title carries 0x9C as a continuation byte. The scan
        // must not cut the title there: the OSC forwards verbatim and no
        // residue leaks to the grid (the stray-"C" / stale-row bug).
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07after");
        assert_eq!(passed(&out), b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07after");
    }

    #[test]
    fn utf8_continuation_0x9c_split_across_feeds_is_payload() {
        // The lead byte arrives in one PTY read, the 0x9C continuation in
        // the next: the rolling tail must survive the chunk boundary.
        let mut ex = Extractor::new();
        let mut out = ex.feed(b"\x1b]0;\xe2");
        out.extend(ex.feed(b"\x9c\xb3 t\x07"));
        assert_eq!(passed(&out), b"\x1b]0;\xe2\x9c\xb3 t\x07");
    }

    #[test]
    fn utf8_continuation_0x9c_inside_dcs_is_payload() {
        // The memchr2 (DCS/APC) arm has the same defect: 末 (U+672B) =
        // E6 9C AB. The non-sixel DCS forwards verbatim, uncut.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1bPnot-sixel \xe6\x9c\xab\x1b\\after");
        assert_eq!(passed(&out), b"\x1bPnot-sixel \xe6\x9c\xab\x1b\\after");
    }

    #[test]
    fn utf8_final_byte_0x9c_of_4byte_char_is_payload() {
        // 💜 (U+1F49C) = F0 9F 92 9C — the 0x9C is the FINAL continuation
        // byte, classified via the full 3-byte look-back window.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]0;\xf0\x9f\x92\x9c\x07x");
        assert_eq!(passed(&out), b"\x1b]0;\xf0\x9f\x92\x9c\x07x");
    }

    #[test]
    fn standalone_raw_st_still_terminates_after_complete_multibyte_char() {
        // A COMPLETE UTF-8 char followed by a raw 0x9C: the first 0x9C (in
        // ✳) is payload, the second is a standalone C1 ST and still
        // terminates — legacy 8-bit-ST emitters keep working.
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]2;\xe2\x9c\xb3\x9cafter");
        assert_eq!(passed(&out), b"\x1b]2;\xe2\x9c\xb3\x1b\\after");
    }

    #[test]
    fn osc_split_across_feeds_is_reassembled() {
        // The sequence accumulator must survive a chunk boundary mid-body
        // (the bulk path's no-terminator arm) and mid-ESC-\ terminator.
        let mut ex = Extractor::new();
        let mut out = ex.feed(b"\x1b]133;");
        out.extend(ex.feed(b"A"));
        out.extend(ex.feed(b"\x1b"));
        out.extend(ex.feed(b"\\x"));
        assert!(
            out.iter()
                .any(|c| matches!(c, Chunk::Prompt(PromptKind::PromptStart))),
            "prompt mark should survive the split, got {out:?}"
        );
        assert_eq!(passed(&out), b"x");
    }

    #[test]
    fn esc_inside_osc_body_is_kept_when_not_st() {
        // ESC followed by anything but `\` inside a sequence body is payload
        // (st_pending unwound), byte-for-byte. The BEL-terminated OSC is
        // re-emitted BEL-terminated (`term_bel` preserved).
        let mut ex = Extractor::new();
        let out = ex.feed(b"\x1b]2;a\x1bzb\x07after");
        assert_eq!(passed(&out), b"\x1b]2;a\x1bzb\x07after");
    }

    /// FIX 1: ConEmu/Windows-Terminal OSC 9 subcommands (`9;1`, `9;2`, `9;3`,
    /// bare `9;4`, …) are structured commands, NOT iTerm2 free-text
    /// notifications. They must NOT fire a spurious desktop notification with a
    /// numeric/garbled title; they forward downstream instead. iTerm2 free text
    /// (digits not directly followed by `;`) must STILL notify.
    #[test]
    fn osc9_conemu_subcommands_do_not_notify() {
        let mut ex = Extractor::new();
        for seq in [
            &b"\x1b]9;1;x\x07"[..],
            &b"\x1b]9;2;x\x07"[..],
            &b"\x1b]9;3;x\x07"[..],
        ] {
            let out = ex.feed(seq);
            assert!(
                !out.iter().any(|c| matches!(c, Chunk::Notification { .. })),
                "structured OSC 9 subcommand must not notify: {seq:?} → {out:?}"
            );
        }
        // A bare `ESC]9;4 ST` (all-digit/whitespace remainder, no payload) must
        // not notify either — it is a degenerate progress/clear command.
        let out = ex.feed(b"\x1b]9;4\x07");
        assert!(
            !out.iter().any(|c| matches!(c, Chunk::Notification { .. })),
            "bare OSC 9;4 must not notify, got {out:?}"
        );
        // iTerm2 free-text notifications STILL fire (digits not followed by `;`).
        let out = ex.feed(b"\x1b]9;build finished\x07");
        assert!(
            out.iter().any(|c| matches!(
                c,
                Chunk::Notification { title, .. } if title == "build finished"
            )),
            "iTerm2 free-text OSC 9 must still notify, got {out:?}"
        );
        let out = ex.feed(b"\x1b]9;100% done\x07");
        assert!(
            out.iter().any(|c| matches!(
                c,
                Chunk::Notification { title, .. } if title == "100% done"
            )),
            "OSC 9 free text beginning with digits (not followed by `;`) must notify, got {out:?}"
        );
    }

    /// An over-budget string is consumed without forwarding a second full copy.
    /// Discarding lasts through its real terminator; the next sequence parses.
    #[test]
    fn over_budget_osc_is_discarded_and_does_not_desync() {
        let limits = crate::GraphicsLimits {
            sequence_bytes: 64,
            ..crate::GraphicsLimits::default()
        };
        let budget = crate::GraphicsBudget::isolated(limits).unwrap();
        let mut ex = Extractor::with_budget(budget);
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b]2;");
        input.extend(std::iter::repeat_n(b'A', limits.sequence_bytes + 16));
        let out = ex.feed(&input);
        assert!(
            out.is_empty(),
            "over-budget bytes must not be copied to Pass"
        );
        let out = ex.feed(b"discarded\x07\x1b]133;A\x07ok");
        assert!(
            out.iter()
                .any(|c| matches!(c, Chunk::Prompt(PromptKind::PromptStart))),
            "a clean sequence after the discarded OSC must parse, got {out:?}"
        );
        assert_eq!(passed(&out), b"ok");
    }

    /// DCS uses the same bounded discard path and recognizes its `ESC \` end.
    #[test]
    fn over_budget_dcs_discards_through_esc_backslash() {
        let limits = crate::GraphicsLimits {
            sequence_bytes: 64,
            ..crate::GraphicsLimits::default()
        };
        let budget = crate::GraphicsBudget::isolated(limits).unwrap();
        let mut ex = Extractor::with_budget(budget);
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1bP");
        input.extend(std::iter::repeat_n(b'B', limits.sequence_bytes + 16));
        let out = ex.feed(&input);
        assert!(out.is_empty());
        let out = ex.feed(b"discarded\x1b\\\x1b]133;A\x07ok");
        assert!(
            out.iter()
                .any(|c| matches!(c, Chunk::Prompt(PromptKind::PromptStart))),
            "a clean sequence after a DCS bail must still parse, got {out:?}"
        );
    }

    #[test]
    fn unterminated_over_budget_sequence_recovers_after_bounded_window() {
        let limits = crate::GraphicsLimits {
            sequence_bytes: 64,
            ..crate::GraphicsLimits::default()
        };
        let budget = crate::GraphicsBudget::isolated(limits).unwrap();
        let mut ex = Extractor::with_budget(budget);
        let mut input = b"\x1b]".to_vec();
        input.extend(std::iter::repeat_n(b'X', limits.sequence_bytes * 2 + 3));

        // The configured sequence allowance and one equally-sized recovery
        // window are swallowed. The suffix after that bounded point is plain
        // output even though the hostile OSC never supplied a terminator.
        let out = ex.feed(&input);
        assert_eq!(passed(&out), b"XXX");

        let out = ex.feed(b"\x1b]133;A\x07ok");
        assert!(
            out.iter()
                .any(|c| matches!(c, Chunk::Prompt(PromptKind::PromptStart))),
            "a clean sequence after bounded recovery must parse, got {out:?}"
        );
        assert_eq!(passed(&out), b"ok");
    }

    #[test]
    fn bounded_recovery_can_end_on_a_split_non_st_escape() {
        let limits = crate::GraphicsLimits {
            sequence_bytes: 64,
            ..crate::GraphicsLimits::default()
        };
        let budget = crate::GraphicsBudget::isolated(limits).unwrap();
        let mut ex = Extractor::with_budget(budget);
        let mut input = b"\x1b]".to_vec();
        input.extend(std::iter::repeat_n(b'X', limits.sequence_bytes * 2 - 1));
        input.push(0x1b);

        assert!(ex.feed(&input).is_empty());
        assert_eq!(
            passed(&ex.feed(b"Zok")),
            b"Zok",
            "the byte after the boundary ESC must be reprocessed in Pass mode"
        );
    }

    /// FIX 3: the OSC 7 percent-decoder must require BOTH escape bytes to be
    /// ASCII hex digits. `u8::from_str_radix` accepts a sign prefix (`+5`),
    /// which would otherwise mis-decode `%+5` to a byte; it must instead pass
    /// the `%` (and the rest) through literally — no panic, no mis-decode.
    #[test]
    fn osc7_percent_decoder_rejects_sign_prefixed_escape() {
        use super::parse_osc7_with_host;
        // `%+5` is not a valid escape — the `%` and following chars are literal.
        assert_eq!(
            parse_osc7_with_host("file:///p%+5x", Some("myhost")),
            Some("/p%+5x".to_string())
        );
        // `%-5` likewise.
        assert_eq!(
            parse_osc7_with_host("file:///p%-5x", Some("myhost")),
            Some("/p%-5x".to_string())
        );
        // A genuine hex escape still decodes (regression guard).
        assert_eq!(
            parse_osc7_with_host("file:///p%41x", Some("myhost")),
            Some("/pAx".to_string())
        );
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
