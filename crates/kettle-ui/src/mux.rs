//! Tabs + panes. Each pane owns an independent terminal; tabs hold one or more
//! panes (Terminator-style splits share a tab, focus cycles between them).

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use kettle_config::Config;
use kettle_core::{TermEvent, Terminal, Waker};

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
}

pub struct Tab {
    pub panes: Vec<Pane>,
    pub focus: usize,
}

pub struct Mux {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub search: SearchState,
    pub broadcast: bool,
}

impl Mux {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            search: SearchState::default(),
            broadcast: false,
        }
    }

    fn spawn_pane(
        cfg: &Config,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        waker: Waker,
    ) -> Result<Pane> {
        let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) = crossbeam_channel::unbounded();
        let term = Terminal::new(
            cfg.shell.as_deref(),
            cfg.scrollback,
            cols,
            rows,
            cell_w,
            cell_h,
            tx,
            waker,
        )?;
        Ok(Pane {
            term,
            rx,
            title: "kettle".into(),
            closed: false,
        })
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
        let pane = Self::spawn_pane(cfg, cols, rows, cw, ch, waker)?;
        self.tabs.push(Tab {
            panes: vec![pane],
            focus: 0,
        });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    pub fn split(
        &mut self,
        cfg: &Config,
        cols: usize,
        rows: usize,
        cw: u16,
        ch: u16,
        waker: Waker,
    ) -> Result<()> {
        let pane = Self::spawn_pane(cfg, cols, rows, cw, ch, waker)?;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.panes.push(pane);
            tab.focus = tab.panes.len() - 1;
        }
        Ok(())
    }

    pub fn active_tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn focused(&mut self) -> Option<&mut Pane> {
        let a = self.active;
        self.tabs.get_mut(a).and_then(|t| t.panes.get_mut(t.focus))
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

    pub fn focus_next_pane(&mut self) {
        if let Some(t) = self.active_tab()
            && !t.panes.is_empty()
        {
            t.focus = (t.focus + 1) % t.panes.len();
        }
    }

    pub fn focus_prev_pane(&mut self) {
        if let Some(t) = self.active_tab()
            && !t.panes.is_empty()
        {
            t.focus = (t.focus + t.panes.len() - 1) % t.panes.len();
        }
    }

    /// Close the focused pane; closes the tab when it was the last pane.
    /// Returns `true` when no tabs remain (the app should exit).
    pub fn close_focused(&mut self) -> bool {
        let a = self.active;
        if let Some(tab) = self.tabs.get_mut(a) {
            if tab.focus < tab.panes.len() {
                tab.panes.remove(tab.focus);
                if tab.focus > 0 {
                    tab.focus -= 1;
                }
            }
            if tab.panes.is_empty() {
                self.tabs.remove(a);
                if self.active >= self.tabs.len() && self.active > 0 {
                    self.active -= 1;
                }
            }
        }
        self.tabs.is_empty()
    }

    /// Reap panes whose child process exited.
    pub fn reap(&mut self) -> bool {
        for tab in &mut self.tabs {
            for p in &mut tab.panes {
                if p.term.child_exited() {
                    p.closed = true;
                }
            }
            tab.panes.retain(|p| !p.closed);
            if tab.focus >= tab.panes.len() && tab.focus > 0 {
                tab.focus = tab.panes.len().saturating_sub(1);
            }
        }
        self.tabs.retain(|t| !t.panes.is_empty());
        if self.active >= self.tabs.len() && self.active > 0 {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.tabs.is_empty()
    }

    /// Write to every pane (broadcast / group input).
    pub fn broadcast_write(&mut self, bytes: &[u8]) {
        for tab in &mut self.tabs {
            for p in &mut tab.panes {
                p.term.write(bytes);
            }
        }
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}
