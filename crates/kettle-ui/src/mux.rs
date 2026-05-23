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
#[allow(dead_code)] // Tab/All/Group consumed by future named-groups sub-cycles.
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
#[allow(dead_code)] // consumed by future named-groups sub-cycles (the broadcast_write migration)
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
        BroadcastScope::Group(name) => all_panes_with_groups
            .iter()
            .filter(|(_, g)| g.as_deref() == Some(name.as_str()))
            .map(|(id, _)| *id)
            .collect(),
    }
}

pub struct Mux {
    pub tabs: Vec<Tab>,
    pub panes: HashMap<u64, Pane>,
    pub active: usize,
    pub search: SearchState,
    pub broadcast: bool,
    /// Cycle 378: set when a LuaEngine subscribes at App startup.
    /// Controls whether spawn_pane attaches the output sidechannel
    /// to new PTYs (zero-cost when false: no per-PTY-read alloc).
    pub lua_output_subscribed: bool,
    /// Ring buffer of recently-closed tab snapshots (cycle 247).
    /// Bounded so a long-running session doesn't accumulate state.
    /// LIFO: `pop_back` returns the most-recently-closed tab.
    pub closed_tabs: std::collections::VecDeque<ClosedTab>,
    next_id: u64,
}

impl Mux {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            panes: HashMap::new(),
            active: 0,
            search: SearchState::default(),
            broadcast: false,
            lua_output_subscribed: false,
            closed_tabs: std::collections::VecDeque::with_capacity(CLOSED_TAB_RING_CAP),
            next_id: 1,
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
        let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) = crossbeam_channel::unbounded();
        // Cycle 378 (Terminator plugin parity, plugin sub-cycle 3):
        // optional output sidechannel for LuaEvent::Output emission.
        // The Mux's output_tx is set when a LuaEngine subscribes
        // (App configures it post-construction); None when no
        // plugin is listening so the alloc-per-PTY-read is skipped.
        let (out_tx, out_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::bounded(64);
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
            cols.max(1),
            rows.max(1),
            cw,
            ch,
            cfg.cursor_blink,
            engine_cursor_shape(cfg.cursor_style),
            Some(cfg.word_delimiters.as_str()),
            &cfg.term,
            &cfg.colorterm,
            cfg.login_shell,
            tx,
            waker,
            output_tx,
        )?;
        let id = self.next_id;
        self.next_id += 1;
        let initial_title = initial_pane_title(argv);
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
                group_name: None,
                closed: false,
                last_history: None,
                argv: argv.to_vec(),
                remote_context: None,
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
    /// the index is out-of-range. Used by the future detachable-
    /// tabs path (cycle-363 design doc sub-cycles 7+8) to wire
    /// up cross-process tab handoff via JSON-over-Unix-socket.
    /// For now: pure-data utility callable from drift guards.
    /// `#[allow(dead_code)]` because the cross-process IPC caller
    /// is the multi-week thread (sub-cycles 7+8); this API ships
    /// as the foundation those cycles consume.
    #[allow(dead_code)]
    pub fn serialize_tab(&self, idx: usize) -> Option<STab> {
        let t = self.tabs.get(idx)?;
        Some(STab {
            root: self.snap(&t.root),
            focus: t.root.leaf_index_of(t.focus).unwrap_or(0),
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
                })
                .collect(),
            active: self.active,
            // Filled in by App::save_session (it owns the active theme).
            theme: None,
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
    ) -> Result<Node> {
        match n {
            SNode::Leaf { cwd, cmd } => {
                let argv = if cmd.is_empty() {
                    shell_argv(cfg)
                } else {
                    cmd.clone()
                };
                let id = self.spawn_pane(cfg, 80, 24, cw, ch, mk(), cwd.as_deref(), &argv)?;
                Ok(Node::Leaf(id))
            }
            SNode::Split {
                vertical,
                ratio,
                a,
                b,
            } => {
                let a = self.build_node(a, cfg, cw, ch, mk)?;
                let b = self.build_node(b, cfg, cw, ch, mk)?;
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
        for (i, st) in s.tabs.iter().enumerate() {
            match self.build_node(&st.root, cfg, cw, ch, mk) {
                Ok(root) => {
                    // Restore the focused leaf at its DFS index (saved
                    // by `snapshot`). `nth_leaf` falls back to the
                    // first leaf if the index is past the end, which
                    // keeps trimmed-tree sessions sane.
                    let focus = root.nth_leaf(st.focus);
                    self.tabs.push(Tab {
                        root,
                        focus,
                        title_override: None,
                        zoomed: false,
                        last_output_at: None,
                        last_seen_at: None,
                        bell: false,
                    });
                }
                Err(e) => {
                    // Don't fail the whole restore — a single broken
                    // tab (e.g. saved cwd no longer exists, PTY
                    // allocation under quota) shouldn't sink the
                    // others. But surface it in the log so a user
                    // wondering "where did my session go?" can see
                    // the cause under `RUST_LOG=warn` (the default
                    // filter). Pre-fix this was a silent skip — the
                    // user just saw fewer tabs than they remembered.
                    log::warn!("session restore: tab {i} failed to rebuild and was skipped: {e}");
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
        let argv = shell_argv(cfg);
        let cwd = self.focused_cwd();
        let new_id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            insert_split(tab, new_id, dir);
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

    /// Toggle zoom (maximize the focused pane) for the active tab.
    pub fn toggle_zoom(&mut self) {
        let a = self.active;
        if let Some(t) = self.tabs.get_mut(a) {
            t.zoomed = !t.zoomed;
        }
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
        fn walk(node: &mut Node, target: u64, clockwise: bool) -> bool {
            if let Node::Split { dir, a, b, .. } = node {
                let a_has = a.contains(target);
                let b_has = b.contains(target);
                if (a_has || b_has)
                    && (matches!(**a, Node::Leaf(_)) || matches!(**b, Node::Leaf(_)))
                {
                    // This Split is the focused leaf's immediate
                    // parent. Flip direction + swap children for the
                    // clockwise rotation per Terminator semantics.
                    *dir = match *dir {
                        Dir::Horizontal => Dir::Vertical,
                        Dir::Vertical => Dir::Horizontal,
                    };
                    if clockwise {
                        std::mem::swap(a, b);
                    }
                    return true;
                }
                if a_has && walk(a, target, clockwise) {
                    return true;
                }
                if b_has && walk(b, target, clockwise) {
                    return true;
                }
            }
            false
        }
        walk(&mut tab.root, focus, clockwise)
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

    /// Move focus to the nearest pane in a direction (by center distance).
    pub fn focus_dir(&mut self, area: Rect, dx: i32, dy: i32) {
        let a = self.active;
        let rects = self.layout(a, area);
        let Some(tab) = self.tabs.get_mut(a) else {
            return;
        };
        let Some(&(_, (fx, fy, fw, fh))) = rects.iter().find(|(id, _)| *id == tab.focus) else {
            return;
        };
        let (fcx, fcy) = (fx + fw / 2.0, fy + fh / 2.0);
        let mut best: Option<(f32, u64)> = None;
        for (id, (x, y, w, h)) in &rects {
            if *id == tab.focus {
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
        if let Some((_, id)) = best {
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
                    // `neighbor` is None only if the focus pane wasn't
                    // in the tree (logic error) or the tree was a single
                    // Leaf (handled by Err(None) below). Fall back to
                    // first_leaf so a stale focus pointer doesn't crash
                    // — same defensive shape as pre-cycle-602.
                    tab.focus = neighbor.unwrap_or_else(|| tab.root.first_leaf());
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
    #[allow(dead_code)]
    pub fn extract_tab(&mut self, idx: usize) -> Option<Tab> {
        if idx >= self.tabs.len() {
            return None;
        }
        // Bound the active idx so it points at a still-existing tab.
        if self.active >= idx && self.active > 0 {
            self.active -= 1;
        }
        Some(self.tabs.remove(idx))
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
        let (argv, cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), usable_cwd(pane.term.current_dir())),
            None => return self.new_tab(cfg, cols, rows, cw, ch, waker),
        };
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
        let (argv, cwd) = match self.active_focus().and_then(|id| self.panes.get(&id)) {
            Some(pane) => (pane.argv.clone(), usable_cwd(pane.term.current_dir())),
            None => return Ok(()),
        };
        let new_id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, cwd.as_deref(), &argv)?;
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            insert_split(tab, new_id, dir);
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
                if p.closed || p.term.child_exited() {
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
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let ids = tab.root.leaf_ids();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                p.term.write(bytes);
            }
        }
    }

    /// Snap every pane in the active tab's broadcast set back to the
    /// bottom of its scrollback. Cycle-173 companion to
    /// `broadcast_write`: `scroll-on-keystroke` (default true) needs to
    /// apply to every targeted pane, not just the focused one, otherwise
    /// the user broadcasting input to N panes sees a confusing mismatch
    /// (typing reaches the remote shells but the local view of any
    /// scrolled-back pane stays pinned to history). Same scoping as
    /// `broadcast_write` — active tab's leaves only, never other tabs.
    pub fn broadcast_scroll_to_bottom(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let ids = tab.root.leaf_ids();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id)
                && let Ok(mut t) = p.term.term.lock()
            {
                t.scroll_display(kettle_core::Scroll::Bottom);
            }
        }
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
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let ids = tab.root.leaf_ids();
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
                p.term.write(bytes);
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
                // Most shells set the title quickly via OSC 2 on every
                // prompt — but until that first prompt fires, our default
                // placeholder "kettle" is the only string we have. Fall
                // back to the focused pane's cwd basename in that gap so
                // a fresh tab opened in `~/Repos/kettle` reads as
                // `kettle` instead of the literal program name (matches
                // iTerm2 / Ghostty / WezTerm where untitled tabs show
                // the cwd / shell). Only used while the title is the
                // placeholder — once a shell sets a real one, that wins.
                if title.is_empty() || title == "kettle" {
                    if let Some(cwd) = pane.and_then(|p| p.term.current_dir())
                        && let Some(name) = std::path::Path::new(&cwd)
                            .file_name()
                            .and_then(|s| s.to_str())
                        && !name.is_empty()
                    {
                        return name.to_string();
                    }
                    return format!("tab {}", i + 1);
                }
                title.to_string()
            })
            .collect()
    }
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
fn insert_split(tab: &mut Tab, new_id: u64, dir: Dir) {
    let focus = tab.focus;
    if tab.root.split_leaf(focus, new_id, dir) {
        tab.focus = new_id;
        tab.zoomed = false;
    }
}

fn shell_argv(cfg: &Config) -> Vec<String> {
    match &cfg.shell {
        Some(s) => vec![s.clone()],
        None => Vec::new(),
    }
}

/// Keep a candidate cwd only if it still names an existing directory — a
/// pane may have been `cd`'d into a since-removed path, in which case a new
/// tab/split should fall back to the default rather than fail to spawn.
fn usable_cwd(dir: Option<String>) -> Option<String> {
    dir.filter(|d| std::path::Path::new(d).is_dir())
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
        // Group("fleet"): every pane tagged "fleet", regardless of tab.
        assert_eq!(
            compute_broadcast_targets(
                &BroadcastScope::Group("fleet".to_string()),
                2,
                &in_tab,
                &all
            ),
            vec![1, 2, 5]
        );
        // Group with no matches → empty.
        assert!(
            compute_broadcast_targets(
                &BroadcastScope::Group("nonexistent".to_string()),
                2,
                &in_tab,
                &all
            )
            .is_empty()
        );
        // Default scope is Off.
        assert_eq!(BroadcastScope::default(), BroadcastScope::Off);
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
            !m.broadcast,
            "Mux::new must start with broadcast disabled; \
             enabling at startup mirrors keystrokes across panes \
             without the user opting in"
        );
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
}
