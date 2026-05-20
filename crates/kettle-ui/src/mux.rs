//! Tabs + a binary split tree (Terminator-style tiling). Each leaf owns an
//! independent terminal; splits tile the tab area; focus moves by geometry.

use std::collections::HashMap;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use kettle_config::{Config, CursorStyle};
use kettle_core::{CursorShape, TermEvent, Terminal, Waker};

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
    pub title: String,
    pub closed: bool,
    /// Scrollback `history_size()` observed at the *previous* redraw — used
    /// to detect new output for `scroll-on-output`. `None` while no frame
    /// has been drawn yet (so the first frame doesn't look like growth).
    pub last_history: Option<usize>,
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
    /// When true, only the focused pane is shown at full size.
    pub zoomed: bool,
}

pub struct Mux {
    pub tabs: Vec<Tab>,
    pub panes: HashMap<u64, Pane>,
    pub active: usize,
    pub search: SearchState,
    pub broadcast: bool,
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
            next_id: 1,
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
        let term = Terminal::new(
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
            tx,
            waker,
        )?;
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(
            id,
            Pane {
                term,
                rx,
                title: "kettle".into(),
                closed: false,
                last_history: None,
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
        for st in &s.tabs {
            if let Ok(root) = self.build_node(&st.root, cfg, cw, ch, mk) {
                // Restore the focused leaf at its DFS index (saved by
                // `snapshot`). `nth_leaf` falls back to the first leaf
                // if the index is past the end, which keeps trimmed-
                // tree sessions sane.
                let focus = root.nth_leaf(st.focus);
                self.tabs.push(Tab {
                    root,
                    focus,
                    zoomed: false,
                });
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
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
        });
        self.active = self.tabs.len() - 1;
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
        let id = self.spawn_pane(cfg, cols, rows, cw, ch, waker, None, &argv)?;
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
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
            let focus = tab.focus;
            if tab.root.split_leaf(focus, new_id, dir) {
                tab.focus = new_id;
            }
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

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
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
    pub fn close_focused(&mut self) -> bool {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            let focus = tab.focus;
            let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
            match root.remove_leaf(focus) {
                Ok(n) => {
                    tab.root = n;
                    tab.focus = tab.root.first_leaf();
                    self.panes.remove(&focus);
                }
                Err(_) => {
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

    /// Close the tab at `idx` (all its panes). Returns true if no tabs remain.
    pub fn close_tab_at(&mut self, idx: usize) -> bool {
        if idx < self.tabs.len() {
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
        for id in dead {
            self.panes.remove(&id);
            let mut ti = 0;
            while ti < self.tabs.len() {
                let root = std::mem::replace(&mut self.tabs[ti].root, Node::Leaf(0));
                match root.remove_leaf(id) {
                    Ok(n) => {
                        self.tabs[ti].root = n;
                        if !self.tabs[ti].root.contains(self.tabs[ti].focus) {
                            self.tabs[ti].focus = self.tabs[ti].root.first_leaf();
                        }
                        ti += 1;
                    }
                    Err(_) => {
                        self.tabs.remove(ti);
                    }
                }
            }
        }
        if self.active >= self.tabs.len() && self.active > 0 {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.tabs.is_empty()
    }

    pub fn broadcast_write(&mut self, bytes: &[u8]) {
        for p in self.panes.values_mut() {
            p.term.write(bytes);
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
                zoomed: false,
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
                zoomed: false,
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
    fn zoom_collapses_layout_to_focused_pane() {
        let mut m = Mux::new();
        let mut root = Node::Leaf(1);
        root.split_leaf(1, 2, Dir::Horizontal);
        m.tabs.push(Tab {
            root,
            focus: 2,
            zoomed: false,
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
}
