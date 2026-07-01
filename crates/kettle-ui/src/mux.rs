//! Tabs + a binary split tree (Terminator-style tiling). Each leaf owns an
//! independent terminal; splits tile the tab area; focus moves by geometry.

use std::collections::HashMap;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use kettle_config::{Config, CursorStyle};
use kettle_core::{CursorShape, TermEvent, Terminal, Waker};

/// Initial pane title seeded from the launching argv before the program's
/// first OSC 2. Plain shells use the placeholder "kettle" — cycle 89's
/// cwd-basename fallback fills in for those once OSC 7 arrives. SSH panes
/// have no local cwd, so we surface the target inline (`ssh me@box`) so a
/// tab full of them is distinguishable while connections are
/// establishing. For any *other* explicit `-e PROG` (e.g. `kettle -e htop`,
/// `kettle -e vim file`), the user has already told us what's running —
/// surface that program's basename instead of the generic "kettle", since
/// many TUIs (htop, top, less, vim's default, …) never emit OSC 2 and
/// have no usable cwd to back-fill from. Pure so the argv → title decision
/// is unit-tested.
fn initial_pane_title(argv: &[String]) -> String {
    let Some(arg0) = argv.first().map(String::as_str) else {
        return "kettle".into();
    };
    if arg0 == "ssh" {
        let host = argv
            .iter()
            .skip(1)
            .find(|a| !a.starts_with('-'))
            .cloned()
            .unwrap_or_default();
        return if host.is_empty() {
            "ssh".into()
        } else {
            format!("ssh {host}")
        };
    }
    // Basename of the program path — `/usr/bin/htop` → `htop`. Falls back
    // to the raw arg if it has no path separators.
    let base = std::path::Path::new(arg0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(arg0);
    // Shells are intentionally placeholders: the cwd-basename fallback
    // (`~/repos/kettle` → `kettle`) is more useful than the literal "bash".
    // List covers POSIX shells + common alt-shells; case-sensitive because
    // argv comes through unchanged on Unix and Windows shells go by other
    // names (cmd.exe, powershell.exe).
    const SHELLS: &[&str] = &[
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "ash",
        "ksh",
        "csh",
        "tcsh",
        "nu",
        "elvish",
        "xonsh",
        "pwsh",
        "powershell",
        "cmd",
        "cmd.exe",
        "powershell.exe",
        "pwsh.exe",
    ];
    if SHELLS.contains(&base) {
        return "kettle".into();
    }
    base.to_string()
}

/// Map the kettle config cursor style to the engine's seed shape. `Bar` and
/// `Beam` are the same thing under different names (vertical thin stroke);
/// the engine has more variants (`HollowBlock`, `Hidden`) that only ever
/// arrive via DECSCUSR from a running program, so they're never the
/// *default*.
fn engine_cursor_shape(s: CursorStyle) -> CursorShape {
    match s {
        CursorStyle::Block => CursorShape::Block,
        CursorStyle::Underline => CursorShape::Underline,
        CursorStyle::Bar => CursorShape::Beam,
    }
}

use crate::session::{SNode, STab, Session};

/// Pixel rectangle: `(x, y, w, h)`.
pub type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Children placed side-by-side (vertical divider between them).
    Horizontal,
    /// Children stacked (horizontal divider between them).
    Vertical,
}

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<kettle_core::Match>,
    pub index: usize,
}

pub struct Pane {
    pub term: Terminal,
    pub rx: Receiver<TermEvent>,
    /// Cycle 378 (Terminator plugin parity, plugin sub-cycle 3):
    /// optional output sidechannel. `Some` when the LuaEngine
    /// subscribed at App startup; the App drains it each tick and
    /// fires LuaEvent::Output(pane_id, bytes).
    pub output_rx: Option<Receiver<Vec<u8>>>,
    pub title: String,
    /// v2.29.0: whether `title` is still the generated seed (no genuine OSC 2
    /// title has arrived). Tab/window/pane labels treat a placeholder title as
    /// "show the cwd instead". Crucially this lets us IGNORE the bogus full-exe
    /// path that conhost/ConPTY injects as the startup OSC 2 title for a native
    /// Windows shell (it would otherwise outrank the cwd). Set `false` the
    /// instant any real OSC 2 title is stored, so a program-set title is never
    /// suppressed. Seeded `true` only for generic-shell panes (see `spawn_pane`).
    pub title_is_placeholder: bool,
    /// Cycle 406 (Terminator parity, named broadcast groups
    /// foundation): per-pane group name. When set, the pane is
    /// part of a named broadcast group; keyboard input to any
    /// member of the group broadcasts to every member. None
    /// means the pane has no group (Terminator default).
    ///
    /// Distinct from the cycle-178 per-tab broadcast (which is
    /// scope=tab, no name): named groups can span multiple tabs +
    /// be selectively enabled. Per-tab broadcast remains the
    /// quick-toggle path.
    pub group_name: Option<String>,
    pub closed: bool,
    /// Cycle 912 (audit): `exit-action = hold` was silently broken — `reap()`
    /// removed any pane whose child had exited regardless of intent. `held`
    /// marks a pane deliberately KEPT on screen after its shell exited (Hold);
    /// reap skips it until the user explicitly closes it (which sets `closed`).
    pub held: bool,
    /// Scrollback `history_size()` observed at the *previous* redraw — used
    /// to detect new output for `scroll-on-output`. `None` while no frame
    /// has been drawn yet (so the first frame doesn't look like growth).
    pub last_history: Option<usize>,
    /// Launching argv ([] means the configured shell). Held so a
    /// closed-tab snapshot can re-spawn the same program in
    /// `Action::UndoCloseTab` (cycle 247) — SSH tabs and `-e PROG`
    /// tabs reopen as the same SSH connection / TUI, not a generic
    /// shell. Doesn't track environment / cwd-after-launch — those
    /// re-derive from the OSC-7 cwd that's already snapshotted.
    pub argv: Vec<String>,
    /// Cycle 655 (Terminator parity, `plugins/remote.py`, sub-cycle
    /// 6 of [`TERMINATOR-REMOTE-DESIGN.md`](
    /// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): the most-recently
    /// detected remote-session context for this pane. Updated by
    /// the App's periodic poll (sub-cycle 6, to be wired). `None`
    /// means either the pane's process tree has no SSH / container
    /// descendant, or the poll hasn't run yet. When non-None, the
    /// pane title shows `format_remote_title(...)` and the right-
    /// click menu (sub-cycle 7) exposes a "Clone session" entry.
    pub remote_context: Option<kettle_remote::RemoteContext>,
    /// Cycle 934 (agent-first A4): set while an agent control connection has
    /// targeted this pane (a mutating method or `subscribe`). Drives the
    /// titlebar agent badge; cleared when the last attached connection drops.
    pub agent_attached: bool,
    /// Cycle 941 (Terminator parity, terminal_popup_menu.py "Read only"): when
    /// true, user input (keystrokes / paste / broadcast) is dropped before it
    /// reaches this pane's PTY — the child keeps producing output, but the pane
    /// can't be typed into. Toggled via `Action::TogglePaneReadOnly` or the
    /// right-click "Read only" item; shown as `[RO]` in the titlebar.
    pub read_only: bool,
}

impl Pane {
    /// Cycle 941 (Terminator parity): write user-originated input (keystroke /
    /// paste / IME / drag-drop / send-text) to the PTY, honoring the read-only
    /// toggle. Returns `true` when the bytes were written. VTE
    /// `feed_child` + `input-enabled` semantics: read-only blocks the *user*
    /// (and anything acting as the user — Lua send_text, remote.cmd, agent
    /// `send_text`/`run_command`), NOT the terminal protocol; replies like
    /// focus/mouse reports and DSR keep flowing through `term.write` directly.
    pub fn feed_input(&self, bytes: &[u8]) -> bool {
        if self.read_only {
            return false;
        }
        self.term.write(bytes);
        true
    }
}

/// Cycle 912 (audit): pure reap predicate. A pane is removed when explicitly
/// closed (Close / Restart / ClosePane set `closed`) OR its child exited and it
/// is not being HELD on screen (`exit-action = hold`). Without the `!held` guard
/// Hold behaved identically to Close — the dead shell vanished on the next
/// event-loop turn, defeating the feature entirely.
pub(crate) fn is_reapable(closed: bool, held: bool, child_exited: bool) -> bool {
    closed || (!held && child_exited)
}

pub enum Node {
    Leaf(u64),
    Split {
        dir: Dir,
        /// Fraction of the area given to child `a` (0.05..0.95).
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn first_leaf(&self) -> u64 {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { a, .. } => a.first_leaf(),
        }
    }

    /// Cycle 602: find the leaf id that should receive focus when
    /// `id` is removed from this tree. Returns the first leaf of
    /// `id`'s sibling subtree at the deepest Split containing
    /// `id`. Returns `None` if `id` isn't a leaf in this tree, or
    /// if the tree is a single Leaf (no sibling to promote).
    ///
    /// User-reported bug pre-cycle-602: `close_focused` was setting
    /// `tab.focus = tab.root.first_leaf()` after the close, which
    /// always jumps to the LEFTMOST leaf of the whole tab — i.e.,
    /// the first pane the user split from. Closing a deeply-nested
    /// pane felt teleporting. `neighbor_of` walks to the closed
    /// pane's split-mate instead, matching what every other
    /// terminal multiplexer does (tmux, wezterm, kitty).
    fn neighbor_of(&self, id: u64) -> Option<u64> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { a, b, .. } => {
                // If `id` is a direct Leaf child of this Split, the
                // sibling subtree's first leaf is the right neighbor.
                // Otherwise recurse — the deeper recursion finds the
                // sibling at the actual Split that contains `id`.
                if matches!(a.as_ref(), Node::Leaf(x) if *x == id) {
                    return Some(b.first_leaf());
                }
                if matches!(b.as_ref(), Node::Leaf(x) if *x == id) {
                    return Some(a.first_leaf());
                }
                a.neighbor_of(id).or_else(|| b.neighbor_of(id))
            }
        }
    }

    fn contains(&self, id: u64) -> bool {
        match self {
            Node::Leaf(x) => *x == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// DFS-order index of the leaf with id `target`, or `None` if not
    /// present. Used by session save to record which leaf is focused
    /// without depending on the per-pane numeric id (which is reallocated
    /// across restores). Walk order is the same `first → second` child
    /// order that `nth_leaf` uses, so the round trip is symmetric.
    fn leaf_index_of(&self, target: u64) -> Option<usize> {
        fn walk(n: &Node, target: u64, idx: &mut usize) -> Option<usize> {
            match n {
                Node::Leaf(id) => {
                    let here = *idx;
                    *idx += 1;
                    if *id == target { Some(here) } else { None }
                }
                Node::Split { a, b, .. } => walk(a, target, idx).or_else(|| walk(b, target, idx)),
            }
        }
        let mut idx = 0;
        walk(self, target, &mut idx)
    }

    /// All leaf ids in DFS-order. Used by `broadcast_write` to scope
    /// broadcast input to one tab's panes rather than every pane in every
    /// tab (cycle 112 — `Action::ToggleBroadcastAll` was originally
    /// "every pane in the whole mux", a footgun for users with several
    /// tabs since typing one char would echo into every pane everywhere;
    /// per-tab matches Terminator's `broadcast_all` and is what users
    /// actually mean when they're paralleling SSH sessions).
    pub fn leaf_ids(&self) -> Vec<u64> {
        fn walk(n: &Node, out: &mut Vec<u64>) {
            match n {
                Node::Leaf(id) => out.push(*id),
                Node::Split { a, b, .. } => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut v = Vec::new();
        walk(self, &mut v);
        v
    }

    /// Leaf id at DFS-order position `n`, or the first leaf if `n` is past
    /// the end (graceful fallback so a session pointing to a no-longer-
    /// existent pane still produces a focused tab).
    fn nth_leaf(&self, n: usize) -> u64 {
        fn walk(node: &Node, n: usize, idx: &mut usize) -> Option<u64> {
            match node {
                Node::Leaf(id) => {
                    if *idx == n {
                        return Some(*id);
                    }
                    *idx += 1;
                    None
                }
                Node::Split { a, b, .. } => walk(a, n, idx).or_else(|| walk(b, n, idx)),
            }
        }
        let mut idx = 0;
        walk(self, n, &mut idx).unwrap_or_else(|| self.first_leaf())
    }

    /// Replace the leaf `id` with a split of itself and `new_id`.
    fn split_leaf(&mut self, id: u64, new_id: u64, dir: Dir) -> bool {
        match self {
            Node::Leaf(x) if *x == id => {
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(id)),
                    b: Box::new(Node::Leaf(new_id)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.split_leaf(id, new_id, dir) || b.split_leaf(id, new_id, dir)
            }
        }
    }

    /// Remove leaf `id`; `Err(None)` means this node was the leaf (caller
    /// drops it), `Err(Some(sibling))` means replace this node with `sibling`.
    fn remove_leaf(self, id: u64) -> Result<Node, Option<Node>> {
        match self {
            Node::Leaf(x) if x == id => Err(None),
            Node::Leaf(x) => Ok(Node::Leaf(x)),
            Node::Split { dir, ratio, a, b } => match a.remove_leaf(id) {
                Err(None) => Err(Some(*b)),
                Err(Some(n)) => Ok(Node::Split {
                    dir,
                    ratio,
                    a: Box::new(n),
                    b,
                }),
                Ok(a) => match b.remove_leaf(id) {
                    Err(None) => Err(Some(a)),
                    Err(Some(n)) => Ok(Node::Split {
                        dir,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(n),
                    }),
                    Ok(b) => Ok(Node::Split {
                        dir,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                },
            },
        }
    }

    fn layout(&self, rect: Rect, out: &mut Vec<(u64, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { dir, ratio, a, b } => {
                let (x, y, w, h) = rect;
                let r = ratio.clamp(0.05, 0.95);
                match dir {
                    Dir::Horizontal => {
                        let aw = (w * r).round();
                        a.layout((x, y, aw, h), out);
                        b.layout((x + aw, y, w - aw, h), out);
                    }
                    Dir::Vertical => {
                        let ah = (h * r).round();
                        a.layout((x, y, w, ah), out);
                        b.layout((x, y + ah, w, h - ah), out);
                    }
                }
            }
        }
    }

    /// v2.20.0 (`equalize_splits`, Ghostty/Terminator parity): rebalance the
    /// whole tree so every LEAF gets equal area. Each split's ratio becomes
    /// `leaves(a) / (leaves(a) + leaves(b))` — for a chain of N panes along
    /// one axis that yields 1/N each; mixed orientations get equal areas
    /// proportionally. Returns the subtree's leaf count. Pure tree math
    /// (unit-tested); the caller follows with `resize_all` to push the new
    /// geometry into the PTYs.
    pub(crate) fn equalize(&mut self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, ratio, .. } => {
                let la = a.equalize();
                let lb = b.equalize();
                *ratio = (la as f32 / (la + lb) as f32).clamp(0.05, 0.95);
                la + lb
            }
        }
    }

    /// Adjust the ratio of the innermost split matching `dir` that contains
    /// `focus`.
    fn resize(&mut self, focus: u64, dir: Dir, delta: f32) -> bool {
        if let Node::Split {
            dir: d,
            ratio,
            a,
            b,
        } = self
        {
            if a.resize(focus, dir, delta) || b.resize(focus, dir, delta) {
                return true;
            }
            if *d == dir && (a.contains(focus) || b.contains(focus)) {
                *ratio = (*ratio + delta).clamp(0.05, 0.95);
                return true;
            }
        }
        false
    }

    /// Cycle 904 (audit): collect every split's divider seam, each tagged with
    /// the `path` (a/b descent from the root) that addresses it for mutation,
    /// its `dir`, the split's full `rect`, and the seam coordinate `pos` (x for
    /// a Horizontal split's vertical divider, y for a Vertical split's
    /// horizontal divider). Mirrors `layout`'s geometry exactly so a hit-test
    /// against these seams matches what the renderer drew. Drives mouse
    /// drag-to-resize of split dividers.
    fn dividers(&self, rect: Rect, path: &mut Vec<bool>, out: &mut Vec<SplitSeam>) {
        if let Node::Split { dir, ratio, a, b } = self {
            let (x, y, w, h) = rect;
            let r = ratio.clamp(0.05, 0.95);
            let (a_rect, b_rect, pos) = match dir {
                Dir::Horizontal => {
                    let aw = (w * r).round();
                    ((x, y, aw, h), (x + aw, y, w - aw, h), x + aw)
                }
                Dir::Vertical => {
                    let ah = (h * r).round();
                    ((x, y, w, ah), (x, y + ah, w, h - ah), y + ah)
                }
            };
            out.push(SplitSeam {
                path: path.clone(),
                dir: *dir,
                rect,
                pos,
            });
            path.push(false);
            a.dividers(a_rect, path, out);
            path.pop();
            path.push(true);
            b.dividers(b_rect, path, out);
            path.pop();
        }
    }

    /// Cycle 904: set the ratio of the split addressed by `path` (the a/b
    /// descent produced by `dividers`). Returns false if the path doesn't land
    /// on a split (stale path after a layout change). The ratio is clamped to
    /// the same [0.05, 0.95] band `layout` enforces, so a pane can't be dragged
    /// to zero width.
    fn set_ratio_at(&mut self, path: &[bool], ratio: f32) -> bool {
        match self {
            Node::Split { ratio: r, a, b, .. } => match path.split_first() {
                None => {
                    *r = ratio.clamp(0.05, 0.95);
                    true
                }
                Some((&go_b, rest)) => {
                    if go_b {
                        b.set_ratio_at(rest, ratio)
                    } else {
                        a.set_ratio_at(rest, ratio)
                    }
                }
            },
            Node::Leaf(_) => false,
        }
    }
}

/// Cycle 904 (audit): one split divider, addressable for mouse drag-to-resize.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitSeam {
    /// a/b descent from the tab root that uniquely addresses the split node.
    pub path: Vec<bool>,
    pub dir: Dir,
    /// The split's full rect — the basis for converting a cursor position into
    /// a new ratio.
    pub rect: Rect,
    /// Seam coordinate: x for a Horizontal split (vertical divider line), y for
    /// a Vertical split (horizontal divider line).
    pub pos: f32,
}

/// Cycle 904: the ratio a Horizontal/Vertical split should take so its divider
/// sits under the cursor, clamped to the same band `layout` enforces.
pub fn ratio_from_pos(rect: Rect, dir: Dir, px: f32, py: f32) -> f32 {
    let (x, y, w, h) = rect;
    let raw = match dir {
        Dir::Horizontal => {
            if w > 0.0 {
                (px - x) / w
            } else {
                0.5
            }
        }
        Dir::Vertical => {
            if h > 0.0 {
                (py - y) / h
            } else {
                0.5
            }
        }
    };
    raw.clamp(0.05, 0.95)
}

/// Cycle 904: index of the first seam within `tol` px of the cursor (along the
/// seam's perpendicular axis) and inside the split's cross-axis extent. Inner
/// (deeper) seams are pushed AFTER their ancestors by `dividers`, so a tie near
/// nested dividers resolves to the outer split — a stable, predictable pick.
pub fn seam_at(seams: &[SplitSeam], px: f32, py: f32, tol: f32) -> Option<usize> {
    seams.iter().position(|s| {
        let (x, y, w, h) = s.rect;
        match s.dir {
            // Vertical divider line at x = pos; cursor must be near it
            // horizontally and within the split's vertical span.
            Dir::Horizontal => (px - s.pos).abs() <= tol && py >= y && py <= y + h,
            // Horizontal divider line at y = pos.
            Dir::Vertical => (py - s.pos).abs() <= tol && px >= x && px <= x + w,
        }
    })
}

pub struct Tab {
    pub root: Node,
    pub focus: u64,
    /// Cycle 354 (Terminator parity, terminatorlib/notebook.py): an
    /// optional user-set title override. When `Some(s)`, the tab
    /// bar displays `s` instead of the focused pane's title.
    /// Cleared automatically when the user opens a new tab (cycle-X
    /// new-tab path) — sticky-override behavior matches Terminator.
    pub title_override: Option<String>,
    /// When true, only the focused pane is shown at full size.
    pub zoomed: bool,
    /// Cycle 246: per-tab activity state for the tab-bar dot
    /// indicator. `last_output_at` updates whenever any pane in this
    /// tab produces output. `last_seen_at` updates when this tab
    /// becomes active. The renderer compares the two to decide
    /// whether to draw the "new output in inactive tab" dot. `bell`
    /// latches a `TermEvent::Bell` from any pane in this tab until
    /// the user activates the tab. Matches the Terminator "Activity
    /// Watcher" affordance.
    pub last_output_at: Option<std::time::Instant>,
    pub last_seen_at: Option<std::time::Instant>,
    pub bell: bool,
}

/// Activity state of an *inactive* tab, used by the renderer to pick
/// the tab-bar indicator-dot color. Active tabs are always `Normal`
/// (the focused tab's "you are here" accent already says enough).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabActivity {
    /// Nothing to surface — active tab OR inactive tab with no output
    /// since the user last saw it.
    Normal,
    /// Output arrived since the user last looked at this tab. Drawn
    /// as a cyan dot — the standard "something happened" cue.
    Output,
    /// A `TermEvent::Bell` fired since the user last looked. Drawn as
    /// a yellow dot, overrides `Output` because a bell is a stronger
    /// signal (the focused program explicitly asked for attention).
    Bell,
    /// Tab had unseen output but no further bytes for ≥ the
    /// configured `tab-silence-threshold-ms`. Terminator's "Silence
    /// Watcher" affordance — useful for tail-following long jobs
    /// (`tail -f`, build watchers, network monitors) where the
    /// *absence* of recent output is the signal the user wants.
    /// Drawn as a dim chrome-gray dot to read as a state distinct
    /// from `Output` (cyan) and `Bell` (yellow).
    Silent,
}

/// Pure: classify an inactive tab's activity from its state. Active
/// tabs short-circuit to `Normal` because the focused-pane border and
/// the tab-bar accent already convey focus — adding a dot there would
/// be redundant.
///
/// `now` and `silence_threshold` drive the cycle-252 Silent variant
/// — when an inactive tab had unseen output that's been quiet for at
/// least the threshold, the indicator transitions Output → Silent.
/// Passing the wall clock in (rather than calling `Instant::now()`
/// internally) keeps the function pure and unit-testable.
pub fn classify_tab_activity(
    is_active: bool,
    bell: bool,
    last_output_at: Option<std::time::Instant>,
    last_seen_at: Option<std::time::Instant>,
    now: std::time::Instant,
    silence_threshold: std::time::Duration,
) -> TabActivity {
    if is_active {
        return TabActivity::Normal;
    }
    if bell {
        return TabActivity::Bell;
    }
    let unseen_output = match (last_output_at, last_seen_at) {
        (Some(o), Some(s)) => o > s,
        (Some(_), None) => true,
        _ => false,
    };
    if !unseen_output {
        return TabActivity::Normal;
    }
    // Unwrap-safe: `unseen_output` is true only when `last_output_at`
    // is Some.
    let last_out = last_output_at.unwrap();
    // `saturating_duration_since` so a tab whose `last_output_at` is
    // (somehow) in the future doesn't flip Silent — that'd be a
    // monotonic-clock bug, not a tab actually going quiet.
    if now.saturating_duration_since(last_out) >= silence_threshold {
        TabActivity::Silent
    } else {
        TabActivity::Output
    }
}

/// Snapshot of a tab captured at close time so `Action::UndoCloseTab`
/// can re-spawn the same program in the same directory. WezTerm /
/// browser-tab convention; closing a tab is no longer irreversible.
/// Tree topology isn't preserved — undo re-creates as a single pane
/// from the first leaf's argv+cwd (the user's complaint is "bring my
/// tab back," not "reproduce my exact split layout from N closes ago").
#[derive(Clone)]
pub struct ClosedTab {
    /// Tab index at the time of close. On undo we clamp to the
    /// current tab-count so an `undo` after several intervening
    /// `new_tab`s still lands somewhere sensible.
    pub original_index: usize,
    /// Argv of the first leaf — empty means the configured shell.
    pub argv: Vec<String>,
    /// OSC-7 cwd of the first leaf at the moment of close, or `None`
    /// if no usable cwd was reported.
    pub cwd: Option<String>,
}

/// Max closed-tab snapshots held for `Action::UndoCloseTab`. Browser-
/// standard is 8-10; we keep 10 to amortize accidental close-bursts.
const CLOSED_TAB_RING_CAP: usize = 10;

/// Cycle 678 (sub-cycle 2 of [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
/// ../../../docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)): the
/// broadcast-scope enum the design proposes. The existing
/// `mux.broadcast: bool` represents `Off | Tab`; future cycles
/// will migrate it to this richer enum so `Group(name)`
/// (scope-by-name-across-tabs) becomes expressible.
///
/// Lands the type now so the cycle-642 `Action::GroupTab` etc.
/// dispatch can be wired against the final shape ahead of the
/// refactor.
// Cycle 720 (2026-05-23): removed stale `#[allow(dead_code)]`. The
// All + Group variants are now consumed by the cycle-679/681/682
// GroupTab + GroupWindow + CreateGroup dispatch arms in app.rs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BroadcastScope {
    #[default]
    Off,
    /// Cycle-178 per-tab broadcast: every pane in the focused
    /// tab receives input. Today's `mux.broadcast = true`
    /// behavior.
    Tab,
    /// Window-wide: every pane in every tab receives input.
    All,
    /// Named group: every pane whose `Pane::group_name` matches
    /// receives input. Span across tabs is what makes named
    /// groups distinct from per-tab broadcast.
    Group(String),
}

/// Cycle 678: pure helper that computes the set of pane IDs that
/// should receive a broadcast for the given scope. Pure — takes
/// `(scope, focused_pane_id, panes_in_focused_tab, all_panes_with_groups)`
/// and returns the target list. Unit-testable.
///
/// `all_panes_with_groups` is a slice of `(pane_id, Option<&str>
/// group)` pairs covering every pane in every tab. The caller
/// is responsible for assembling it (a one-liner over
/// `self.panes.iter()`).
// Cycle 720 (2026-05-23): removed stale `#[allow(dead_code)]`.
// `compute_broadcast_targets` is the impl behind the public
// `Mux::broadcast_targets` (called from app.rs).
pub fn compute_broadcast_targets(
    scope: &BroadcastScope,
    focused_pane: u64,
    panes_in_focused_tab: &[u64],
    all_panes_with_groups: &[(u64, Option<&str>)],
) -> Vec<u64> {
    match scope {
        BroadcastScope::Off => vec![focused_pane],
        BroadcastScope::Tab => panes_in_focused_tab.to_vec(),
        BroadcastScope::All => all_panes_with_groups.iter().map(|(id, _)| *id).collect(),
        BroadcastScope::Group(name) => {
            // Every pane tagged with this group, regardless of tab — plus the
            // focused (on-screen) pane, so input is never routed AWAY from the
            // pane the user is looking at with no cue. The focused pane may not
            // be a group member (e.g. broadcasting from an ungrouped pane into a
            // named group); union it in (deduped) so the on-screen pane always
            // receives input, mirroring how Off/Tab/All already include it.
            let mut targets: Vec<u64> = all_panes_with_groups
                .iter()
                .filter(|(_, g)| g.as_deref() == Some(name.as_str()))
                .map(|(id, _)| *id)
                .collect();
            if !targets.contains(&focused_pane) {
                targets.push(focused_pane);
            }
            targets
        }
    }
}

/// C2 (multi-window): process-global pane-id allocator. Pane ids must be
/// unique across EVERY window's Mux — the agent control API (`kettle ctl
/// --pane N`), Lua hooks, and `pending_runs` all address panes by bare id,
/// and a live tab move (C5) carries its panes' ids into another window's Mux.
/// A per-Mux counter would collide the moment a second window spawned a pane.
/// Starts at 1 (id 0 is never a valid pane, matching the old per-Mux seed).
static NEXT_PANE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A tab lifted out of one Mux, panes and all, ready to be attached to
/// another (the C5 live tab move — PTYs keep running, nothing respawns).
/// Pane ids stay valid across the move because they're process-global.
pub struct DetachedTab {
    pub tab: Tab,
    pub panes: Vec<(u64, Pane)>,
}

pub struct Mux {
    pub tabs: Vec<Tab>,
    pub panes: HashMap<u64, Pane>,
    pub active: usize,
    pub search: SearchState,
    /// Cycle 679 (sub-cycle 3 of named-groups design):
    /// migrated from `bool` to `BroadcastScope`. The cycle-178
    /// per-tab broadcast = `BroadcastScope::Tab`; old "off" =
    /// `Off`. New variants: `All` (window-wide), `Group(name)`
    /// (cross-tab named group). Callers that just want a
    /// yes/no should use `Mux::is_broadcast_on()`.
    pub broadcast: BroadcastScope,
    /// Cycle 378: set when a LuaEngine subscribes at App startup.
    /// Controls whether spawn_pane attaches the output sidechannel
    /// to new PTYs (zero-cost when false: no per-PTY-read alloc).
    pub lua_output_subscribed: bool,
    /// Cycle 881: when the dev-record recorder is teeing PTY output, make the
    /// output sidechannel UNBOUNDED instead of the lossy `bounded(64)` used for
    /// Lua plugins — so a fast output burst can't silently drop chunks and put
    /// holes in the asciicast trace. Same rationale as the (already unbounded)
    /// event channel: growth only happens if the UI thread is wedged, and the
    /// App drains it every frame. False (= bounded, drop-on-full) otherwise.
    pub record_lossless: bool,
    /// Ring buffer of recently-closed tab snapshots (cycle 247).
    /// Bounded so a long-running session doesn't accumulate state.
    /// LIFO: `pop_back` returns the most-recently-closed tab.
    pub closed_tabs: std::collections::VecDeque<ClosedTab>,
}

impl Mux {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            panes: HashMap::new(),
            active: 0,
            search: SearchState::default(),
            broadcast: BroadcastScope::Off,
            lua_output_subscribed: false,
            record_lossless: false,
            closed_tabs: std::collections::VecDeque::with_capacity(CLOSED_TAB_RING_CAP),
        }
    }

    /// Mark the active tab as just-seen by the user — clears its bell
    /// flag and updates `last_seen_at` so `classify_tab_activity` no
    /// longer reports `Output` / `Bell` on it. Call after any
    /// `self.active = ...` change so the tab the user just switched
    /// to drops its indicator immediately. Cycle 246.
    pub fn touch_active_tab_seen(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.last_seen_at = Some(std::time::Instant::now());
            tab.bell = false;
        }
    }

    /// Find the tab containing `pane_id` and record output activity on
    /// it. Skipped for the currently-active tab (the user is looking at
    /// it; surfacing a dot would be visual noise). Called from the
    /// chrome layer on every pane redraw — see `App::drain_events`.
    pub fn touch_tab_output(&mut self, pane_id: u64) {
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == active {
                continue;
            }
            if tab.root.contains(pane_id) {
                tab.last_output_at = Some(std::time::Instant::now());
                return;
            }
        }
    }

    /// Latch a `TermEvent::Bell` from `pane_id` onto its containing
    /// tab so the indicator survives until the user activates the
    /// tab. Skipped for the active tab (the visual-bell flash already
    /// surfaces it there).
    pub fn touch_tab_bell(&mut self, pane_id: u64) {
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i == active {
                continue;
            }
            if tab.root.contains(pane_id) {
                tab.bell = true;
                return;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_pane(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        cwd: Option<&str>,
        argv: &[String],
    ) -> Result<u64> {
        // Cycle 787 (audit B1, investigated + intentionally kept unbounded):
        // a bounded channel here is UNSAFE in both variants. `TermEvent` is
        // `alacritty_terminal::event::Event`, which carries one-shot events that
        // must never be dropped — `Exit` (child gone → pane close; dropping it
        // zombies the pane) and `PtyWrite(..)` (protocol replies written back to
        // the PTY, e.g. cursor-position / device-attribute answers; dropping one
        // hangs the querying program). So `bounded` + `try_send` (drop-on-full)
        // is out. And `bounded` + blocking `send` deadlocks: the sender
        // (`EventProxy::send_event`) runs inside `processor.advance(&mut *t, ..)`
        // while the reader holds `term.lock()` (term.rs); a full channel would
        // block the reader *with the lock held*, and the UI thread — which locks
        // the same `term` to render — would block forever waiting for it. The
        // channel is drained every UI iteration via `try_recv` and the waker
        // fires per event, so it does not grow unbounded in normal operation;
        // sustained growth only happens if the UI thread is already wedged, at
        // which point OOM is a symptom, not the disease. Keep it unbounded.
        let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) = crossbeam_channel::unbounded();
        // Cycle 378 (Terminator plugin parity, plugin sub-cycle 3):
        // optional output sidechannel for LuaEvent::Output emission.
        // The Mux's output_tx is set when a LuaEngine subscribes
        // (App configures it post-construction); None when no
        // plugin is listening so the alloc-per-PTY-read is skipped.
        // Cycle 881: lossless (unbounded) when the recorder is teeing output so
        // the asciicast trace can't lose chunks under a fast burst; the lossy
        // `bounded(64)` (drop-on-full) is kept for the Lua-plugin case where
        // dropping under back-pressure is acceptable.
        let (out_tx, out_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = if self.record_lossless {
            crossbeam_channel::unbounded()
        } else {
            crossbeam_channel::bounded(64)
        };
        let output_tx = if self.lua_output_subscribed {
            Some(out_tx)
        } else {
            None
        };
        // Cycle 343 Terminator parity: route through new_with_env so
        // cfg.term / cfg.colorterm / cfg.login_shell take effect at
        // PTY spawn. The legacy `Terminal::new` shim still exists
        // for non-Mux callers (currently none in-tree).
        let term = Terminal::new_with_env_and_output(
            argv,
            cwd,
            cfg.scrollback,
            cfg.scrollback_bytes,
            cols.max(1),
            rows.max(1),
            cw,
            ch,
            cfg.cursor_blink,
            engine_cursor_shape(cfg.cursor_style),
            Some(cfg.word_delimiters.as_str()),
            &cfg.term,
            &cfg.colorterm,
            &cfg.env,
            cfg.login_shell,
            cfg.shell_integration,
            tx,
            waker,
            output_tx,
        )?;
        let id = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let initial_title = initial_pane_title(argv);
        // Only generic-shell panes ("kettle" seed) are eligible for conhost
        // startup-title suppression + cwd labelling; a `-e htop`/`ssh` pane keeps
        // its real seed and is never treated as a placeholder.
        let title_is_placeholder = initial_title == "kettle";
        let output_rx = if self.lua_output_subscribed {
            Some(out_rx)
        } else {
            None
        };
        self.panes.insert(
            id,
            Pane {
                term,
                rx,
                output_rx,
                title: initial_title,
                title_is_placeholder,
                group_name: None,
                closed: false,
                held: false,
                last_history: None,
                argv: argv.to_vec(),
                remote_context: None,
                agent_attached: false,
                read_only: false,
            },
        );
        Ok(id)
    }

    fn snap(&self, n: &Node) -> SNode {
        match n {
            Node::Leaf(id) => SNode::Leaf {
                cwd: self.panes.get(id).and_then(|p| p.term.current_dir()),
                cmd: self
                    .panes
                    .get(id)
                    .map(|p| p.term.argv.clone())
                    .unwrap_or_default(),
                // C7 (audit v2.32.0): persist broadcast-group membership so a
                // restored pane rejoins its group instead of silently losing it.
                group: self.panes.get(id).and_then(|p| p.group_name.clone()),
            },
            Node::Split { dir, ratio, a, b } => SNode::Split {
                vertical: *dir == Dir::Vertical,
                ratio: *ratio,
                a: Box::new(self.snap(a)),
                b: Box::new(self.snap(b)),
            },
        }
    }

    /// Cycle 397 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 2): serialize ONE tab (by index) to the same
    /// STab wire format that session.json uses. Returns None when
    /// the index is out-of-range.
    // C5: its production callers were the serialize-and-respawn handoff
    // senders, retired in favor of the live in-process tab move. Kept (tests
    // pin the contract) — the deprecated `--tab-handoff` receive path still
    // consumes the wire format for one release, and C7's per-window session
    // serialization is the natural next consumer.
    #[allow(dead_code)]
    pub fn serialize_tab(&self, idx: usize) -> Option<STab> {
        let t = self.tabs.get(idx)?;
        Some(STab {
            root: self.snap(&t.root),
            focus: t.root.leaf_index_of(t.focus).unwrap_or(0),
            title_override: t.title_override.clone(),
            zoomed: t.zoomed,
        })
    }

    /// Capture the full tab/split tree + per-pane cwd.
    pub fn snapshot(&self) -> Session {
        Session {
            tabs: self
                .tabs
                .iter()
                .map(|t| STab {
                    root: self.snap(&t.root),
                    // DFS-order index of the focused leaf so restore can
                    // recreate the focus on the new tree (pane ids are
                    // reallocated across restores, so the id itself isn't
                    // portable). `0` means "first leaf" — same as the
                    // pre-cycle behavior, which is what missing-field
                    // restores fall back to via #[serde(default)].
                    focus: t.root.leaf_index_of(t.focus).unwrap_or(0),
                    title_override: t.title_override.clone(),
                    zoomed: t.zoomed,
                })
                .collect(),
            active: self.active,
            // Filled in by App::save_session (it owns the active theme).
            theme: None,
            // C7: snapshot() is the ONE-window serializer; App::save_session
            // assembles the multi-window `windows` vec from per-window calls.
            windows: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_node(
        &mut self,
        n: &SNode,
        cfg: &Config,
        cw: u16,
        ch: u16,
        mk: &dyn Fn() -> Waker,
        // Cycle 893 (audit): every pane id spawned while building this
        // subtree is appended here so the caller can reap them if a LATER
        // sibling fails. Without it, a split whose first child spawned but
        // whose second child failed left the first child's PTY + child
        // process orphaned in `self.panes` (attached to no tab) — a leaked
        // process per partially-restored split.
        spawned: &mut Vec<u64>,
    ) -> Result<Node> {
        match n {
            SNode::Leaf { cwd, cmd, group } => {
                let argv = if cmd.is_empty() {
                    shell_argv(cfg)
                } else {
                    cmd.clone()
                };
                let id = self.spawn_pane(cfg, 80, 24, cw, ch, mk(), cwd.as_deref(), &argv)?;
                spawned.push(id);
                // C7 (audit v2.32.0): rejoin the saved broadcast group.
                if let Some(p) = self.panes.get_mut(&id) {
                    p.group_name = group.clone();
                }
                Ok(Node::Leaf(id))
            }
            SNode::Split {
                vertical,
                ratio,
                a,
                b,
            } => {
                let a = self.build_node(a, cfg, cw, ch, mk, spawned)?;
                let b = self.build_node(b, cfg, cw, ch, mk, spawned)?;
                Ok(Node::Split {
                    dir: if *vertical {
                        Dir::Vertical
                    } else {
                        Dir::Horizontal
                    },
                    ratio: *ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                })
            }
        }
    }

    /// Rebuild tabs/splits from a saved session, spawning shells in their
    /// recorded directories. Returns whether anything was restored.
    pub fn restore(
        &mut self,
        s: &Session,
        cfg: &Config,
        cw: u16,
        ch: u16,
        mk: &dyn Fn() -> Waker,
    ) -> bool {
        // Cycle 863 (audit): bound the total PTY fan-out. The 16 MiB file-size
        // cap is a weak proxy — a small session.json of minimal flat leaves
        // (~30 bytes each) could ask to fork hundreds of thousands of shells on
        // launch, hanging/OOMing the machine. Stop restoring further tabs once
        // the running pane count would exceed the cap (256 panes is far past any
        // real layout) and surface why.
        const MAX_RESTORE_PANES: usize = 256;
        let mut spawned = 0usize;
        for (i, st) in s.tabs.iter().enumerate() {
            let tab_leaves = st.root.leaf_count();
            if spawned + tab_leaves > MAX_RESTORE_PANES {
                log::warn!(
                    "session restore: stopping at tab {i} — would exceed the \
                     {MAX_RESTORE_PANES}-pane restore cap (session may be corrupt or crafted)"
                );
                break;
            }
            spawned += tab_leaves;
            // Cycle 893 (audit): track every pane id this tab's tree spawns so
            // a partial failure (e.g. the 2nd pane of a split fails to fork)
            // can reap the panes already created for the same tree instead of
            // leaking their PTYs + child processes.
            let mut tab_pane_ids: Vec<u64> = Vec::new();
            match self.build_node(&st.root, cfg, cw, ch, mk, &mut tab_pane_ids) {
                Ok(root) => {
                    // Restore the focused leaf at its DFS index (saved
                    // by `snapshot`). `nth_leaf` falls back to the
                    // first leaf if the index is past the end, which
                    // keeps trimmed-tree sessions sane.
                    let focus = root.nth_leaf(st.focus);
                    self.tabs.push(Tab {
                        root,
                        focus,
                        // C7 (audit v2.32.0): restore the saved tab title
                        // override + zoom state (was hardcoded to defaults).
                        title_override: st.title_override.clone(),
                        zoomed: st.zoomed,
                        last_output_at: None,
                        last_seen_at: None,
                        bell: false,
                    });
                }
                Err(e) => {
                    // Cycle 893 (audit): reap any panes the partially-built
                    // tree already spawned. A split's first child can fork
                    // fine and the second fail (cwd gone, fork under quota);
                    // those orphans would otherwise sit in `self.panes`
                    // attached to no tab, leaking a PTY + child process each.
                    for id in &tab_pane_ids {
                        self.panes.remove(id);
                    }
                    // Don't fail the whole restore — a single broken
                    // tab (e.g. saved cwd no longer exists, PTY
                    // allocation under quota) shouldn't sink the
                    // others. But surface it in the log so a user
                    // wondering "where did my session go?" can see
                    // the cause under `RUST_LOG=warn` (the default
                    // filter). Pre-fix this was a silent skip — the
                    // user just saw fewer tabs than they remembered.
                    log::warn!(
                        "session restore: tab {i} failed to rebuild and was skipped \
                         ({} orphaned pane(s) reaped): {e}",
                        tab_pane_ids.len()
                    );
                }
            }
        }
        self.active = s.active.min(self.tabs.len().saturating_sub(1));
        !self.tabs.is_empty()
    }

    pub fn new_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        let argv = shell_argv(cfg);
        let cwd = self.focused_cwd();
        self.new_tab_with(cfg, cols, rows, cw, ch, waker, &argv, cwd.as_deref())
    }

    /// Open a new tab running an explicit `argv` in `cwd` (CLI `-e`/`-d`);
    /// an empty `argv` means the configured shell.
    #[allow(clippy::too_many_arguments)]
    pub fn new_tab_with(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<()> {
        let id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd, argv)?;
        let new_tab = Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        // Cycle 349 (Terminator parity, terminatorlib/config.py:97
        // `new_tab_after_current_tab`): when true, insert the new
        // tab right AFTER the active one (vs at the end of the
        // tabs list). The new tab becomes active either way.
        if cfg.new_tab_after_current_tab && self.active + 1 < self.tabs.len() {
            self.tabs.insert(self.active + 1, new_tab);
            self.active += 1;
        } else if cfg.new_tab_after_current_tab && self.active + 1 == self.tabs.len() {
            // Already at the end — same as appending.
            self.tabs.push(new_tab);
            self.active = self.tabs.len() - 1;
        } else {
            self.tabs.push(new_tab);
            self.active = self.tabs.len() - 1;
        }
        Ok(())
    }

    /// Cycle 912 (audit): new tab running an explicit `argv` + cwd, with the same
    /// WSL-aware `--cd` dir translation `split_with` applies. The new-tab ▾
    /// dropdown's WSL entry routed through `new_tab_with` directly (a raw spawn),
    /// so a WSL launcher's Linux cwd failed the Windows `is_dir` gate and the new
    /// tab fell back to the home dir — the cycle-887 regression class, where only
    /// splits/duplicates were wired through `launch_cwd`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_tab_with_launch(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        let (argv, cwd) = launch_cwd(argv, raw_cwd);
        self.new_tab_with(cfg, cols, rows, cw, ch, waker, &argv, cwd.as_deref())
    }

    /// Open a new tab running `ssh -t <target>` (SSH multiplexing).
    #[allow(clippy::too_many_arguments)]
    pub fn new_ssh_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        target: &str,
    ) -> Result<()> {
        let argv = vec!["ssh".to_string(), "-t".to_string(), target.to_string()];
        // `spawn_pane` sees argv[0] == "ssh" and seeds the pane title to
        // `ssh <target>` so the tab is distinguishable from a regular
        // shell tab while the connection is establishing. The OSC 2
        // handler overwrites this when the remote shell sets a title.
        let id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, None, &argv)?;
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// Cycle 886/887: the `(argv, spawn-cwd)` that reproduces the focused pane
    /// in a new pane/tab — clones its launch command and inherits its cwd, so a
    /// pane launched as WSL / ssh / a specific shell duplicates into the same.
    /// A default-shell pane's argv IS the configured shell, so the common case
    /// is unchanged; an empty argv (legacy "≡ configured shell") falls back to
    /// the shell.
    ///
    /// WSL-aware dir: WSL reports a Linux cwd (`/mnt/c/...` or a native path) a
    /// Windows spawn can't `cd` into, so for a `wsl` launcher the dir is carried
    /// via `wsl --cd <dir>` (which accepts both Windows and Linux paths) and the
    /// Windows spawn cwd is left unset — otherwise the new pane would fall back
    /// to the home dir (the bug the user hit: split a WSL pane → pwsh in ~).
    fn clone_focused_launch(&self, cfg: &Config) -> (Vec<String>, Option<String>) {
        let (mut argv, raw_cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), pane.term.current_dir()),
            None => (Vec::new(), None),
        };
        if argv.is_empty() {
            argv = shell_argv(cfg);
        }
        launch_cwd(argv, raw_cwd)
    }

    /// Resolve the command a split should spawn when no interactive foreground
    /// shell was detected by the process scanner. Duplicate actions need exact
    /// launch cloning, but Split should stay a "give me another usable prompt"
    /// action. Direct agent/editor launches (`kettle -e codex`, `-e nvim`, etc.)
    /// often have transient helper shells underneath them; if we clone the
    /// direct launch argv, the new pane can immediately exit or open another
    /// full-screen app instead of becoming a prompt. Use the configured shell in
    /// the focused cwd for those direct launchers, while preserving exact cloning
    /// for shells, WSL, SSH, and ordinary explicit commands.
    fn split_focused_launch(&self, cfg: &Config) -> (Vec<String>, Option<String>) {
        let (mut argv, raw_cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), pane.term.current_dir()),
            None => (Vec::new(), None),
        };
        if argv.is_empty() || direct_launch_splits_to_shell(&argv) {
            argv = shell_argv(cfg);
        }
        launch_cwd(argv, raw_cwd)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        if self.tabs.is_empty() {
            return self.new_tab(cfg, cols, rows, cw, ch, waker);
        }
        // Cycle 886/887 + v2.33.1: clone shell-like launches, but keep direct
        // agent/editor panes split-friendly by falling back to a shell in the
        // focused cwd. See `split_focused_launch`.
        let (argv, cwd) = self.split_focused_launch(cfg);
        let new_id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // Cycle 917 (#2 hardening): the graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    /// Cycle 888: split running an explicit `argv` + cwd — e.g. a shell detected
    /// running inside the focused pane (`pwsh → wsl`). Mirrors `split` but with a
    /// caller-supplied command instead of cloning the pane's launch argv; the
    /// WSL `--cd` dir handling is applied via `launch_cwd`.
    #[allow(clippy::too_many_arguments)]
    pub fn split_with(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
        argv: Vec<String>,
        raw_cwd: Option<String>,
    ) -> Result<()> {
        let (argv, cwd) = launch_cwd(argv, raw_cwd);
        if self.tabs.is_empty() {
            return self.new_tab_with(cfg, cols, rows, cw, ch, waker, &argv, cwd.as_deref());
        }
        let new_id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // Cycle 917 (#2 hardening): the graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    pub fn layout(&self, tab: usize, area: Rect) -> Vec<(u64, Rect)> {
        let mut v = Vec::new();
        if let Some(t) = self.tabs.get(tab) {
            if t.zoomed {
                // Zoomed: only the focused pane, full area.
                v.push((t.focus, area));
            } else {
                t.root.layout(area, &mut v);
            }
        }
        v
    }

    /// Cycle 904 (audit): the divider seams of `tab` laid out over `area`,
    /// matching `layout`'s geometry. Empty when the tab is zoomed (one pane, no
    /// dividers) — so mouse drag-to-resize is inert in zoom, as it should be.
    pub fn split_seams(&self, tab: usize, area: Rect) -> Vec<SplitSeam> {
        let mut out = Vec::new();
        if let Some(t) = self.tabs.get(tab)
            && !t.zoomed
        {
            let mut path = Vec::new();
            t.root.dividers(area, &mut path, &mut out);
        }
        out
    }

    /// Cycle 904: set the ratio of the split addressed by `path` in `tab`.
    /// Returns whether a split was found (false on a stale path). The ratio is
    /// clamped to layout's [0.05, 0.95] band.
    pub fn set_split_ratio(&mut self, tab: usize, path: &[bool], ratio: f32) -> bool {
        self.tabs
            .get_mut(tab)
            .map(|t| t.root.set_ratio_at(path, ratio))
            .unwrap_or(false)
    }

    /// Toggle zoom (maximize the focused pane) for the active tab.
    pub fn toggle_zoom(&mut self) {
        let a = self.active;
        if let Some(t) = self.tabs.get_mut(a) {
            t.zoomed = !t.zoomed;
        }
    }

    /// Cycle 693. Whether the active tab is currently zoomed (used by
    /// `Action::ScaledZoom` to decide whether it's the enter-zoom path
    /// — which scales the font up — or the leave-zoom path — which
    /// restores the saved size).
    pub fn is_zoomed(&self) -> bool {
        self.tabs
            .get(self.active)
            .map(|t| t.zoomed)
            .unwrap_or(false)
    }

    pub fn active_focus(&self) -> Option<u64> {
        self.tabs.get(self.active).map(|t| t.focus)
    }

    /// The focused pane's current directory (reported via OSC 7), used so a
    /// new tab/split opens where you are — like WezTerm/iTerm/kitty. A
    /// since-deleted directory falls back to the default (handled by
    /// [`usable_cwd`]).
    fn focused_cwd(&self) -> Option<String> {
        let id = self.active_focus()?;
        usable_cwd(self.panes.get(&id).and_then(|p| p.term.current_dir()))
    }

    pub fn focused(&mut self) -> Option<&mut Pane> {
        let id = self.tabs.get(self.active)?.focus;
        self.panes.get_mut(&id)
    }

    /// Cycle 347 (Terminator parity, terminatorlib/terminal.py:key_rotate_cw):
    /// rotate the focused leaf's parent split by flipping its direction
    /// (Horizontal ↔ Vertical) and optionally swapping its children.
    /// `clockwise = true` matches Terminator's rotate_cw (vertical→
    /// horizontal-with-swap, horizontal→vertical-no-swap); `false`
    /// is the inverse.
    ///
    /// No-op when the focused leaf has no parent split (i.e., the
    /// tab has a single pane).
    pub fn rotate_focused_split(&mut self, clockwise: bool) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let focus = tab.focus;
        rotate_node(&mut tab.root, focus, clockwise)
    }

    /// Cycle 345: 0-based index of the focused pane within its tab's
    /// in-order traversal of the binary split tree. Used by
    /// `InsertPaneNumber` + `InsertPanePadded` to send the pane index
    /// to the PTY. Returns None when no tab exists.
    pub fn focused_pane_index_in_tab(&self) -> Option<usize> {
        let tab = self.tabs.get(self.active)?;
        let focus = tab.focus;
        fn walk(node: &Node, target: u64, idx: &mut usize) -> bool {
            match node {
                Node::Leaf(id) => {
                    if *id == target {
                        return true;
                    }
                    *idx += 1;
                    false
                }
                Node::Split { a, b, .. } => walk(a, target, idx) || walk(b, target, idx),
            }
        }
        let mut idx = 0;
        if walk(&tab.root, focus, &mut idx) {
            Some(idx)
        } else {
            None
        }
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
            self.touch_active_tab_seen();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
            self.touch_active_tab_seen();
        }
    }

    /// Move focus to the pane immediately **adjacent** in a direction, the way
    /// tmux / Terminator do: among panes that border the focused pane on the
    /// pressed side AND overlap it on the perpendicular axis, pick the smallest
    /// primary-axis gap, tie-broken by perpendicular center proximity.
    ///
    /// Cycle 917 (#1, user-reported on native Ubuntu): the old rule ranked
    /// candidates purely by Euclidean distance between pane **centers**, gated
    /// only by "candidate center is on the requested side". In a nested layout a
    /// **diagonal** pane whose center happened to be closer than a directly
    /// bordering pane's center would win — focus "jumped to a diagonal pane" and
    /// "skipped the adjacent one" (and a Right press could even select an
    /// up-and-to-the-right pane whose center merely had a larger x). Comparing
    /// pane **edges** with a required perpendicular overlap fixes both: a pane
    /// that only shares a corner (zero overlap) is never a neighbor.
    ///
    /// No-op when nothing borders the focused pane in that direction. Zoomed
    /// tabs no-op implicitly: `layout` returns only the focused pane while
    /// zoomed, so the candidate loop is empty.
    pub fn focus_dir(&mut self, area: Rect, dx: i32, dy: i32) {
        // `layout` rounds split seams with `.round()`, so a shared border between
        // adjacent panes can drift by up to ~1px; admit that slack on the side
        // test and clamp a tiny negative gap to 0.
        const EPS: f32 = 1.0;
        let a = self.active;
        let rects = self.layout(a, area);
        let Some(tab) = self.tabs.get_mut(a) else {
            return;
        };
        let Some(&(_, (fx, fy, fw, fh))) = rects.iter().find(|(id, _)| *id == tab.focus) else {
            return;
        };
        let (fl, fr, ft, fb) = (fx, fx + fw, fy, fy + fh);
        let (fcx, fcy) = (fx + fw / 2.0, fy + fh / 2.0);

        // best = (primary-axis gap, perpendicular center distance, id);
        // smaller gap wins, ties broken by smaller perpendicular distance.
        let mut best: Option<(f32, f32, u64)> = None;
        for (id, (x, y, w, h)) in &rects {
            if *id == tab.focus {
                continue;
            }
            let (l, r, t, b) = (*x, *x + *w, *y, *y + *h);
            let (cx, cy) = (*x + *w / 2.0, *y + *h / 2.0);

            let (gap, perp) = if dx < 0 {
                if r > fl + EPS {
                    continue; // must lie to the LEFT (its right edge at/before our left)
                }
                if fb.min(b) - ft.max(t) <= 0.0 {
                    continue; // no vertical overlap → diagonal, not a neighbor
                }
                ((fl - r).max(0.0), (cy - fcy).abs())
            } else if dx > 0 {
                if l < fr - EPS {
                    continue;
                }
                if fb.min(b) - ft.max(t) <= 0.0 {
                    continue;
                }
                ((l - fr).max(0.0), (cy - fcy).abs())
            } else if dy < 0 {
                if b > ft + EPS {
                    continue; // must lie ABOVE (its bottom edge at/before our top)
                }
                if fr.min(r) - fl.max(l) <= 0.0 {
                    continue; // no horizontal overlap
                }
                ((ft - b).max(0.0), (cx - fcx).abs())
            } else {
                if t < fb - EPS {
                    continue;
                }
                if fr.min(r) - fl.max(l) <= 0.0 {
                    continue;
                }
                ((t - fb).max(0.0), (cx - fcx).abs())
            };

            let better = match best {
                None => true,
                // A small slack keeps two real neighbors whose gaps differ only
                // by rounding in the same tier so the perpendicular tie-break
                // (closest to the focused pane's cross-axis center) decides.
                Some((bg, bp, _)) => gap < bg - 1e-3 || ((gap - bg).abs() <= 1e-3 && perp < bp),
            };
            if better {
                best = Some((gap, perp, *id));
            }
        }
        if let Some((_, _, id)) = best {
            tab.focus = id;
        }
    }

    pub fn focus_cycle(&mut self, area: Rect, forward: bool) {
        let a = self.active;
        let rects = self.layout(a, area);
        if let Some(tab) = self.tabs.get_mut(a)
            && let Some(pos) = rects.iter().position(|(id, _)| *id == tab.focus)
        {
            let n = rects.len();
            let next = if forward {
                (pos + 1) % n
            } else {
                (pos + n - 1) % n
            };
            tab.focus = rects[next].0;
        }
    }

    pub fn resize_focus(&mut self, dir: Dir, delta: f32) {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            let f = tab.focus;
            tab.root.resize(f, dir, delta);
        }
    }

    /// Swap the active tab with its neighbor `delta` positions away.
    /// `delta > 0` moves the tab right, `delta < 0` moves it left. Clamps
    /// at the edges (no wrap, matching iTerm2 / Ghostty / WezTerm — wrap
    /// would have the tab bar lurch across the bar on every press).
    /// Returns `true` if the tab actually moved.
    pub fn move_active_tab(&mut self, delta: i32) -> bool {
        let n = self.tabs.len();
        if n < 2 || delta == 0 {
            return false;
        }
        let from = self.active as i32;
        let to = (from + delta).clamp(0, n as i32 - 1) as usize;
        if to == self.active {
            return false;
        }
        self.tabs.swap(self.active, to);
        self.active = to;
        true
    }

    /// Focus whichever pane contains the pixel `(px, py)`.
    pub fn focus_at(&mut self, area: Rect, px: f32, py: f32) {
        let a = self.active;
        let rects = self.layout(a, area);
        if let Some(tab) = self.tabs.get_mut(a) {
            for (id, (x, y, w, h)) in rects {
                if px >= x && px < x + w && py >= y && py < y + h {
                    tab.focus = id;
                    break;
                }
            }
        }
    }

    /// Close the focused pane. Returns true if no tabs remain.
    ///
    /// `Node::remove_leaf` returns three distinct shapes that need three
    /// different responses; the previous `match Err(_)` arm conflated two
    /// of them and closed the whole tab when only a sibling-promote was
    /// needed:
    ///
    /// - `Ok(n)` — the leaf was nested deep; tree was restructured around
    ///   it. Replace the root with `n` and keep the tab.
    /// - `Err(Some(n))` — the focused leaf was directly under the root
    ///   `Split`; the sibling `n` is now the new root. Keep the tab —
    ///   `Ctrl+Shift+E` then `Ctrl+Shift+W` should close the pane, not
    ///   the whole tab.
    /// - `Err(None)` — the focused leaf was the only one in the tab
    ///   (single-pane tab); the tab is now empty and should close.
    pub fn close_focused(&mut self) -> bool {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            let focus = tab.focus;
            // Cycle 602: pick the post-close focus BEFORE removing the leaf
            // so we know which sibling subtree to promote. Pre-cycle-602
            // this was `tab.root.first_leaf()` POST-remove, which always
            // jumped to the leftmost leaf of the whole tab — a regression
            // the user described as "closing a pane sets my cursor back
            // to my first focused terminal" (the leftmost = first split).
            // `neighbor_of` walks the tree and returns the first leaf of
            // the closed pane's sibling subtree, matching tmux/wezterm/
            // kitty's neighbor-promotion semantics.
            let neighbor = tab.root.neighbor_of(focus);
            let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
            match root.remove_leaf(focus) {
                Ok(n) | Err(Some(n)) => {
                    tab.root = n;
                    // Cycle 917 (#2 hardening): only repair focus when it's no
                    // longer a leaf in the collapsed tree — the same guard
                    // `reap_tabs` already has (mux.rs ~1809). close_focused always
                    // removes the focused leaf so this normally fires; matching the
                    // two close paths keeps focus on a valid leaf if that ever
                    // changes. `neighbor` is None only on a single-Leaf tree
                    // (handled by Err(None) below), so first_leaf is the safe
                    // fallback against a stale focus pointer.
                    if !tab.root.contains(tab.focus) {
                        tab.focus = neighbor.unwrap_or_else(|| tab.root.first_leaf());
                    }
                    self.panes.remove(&focus);
                }
                Err(None) => {
                    self.panes.remove(&focus);
                    self.tabs.remove(a);
                    if self.active >= self.tabs.len() && self.active > 0 {
                        self.active -= 1;
                    }
                }
            }
        }
        self.tabs.is_empty()
    }

    pub fn close_tab(&mut self) -> bool {
        let a = self.active;
        self.close_tab_at(a)
    }

    /// Cycle 398 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 4): extract a tab from the tabs list WITHOUT
    /// dropping its panes' PTYs. Used by the cross-process tab
    /// handoff path (sub-cycle 7): the source process extracts
    /// the tab → sends the serialized state + PTY fds via
    /// SCM_RIGHTS to the target process → target reconstructs
    /// the tab.
    ///
    /// Returns the extracted Tab struct + the focused pane id;
    /// the Pane structs themselves stay in self.panes (extract
    /// only touches tabs vec). The caller is responsible for
    /// transferring or dropping those Pane refs.
    ///
    /// Returns None for out-of-range idx.
    ///
    /// Cycle 720 (2026-05-23): the `#[allow(dead_code)]` covered
    /// the period before the cycle-411 SCM_RIGHTS IPC actually
    /// landed. Today this is exercised by `mux::tests` round-trip
    /// drift guards (extract→insert restores the tab state) — the
    /// IPC integration ships under a feature gate the binary
    /// activates via `--tab-handoff-fd`. Production consumes
    /// `serialize_tab` directly; this helper stays available for
    /// the upcoming live-PTY adoption work.
    #[allow(dead_code)]
    pub fn extract_tab(&mut self, idx: usize) -> Option<Tab> {
        if idx >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(idx);
        // Keep `active` valid + consistent with close_tab_at/reap_tabs: shift
        // left only when a tab strictly BEFORE active was removed; when the
        // active tab itself is removed the right neighbor slides into the slot
        // so focus moves RIGHT (active stays put), clamping if it ran off the
        // end (removing the last tab).
        if self.active > idx {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        Some(tab)
    }

    /// Cycle 398 (companion to extract_tab): insert a Tab into
    /// the tabs vec at the given index. Used by the cross-process
    /// receive path (sub-cycle 8) when an incoming handoff lands.
    /// `at` clamps to [0, tabs.len()].
    #[allow(dead_code)]
    pub fn insert_tab(&mut self, at: usize, tab: Tab) {
        let pos = at.min(self.tabs.len());
        self.tabs.insert(pos, tab);
        // Make the inserted tab active so the user sees the
        // transferred work immediately.
        self.active = pos;
    }

    /// C2 (multi-window): lift the tab at `idx` out of this Mux, LIVE —
    /// the Tab struct plus its `Pane`s (PTYs, reader threads, scrollback,
    /// everything) leave together, untouched. The C5 in-process tab move
    /// feeds the result straight into another window's `attach_tab`.
    ///
    /// Composition contract: `extract_tab` handles the tabs-vec removal and
    /// the active-index fixup (shift-left / clamp, drift-guarded by
    /// `extract_and_insert_tab_roundtrip`); this adds the pane transfer.
    /// Unlike `close_tab_at`, nothing is pushed to `closed_tabs` — the tab
    /// isn't closing, it's moving. Returns `None` for an out-of-range idx.
    pub fn detach_tab(&mut self, idx: usize) -> Option<DetachedTab> {
        let tab = self.extract_tab(idx)?;
        let mut ids = Vec::new();
        collect_ids(&tab.root, &mut ids);
        let panes = ids
            .into_iter()
            .filter_map(|id| self.panes.remove(&id).map(|p| (id, p)))
            .collect();
        // The user lands on whichever tab slid into focus; mark it seen so
        // its activity dot clears (same as every other tab-switch path).
        self.touch_active_tab_seen();
        Some(DetachedTab { tab, panes })
    }

    /// C2 (multi-window): attach a detached tab (panes and all) to this Mux
    /// at `at` (clamped; `None` = append). The inserted tab becomes active —
    /// `insert_tab` semantics — and is marked seen. Returns the index it
    /// landed at. Pane ids can't collide: they're process-global
    /// (`NEXT_PANE_ID`), and a detached tab's ids left their source map.
    pub fn attach_tab(&mut self, dt: DetachedTab, at: Option<usize>) -> usize {
        for (id, p) in dt.panes {
            debug_assert!(
                !self.panes.contains_key(&id),
                "pane id {id} already present in target Mux (global-id invariant broken)"
            );
            // Release builds must SURVIVE an id collision, not silently corrupt:
            // a blind `insert` would overwrite the resident pane and LEAK it (its
            // PTY + child process would dangle, untracked, until the OS reaps
            // them). The global `NEXT_PANE_ID` allocator makes a collision a bug,
            // but if one ever slips through (a stale detached tab re-attached
            // twice, a future per-Mux regression), log it and DROP the displaced
            // pane so its PTY/child end cleanly instead of leaking. The
            // `debug_assert!` above still trips the invariant in test builds.
            if let Some(old) = self.panes.insert(id, p) {
                log::error!(
                    "attach_tab: pane id {id} collided with an existing pane in the \
                     target Mux (global-id invariant broken); dropping the displaced \
                     pane to end its PTY/child"
                );
                drop(old);
            }
        }
        let pos = at.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.insert_tab(pos, dt.tab);
        self.touch_active_tab_seen();
        pos
    }

    /// Close the entire window: drop every pane in every tab. The caller
    /// (the chrome layer) then exits the event loop because `tabs` is
    /// empty. Distinct from `close_tab` which only closes the focused
    /// tab — cycle 113 split them apart so the keybinds (`close_tab`
    /// vs `close_window`) finally do different things. Returns true
    /// (kept for parity with `close_tab` / `close_tab_at`; the chrome
    /// callers use it as "exit now").
    pub fn close_window(&mut self) -> bool {
        self.panes.clear();
        self.tabs.clear();
        self.active = 0;
        true
    }

    /// Close the tab at `idx` (all its panes). Returns true if no tabs remain.
    pub fn close_tab_at(&mut self, idx: usize) -> bool {
        if idx < self.tabs.len() {
            // Cycle 247: snapshot the first leaf's argv+cwd before
            // dropping the tab so `Action::UndoCloseTab` can bring it
            // back. The ring is LIFO-bounded; closing tabs faster than
            // we undo evicts the oldest.
            let first_leaf = self.tabs[idx].root.first_leaf();
            if let Some(pane) = self.panes.get(&first_leaf) {
                let snap = ClosedTab {
                    original_index: idx,
                    argv: pane.argv.clone(),
                    cwd: usable_cwd(pane.term.current_dir()),
                };
                if self.closed_tabs.len() >= CLOSED_TAB_RING_CAP {
                    self.closed_tabs.pop_front();
                }
                self.closed_tabs.push_back(snap);
            }
            let mut ids = Vec::new();
            collect_ids(&self.tabs[idx].root, &mut ids);
            for id in ids {
                self.panes.remove(&id);
            }
            self.tabs.remove(idx);
            // Keep `active` valid: clamp if it ran off the end, or shift
            // left if a tab before it was removed.
            if (self.active >= self.tabs.len() || self.active > idx) && self.active > 0 {
                self.active -= 1;
            }
        }
        self.tabs.is_empty()
    }

    /// Open a new tab that duplicates the focused pane's argv + OSC-7
    /// cwd (iTerm2's "Duplicate Tab" affordance, cycle 248). Falls
    /// back to the configured shell when the focused pane has empty
    /// argv (`new_tab_with` semantics — empty argv ≡ shell). Returns
    /// `Ok(())` even if there's no focused tab to duplicate; the
    /// chrome layer treats that as a no-op the same way it treats
    /// `new_tab` on an empty mux.
    #[allow(clippy::too_many_arguments)]
    pub fn duplicate_focused_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        if self
            .active_focus()
            .and_then(|id| self.panes.get(&id))
            .is_none()
        {
            return self.new_tab(cfg, cols, rows, cw, ch, waker);
        }
        // Cycle 886/887: clone via the shared helper so a WSL tab duplicates
        // with `wsl --cd <dir>` instead of falling back to the home dir.
        let (argv, cwd) = self.clone_focused_launch(cfg);
        self.new_tab_with(cfg, cols, rows, cw, ch, waker, &argv, cwd.as_deref())
    }

    /// Split the focused pane and run the *same* program in the new
    /// half (iTerm2's "Duplicate Pane" affordance). Mirrors `split`
    /// but reads the focused pane's argv instead of the configured
    /// shell — so a `kettle -e vim file` pane duplicates into a
    /// second vim instance in the same cwd.
    #[allow(clippy::too_many_arguments)]
    pub fn duplicate_focused_pane(
        &mut self,
        dir: Dir,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        if self.tabs.is_empty() {
            return self.new_tab(cfg, cols, rows, cw, ch, waker);
        }
        // Cycle 886/887: shares `clone_focused_launch` with `split` (now also a
        // clone) — clones the focused pane's argv + cwd, WSL-aware.
        let (argv, cwd) = self.clone_focused_launch(cfg);
        let new_id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        let grafted = self
            .tabs
            .get_mut(a)
            .map(|tab| insert_split(tab, new_id, dir))
            .unwrap_or(false);
        if !grafted {
            // Cycle 917 (#2 hardening): the graft failed (no active tab, or the
            // tree had no leaf to attach to). Reap the just-spawned pane rather
            // than leaking its PTY, and surface a real error — this path was a
            // silent `Ok(())` that left an orphaned pane behind.
            self.panes.remove(&new_id);
            anyhow::bail!("split failed: no pane available to attach the new split");
        }
        Ok(())
    }

    /// Restore the most-recently-closed tab. Returns `true` if a tab
    /// was actually restored. Inserts at the original index (clamped
    /// to the current tab count); the new tab becomes active.
    #[allow(clippy::too_many_arguments)]
    pub fn undo_close_tab(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<bool> {
        let Some(snap) = self.closed_tabs.pop_back() else {
            return Ok(false);
        };
        // Re-spawn the same argv + cwd. Empty argv → configured shell
        // (matches `new_tab_with`'s contract).
        let id = self.spawn_pane(
            cfg,
            cols,
            rows,
            cw,
            ch,
            waker,
            snap.cwd.as_deref(),
            &snap.argv,
        )?;
        let insert_at = snap.original_index.min(self.tabs.len());
        self.tabs.insert(
            insert_at,
            Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            },
        );
        self.active = insert_at;
        self.touch_active_tab_seen();
        Ok(true)
    }

    /// Reap panes whose child exited; prune empty splits/tabs.
    pub fn reap(&mut self) -> bool {
        let dead: Vec<u64> = self
            .panes
            .iter_mut()
            .filter_map(|(id, p)| {
                if is_reapable(p.closed, p.held, p.term.child_exited()) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        for id in &dead {
            self.panes.remove(id);
        }
        Self::reap_tabs(&mut self.tabs, &mut self.active, &dead);
        self.tabs.is_empty()
    }

    /// Pure helper for `reap`'s tab-mutation step: walk every tab,
    /// prune dead panes from its split tree, drop any tab whose tree
    /// collapses to empty, and keep `active` pointing at the *same
    /// tab the user is focused on* after the shift (not the same
    /// numeric index). Extracted so the active-index bookkeeping is
    /// testable without spawning real PTYs to populate `self.panes`.
    pub(crate) fn reap_tabs(tabs: &mut Vec<Tab>, active: &mut usize, dead_ids: &[u64]) {
        for id in dead_ids {
            let mut ti = 0;
            while ti < tabs.len() {
                // Cycle 603: companion to cycle 602's `close_focused`
                // fix. When a PTY exits and the dying leaf IS the
                // focused one, capture the neighbor BEFORE the
                // destructive `remove_leaf` so the post-rebuild
                // focus can promote it instead of jumping to the
                // leftmost leaf of the whole tab. Pre-cycle-603,
                // typing `exit` in the rightmost pane of a 4-pane
                // tab would jump focus to the leftmost pane —
                // exactly the same user-described "first focused
                // terminal" symptom that motivated cycle 602, just
                // triggered by shell exit instead of `close-pane`.
                let neighbor_if_focused = if tabs[ti].focus == *id {
                    tabs[ti].root.neighbor_of(*id)
                } else {
                    None
                };
                let root = std::mem::replace(&mut tabs[ti].root, Node::Leaf(0));
                match root.remove_leaf(*id) {
                    // Cycle 603 part-B: previously this match used
                    // `Err(_) => tabs.remove(ti)` which conflated two
                    // distinct outcomes. `Err(Some(n))` means the
                    // dying leaf was a direct child of root and `n`
                    // is the surviving sibling — the tab MUST stay
                    // with `n` as the new root. Pre-fix, any 2-pane
                    // tab + `exit` in either pane deleted the whole
                    // tab (the surviving sibling went with it).
                    // Reachable in production via `child_exited()` in
                    // `Mux::reap`. Mirrors the cycle-285 distinction
                    // already in `close_focused` below.
                    Ok(n) | Err(Some(n)) => {
                        tabs[ti].root = n;
                        if !tabs[ti].root.contains(tabs[ti].focus) {
                            tabs[ti].focus =
                                neighbor_if_focused.unwrap_or_else(|| tabs[ti].root.first_leaf());
                        }
                        ti += 1;
                    }
                    Err(None) => {
                        tabs.remove(ti);
                        // Cycle 120: keep `active` pointing at the
                        // same tab the user is focused on after the
                        // shift, not the same numeric index. Removing
                        // a tab at `ti < active` shifts every later
                        // tab left by one, so subtract one from
                        // active. `ti == active` (the user IS focused
                        // on the tab being closed): leave active
                        // alone so focus naturally falls on the tab
                        // that takes its slot (the previous tab+1,
                        // matching every modern terminal — close
                        // current tab, focus moves to its right
                        // neighbor; the trailing-clamp below catches
                        // the case where active was the last tab).
                        if ti < *active {
                            *active -= 1;
                        }
                    }
                }
            }
        }
        if *active >= tabs.len() && *active > 0 {
            *active = tabs.len().saturating_sub(1);
        }
    }

    /// Send `bytes` to every pane in the **active tab** (not every tab in
    /// the mux). Cycle 112 fix: the old implementation broadcast across
    /// every pane in every tab — typing one character with broadcast on
    /// echoed into the user's other tabs too (often unrelated work, often
    /// where the user *didn't* want their fan-out keystroke). Terminator's
    /// `broadcast_all` is per-window-per-tab; iTerm2's "Send Input to All
    /// Sessions" defaults per-window; kitty's `send_text` targets all
    /// windows in the current tab. We follow that convention.
    pub fn broadcast_write(&mut self, bytes: &[u8]) {
        // Cycle 679 (named-groups sub-cycle 3): respect the new
        // BroadcastScope enum. Off short-circuits; Tab keeps the
        // cycle-178 active-tab behavior; All targets every pane
        // window-wide; Group(name) targets cross-tab matches.
        let ids = self.broadcast_target_ids();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                // Cycle 941: a read-only pane drops user input (keystroke /
                // paste / broadcast). The child still produces output.
                p.feed_input(bytes);
            }
        }
    }

    /// Cycle 941: toggle the focused pane's read-only state; returns the new
    /// value (or `false` if there's no focused pane).
    pub fn toggle_focused_read_only(&mut self) -> bool {
        if let Some(p) = self.focused() {
            p.read_only = !p.read_only;
            p.read_only
        } else {
            false
        }
    }

    /// Cycle 679: is broadcast active in any scope (Tab/All/Group)?
    /// Most callers just need a yes/no — this preserves the old
    /// `bool` ergonomics post-migration.
    pub fn is_broadcast_on(&self) -> bool {
        !matches!(self.broadcast, BroadcastScope::Off)
    }

    /// Cycle 679: compute the pane IDs that should receive a
    /// broadcast given the current `self.broadcast` scope. Returns
    /// an empty Vec when scope is Off. Used by `broadcast_write`
    /// and `broadcast_paste`.
    fn broadcast_target_ids(&self) -> Vec<u64> {
        if matches!(self.broadcast, BroadcastScope::Off) {
            return Vec::new();
        }
        // No active tab → no anchor pane and nothing to broadcast to.
        // Previously the focused-pane id fell back to `0` (a sentinel that
        // is never a real pane), which `compute_broadcast_targets` would
        // hand back as a phantom target in `Off` scope; guarding here keeps
        // an invalid id from ever entering the pipeline.
        let Some(tab) = self.tabs.get(self.active) else {
            return Vec::new();
        };
        let panes_in_focused_tab = tab.root.leaf_ids();
        let all_with_groups: Vec<(u64, Option<&str>)> = self
            .panes
            .iter()
            .map(|(id, p)| (*id, p.group_name.as_deref()))
            .collect();
        let targets = compute_broadcast_targets(
            &self.broadcast,
            tab.focus,
            &panes_in_focused_tab,
            &all_with_groups,
        );
        // Self-heal an emptied named group: if the active scope is a named
        // Group but no pane currently matches it (the last member was closed
        // or ungrouped, or the focused pane was never in the group), the
        // target set is empty — which would BLACK-HOLE every keystroke while
        // the broadcast indicator stays lit (the user types and nothing
        // happens, with no cue). Fall back to the focused pane so input is
        // never silently swallowed. This single point covers ungroup /
        // last-member-closed / focused-not-in-group; Off/Tab/All can't reach
        // here empty (they always include the focused/tab panes).
        if targets.is_empty() && matches!(self.broadcast, BroadcastScope::Group(_)) {
            return vec![tab.focus];
        }
        targets
    }

    /// Snap every pane in the active tab's broadcast set back to the
    /// bottom of its scrollback. Cycle-173 companion to
    /// `broadcast_write`: `scroll-on-keystroke` (default true) needs to
    /// apply to every targeted pane, not just the focused one, otherwise
    /// the user broadcasting input to N panes sees a confusing mismatch
    /// (typing reaches the remote shells but the local view of any
    /// scrolled-back pane stays pinned to history). Same scoping as
    /// `broadcast_write` — active tab's leaves only, never other tabs.
    /// Cycle 942 (audit): skips read-only panes — their keystroke was dropped
    /// by `feed_input`, so yanking their viewport would break the "no input,
    /// no snap" rule the focused-pane path follows (a scrolled-back read-only
    /// monitoring pane must stay where the user put it).
    pub fn broadcast_scroll_to_bottom(&mut self) {
        // Cycle 679 (named-groups sub-cycle 3): scope-aware
        // target set, same as broadcast_write / broadcast_paste.
        let ids = self.broadcast_target_ids();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id)
                && !p.read_only
                && let Ok(mut t) = p.term.term.lock()
            {
                t.scroll_display(kettle_core::Scroll::Bottom);
            }
        }
    }

    /// Return true when any writable pane in the active broadcast target set
    /// would receive a raw paste instead of bracketed paste wrapping. Used by
    /// the app-level paste protection prompt: a multi-line paste is safe to
    /// send directly only when every target has enabled BRACKETED_PASTE.
    pub fn broadcast_paste_has_raw_writable_target(&self) -> bool {
        self.broadcast_target_ids().into_iter().any(|id| {
            self.panes.get(&id).is_some_and(|p| {
                !p.read_only
                    && !p
                        .term
                        .term
                        .lock()
                        .ok()
                        .map(|t| t.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                        .unwrap_or(false)
            })
        })
    }

    /// Distribute a clipboard paste to every pane in the active tab's
    /// broadcast set. Cycle-174 companion to `broadcast_write` and
    /// `broadcast_scroll_to_bottom`: with broadcast on (group-input
    /// mode, Ctrl+Shift+G), keystrokes go to every pane, and paste is
    /// also user input so it should follow the same scoping. Each pane
    /// gets its own `BRACKETED_PASTE` wrap decision read from its own
    /// `Term::mode()` — panes can disagree on whether the running
    /// program enabled bracketed paste (e.g. one is in vim and one is
    /// at a shell prompt), and wrapping the wrong way would either
    /// inject literal `\e[200~`/`\e[201~` markers into the shell's
    /// command line or leave bytes vulnerable to the bracketed-paste
    /// auto-execute attack inside vim. Pure modulo the writes; the
    /// per-pane wrap is the only logic here.
    pub fn broadcast_paste(&mut self, text: &str) {
        // Cycle 679 (named-groups sub-cycle 3): route through the
        // scope-aware target computation (same as broadcast_write).
        let ids = self.broadcast_target_ids();
        if ids.is_empty() {
            return;
        }
        // Build the two possible payloads lazily — only when we hit the
        // first pane that needs each variant. With a 4 MiB clipboard
        // paste and 5 panes (or more, for shells-broadcast-on-CI
        // patterns), the pre-cycle-191 code allocated 5 copies of the
        // wrap (5 × 4 MiB = 20 MiB temporary). With caching, at most
        // two copies regardless of pane count. `OnceCell`-style lazy
        // via `Option`: skip even one allocation when the broadcast
        // set is entirely one BRACKETED_PASTE state. Cycle 191.
        let mut raw: Option<Vec<u8>> = None;
        let mut wrapped: Option<Vec<u8>> = None;
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                let bracketed = p
                    .term
                    .term
                    .lock()
                    .ok()
                    .map(|t| t.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
                    .unwrap_or(false);
                let bytes: &[u8] = if bracketed {
                    wrapped.get_or_insert_with(|| crate::input::paste_payload(text, true))
                } else {
                    raw.get_or_insert_with(|| crate::input::paste_payload(text, false))
                };
                // Cycle 941: paste is user input — read-only panes drop it.
                p.feed_input(bytes);
            }
        }
    }

    pub fn tab_titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pane = self.panes.get(&t.focus);
                let title = pane.map(|p| p.title.as_str()).unwrap_or("");
                // A pane we can't find (shouldn't happen) is treated as a
                // placeholder so the cwd/`tab N` fallback applies, not an empty
                // verbatim title.
                let placeholder = pane.map(|p| p.title_is_placeholder).unwrap_or(true);
                let cwd = pane.and_then(|p| p.term.current_dir_or_native());
                resolve_tab_title(
                    t.title_override.as_deref(),
                    title,
                    placeholder,
                    cwd.as_deref(),
                    i,
                )
            })
            .collect()
    }

    /// v2.26.0: like [`tab_titles`](Self::tab_titles) but also returns, for tabs
    /// whose label comes from the working directory, the home-abbreviated full
    /// path so the renderer can tier the label (full path → leaf dir name →
    /// truncated tail) to the available tab width.
    pub fn tab_labels(&self) -> Vec<TabLabel> {
        let home = home_dir_string();
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pane = self.panes.get(&t.focus);
                let title = pane.map(|p| p.title.as_str()).unwrap_or("");
                // See `tab_titles`: a missing pane defaults to placeholder so the
                // cwd/`tab N` fallback applies rather than an empty verbatim label.
                let placeholder = pane.map(|p| p.title_is_placeholder).unwrap_or(true);
                let cwd = pane.and_then(|p| p.term.current_dir_or_native());
                resolve_tab_label(
                    t.title_override.as_deref(),
                    title,
                    placeholder,
                    cwd.as_deref(),
                    home.as_deref(),
                    i,
                )
            })
            .collect()
    }
}

/// Display title for one tab, in priority order: an explicit `title_override`
/// (Action::EditTabTitle) wins; else the focused pane's title; else — while the
/// title is still the `kettle` placeholder — the cwd basename; else `tab N`.
///
/// Cycle 829 (audit): the override branch was missing from `tab_titles`, so a
/// custom tab title was stored but never shown (a silent no-op overwritten by
/// the shell's next OSC 2 title). Pulled out as a pure fn so the precedence is
/// drift-tested without standing up a PTY.
///
/// Most shells set the title quickly via OSC 2 on every prompt; until that
/// first prompt fires, the `kettle` placeholder is all we have, so a fresh tab
/// in `~/Repos/kettle` reads as `kettle` instead of the program name (matching
/// iTerm2 / Ghostty / WezTerm). Once a shell sets a real title, that wins.
fn resolve_tab_title(
    title_override: Option<&str>,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    idx: usize,
) -> String {
    resolve_tab_label(title_override, pane_title, placeholder, cwd, None, idx).text
}

/// v2.26.0: a resolved tab label. `text` is the compact display string (used by
/// non-render consumers and as the fallback); `path` carries the home-abbreviated
/// full working-directory path when the label is derived from the cwd, so the
/// renderer can tier it (full path → leaf dir name → truncated tail) to the
/// available tab width. `path` is `None` for explicit/override and shell-set
/// (OSC 2) titles, which are shown verbatim (middle-ellipsized only if they
/// overflow the segment).
pub(crate) struct TabLabel {
    pub(crate) text: String,
    pub(crate) path: Option<String>,
}

/// The pure core of tab-label resolution (precedence: override → real pane title
/// → cwd → `tab N`), additionally surfacing the cwd path for the renderer's
/// width-aware tiering. `home`, when given, collapses a leading `$HOME` to `~` in
/// the surfaced path.
fn resolve_tab_label(
    title_override: Option<&str>,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    home: Option<&str>,
    idx: usize,
) -> TabLabel {
    if let Some(ov) = title_override
        && !ov.is_empty()
    {
        return TabLabel {
            text: ov.to_string(),
            path: None,
        };
    }
    // v2.32.0 (audit): branch on the authoritative `Pane::title_is_placeholder`
    // flag, NOT a string compare against the "kettle" seed. A real shell title
    // that happens to equal the seed string ("kettle") is a genuine title and
    // must be shown verbatim — the flag is the single source of truth (the
    // instant any real OSC 2 title arrives the flag is cleared; consistent with
    // app.rs's `p.title_is_placeholder` titlebar branch).
    if placeholder || pane_title.is_empty() {
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            let full = abbreviate_home(cwd, home);
            // Platform-independent leaf: a cwd's separator style follows the
            // shell, not the build target, so split on BOTH `/` and `\` on every
            // OS. (`std::path::file_name` treats `\` as an ordinary char on Unix,
            // which mis-set the label to the whole `C:\…` string for Windows-style
            // cwds on Linux/macOS.) Matches the renderer's fit_tab_path leaf logic.
            if let Some(name) = cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()) {
                return TabLabel {
                    text: name.to_string(),
                    path: Some(full),
                };
            }
            // Only separators (e.g. "/") — show the (abbreviated) full path.
            return TabLabel {
                text: full.clone(),
                path: Some(full),
            };
        }
        return TabLabel {
            text: format!("tab {}", idx + 1),
            path: None,
        };
    }
    if let Some(cwd) = cwd.filter(|c| !c.is_empty())
        && let Some(label) = cwd_label_for_shell_title(pane_title, cwd, home)
    {
        return label;
    }
    TabLabel {
        text: pane_title.to_string(),
        path: None,
    }
}

/// If a real shell title is just a prompt-rendered cwd (or an ellipsized suffix
/// of it), recover the cwd-derived label so wide tabs/window titles are not
/// stuck with the shell's already-truncated text. Oh My Zsh's term support, for
/// example, emits `%15<..<%~%<<`, yielding titles like `..PI-1/platform` even
/// when Kettle also has the authoritative OSC 7 cwd.
pub(crate) fn cwd_label_for_shell_title(
    title: &str,
    cwd: &str,
    home: Option<&str>,
) -> Option<TabLabel> {
    let leaf = cwd.rsplit(['/', '\\']).find(|s| !s.is_empty())?;
    if !shell_title_matches_cwd(title, cwd, leaf) {
        return None;
    }
    Some(TabLabel {
        text: leaf.to_string(),
        path: Some(abbreviate_home(cwd, home)),
    })
}

fn shell_title_matches_cwd(title: &str, cwd: &str, leaf: &str) -> bool {
    let title = title.trim();
    if title == leaf {
        return true;
    }

    let Some(suffix) = title
        .strip_prefix('…')
        .or_else(|| title.strip_prefix("..."))
        .or_else(|| title.strip_prefix(".."))
    else {
        return false;
    };

    if suffix.chars().count() < 8 {
        return false;
    }

    leaf.ends_with(suffix) || cwd.ends_with(suffix) || abbreviate_home(cwd, None).ends_with(suffix)
}

/// v2.26.0: collapse a leading `$HOME` in `path` to `~` (e.g.
/// `C:\Users\me\Repos\kettle` → `~\Repos\kettle`), preserving the original
/// separator style. Best-effort — a path whose prefix doesn't match `home`
/// (different separator convention, MSYS `/c/...` vs `C:\...`, etc.) is returned
/// unchanged. Pure → unit-tested.
pub(crate) fn abbreviate_home(path: &str, home: Option<&str>) -> String {
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        if path == home {
            return "~".to_string();
        }
        for sep in ['/', '\\'] {
            let prefix = format!("{home}{sep}");
            if let Some(rest) = path.strip_prefix(prefix.as_str()) {
                return format!("~{sep}{rest}");
            }
        }
    }
    path.to_string()
}

/// The user's home directory (`USERPROFILE` on Windows, else `HOME`), used to
/// abbreviate cwd-derived tab labels. `None` when unset/empty.
pub(crate) fn home_dir_string() -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Rotate the split that is the *immediate parent* of pane `target` (the split
/// with `target` as a direct leaf child): flip its axis and, for a clockwise
/// rotation, swap its children. Recurses to find that parent; returns whether a
/// rotation happened. Extracted from `rotate_focused_split` as a free fn so the
/// nested-tree behavior is unit-testable without standing up a Mux (audit,
/// v2.26.0: the old guard fired for any ancestor split that merely had some leaf
/// child, rotating the wrong split in nested trees).
fn rotate_node(node: &mut Node, target: u64, clockwise: bool) -> bool {
    if let Node::Split { dir, a, b, .. } = node {
        if matches!(**a, Node::Leaf(x) if x == target)
            || matches!(**b, Node::Leaf(x) if x == target)
        {
            *dir = match *dir {
                Dir::Horizontal => Dir::Vertical,
                Dir::Vertical => Dir::Horizontal,
            };
            if clockwise {
                std::mem::swap(a, b);
            }
            return true;
        }
        if a.contains(target) && rotate_node(a, target, clockwise) {
            return true;
        }
        if b.contains(target) && rotate_node(b, target, clockwise) {
            return true;
        }
    }
    false
}

/// Apply the *post-spawn* tree mutation for a split: graft the new pane id
/// next to the currently-focused leaf in direction `dir`, move focus to
/// the new pane, and **exit zoom** if it was on.
///
/// Cycle 130: splitting while zoomed used to leave the tab zoomed AND
/// focused on the new pane, so the user only saw the new pane — the
/// half they just split from disappeared from view (still alive, just
/// hidden by `Mux::layout`'s zoom-collapse). Every modern terminal
/// treats `split` as "show me both" — tmux's `display-panes` UX
/// after `split-window`, WezTerm's `SplitHorizontal/Vertical`. Pure so
/// the contract is unit-testable without a real spawn.
fn insert_split(tab: &mut Tab, new_id: u64, dir: Dir) -> bool {
    let focus = tab.focus;
    if tab.root.split_leaf(focus, new_id, dir) {
        tab.focus = new_id;
        tab.zoomed = false;
        return true;
    }
    // Cycle 917 (#2 hardening): `tab.focus` was stale — not a leaf in this tree
    // (a focus-desync class of bug). Previously `split_leaf` silently no-op'd
    // and the freshly-spawned pane was orphaned (leaked PTY + child) while the
    // split still reported success. Repair focus to a real leaf and retry; the
    // caller reaps the pane if even this fails, instead of leaking it.
    let repaired = tab.root.first_leaf();
    if tab.root.split_leaf(repaired, new_id, dir) {
        tab.focus = new_id;
        tab.zoomed = false;
        return true;
    }
    false
}

fn shell_argv(cfg: &Config) -> Vec<String> {
    match &cfg.shell {
        Some(s) => vec![s.clone()],
        None => Vec::new(),
    }
}

fn argv0_base_lower(argv: &[String]) -> String {
    let base = argv
        .first()
        .map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s))
        .unwrap_or("");
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Direct agent/editor launches are poor split templates: cloning them can
/// create a second full-screen app or a short-lived helper-backed pane. Split
/// should produce a usable prompt; Duplicate still preserves exact argv cloning.
fn direct_launch_splits_to_shell(argv: &[String]) -> bool {
    matches!(
        argv0_base_lower(argv).as_str(),
        "codex" | "claude" | "nvim" | "vim"
    )
}

/// Keep a candidate cwd only if it still names an existing directory — a
/// pane may have been `cd`'d into a since-removed path, in which case a new
/// tab/split should fall back to the default rather than fail to spawn.
fn usable_cwd(dir: Option<String>) -> Option<String> {
    dir.filter(|d| std::path::Path::new(d).is_dir())
}

/// Cycle 887: is this argv launching WSL (`wsl` / `wsl.exe`, by argv[0]
/// basename)? Used to route the cloned cwd through `wsl --cd` instead of the
/// Windows spawn cwd. Mirrors `kettle_core`'s private `is_wsl_launcher`.
///
/// v2.29.0: also consulted by the native-cwd poll — wsl.exe is a relay whose
/// own Windows cwd is its launch dir and never tracks the in-distro `cd`, so the
/// native read must be skipped for WSL (OSC 7 from inside the distro is the only
/// correct source there).
pub(crate) fn argv_is_wsl(argv: &[String]) -> bool {
    argv.first()
        .map(|p| {
            let last = p.rsplit(['/', '\\']).next().unwrap_or(p);
            last.eq_ignore_ascii_case("wsl") || last.eq_ignore_ascii_case("wsl.exe")
        })
        .unwrap_or(false)
}

/// Cycle 887: given a cloned `argv` + the focused pane's raw reported cwd,
/// decide the `(argv, spawn-cwd)` to launch with. For a WSL launcher the dir is
/// carried via `wsl --cd <dir>` (which accepts Windows AND Linux paths) and no
/// Windows spawn cwd is set — WSL reports a Linux path a Windows spawn would
/// reject, leaving the new pane in the home dir. Non-WSL panes inherit the
/// usable Windows dir as before. Pure (unit-tested).
fn launch_cwd(mut argv: Vec<String>, raw_cwd: Option<String>) -> (Vec<String>, Option<String>) {
    if argv_is_wsl(&argv) {
        if let Some(d) = raw_cwd.filter(|d| !d.is_empty())
            && !argv.iter().any(|a| a == "--cd")
        {
            // Cycle 894 (audit): insert `--cd <dir>` immediately AFTER the
            // launcher (index 1), in WSL's option section. Appending at the
            // end was wrong whenever argv carried a command —
            // `wsl -d Ubuntu -- bash -l` became
            // `wsl -d Ubuntu -- bash -l --cd <dir>`, where `--cd <dir>` is
            // passed to `bash`, not WSL, so the working dir was ignored.
            // WSL parses all options (in any order) before the command, so
            // placing `--cd` first is always valid and never lands past a
            // `--` separator or a positional command token.
            argv.insert(1, d);
            argv.insert(1, "--cd".to_string());
        }
        (argv, None)
    } else {
        let cwd = usable_cwd(raw_cwd);
        (argv, cwd)
    }
}

fn collect_ids(n: &Node, out: &mut Vec<u64>) {
    match n {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            collect_ids(a, out);
            collect_ids(b, out);
        }
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod node_tests {
    use super::*;

    /// Cycle 789 drift guard (audit D2). Session focus persistence is the core
    /// state machine for relaunch: `snapshot` records the focused pane's
    /// DFS-order *index* via `leaf_index_of` (pane ids are reallocated across
    /// restores, so the id itself isn't portable), and `restore` recreates
    /// focus with `nth_leaf` at that index. The two walk children in the same
    /// `a → b` order and MUST stay exact inverses, or relaunch silently focuses
    /// the wrong pane. An off-by-one here is invisible to a behavioral test
    /// (every shell still spawns) — this pins the invariant directly.
    #[test]
    fn leaf_index_of_and_nth_leaf_are_inverse() {
        // Split( Split(L1,L2), Split(L3,L4) ) — DFS leaf order 1,2,3,4.
        let split = |a, b| Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(a),
            b: Box::new(b),
        };
        let tree = split(
            split(Node::Leaf(1), Node::Leaf(2)),
            split(Node::Leaf(3), Node::Leaf(4)),
        );
        assert_eq!(tree.leaf_ids(), vec![1, 2, 3, 4], "DFS leaf order");
        for (idx, id) in [(0usize, 1u64), (1, 2), (2, 3), (3, 4)] {
            assert_eq!(tree.leaf_index_of(id), Some(idx), "index of leaf {id}");
            assert_eq!(tree.nth_leaf(idx), id, "leaf at index {idx}");
            // The exact round trip restore relies on:
            assert_eq!(tree.nth_leaf(tree.leaf_index_of(id).unwrap()), id);
        }
        // A pane id no longer in the tree → None (snapshot then stores 0).
        assert_eq!(tree.leaf_index_of(999), None);
        // An index past a trimmed tree falls back to the first leaf, so a
        // stale session still produces a valid focus instead of panicking.
        assert_eq!(tree.nth_leaf(99), tree.first_leaf());
        assert_eq!(tree.first_leaf(), 1);
        // Single-leaf tab: index 0 ↔ the lone pane.
        let solo = Node::Leaf(7);
        assert_eq!(solo.leaf_index_of(7), Some(0));
        assert_eq!(solo.nth_leaf(0), 7);
    }

    // ---- Cycle 917 (#1): directional pane-focus navigation scaffolding ----

    /// A representative wide area (matches the user's HiDPI screenshot ratio).
    const AREA: Rect = (0.0, 0.0, 2560.0, 1440.0);

    fn push_tab(m: &mut Mux, root: Node, focus: u64) {
        m.tabs.push(Tab {
            root,
            focus,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        m.active = m.tabs.len() - 1;
    }
    fn hsplit(ratio: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }
    fn vsplit(ratio: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// The screenshot layout. Leaf ids: 1=left (full height), 2=top-wide,
    /// 3=midleft (tall, left of the lower-right region), 4=midL, 5=midR
    /// (the mid row of two), 6=botright (the focused pane).
    fn screenshot_tree() -> Node {
        hsplit(
            0.5,
            Node::Leaf(1),
            vsplit(
                0.33,
                Node::Leaf(2),
                hsplit(
                    0.5,
                    Node::Leaf(3),
                    vsplit(
                        0.5,
                        hsplit(0.5, Node::Leaf(4), Node::Leaf(5)),
                        Node::Leaf(6),
                    ),
                ),
            ),
        )
    }

    /// The OLD Euclidean-center rule, inlined so a future revert to
    /// center-distance fails this test (it documents exactly why it was wrong).
    fn old_focus_dir(rects: &[(u64, Rect)], focus: u64, dx: i32, dy: i32) -> Option<u64> {
        let (_, (fx, fy, fw, fh)) = *rects.iter().find(|(id, _)| *id == focus)?;
        let (fcx, fcy) = (fx + fw / 2.0, fy + fh / 2.0);
        let mut best: Option<(f32, u64)> = None;
        for (id, (x, y, w, h)) in rects {
            if *id == focus {
                continue;
            }
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let ok = (dx > 0 && cx > fcx)
                || (dx < 0 && cx < fcx)
                || (dy > 0 && cy > fcy)
                || (dy < 0 && cy < fcy);
            if !ok {
                continue;
            }
            let d = (cx - fcx).powi(2) + (cy - fcy).powi(2);
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, *id));
            }
        }
        best.map(|(_, id)| id)
    }

    #[test]
    fn focus_dir_screenshot_layout_picks_adjacent_not_diagonal() {
        let mut m = Mux::new();
        push_tab(&mut m, screenshot_tree(), 6);
        let rects = m.layout(0, AREA);

        // (a) Document the bug: the old center-distance rule jumps Left to the
        // DIAGONAL midL (4) and Right to the up-right midR (5) — there is no real
        // right neighbor of botright at all.
        assert_eq!(
            old_focus_dir(&rects, 6, -1, 0),
            Some(4),
            "old rule jumps Left to the diagonal midL (the reported bug)"
        );
        assert_eq!(
            old_focus_dir(&rects, 6, 1, 0),
            Some(5),
            "old rule jumps Right to the up-right midR (phantom neighbor)"
        );

        // (b) The new edge+overlap rule moves to the true neighbors.
        m.focus_dir(AREA, -1, 0);
        assert_eq!(
            m.tabs[0].focus, 3,
            "Left -> the full-height pane bordering botright's left edge"
        );
        m.tabs[0].focus = 6;
        m.focus_dir(AREA, 0, -1);
        assert_eq!(
            m.tabs[0].focus, 4,
            "Up -> the pane directly above (midL wins the midL/midR tie via DFS order)"
        );
        m.tabs[0].focus = 6;
        m.focus_dir(AREA, 1, 0);
        assert_eq!(
            m.tabs[0].focus, 6,
            "Right -> nothing borders the right edge; no-op"
        );
        m.focus_dir(AREA, 0, 1);
        assert_eq!(m.tabs[0].focus, 6, "Down -> nothing below; no-op");
    }

    #[test]
    fn focus_dir_2x2_grid_moves_to_orthogonal_neighbor() {
        // H{ V{A=1,C=2}, V{B=3,D=4} }: A=TL C=BL B=TR D=BR.
        let tree = hsplit(
            0.5,
            vsplit(0.5, Node::Leaf(1), Node::Leaf(2)),
            vsplit(0.5, Node::Leaf(3), Node::Leaf(4)),
        );
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 4); // start at D (bottom-right)
        m.focus_dir(area, 0, -1);
        assert_eq!(m.tabs[0].focus, 3, "D Up -> B");
        m.tabs[0].focus = 4;
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 2, "D Left -> C");
        m.tabs[0].focus = 1; // A (top-left)
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 3, "A Right -> B");
        m.tabs[0].focus = 1;
        m.focus_dir(area, 0, 1);
        assert_eq!(m.tabs[0].focus, 2, "A Down -> C");
    }

    #[test]
    fn focus_dir_two_pane_split_and_edge_noops() {
        let tree = hsplit(0.5, Node::Leaf(1), Node::Leaf(2)); // A | B
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 1);
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 2, "A Right -> B");
        m.tabs[0].focus = 1;
        for (dx, dy) in [(-1, 0), (0, -1), (0, 1)] {
            m.focus_dir(area, dx, dy);
            assert_eq!(m.tabs[0].focus, 1, "no neighbor that way -> stay on A");
        }
        m.tabs[0].focus = 2;
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 1, "B Left -> A");
        m.tabs[0].focus = 2;
        for (dx, dy) in [(1, 0), (0, -1), (0, 1)] {
            m.focus_dir(area, dx, dy);
            assert_eq!(m.tabs[0].focus, 2, "no neighbor that way -> stay on B");
        }
    }

    #[test]
    fn focus_dir_is_reversible_on_grid() {
        let tree = hsplit(
            0.5,
            vsplit(0.5, Node::Leaf(1), Node::Leaf(2)),
            vsplit(0.5, Node::Leaf(3), Node::Leaf(4)),
        );
        let area = (0.0, 0.0, 200.0, 100.0);
        let mut m = Mux::new();
        push_tab(&mut m, tree, 1); // A
        m.focus_dir(area, 1, 0);
        assert_eq!(m.tabs[0].focus, 3);
        m.focus_dir(area, -1, 0);
        assert_eq!(m.tabs[0].focus, 1, "Right then Left returns to A");
        m.focus_dir(area, 0, 1);
        assert_eq!(m.tabs[0].focus, 2);
        m.focus_dir(area, 0, -1);
        assert_eq!(m.tabs[0].focus, 1, "Down then Up returns to A");
    }

    #[test]
    fn focus_dir_noop_when_zoomed() {
        let mut m = Mux::new();
        push_tab(&mut m, screenshot_tree(), 6);
        m.tabs[0].zoomed = true; // layout returns only the focused pane
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            m.focus_dir(AREA, dx, dy);
            assert_eq!(m.tabs[0].focus, 6, "zoomed: focus_dir must be a no-op");
        }
    }

    /// Cycle 893 drift guard (audit). When a saved split-tree partially
    /// rebuilds — the first child spawns, a later sibling fails (cwd gone,
    /// fork under quota) — `build_node` returns `Err` and the whole tree is
    /// discarded, but the panes already spawned for the first child stay in
    /// `self.panes`, orphaned: a leaked PTY + child process each. The fix
    /// threads a `spawned: &mut Vec<u64>` accumulator through `build_node`
    /// and reaps those ids on the restore error path. A behavioral test
    /// would need a real PTY + event-loop `Waker` (unavailable in unit
    /// tests, like every other spawn path here), so the wiring is pinned at
    /// the source level.
    #[test]
    fn build_node_reaps_orphan_panes_on_partial_restore_failure() {
        let src = include_str!("mux.rs");
        assert!(
            src.contains("spawned: &mut Vec<u64>"),
            "build_node must thread a spawned-id accumulator so a partial \
             subtree's panes can be reaped on failure"
        );
        assert!(
            src.contains("spawned.push(id);"),
            "each spawned pane id must be recorded in the accumulator"
        );
        assert!(
            src.contains("for id in &tab_pane_ids {") && src.contains("self.panes.remove(id);"),
            "the restore error arm must reap every pane the partial tree \
             spawned, or a failed split leaks PTYs + child processes"
        );
    }

    /// Cycle 904 (audit): split divider drag-to-resize geometry. `dividers`
    /// must mirror `layout` exactly, `set_ratio_at` must address the right
    /// split via its path, and the pos→ratio + hit-test helpers must be
    /// correct. These are the pieces a behavioral mouse test can't reach
    /// (no window), so unit-test the math directly.
    #[test]
    fn split_divider_geometry_round_trips() {
        use super::{Dir, Node, ratio_from_pos, seam_at};

        // Horizontal split (side-by-side) at ratio 0.5 over a 200x100 area
        // anchored at (0,0): one vertical divider at x=100, spanning y∈[0,100].
        let split = |dir, ratio, a, b| Node::Split {
            dir,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        };
        let root = split(Dir::Horizontal, 0.5, Node::Leaf(1), Node::Leaf(2));
        let mut seams = Vec::new();
        root.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams);
        assert_eq!(seams.len(), 1);
        assert_eq!(seams[0].pos, 100.0);
        assert_eq!(seams[0].dir, Dir::Horizontal);
        assert!(seams[0].path.is_empty(), "root split has empty path");

        // Hit-test: a cursor within tol of the vertical seam (x≈100) and inside
        // the vertical span hits; one far away misses.
        assert_eq!(seam_at(&seams, 102.0, 50.0, 4.0), Some(0));
        assert_eq!(seam_at(&seams, 140.0, 50.0, 4.0), None); // too far in x
        assert_eq!(seam_at(&seams, 100.0, 150.0, 4.0), None); // outside y span

        // pos→ratio: dragging the seam to x=150 over the 200-wide split → 0.75.
        let r = ratio_from_pos(seams[0].rect, seams[0].dir, 150.0, 50.0);
        assert!((r - 0.75).abs() < 1e-6, "ratio was {r}");
        // Clamp: dragging past the edge pins to the band, never 0/1.
        assert_eq!(
            ratio_from_pos(seams[0].rect, Dir::Horizontal, -50.0, 0.0),
            0.05
        );
        assert_eq!(
            ratio_from_pos(seams[0].rect, Dir::Horizontal, 999.0, 0.0),
            0.95
        );

        // Nested tree: root Horizontal(0.5){ Leaf1, Vertical(0.5){Leaf2,Leaf3} }.
        // Two seams: the root vertical divider (path []) and the right child's
        // horizontal divider (path [true]).
        let mut nested = split(
            Dir::Horizontal,
            0.5,
            Node::Leaf(1),
            split(Dir::Vertical, 0.5, Node::Leaf(2), Node::Leaf(3)),
        );
        let mut seams = Vec::new();
        nested.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams);
        assert_eq!(seams.len(), 2);
        // Outer first, then inner (so a tie resolves to the outer split).
        assert_eq!(seams[0].path, Vec::<bool>::new());
        assert_eq!(seams[1].path, vec![true]);
        assert_eq!(seams[1].dir, Dir::Vertical);

        // Set the inner (path [true]) split's ratio and confirm only it moved.
        assert!(nested.set_ratio_at(&[true], 0.8));
        let mut seams2 = Vec::new();
        nested.dividers((0.0, 0.0, 200.0, 100.0), &mut Vec::new(), &mut seams2);
        // Inner Vertical split now at 0.8 of its 100-tall right column → y=80.
        let inner = seams2.iter().find(|s| s.path == vec![true]).unwrap();
        assert_eq!(inner.pos, 80.0);
        // A path that doesn't land on a split returns false (stale path).
        assert!(!nested.set_ratio_at(&[false, true], 0.5)); // descends into Leaf1
    }

    /// Cycle 678 drift guard. `compute_broadcast_targets` is the
    /// pure helper that maps a `BroadcastScope` + focused pane +
    /// tab + window state to the set of target pane IDs.
    /// Sub-cycle 2 of named-groups design.
    #[test]
    fn compute_broadcast_targets_matrix() {
        let in_tab = vec![1u64, 2, 3];
        let all = vec![
            (1u64, Some("fleet")),
            (2u64, Some("fleet")),
            (3u64, None),
            (4u64, Some("misc")),
            (5u64, Some("fleet")),
        ];
        // Off: only the focused pane receives.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::Off, 2, &in_tab, &all),
            vec![2]
        );
        // Tab: every pane in the focused tab.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::Tab, 2, &in_tab, &all),
            vec![1, 2, 3]
        );
        // All: every pane window-wide.
        assert_eq!(
            compute_broadcast_targets(&BroadcastScope::All, 2, &in_tab, &all),
            vec![1, 2, 3, 4, 5]
        );
        // Group("fleet") with the focused pane (2) a MEMBER: every pane tagged
        // "fleet", regardless of tab; the focused pane is already in the set so
        // it is NOT duplicated.
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("fleet".to_string()),
                2,
                &in_tab,
                &all
            ),
            vec![1, 2, 5]
        );
        // Group("fleet") with the focused pane (4) NOT a member: the on-screen
        // pane is unioned in (appended, deduped) so input is never routed away
        // from it. v2.32.0 (audit) — the Group arm now always includes the
        // focused pane, mirroring Off/Tab/All.
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("fleet".to_string()),
                4,
                &in_tab,
                &all
            ),
            vec![1, 2, 5, 4]
        );
        // Group with no group matches still yields the focused pane (never an
        // empty set that would black-hole input). v2.32.0 (audit).
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("nonexistent".to_string()),
                2,
                &in_tab,
                &all
            ),
            vec![2]
        );
        // Default scope is Off.
        assert_eq!(BroadcastScope::default(), BroadcastScope::Off);
    }

    /// `broadcast_target_ids` must never emit a phantom pane id when there
    /// is no active tab. A fresh `Mux` has no tabs/panes; in every scope
    /// the target set is empty rather than the old `[0]` sentinel that the
    /// `Off` arm would have produced from `unwrap_or(0)`.
    #[test]
    fn broadcast_target_ids_empty_when_no_active_tab() {
        let mut mux = Mux::new();
        for scope in [
            BroadcastScope::Off,
            BroadcastScope::Tab,
            BroadcastScope::All,
            BroadcastScope::Group("fleet".to_string()),
        ] {
            mux.broadcast = scope.clone();
            assert!(
                mux.broadcast_target_ids().is_empty(),
                "scope {scope:?} should yield no targets with no active tab"
            );
        }
    }

    /// v2.32.0 (audit, HIGH): an emptied named broadcast Group must NEVER
    /// black-hole input. When the active scope is a `Group` but no pane matches
    /// it (last member closed / ungrouped / the focused pane was never in the
    /// group), `broadcast_target_ids` self-heals to `[focus]` so typing still
    /// reaches the on-screen pane instead of vanishing while the indicator stays
    /// lit. Built without a PTY: the method only reads `tab.focus` and the group
    /// names in `self.panes`, so an empty `panes` map with a `Group` scope
    /// exercises the empty-group path directly.
    #[test]
    fn broadcast_target_ids_self_heals_empty_group_to_focus() {
        let mut mux = Mux::new();
        mux.tabs.push(Tab {
            root: Node::Leaf(42),
            focus: 42,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        });
        mux.active = 0;
        // No pane carries the "fleet" group (panes map is empty) → the raw
        // target set is empty, but the self-heal returns the focused pane.
        mux.broadcast = BroadcastScope::Group("fleet".to_string());
        assert_eq!(
            mux.broadcast_target_ids(),
            vec![42],
            "an empty named group must heal to the focused pane, never black-hole input"
        );
        // Sanity: the self-heal is Group-only — scope Off still short-circuits
        // to an empty set (broadcast disabled, the caller writes to the focused
        // pane directly), unchanged by this fix.
        mux.broadcast = BroadcastScope::Off;
        assert!(mux.broadcast_target_ids().is_empty());
    }

    #[test]
    fn resolve_tab_title_precedence() {
        use super::resolve_tab_title;
        // Cycle 829 (audit): an explicit override wins over a real pane title
        // AND over the cwd fallback — the bug was that it was ignored entirely.
        // (The `bool` arg is `Pane::title_is_placeholder`.)
        assert_eq!(
            resolve_tab_title(Some("deploy"), "bash", false, Some("/home/u/proj"), 0),
            "deploy"
        );
        assert_eq!(
            resolve_tab_title(Some("notes"), "kettle", true, Some("/home/u/proj"), 2),
            "notes"
        );
        // Empty override is ignored (falls through to the normal chain).
        assert_eq!(resolve_tab_title(Some(""), "vim", false, None, 0), "vim");
        // No override: a real shell title wins.
        assert_eq!(
            resolve_tab_title(None, "vim - main.rs", false, None, 0),
            "vim - main.rs"
        );
        // Placeholder title (still the seed) → cwd basename.
        assert_eq!(
            resolve_tab_title(None, "kettle", true, Some("/home/u/Repos/kettle"), 0),
            "kettle"
        );
        // v2.32.0 (audit): a REAL shell title that happens to equal the seed
        // string "kettle" (placeholder = false) is shown VERBATIM — it must NOT
        // be re-derived as a placeholder via a string compare against the seed.
        assert_eq!(
            resolve_tab_title(None, "kettle", false, Some("/home/u/Repos/proj"), 0),
            "kettle"
        );
        // Placeholder + no cwd → "tab N" (1-based).
        assert_eq!(resolve_tab_title(None, "kettle", true, None, 3), "tab 4");
        // Empty title is always a placeholder regardless of the flag.
        assert_eq!(resolve_tab_title(None, "", true, None, 0), "tab 1");
        assert_eq!(resolve_tab_title(None, "", false, None, 0), "tab 1");
    }

    #[test]
    fn resolve_tab_label_surfaces_cwd_path() {
        use super::resolve_tab_label;
        // cwd fallback (placeholder title still the seed): compact text is the
        // leaf, but the full (abbreviated) path is surfaced for the renderer to
        // tier. (The `bool` arg is `Pane::title_is_placeholder`.)
        let l = resolve_tab_label(
            None,
            "kettle",
            true,
            Some("/home/u/Repos/kettle"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~/Repos/kettle"));
        // No home match → full path unabbreviated.
        let l = resolve_tab_label(None, "kettle", true, Some("/srv/app"), Some("/home/u"), 0);
        assert_eq!(l.text, "app");
        assert_eq!(l.path.as_deref(), Some("/srv/app"));
        // v2.32.0 (audit): a REAL title equal to the seed string "kettle"
        // (placeholder = false) is shown verbatim and carries NO cwd path —
        // the flag, not a string compare, decides placeholder-ness.
        let l = resolve_tab_label(
            None,
            "kettle",
            false,
            Some("/home/u/Repos/proj"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert!(l.path.is_none());
        // A real shell title that is exactly the cwd leaf still carries the
        // full cwd path for width-aware tab fitting. The title itself wins;
        // the path is metadata only.
        let l = resolve_tab_label(
            None,
            "flight-event-line-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        // Shells/prompts may set an already-left-truncated title. When that
        // title is a clear suffix of the cwd leaf, recover the full leaf/path so
        // wide tabs can show all available context instead of preserving stale
        // truncation.
        let l = resolve_tab_label(
            None,
            "..ine-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        let l = resolve_tab_label(
            None,
            "…ine-server-go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "flight-event-line-server-go");
        assert_eq!(
            l.path.as_deref(),
            Some("~/Repos/SPI-1/flight-event-line-server-go")
        );
        let l = resolve_tab_label(
            None,
            "..go",
            false,
            Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "..go");
        assert!(l.path.is_none());
        for truncated in ["...PI-1/platform", "..PI-1/platform", "…PI-1/platform"] {
            let l = resolve_tab_label(
                None,
                truncated,
                false,
                Some("/home/u/Repos/SPI-1/platform"),
                Some("/home/u"),
                0,
            );
            assert_eq!(l.text, "platform", "{truncated}");
            assert_eq!(l.path.as_deref(), Some("~/Repos/SPI-1/platform"));
        }
        let l = resolve_tab_label(
            None,
            "..PI-1/platform",
            false,
            Some("/home/u/Repos/other/platform"),
            Some("/home/u"),
            0,
        );
        assert_eq!(l.text, "..PI-1/platform");
        assert!(l.path.is_none());
        // Override / real title / no-cwd carry no path (shown verbatim).
        assert!(
            resolve_tab_label(Some("deploy"), "bash", false, Some("/x/y"), None, 0)
                .path
                .is_none()
        );
        assert!(
            resolve_tab_label(None, "vim - main.rs", false, None, None, 0)
                .path
                .is_none()
        );
        assert!(
            resolve_tab_label(None, "kettle", true, None, None, 3)
                .path
                .is_none()
        );
        // Windows-style separators abbreviate too.
        let l = resolve_tab_label(
            None,
            "kettle",
            true,
            Some("C:\\Users\\me\\Repos\\kettle"),
            Some("C:\\Users\\me"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~\\Repos\\kettle"));
        let l = resolve_tab_label(
            None,
            "...Repos\\kettle",
            false,
            Some("C:\\Users\\me\\Repos\\kettle"),
            Some("C:\\Users\\me"),
            0,
        );
        assert_eq!(l.text, "kettle");
        assert_eq!(l.path.as_deref(), Some("~\\Repos\\kettle"));
    }

    #[test]
    fn abbreviate_home_rules() {
        use super::abbreviate_home;
        assert_eq!(abbreviate_home("/home/u/proj", Some("/home/u")), "~/proj");
        assert_eq!(abbreviate_home("/home/u", Some("/home/u")), "~");
        // Not under home → unchanged.
        assert_eq!(abbreviate_home("/etc/hosts", Some("/home/u")), "/etc/hosts");
        // No home → unchanged.
        assert_eq!(abbreviate_home("/home/u/proj", None), "/home/u/proj");
        // A non-boundary prefix must NOT match (/home/user vs home /home/u).
        assert_eq!(
            abbreviate_home("/home/user/x", Some("/home/u")),
            "/home/user/x"
        );
        assert_eq!(
            abbreviate_home("C:\\Users\\me\\p", Some("C:\\Users\\me")),
            "~\\p"
        );
    }

    #[test]
    fn rotate_node_targets_the_immediate_parent_in_nested_trees() {
        use super::{Dir, Node, rotate_node};
        // Split1{ a: Split2{L1,L2} (Horizontal), b: L3 } (Vertical), focus L1.
        // L1's immediate parent is Split2 — rotating must flip Split2, NOT the
        // outer Split1 (the audited bug rotated Split1 because its child L3 is a
        // leaf).
        let mut root = Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(1)),
                b: Box::new(Node::Leaf(2)),
            }),
            b: Box::new(Node::Leaf(3)),
        };
        assert!(rotate_node(&mut root, 1, false));
        match &root {
            Node::Split { dir: outer, a, .. } => {
                assert!(
                    matches!(outer, Dir::Vertical),
                    "outer split must NOT rotate"
                );
                assert!(
                    matches!(
                        a.as_ref(),
                        Node::Split {
                            dir: Dir::Vertical,
                            ..
                        }
                    ),
                    "inner split (L1's parent) flips H->V"
                );
            }
            _ => panic!("root should still be a split"),
        }
        // Unknown target → no-op.
        assert!(!rotate_node(&mut root, 999, false));
    }

    #[test]
    fn tab_title_falls_back_to_cwd_basename() {
        // The fallback only kicks in when the pane's title is the
        // initial placeholder "kettle" (or empty) — once a real shell
        // sets `\e]2;…\007`, that title wins. This is a small pure
        // test of the path-basename logic since the full title path
        // requires a real Terminal/PTY.
        let path = "/home/user/Repos/kettle";
        let basename = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str());
        assert_eq!(basename, Some("kettle"));
        // Trailing slash: `file_name` returns None on root-like paths
        // — those should fall through to "tab N" by the same code.
        assert_eq!(std::path::Path::new("/").file_name(), None);
        // Edge: empty path / no-cwd case (Terminal::current_dir = None)
        // also routes to "tab N" naturally.
    }

    #[test]
    fn initial_pane_title_seeds_ssh_with_target_else_kettle() {
        // Plain shell (or empty argv) → "kettle" placeholder; cycle-89
        // cwd-basename fallback fills it in once OSC 7 arrives.
        assert_eq!(initial_pane_title(&[]), "kettle");
        assert_eq!(initial_pane_title(&["bash".into()]), "kettle");
        assert_eq!(initial_pane_title(&["zsh".into(), "-i".into()]), "kettle");
        // Path-qualified shell is still treated as a shell (basename match).
        assert_eq!(initial_pane_title(&["/bin/bash".into()]), "kettle");
        assert_eq!(initial_pane_title(&["/usr/bin/fish".into()]), "kettle");
        // Windows shells too — names differ from POSIX so list them explicitly.
        assert_eq!(initial_pane_title(&["pwsh.exe".into()]), "kettle");
        assert_eq!(initial_pane_title(&["cmd.exe".into()]), "kettle");
        // SSH: surface the target so the tab is identifiable while
        // connecting. `-t`/`-A`/etc are skipped to find the host.
        assert_eq!(
            initial_pane_title(&["ssh".into(), "-t".into(), "me@example.com".into()]),
            "ssh me@example.com"
        );
        assert_eq!(initial_pane_title(&["ssh".into(), "box".into()]), "ssh box");
        // `ssh` with no positional arg → just "ssh" (rare but defined).
        assert_eq!(initial_pane_title(&["ssh".into(), "-V".into()]), "ssh");
        // Explicit `-e PROG` for non-shells uses the program basename, so
        // `kettle -e htop` doesn't show the generic "kettle" forever
        // (htop never emits OSC 2 and has no useful cwd to back-fill from).
        assert_eq!(initial_pane_title(&["htop".into()]), "htop");
        assert_eq!(initial_pane_title(&["/usr/bin/htop".into()]), "htop");
        assert_eq!(initial_pane_title(&["vim".into(), "file.rs".into()]), "vim");
        assert_eq!(
            initial_pane_title(&["python3".into(), "script.py".into()]),
            "python3"
        );
        assert_eq!(initial_pane_title(&["tmux".into()]), "tmux");
    }

    #[test]
    fn engine_cursor_shape_maps_config_to_engine() {
        // Block / Underline are 1:1. `Bar` (kettle config name) → `Beam`
        // (engine name) — same thin vertical stroke. The engine also has
        // `HollowBlock` and `Hidden` but those only ever arrive via
        // DECSCUSR/DEC?25 from a running program, never as a seed.
        assert_eq!(engine_cursor_shape(CursorStyle::Block), CursorShape::Block);
        assert_eq!(
            engine_cursor_shape(CursorStyle::Underline),
            CursorShape::Underline
        );
        assert_eq!(engine_cursor_shape(CursorStyle::Bar), CursorShape::Beam);
    }

    #[test]
    fn argv_is_wsl_detects_launcher_by_basename() {
        assert!(argv_is_wsl(&["wsl".to_string()]));
        assert!(argv_is_wsl(&["wsl.exe".to_string()]));
        assert!(argv_is_wsl(&["WSL.EXE".to_string()]));
        assert!(argv_is_wsl(&[
            "C:\\Windows\\System32\\wsl.exe".to_string(),
            "-d".to_string()
        ]));
        assert!(!argv_is_wsl(&["pwsh.exe".to_string()]));
        assert!(!argv_is_wsl(&["bash".to_string()]));
        assert!(!argv_is_wsl(&[]));
    }

    #[test]
    fn direct_agent_editor_launches_split_to_shell() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        assert!(direct_launch_splits_to_shell(&s(&["codex"])));
        assert!(direct_launch_splits_to_shell(&s(&["/usr/bin/claude"])));
        assert!(direct_launch_splits_to_shell(&s(&[
            "C:\\Users\\me\\bin\\CODEX.EXE"
        ])));
        assert!(direct_launch_splits_to_shell(&s(&["nvim", "file.rs"])));
        assert!(direct_launch_splits_to_shell(&s(&["vim", "file.rs"])));

        // Shell/session launchers remain exact split templates.
        assert!(!direct_launch_splits_to_shell(&s(&["bash"])));
        assert!(!direct_launch_splits_to_shell(&s(&["zsh", "-l"])));
        assert!(!direct_launch_splits_to_shell(&s(&["wsl.exe"])));
        assert!(!direct_launch_splits_to_shell(&s(&["ssh", "box"])));
        // Ordinary explicit commands keep the pre-existing split clone behavior.
        assert!(!direct_launch_splits_to_shell(&s(&["htop"])));
        assert!(!direct_launch_splits_to_shell(&s(&[
            "python3",
            "script.py"
        ])));
        assert!(!direct_launch_splits_to_shell(&[]));
    }

    /// Cycle 886/887: splitting/duplicating clones the focused pane's command;
    /// for WSL the dir is carried via `wsl --cd` (a Windows spawn can't `cd`
    /// into the Linux path WSL reports). Guards the pure decision.
    #[test]
    fn launch_cwd_routes_wsl_dir_through_cd_flag() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let tmp = std::env::temp_dir().to_string_lossy().into_owned();

        // Non-WSL + a real Windows dir → inherited as the spawn cwd, argv as-is.
        let (argv, cwd) = launch_cwd(s(&["pwsh.exe"]), Some(tmp.clone()));
        assert_eq!(argv, s(&["pwsh.exe"]));
        assert_eq!(cwd, Some(tmp));

        // Non-WSL + a non-directory (e.g. a Linux path) → no spawn cwd.
        let (argv, cwd) = launch_cwd(s(&["pwsh.exe"]), Some("/mnt/c/nope-xyz".into()));
        assert_eq!(argv, s(&["pwsh.exe"]));
        assert_eq!(cwd, None);

        // WSL + a reported (Linux) dir → carried via `--cd`, inserted in the
        // option section right after the launcher (cycle 894), no spawn cwd.
        let (argv, cwd) = launch_cwd(
            s(&["wsl.exe", "-d", "Ubuntu"]),
            Some("/mnt/c/Users/me/proj".into()),
        );
        assert_eq!(
            argv,
            s(&["wsl.exe", "--cd", "/mnt/c/Users/me/proj", "-d", "Ubuntu"])
        );
        assert_eq!(cwd, None);

        // Cycle 894 (audit): WSL carrying a command after `--`. `--cd` MUST
        // land before the `--` separator so it reaches WSL, not the command.
        // Appending at the end (the pre-894 bug) put it after `bash -l`.
        let (argv, cwd) = launch_cwd(
            s(&["wsl.exe", "-d", "Ubuntu", "--", "bash", "-l"]),
            Some("/home/me/proj".into()),
        );
        assert_eq!(
            argv,
            s(&[
                "wsl.exe",
                "--cd",
                "/home/me/proj",
                "-d",
                "Ubuntu",
                "--",
                "bash",
                "-l"
            ])
        );
        assert_eq!(cwd, None);

        // Cycle 894: WSL with a bare command positional (no `--`). `--cd`
        // still goes first so it isn't consumed as an argument to the command.
        let (argv, _) = launch_cwd(s(&["wsl.exe", "htop"]), Some("/home/me".into()));
        assert_eq!(argv, s(&["wsl.exe", "--cd", "/home/me", "htop"]));

        // WSL + no reported dir → unchanged argv, no spawn cwd.
        let (argv, cwd) = launch_cwd(s(&["wsl"]), None);
        assert_eq!(argv, s(&["wsl"]));
        assert_eq!(cwd, None);

        // WSL already specifying --cd → not double-injected.
        let (argv, _) = launch_cwd(
            s(&["wsl.exe", "--cd", "/home/me"]),
            Some("/mnt/c/other".into()),
        );
        assert_eq!(argv, s(&["wsl.exe", "--cd", "/home/me"]));
    }

    #[test]
    fn usable_cwd_keeps_only_existing_dirs() {
        // An existing directory is kept (new tab/split opens here).
        assert_eq!(usable_cwd(Some("/".to_string())), Some("/".to_string()));
        let tmp = std::env::temp_dir();
        assert_eq!(
            usable_cwd(Some(tmp.to_string_lossy().into_owned())),
            Some(tmp.to_string_lossy().into_owned())
        );
        // A since-deleted path or a file → fall back to the default.
        assert_eq!(usable_cwd(Some("/no/such/kettle/xyz".to_string())), None);
        assert_eq!(usable_cwd(None), None);
    }

    #[test]
    fn split_layout_tiles_without_gaps_or_overlap() {
        let mut n = Node::Leaf(1);
        assert!(n.split_leaf(1, 2, Dir::Horizontal));
        let mut rects = Vec::new();
        n.layout((0.0, 0.0, 100.0, 40.0), &mut rects);
        assert_eq!(rects.len(), 2);
        let (_, a) = rects[0];
        let (_, b) = rects[1];
        assert_eq!(a.2 + b.2, 100.0); // widths sum to full
        assert_eq!(a.0, 0.0);
        assert_eq!(b.0, a.2); // b starts where a ends
        assert_eq!(a.3, 40.0);
    }

    #[test]
    fn remove_leaf_collapses_parent() {
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Vertical);
        assert!(n.contains(2));
        // Removing one child of a 2-leaf split collapses to the sibling,
        // signalled as `Err(Some(sibling))` to the parent.
        match n.remove_leaf(2) {
            Err(Some(Node::Leaf(1))) => {}
            _ => panic!("removing one child should collapse to the sibling"),
        }
    }

    #[test]
    fn leaf_ids_walks_dfs_order() {
        // Same DFS-order traversal that nth_leaf / leaf_index_of /
        // session-save use, so any caller switching between these
        // helpers gets a consistent enumeration. Used by broadcast_write
        // (cycle 112) to scope broadcast input to a single tab.
        let single = Node::Leaf(7);
        assert_eq!(single.leaf_ids(), vec![7]);
        // Build:  Split(a=Leaf(1), b=Split(a=Leaf(2), b=Leaf(3)))
        // DFS:    [1, 2, 3]
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal);
        n.split_leaf(2, 3, Dir::Vertical);
        assert_eq!(n.leaf_ids(), vec![1, 2, 3]);
        // Symmetric with nth_leaf for the same positions.
        for (i, id) in n.leaf_ids().iter().enumerate() {
            assert_eq!(n.nth_leaf(i), *id);
        }
    }

    #[test]
    fn nested_splits_keep_all_leaves() {
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal);
        n.split_leaf(2, 3, Dir::Vertical);
        let mut rects = Vec::new();
        n.layout((0.0, 0.0, 200.0, 100.0), &mut rects);
        let ids: Vec<u64> = rects.iter().map(|(i, _)| *i).collect();
        assert!(ids.contains(&1) && ids.contains(&2) && ids.contains(&3));
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn move_active_tab_swaps_and_clamps() {
        // Build a 4-tab mux without spawning real terminals; use the leaf
        // ids as a fingerprint so we can verify the active tab actually
        // moved (not just that the index changed).
        let mut m = Mux::new();
        for id in 1..=4u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        // Move tab at index 1 (id=2) one place right → swap with id=3.
        m.active = 1;
        assert!(m.move_active_tab(1));
        assert_eq!(m.active, 2);
        assert!(matches!(m.tabs[1].root, Node::Leaf(3)));
        assert!(matches!(m.tabs[2].root, Node::Leaf(2)));
        // Move the same tab three steps right — clamps to the last index.
        assert!(m.move_active_tab(3));
        assert_eq!(m.active, 3);
        assert!(matches!(m.tabs[3].root, Node::Leaf(2)));
        // No-op moves return false: zero delta, already at the right edge.
        assert!(!m.move_active_tab(0));
        assert!(!m.move_active_tab(5));
        assert_eq!(m.active, 3);
        // Move left clamps at 0.
        assert!(m.move_active_tab(-100));
        assert_eq!(m.active, 0);
        assert!(matches!(m.tabs[0].root, Node::Leaf(2)));
        // With < 2 tabs the move is a no-op (clamp still leaves us put).
        let mut single = Mux::new();
        single.tabs.push(Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        assert!(!single.move_active_tab(1));
    }

    #[test]
    fn close_tab_at_keeps_active_valid() {
        // Build a 3-tab mux without spawning real terminals.
        let mut m = Mux::new();
        for id in 1..=3u64 {
            m.tabs.push(Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        m.active = 2; // third tab
        // Close the first tab → active shifts left to stay on the same tab.
        assert!(!m.close_tab_at(0));
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.active, 1);
        // Close the (now) last tab while it's active → clamps.
        m.active = 1;
        assert!(!m.close_tab_at(1));
        assert_eq!(m.active, 0);
        // Closing the final tab reports "empty".
        assert!(m.close_tab_at(0));
        assert!(m.tabs.is_empty());
    }

    #[test]
    fn close_focused_promotes_sibling_in_two_pane_split() {
        // Repro for the `Ctrl+Shift+E` then `Ctrl+Shift+W` regression:
        // `match Err(_)` used to conflate two distinct `Node::remove_leaf`
        // results — `Err(None)` (the focused leaf was the only one, close
        // the tab) and `Err(Some(sibling))` (the focused leaf had a
        // sibling, promote it). The wrong arm fired for the second case
        // and closed the whole tab on what should have been a per-pane
        // close. Pin the contract here so a future refactor that
        // re-conflates them fails CI rather than re-introducing the bug.
        let mut m = Mux::new();
        let mut root = Node::Leaf(10);
        assert!(root.split_leaf(10, 20, Dir::Horizontal));
        m.tabs.push(Tab {
            root,
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        // Close the focused (left) pane → tab survives with the right
        // pane promoted to root.
        assert!(!m.close_focused(), "tab should NOT be reported empty");
        assert_eq!(m.tabs.len(), 1, "tab should still exist");
        assert_eq!(m.active, 0);
        assert!(
            matches!(m.tabs[0].root, Node::Leaf(20)),
            "sibling (id=20) should be the new root after closing the focused leaf"
        );
        assert_eq!(
            m.tabs[0].focus, 20,
            "focus should move to the promoted sibling, not linger on the closed leaf"
        );
        // Closing the now-last pane drains the tab.
        assert!(m.close_focused(), "last-pane close should report empty");
        assert!(m.tabs.is_empty());
    }

    /// Cycle 602: user-reported bug. When the user splits many times
    /// and then closes a pane deep in the tree, focus jumps back to
    /// the leftmost (first focused) pane instead of the deeper
    /// neighbor of the closed pane.
    ///
    /// Repro: build tree
    ///
    ///     Split{Horiz,
    ///         a: Leaf(10),
    ///         b: Split{Vert,
    ///             a: Leaf(20),
    ///             b: Split{Horiz,
    ///                 a: Leaf(30),
    ///                 b: Leaf(40)}}}
    ///
    /// User focuses Leaf(40) and closes it. Pre-cycle-602:
    /// `tab.root.first_leaf()` returns 10 (the leftmost of the WHOLE
    /// tree). Expected: focus moves to Leaf(30) — the immediate
    /// neighbor that took 40's slot in the deepest split.
    #[test]
    fn close_focused_picks_nearest_neighbor_not_leftmost_root() {
        let mut m = Mux::new();
        // Build the 4-leaf nested tree by hand (testing the Node logic
        // directly; bypasses the Pane/PTY infra which split_leaf would
        // touch in the full Mux::split flow).
        let root = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(10)),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(20)),
                b: Box::new(Node::Split {
                    dir: Dir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(30)),
                    b: Box::new(Node::Leaf(40)),
                }),
            }),
        };
        m.tabs.push(Tab {
            root,
            focus: 40,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        assert!(!m.close_focused(), "tab still has 3 panes after closing 40");
        assert_eq!(
            m.tabs[0].focus, 30,
            "focus must move to the *nearest neighbor* (30), not jump back \
             to the leftmost-leaf-of-the-tab (10) — that's the cycle-602 bug"
        );
    }

    /// Cycle 912 (audit): `exit-action = hold` survival. Before the fix `reap`
    /// removed any child-exited pane, so Hold behaved like Close. `is_reapable`
    /// now keeps a held child-exited pane until it's explicitly closed.
    #[test]
    fn is_reapable_holds_child_exited_pane_until_closed() {
        use super::is_reapable;
        // Live pane (child still running) — never reaped.
        assert!(!is_reapable(false, false, false));
        // Default (Close): child exited, not held -> reaped.
        assert!(is_reapable(false, false, true));
        // Hold: child exited but held -> NOT reaped (the cycle-912 fix).
        assert!(!is_reapable(false, true, true));
        // Explicit close (ClosePane / Restart set `closed`) always reaps, even
        // a held pane — so the user can still dismiss a held dead shell.
        assert!(is_reapable(true, true, true));
        assert!(is_reapable(true, false, false));
    }

    /// Cycle 603: companion to cycle 602's close-focused fix —
    /// the PTY-died-while-focused path through `reap_tabs` had
    /// the same `tab.root.first_leaf()` anti-pattern. When the
    /// user runs `exit` in the focused pane (or its process
    /// crashes), focus should land on the immediate neighbor,
    /// not jump back to the leftmost leaf of the whole tab.
    ///
    /// Same 4-leaf tree as cycle 602's test: focus = 40, reap
    /// dead leaf 40. Pre-cycle-603: focus = 10 (leftmost). Post-
    /// fix: focus = 30 (the immediate neighbor of 40).
    #[test]
    fn reap_tabs_promotes_neighbor_when_focused_pane_dies() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Split {
                    dir: Dir::Vertical,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(20)),
                    b: Box::new(Node::Split {
                        dir: Dir::Horizontal,
                        ratio: 0.5,
                        a: Box::new(Node::Leaf(30)),
                        b: Box::new(Node::Leaf(40)),
                    }),
                }),
            },
            focus: 40,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        // Pane 40's PTY exits → reap it.
        Mux::reap_tabs(&mut tabs, &mut active, &[40]);
        assert_eq!(tabs.len(), 1, "tab survives with 3 panes");
        assert_eq!(
            tabs[0].focus, 30,
            "focus must move to the *nearest neighbor* (30), not jump back \
             to the leftmost-leaf-of-the-tab (10) — that's the cycle-603 bug"
        );
    }

    /// Cycle 603 part-B: the EXISTING `reap_tabs` match arm
    /// conflated `Err(None)` (tab is empty) with `Err(Some(sibling))`
    /// (focused leaf was a direct child of root and the sibling
    /// was promoted). For a 2-pane tab where one pane's PTY exits,
    /// `remove_leaf` returns `Err(Some(surviving_sibling))` — and
    /// the pre-fix `Err(_) => tabs.remove(ti)` arm then deleted
    /// the WHOLE tab, losing the surviving sibling along with it.
    ///
    /// Latent bug surfaced by cycle 603's broader audit: any
    /// 2-pane tab + `exit` in either pane = both panes vanish.
    /// Reachable in production via the `child_exited()` check in
    /// `Mux::reap`.
    #[test]
    fn reap_tabs_preserves_tab_when_2_pane_split_has_one_pane_exit() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Leaf(20)),
            },
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        // Pane 20's PTY exits. Pre-fix: tab is removed — the
        // surviving Leaf(10) goes with it. Post-fix: tab survives
        // with root collapsed to Leaf(10).
        Mux::reap_tabs(&mut tabs, &mut active, &[20]);
        assert_eq!(
            tabs.len(),
            1,
            "tab must survive a 2-pane sibling promotion (pre-fix this \
             was 0 — `Err(_) => tabs.remove(ti)` ate the surviving pane)"
        );
        assert!(matches!(tabs[0].root, Node::Leaf(10)));
        assert_eq!(tabs[0].focus, 10);
    }

    /// Cycle 603 negative-case: if the dying pane is NOT the
    /// focused one, focus must stay put — the existing
    /// `contains(focus)` guard already covers this, so this test
    /// catches a regression where the cycle-603 neighbor-capture
    /// accidentally triggers for non-focused dyings.
    #[test]
    fn reap_tabs_keeps_focus_when_dying_pane_is_not_focused() {
        let mut tabs = vec![Tab {
            root: Node::Split {
                dir: Dir::Horizontal,
                ratio: 0.5,
                a: Box::new(Node::Leaf(10)),
                b: Box::new(Node::Leaf(20)),
            },
            // Focus is on 10; pane 20's PTY dies.
            focus: 10,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        }];
        let mut active = 0;
        Mux::reap_tabs(&mut tabs, &mut active, &[20]);
        assert_eq!(
            tabs[0].focus, 10,
            "focus on 10 must survive — pane 20's death shouldn't move focus"
        );
    }

    /// Cycle 602: companion to the test above. The `neighbor_of`
    /// helper drives the focus-restoration. Asserts the contract
    /// directly so a future refactor of `close_focused` that
    /// stops calling `neighbor_of` (or breaks the helper) fails
    /// the gauntlet rather than re-introducing the user-reported
    /// bug.
    #[test]
    fn node_neighbor_of_finds_sibling_subtree_first_leaf() {
        // Same shape as the close-focused repro above.
        let root = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(10)),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(20)),
                b: Box::new(Node::Split {
                    dir: Dir::Horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(30)),
                    b: Box::new(Node::Leaf(40)),
                }),
            }),
        };
        // Neighbor of the deepest right leaf is its split-mate (30).
        assert_eq!(root.neighbor_of(40), Some(30));
        // Neighbor of 30 is 40 (the other side of the deepest split).
        assert_eq!(root.neighbor_of(30), Some(40));
        // Neighbor of 20 is the first leaf of its sibling subtree
        // (Split{30, 40}) → 30.
        assert_eq!(root.neighbor_of(20), Some(30));
        // Neighbor of 10 is the first leaf of its sibling subtree
        // (the deeper Split) → 20.
        assert_eq!(root.neighbor_of(10), Some(20));
        // Leaf id not in the tree → None.
        assert_eq!(root.neighbor_of(999), None);
        // Single-leaf tree → no neighbor.
        let lonely = Node::Leaf(1);
        assert_eq!(lonely.neighbor_of(1), None);
    }

    #[test]
    fn classify_tab_activity_picks_the_right_indicator() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let earlier = now - Duration::from_secs(5);
        let later = now + Duration::from_secs(5);
        // Default 10 s silence threshold matches the config default;
        // the existing transitions still fire under it.
        let silence = Duration::from_secs(10);

        // Active tab → always Normal, regardless of output / bell. The
        // focused-tab accent + window-title already telegraph "you're
        // here" so adding a dot would be redundant.
        assert_eq!(
            classify_tab_activity(true, true, Some(later), Some(earlier), now, silence),
            TabActivity::Normal
        );
        assert_eq!(
            classify_tab_activity(true, false, Some(later), Some(earlier), now, silence),
            TabActivity::Normal
        );

        // Inactive tab + bell → Bell, regardless of output state.
        // Bell is the stronger signal (the focused program explicitly
        // asked for attention) so it wins over plain output activity.
        assert_eq!(
            classify_tab_activity(false, true, None, None, now, silence),
            TabActivity::Bell
        );
        assert_eq!(
            classify_tab_activity(false, true, Some(later), Some(earlier), now, silence),
            TabActivity::Bell
        );

        // Inactive tab + output after last-seen → Output (fresh, hasn't
        // exceeded silence threshold yet).
        assert_eq!(
            classify_tab_activity(false, false, Some(later), Some(earlier), now, silence),
            TabActivity::Output
        );

        // Inactive tab + output BEFORE the user last looked → Normal.
        // The user already saw this output; no need to nudge again.
        assert_eq!(
            classify_tab_activity(false, false, Some(earlier), Some(later), now, silence),
            TabActivity::Normal
        );

        // First-output edge: no last_seen_at yet → Output (the user
        // has never been on this tab and something happened on it).
        assert_eq!(
            classify_tab_activity(false, false, Some(later), None, now, silence),
            TabActivity::Output
        );

        // No activity recorded at all → Normal.
        assert_eq!(
            classify_tab_activity(false, false, None, None, now, silence),
            TabActivity::Normal
        );
        assert_eq!(
            classify_tab_activity(false, false, None, Some(earlier), now, silence),
            TabActivity::Normal
        );
    }

    #[test]
    fn classify_tab_activity_transitions_to_silent_after_threshold() {
        // Cycle 252: Output → Silent transition once the last unseen
        // output is older than the silence threshold. The test fakes
        // a clock by passing `now` explicitly — same trick the
        // primary classifier test uses, keeping the function pure.
        use std::time::{Duration, Instant};
        let base = Instant::now();
        let silence = Duration::from_secs(10);
        // Tab last looked at 60 s ago; output arrived at 30 s ago
        // (so unseen — output > seen).
        let last_seen = base - Duration::from_secs(60);
        let last_out = base - Duration::from_secs(30);
        // Just-after-output: 5 s elapsed since output, below the 10 s
        // threshold → Output.
        let now_fresh = last_out + Duration::from_secs(5);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_fresh,
                silence
            ),
            TabActivity::Output,
            "5 s after output should still be Output (threshold = 10 s)"
        );
        // Exactly at threshold: 10 s elapsed → Silent (the `>=` arm).
        let now_at_threshold = last_out + silence;
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_at_threshold,
                silence
            ),
            TabActivity::Silent,
            "elapsed = threshold should be Silent (inclusive boundary)"
        );
        // Well past threshold: 30 s elapsed → Silent.
        let now_late = last_out + Duration::from_secs(30);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_late,
                silence
            ),
            TabActivity::Silent
        );
        // Bell still beats Silent — explicit attention wins over
        // implicit "things stopped" signal.
        assert_eq!(
            classify_tab_activity(
                false,
                true,
                Some(last_out),
                Some(last_seen),
                now_late,
                silence
            ),
            TabActivity::Bell
        );
        // Backward clock (now < last_out — should only happen with a
        // bug or clock-skew adjustment between calls): treat as fresh
        // Output rather than triggering Silent on a saturating-zero
        // duration.
        let now_before = last_out - Duration::from_secs(1);
        assert_eq!(
            classify_tab_activity(
                false,
                false,
                Some(last_out),
                Some(last_seen),
                now_before,
                silence
            ),
            TabActivity::Output,
            "backward clock should NOT trigger Silent"
        );
    }

    #[test]
    fn closed_tab_ring_bounded_and_lifo() {
        // Cycle 247: snapshot ring is bounded at `CLOSED_TAB_RING_CAP`
        // and pops LIFO (most-recent first). Builds a fake ring
        // directly so we don't need to spawn real PTYs.
        let mut m = Mux::new();
        for i in 0..(super::CLOSED_TAB_RING_CAP + 3) {
            if m.closed_tabs.len() >= super::CLOSED_TAB_RING_CAP {
                m.closed_tabs.pop_front();
            }
            m.closed_tabs.push_back(super::ClosedTab {
                original_index: i,
                argv: vec![format!("argv-{i}")],
                cwd: Some(format!("/tmp/{i}")),
            });
        }
        // Cap honored: oldest 3 entries fell off the front.
        assert_eq!(m.closed_tabs.len(), super::CLOSED_TAB_RING_CAP);
        assert_eq!(m.closed_tabs.front().unwrap().original_index, 3);
        assert_eq!(
            m.closed_tabs.back().unwrap().original_index,
            super::CLOSED_TAB_RING_CAP + 2
        );
        // LIFO: pop_back gives the most-recently-closed snapshot.
        let last = m.closed_tabs.pop_back().unwrap();
        assert_eq!(last.original_index, super::CLOSED_TAB_RING_CAP + 2);
        assert_eq!(
            last.argv,
            vec![format!("argv-{}", super::CLOSED_TAB_RING_CAP + 2)]
        );
    }

    #[test]
    fn reap_tabs_keeps_active_pointed_at_the_same_tab() {
        // Cycle-120 contract. `reap` used to handle only the "active
        // tab was the last one and the list shrunk" case via the
        // trailing clamp, missing the much more common "a tab BEFORE
        // active died" case which silently shifted what `active`
        // pointed to. Each scenario builds a fresh `tabs` Vec where
        // we can recognize each tab by its single leaf id, then
        // calls `reap_tabs` with the dead set and asserts which
        // leaf id `active` now indexes.
        fn tab(id: u64) -> Tab {
            Tab {
                root: Node::Leaf(id),
                focus: id,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            }
        }
        // Scenario 1 (the cycle-120 bug): focused on the middle tab
        // (B); the leftmost tab (A) dies. Pre-fix: active stayed 1
        // and now indexed C — focus silently jumped past B. Post-
        // fix: active decrements to 0 so it still points at B.
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 1; // B
        Mux::reap_tabs(&mut tabs, &mut active, &[1]); // A dies
        assert_eq!(tabs.len(), 2);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 2, "still focused on B"),
            _ => panic!("expected leaf"),
        }
        // Scenario 2: focused on the rightmost (C); leftmost (A) dies.
        // Pre-fix: trailing-clamp didn't fire (active was still in
        // bounds), so active=2 became C's new neighbor — wrong.
        // Post-fix: decrements 2→1, still C.
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 2;
        Mux::reap_tabs(&mut tabs, &mut active, &[1]);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "still focused on C"),
            _ => panic!("expected leaf"),
        }
        // Scenario 3: the active tab itself dies. Focus should fall
        // on its right neighbor (matches every modern terminal's
        // close-current-tab behavior).
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 1; // B
        Mux::reap_tabs(&mut tabs, &mut active, &[2]); // B dies
        assert_eq!(tabs.len(), 2);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "active falls on right neighbor"),
            _ => panic!("expected leaf"),
        }
        // Scenario 4: active is the LAST tab and dies — trailing-clamp
        // brings active back to the new last tab (the existing
        // behavior; regression guard).
        let mut tabs = vec![tab(1), tab(2), tab(3)];
        let mut active = 2;
        Mux::reap_tabs(&mut tabs, &mut active, &[3]);
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 2, "active clamped to new last"),
            _ => panic!("expected leaf"),
        }
        // Scenario 5: multiple dead. focused on C (index 2); A and B
        // both die.
        let mut tabs = vec![tab(1), tab(2), tab(3), tab(4)];
        let mut active = 2; // C
        Mux::reap_tabs(&mut tabs, &mut active, &[1, 2]); // A + B die
        match tabs[active].root {
            Node::Leaf(id) => assert_eq!(id, 3, "still focused on C"),
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn close_window_drops_every_tab_and_pane() {
        // Cycle 113: close_window is *not* an alias for close_tab.
        // Build a multi-tab, multi-pane mux and verify everything is
        // gone after close_window, including the active index reset.
        let mut m = Mux::new();
        for id in 1..=3u64 {
            // Each tab is a 2-pane split so we also confirm both
            // panes-per-tab get reaped (not just the focused leaf).
            let mut root = Node::Leaf(id * 10);
            root.split_leaf(id * 10, id * 10 + 1, Dir::Horizontal);
            m.tabs.push(Tab {
                root,
                focus: id * 10,
                title_override: None,
                zoomed: false,
                last_output_at: None,
                last_seen_at: None,
                bell: false,
            });
        }
        m.active = 1;
        // Sanity: pre-state has tabs (panes map is empty in this test
        // because we didn't spawn real Pane records — we only need to
        // observe the tab + active-index reset).
        assert_eq!(m.tabs.len(), 3);
        assert_eq!(m.active, 1);

        let empty = m.close_window();
        assert!(empty, "close_window always reports the mux empty");
        assert!(m.tabs.is_empty(), "all tabs gone");
        assert!(m.panes.is_empty(), "all panes gone");
        assert_eq!(m.active, 0, "active reset to 0");
    }

    #[test]
    fn insert_split_exits_zoom_and_focuses_new_pane() {
        // Cycle-130 contract. With a single-leaf tab zoomed (one pane
        // visible), splitting should produce a 2-leaf tab, focus the
        // new pane, and exit zoom so the user sees both halves —
        // matching tmux / WezTerm. Pre-cycle, zoom stayed on and the
        // old half silently hid.
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: true, // already zoomed before the split
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        super::insert_split(&mut tab, 2, Dir::Horizontal);
        assert_eq!(tab.focus, 2, "focus moves to the new pane");
        assert!(!tab.zoomed, "zoom is exited so both halves render");
        // Tree now contains both leaves.
        let mut rects = Vec::new();
        tab.root.layout((0.0, 0.0, 100.0, 50.0), &mut rects);
        assert_eq!(rects.len(), 2, "split produced two leaves");

        // Unzoomed → unzoomed (no-op on the zoom flag).
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 1,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        super::insert_split(&mut tab, 2, Dir::Vertical);
        assert!(!tab.zoomed);
        assert_eq!(tab.focus, 2);
    }

    /// Cycle 919 (audit L5): the cycle-917 stale-focus retry. When `tab.focus`
    /// points at a leaf NOT in the tree (a focus-desync), `split_leaf` no-ops on
    /// the stale id; `insert_split` must repair focus to `first_leaf()`, retry,
    /// graft the new pane, and return true — instead of the old silent no-op that
    /// orphaned the just-spawned pane (a leaked PTY). The existing test always
    /// has focus on a valid leaf, so it never exercised this branch.
    #[test]
    fn insert_split_repairs_stale_focus_and_grafts() {
        let mut tab = Tab {
            root: Node::Leaf(1),
            focus: 99, // stale: not a leaf in the tree
            zoomed: true,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        };
        assert!(
            super::insert_split(&mut tab, 2, Dir::Horizontal),
            "stale focus must be repaired + the split grafted (returns true)"
        );
        assert_eq!(tab.focus, 2, "focus moves to the newly-grafted pane");
        assert!(!tab.zoomed, "zoom exited");
        let mut rects = Vec::new();
        tab.root.layout((0.0, 0.0, 100.0, 50.0), &mut rects);
        let ids: Vec<u64> = rects.iter().map(|(id, _)| *id).collect();
        assert!(
            ids.contains(&1) && ids.contains(&2),
            "both leaves present: {ids:?}"
        );
    }

    /// Cycle 919 (audit L5) drift guard: every split caller that may graft a
    /// freshly-spawned pane must REAP it (`self.panes.remove(&new_id)`) if the
    /// graft fails, instead of leaking the PTY/child. There are three such
    /// callers (`split`, `split_with`, `duplicate_focused_pane`); `>= 3` lets a
    /// future fourth variant be added without silently skipping the reap (it
    /// would have to add the reap to keep the count, or fail this guard).
    #[test]
    fn split_callers_reap_orphaned_pane_on_graft_failure() {
        let src = include_str!("mux.rs");
        let reaps = src.matches("self.panes.remove(&new_id)").count();
        assert!(
            reaps >= 3,
            "expected >= 3 orphan-reap sites (split / split_with / duplicate_focused_pane); found {reaps}"
        );
    }

    #[test]
    fn zoom_collapses_layout_to_focused_pane() {
        let mut m = Mux::new();
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Dir::Horizontal);
        m.tabs.push(Tab {
            root,
            focus: 2,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
            title_override: None,
        });
        m.active = 0;
        assert_eq!(m.layout(0, (0.0, 0.0, 100.0, 50.0)).len(), 2);
        m.toggle_zoom();
        let z = m.layout(0, (0.0, 0.0, 100.0, 50.0));
        assert_eq!(z.len(), 1);
        assert_eq!(z[0], (2, (0.0, 0.0, 100.0, 50.0)));
        m.toggle_zoom();
        assert_eq!(m.layout(0, (0.0, 0.0, 100.0, 50.0)).len(), 2);
    }

    #[test]
    fn serialize_tab_handles_out_of_range_idx() {
        // Cycle 397 drift guard. Out-of-range index returns
        // None without panic.
        let m = Mux::new();
        assert!(m.serialize_tab(0).is_none());
        assert!(m.serialize_tab(99).is_none());
    }

    #[test]
    fn mux_new_starts_with_broadcast_off() {
        // Cycle 560 drift guard. A fresh Mux MUST start with
        // broadcast disabled. The cycle-357 bug seeded broadcast=true
        // from `broadcast_default = group` (the default), so every
        // kettle window started broadcasting input across all panes
        // in the active tab — users typing in one pane saw the
        // input mirrored everywhere.
        //
        // The fix (cycle 560) removed the bad seeding in App::new;
        // this guard pins the Mux::new contract so a future App-
        // side re-introduction of broadcast-on-startup gets caught
        // by the App-side construction path being out of sync with
        // this baseline.
        let m = Mux::new();
        assert!(
            !m.is_broadcast_on(),
            "Mux::new must start with broadcast disabled; \
             enabling at startup mirrors keystrokes across panes \
             without the user opting in"
        );
        assert_eq!(m.broadcast, BroadcastScope::Off);
    }

    #[test]
    fn extract_and_insert_tab_roundtrip() {
        // Cycle 398 drift guard. extract_tab → insert_tab
        // reproduces the same tab + the active idx tracks
        // correctly across the operation.
        let mut m = Mux::new();
        let mk = |id: u64| Tab {
            root: Node::Leaf(id),
            focus: id,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        };
        m.tabs.push(mk(1));
        m.tabs.push(mk(2));
        m.tabs.push(mk(3));
        m.active = 2; // focus on tab 3.
        let extracted = m.extract_tab(1).expect("extract 1");
        // Tab 2 removed; remaining tabs are [1, 3].
        assert_eq!(m.tabs.len(), 2);
        // active=2 was past the removed idx; clamped to 1.
        assert_eq!(m.active, 1);
        // Insert the extracted tab back at the head.
        m.insert_tab(0, extracted);
        // Tabs are now [2, 1, 3]; active=0 (insert_tab sets
        // active to the new position so the moved tab is focused).
        assert_eq!(m.tabs.len(), 3);
        assert_eq!(m.active, 0);
        // Out-of-range extract returns None.
        assert!(m.extract_tab(99).is_none());
        // Out-of-range insert clamps to end.
        m.insert_tab(99, mk(4));
        assert_eq!(m.tabs.len(), 4);
        assert_eq!(m.active, 3);
    }

    #[test]
    fn detach_attach_tab_moves_between_muxes() {
        // C2 (multi-window) drift guard: detach_tab → attach_tab moves a tab
        // from one Mux to another with the same index semantics as the
        // extract/insert pair it composes, does NOT snapshot to closed_tabs
        // (the tab is moving, not closing), and the source's active index
        // stays valid.
        let mk = |id: u64| Tab {
            root: Node::Leaf(id),
            focus: id,
            title_override: None,
            zoomed: false,
            last_output_at: None,
            last_seen_at: None,
            bell: false,
        };
        let mut src = Mux::new();
        src.tabs.push(mk(101));
        src.tabs.push(mk(102));
        src.tabs.push(mk(103));
        src.active = 1; // detach the active tab itself
        let dt = src.detach_tab(1).expect("detach");
        assert_eq!(dt.tab.focus, 102);
        // Panes vec is empty here (no real PTYs in this fixture) — the pane
        // transfer itself is exercised by the C5 live-move e2e.
        assert!(dt.panes.is_empty());
        assert_eq!(src.tabs.len(), 2);
        // Removing the active tab keeps focus position (right neighbor
        // slides in), clamped — extract_tab semantics.
        assert_eq!(src.active, 1);
        assert!(
            src.closed_tabs.is_empty(),
            "a moved tab must not appear in the undo-close ring"
        );

        let mut dst = Mux::new();
        dst.tabs.push(mk(201));
        let at = dst.attach_tab(dt, None);
        assert_eq!(at, 1, "None appends");
        assert_eq!(dst.tabs.len(), 2);
        assert_eq!(dst.active, 1, "the attached tab becomes active");
        assert_eq!(dst.tabs[1].focus, 102);

        // Out-of-range detach is None; attach at an oversized index clamps.
        assert!(src.detach_tab(99).is_none());
        let dt2 = src.detach_tab(0).expect("detach head");
        let at2 = dst.attach_tab(dt2, Some(99));
        assert_eq!(at2, 2, "oversized attach index clamps to append");
    }

    #[test]
    fn pane_id_allocator_is_process_global() {
        // C2 drift guard: pane ids come from the shared NEXT_PANE_ID static
        // (never a per-Mux counter), so ids stay unique across every window's
        // Mux — the agent API, Lua hooks, and pending_runs address panes by
        // bare id, and a live tab move carries ids into another Mux. If this
        // stops compiling because NEXT_PANE_ID is gone, per-Mux ids came back.
        let a = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let b = NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(b > a, "monotonic process-wide allocation");
    }
}
