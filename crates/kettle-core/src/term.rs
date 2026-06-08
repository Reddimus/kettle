//! A single terminal instance: PTY + `alacritty_terminal` grid + VT parser,
//! driven by a dedicated reader thread.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};
use anyhow::Result;
use kettle_vt::placeholder::{self, CellDiacritics, RawCell};
use kettle_vt::{Chunk, Extractor, Progress, PromptKind};
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, TermEvent, Waker};
use crate::images::{
    AnimEntry, Animations, Images, Placement, RelEntry, Relatives, VirtualEntry, Virtuals,
    relative_origin, resolve_chain,
};

/// A `Write` sink that discards everything. Cycle 742: on `Terminal`
/// teardown the PTY writer (the child's stdin / conin) is swapped for this
/// so dropping the real writer closes the input handle immediately — an EOF
/// nudge for shells that exit on stdin close — without leaving the field
/// holding a dangling handle. Zero-sized; never errors.
struct NullWrite;

impl std::io::Write for NullWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Grid dimensions passed to `alacritty_terminal` (implements `Dimensions`).
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

pub type SharedTerm = Arc<Mutex<Term<EventProxy>>>;

/// Best-effort "user home" directory for a freshly spawned shell whose
/// recorded cwd is missing or no longer on disk. Probes the platform-
/// conventional env vars in order:
/// - `HOME` — always set on Linux / macOS
/// - `USERPROFILE` — the Windows-native home (`C:\Users\Bob`)
/// - `APPDATA` — Windows last-ditch fallback (`...\AppData\Roaming`)
///
/// An *empty* env var (e.g., `HOME=""` — possible in stripped-down CI
/// containers or after a misconfigured shell `unset HOME` / `export
/// HOME=`) is treated as unset and the probe continues to the next
/// variable. Pre-cycle-180, `var_os("HOME")` would return
/// `Some(OsString::new())` and this function returned `PathBuf::from("")`
/// — `CommandBuilder::cwd("")` then fed an invalid empty path to the
/// OS spawn call (which on Unix means "no cwd" but the intent here is
/// to actively *pick* a home, so the silent fall-through was wrong).
///
/// Returns `None` only on a stripped-down environment where none of
/// the three are set to a non-empty value; callers leave
/// `CommandBuilder::cwd` unset in that case, which makes `portable_pty`
/// inherit kettle's launch directory.
///
/// `lookup` is passed in so the env-probe order is unit-testable
/// without touching the process env (which would race with parallel
/// tests). Production code calls with `|k| std::env::var_os(k)`.
pub(crate) fn home_dir_fallback(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let pick = |k: &str| lookup(k).filter(|v| !v.is_empty());
    pick("HOME")
        .or_else(|| pick("USERPROFILE"))
        .or_else(|| pick("APPDATA"))
        .map(std::path::PathBuf::from)
}

/// Cycle 612 (Terminator parity, `command_notify.py` plugin): a single
/// completed-command event for the App's notification dispatcher. Built
/// from the OSC 133 `OutputStart` → `CommandEnd` transition; the App
/// uses `duration` + window focus to decide whether to fire a desktop
/// notification. `exit_code` is the OSC 133 D payload (`None` when the
/// shell didn't ship one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFinished {
    pub duration: std::time::Duration,
    pub exit_code: Option<i32>,
}

pub struct Terminal {
    pub term: SharedTerm,
    // Cycle 742: `Option` so `Drop` can `.take()` and drop the master
    // (ClosePseudoConsole on Windows / close the master fd on Unix) WITHOUT
    // moving a non-`Option` field out of `&mut self`. Always `Some` during
    // normal operation; only `None` transiently inside `Drop`.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    reader_thread: Option<JoinHandle<()>>,
    // Cycle 742: cooperative stop flag for the reader thread. `Drop` sets it
    // so the reader exits promptly during teardown instead of looping back
    // into another blocking PTY read once the pseudoconsole closes.
    stop: Arc<AtomicBool>,
    pub cols: usize,
    pub rows: usize,
    pub images: Images,
    /// kitty `U=1` virtual images, keyed by image id (for placeholder draw).
    pub virtuals: Virtuals,
    /// kitty animations, keyed by image id (frame substituted at draw time).
    pub anims: Animations,
    /// kitty relative placements, keyed by `(child img, child placement)`.
    pub relatives: Relatives,
    /// Absolute lines (history-aware) where OSC 133 prompts started.
    /// Cycle 902 (audit): a `VecDeque` so the ring-buffer trim is an O(1)
    /// `pop_front`, not a `Vec::drain(0..1)` that shifts all ~2048 elements on
    /// every prompt once full — this is the hot reader-thread path.
    pub prompts: Arc<Mutex<std::collections::VecDeque<i64>>>,
    /// Cycle 612 (Terminator parity, `command_notify.py` plugin):
    /// OSC 133 OutputStart timestamp — set when `OutputStart` fires,
    /// cleared when the matching `CommandEnd` fires. The reader
    /// thread updates this; the App reads `command_finished` (below)
    /// to learn that a command completed + how long it took.
    pub output_started_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// Cycle 612: per-pane queue of completed-command events.
    /// Populated by the reader thread on OSC 133 D (CommandEnd) when
    /// `output_started_at` is `Some`; drained by the App each tick
    /// to fire desktop notifications (if the window isn't focused
    /// and the command ran longer than `cfg.command_notify_threshold_ms`).
    /// Bounded at 32 entries — a hostile / runaway script that
    /// emitted thousands of fake OSC 133 D sequences would otherwise
    /// grow this Vec indefinitely.
    pub command_finished: Arc<Mutex<Vec<CommandFinished>>>,
    /// Latest working directory reported via OSC 7.
    pub cwd: Arc<Mutex<Option<String>>>,
    /// Cycle 745: latest OSC 9;4 progress state (drives the OS taskbar
    /// indicator); `None` until the program reports progress / after clear.
    pub progress: Arc<Mutex<Option<Progress>>>,
    /// The argv this pane was launched with (empty = default shell);
    /// persisted so SSH/remote panes can be restored.
    pub argv: Vec<String>,
    /// Cycle 621 (Terminator parity, `plugins/logger.py`): optional
    /// per-pane session log. When `Some(file)`, the reader thread
    /// writes a copy of every raw PTY byte to the file (best-effort:
    /// I/O errors are swallowed so a full disk doesn't take down
    /// the reader). When `None`, no-op cost on the hot path —
    /// just a Mutex lock + Option check per buf read.
    pub log_file: Arc<Mutex<Option<std::fs::File>>>,
    /// Cycle 625: when `true`, the logger strips ANSI escape
    /// sequences (CSI / OSC / single-char ESC) from the bytes
    /// before writing — leaving plain-text-searchable logs.
    /// Default `false` preserves the cycle-621 raw-stream
    /// behavior (replayable via `cat <log>` in a terminal).
    pub log_strip_ansi: Arc<Mutex<bool>>,
    cell_px: Arc<Mutex<(u16, u16)>>,
}

/// Cycle 743: pick Windows' default shell when the user configured no
/// `command` / `shell`. Prefers PowerShell 7+ (`pwsh.exe`), then Windows
/// PowerShell 5.1 (`powershell.exe`); returns `None` to let the caller fall
/// back to portable_pty's default (`%ComSpec%` → `cmd.exe`). This matches
/// Windows Terminal, which defaults to pwsh 7 when it is installed — a plain
/// `cmd.exe` default feels dated on a modern Windows 11 box. `resolve` maps
/// an exe name to its full path if present on `PATH`; it is injected so the
/// preference order is unit-testable without depending on what is installed.
#[cfg(windows)]
fn pick_windows_default_shell(
    resolve: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    ["pwsh.exe", "powershell.exe"].into_iter().find_map(resolve)
}

/// Cycle 743: full path of `exe` if it is a file on any `PATH` entry.
/// pwsh 7's installer adds `C:\Program Files\PowerShell\7` to `PATH`, and
/// `powershell.exe` lives in `System32` (always on `PATH`), so a bare-name
/// PATH walk resolves both without hard-coding install locations.
#[cfg(windows)]
fn find_on_path(exe: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|p| {
            // NOT `is_file()`: that follows reparse points and FAILS on the
            // Store "app execution alias" stubs (0-byte reparse points under
            // `%LOCALAPPDATA%\Microsoft\WindowsApps\`) that a Store-installed
            // pwsh 7 uses. `symlink_metadata` (lstat) succeeds on the alias
            // itself, so it detects both a real `pwsh.exe` and a Store alias;
            // exclude directories so a stray dir named `pwsh.exe` can't match.
            std::fs::symlink_metadata(p)
                .map(|m| !m.is_dir())
                .unwrap_or(false)
        })
}

/// Cycle 748: is `prog` the WSL launcher (`wsl` / `wsl.exe`, possibly given as
/// a full path)? The cycle-343 `login_shell` flag prepends `-l` for POSIX
/// `bash -l` login-shell semantics — but `wsl.exe -l` means **list
/// distributions**: it would print the distro list and exit instead of opening
/// an interactive shell. So the `-l` injection is suppressed for wsl. A user
/// who wants a WSL *login* shell should request it inside the distro (e.g.
/// `command = wsl.exe -d Ubuntu -- bash -l`), where `-l` reaches bash, not wsl.
/// Case-insensitive so `wsl`, `wsl.exe`, and `C:\…\wsl.exe` all match.
///
/// Splits on BOTH `/` and `\` rather than using `std::path::Path::file_stem`,
/// because `Path` only treats `\` as a separator on Windows targets — on a
/// Linux/macOS build (incl. CI) `C:\Windows\System32\wsl.exe` would be one
/// opaque component and the stem check would miss it. wsl.exe only runs on
/// Windows, but a target-independent check keeps the function and its unit
/// test correct everywhere (the cross-platform CI pretest caught the
/// `Path`-based version).
fn is_wsl_launcher(prog: &str) -> bool {
    let last = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    last.eq_ignore_ascii_case("wsl") || last.eq_ignore_ascii_case("wsl.exe")
}

/// Whether the platform's default shell (`default_prog`) accepts the POSIX `-l`
/// login switch. Cycle 822 (audit): `false` on Windows, where `default_prog`
/// resolves to pwsh/powershell/cmd — none of which treat `-l` as a login flag —
/// so `login-shell = true` must not inject it there. `true` everywhere else,
/// where the default shell is a POSIX shell that honors `-l`.
const fn default_shell_accepts_login_flag() -> bool {
    cfg!(not(windows))
}

/// Whether an EXPLICIT `command = <prog>` accepts the POSIX `-l` login switch.
///
/// Cycle 840 (audit): the cycle-822 guard only covered the no-argv default-shell
/// arm; the explicit-argv arm still injected `-l` for `wsl.exe` (where `-l`
/// means "list distros") only via `!is_wsl_launcher`, leaving Windows-native
/// shells (`pwsh`/`powershell`/`cmd`) to receive a `-l` they reject. Exclude
/// both, matching on the case-insensitive basename sans `.exe`. POSIX shells
/// (bash/zsh/fish/…) and anything else honor `-l`.
fn prog_accepts_login_flag(prog: &str) -> bool {
    if is_wsl_launcher(prog) {
        return false;
    }
    let base = prog
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prog)
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    !matches!(base, "pwsh" | "powershell" | "cmd")
}

/// Cycle 799: build the `WSLENV` value that propagates kettle's
/// terminal-identity env vars into a WSL distro. WSL only forwards Windows
/// env vars listed in `WSLENV`; each is suffixed `/u` ("pass Windows→WSL
/// only"). Preserves `existing` (the user's own WSLENV) verbatim and skips
/// any `var` already present (matching on the name before any `/flags`), so
/// re-launches don't accumulate duplicates. Pure — unit-tested.
fn augment_wslenv(existing: &str, vars: &[&str]) -> String {
    let mut out = existing.to_string();
    for &var in vars {
        let present = out.split(':').any(|e| e.split('/').next() == Some(var));
        if !present {
            if !out.is_empty() {
                out.push(':');
            }
            out.push_str(var);
            out.push_str("/u");
        }
    }
    out
}

/// Cycle 805: a shell choice for the new-tab `▾` dropdown — a display label and
/// the argv to spawn for it.
pub type ShellChoice = (String, Vec<String>);

/// Cycle 805: auto-detect the shells to offer in the new-tab dropdown,
/// Windows-Terminal style. Always returns at least one entry.
/// - Windows: Command Prompt, Windows PowerShell, PowerShell 7 (each only when
///   found on `PATH`), then one entry per installed WSL distro.
/// - Other platforms: `$SHELL` first, then bash/zsh/fish found on `PATH`
///   (de-duped by basename).
///
/// Detection is injected into the inner helpers so they are unit-testable
/// without depending on what is installed on the host.
pub fn detect_shells() -> Vec<ShellChoice> {
    #[cfg(windows)]
    {
        detect_shells_windows(|e| find_on_path(e).is_some(), list_wsl_distros)
    }
    #[cfg(not(windows))]
    {
        detect_shells_unix(std::env::var("SHELL").ok(), unix_on_path)
    }
}

#[cfg(windows)]
fn detect_shells_windows(
    available: impl Fn(&str) -> bool,
    distros: impl Fn() -> Vec<String>,
) -> Vec<ShellChoice> {
    let mut out: Vec<ShellChoice> = Vec::new();
    for (label, exe) in [
        ("Command Prompt", "cmd.exe"),
        ("Windows PowerShell", "powershell.exe"),
        ("PowerShell 7", "pwsh.exe"),
    ] {
        if available(exe) {
            out.push((label.to_string(), vec![exe.to_string()]));
        }
    }
    if available("wsl.exe") {
        for d in distros() {
            out.push((
                format!("WSL: {d}"),
                vec!["wsl.exe".to_string(), "-d".to_string(), d],
            ));
        }
    }
    // Never hand back an empty menu — the `▾` click must always do something.
    if out.is_empty() {
        out.push(("Command Prompt".to_string(), vec!["cmd.exe".to_string()]));
    }
    out
}

#[cfg(not(windows))]
fn unix_on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                std::fs::metadata(dir.join(exe))
                    .map(|m| m.is_file())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn detect_shells_unix(
    shell_env: Option<String>,
    available: impl Fn(&str) -> bool,
) -> Vec<ShellChoice> {
    let basename = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let mut out: Vec<ShellChoice> = Vec::new();
    if let Some(s) = shell_env.filter(|s| !s.is_empty()) {
        out.push((basename(&s), vec![s]));
    }
    for sh in ["bash", "zsh", "fish"] {
        if available(sh) && !out.iter().any(|(_, argv)| basename(&argv[0]) == sh) {
            out.push((sh.to_string(), vec![sh.to_string()]));
        }
    }
    if out.is_empty() {
        out.push(("Shell".to_string(), vec!["/bin/sh".to_string()]));
    }
    out
}

/// Cycle 805: parse distro names from `wsl.exe -l -q` output — one per line,
/// stripping a UTF-16 BOM artifact, surrounding whitespace, and trailing NULs,
/// dropping blanks. Pure → unit-testable without wsl.exe (built in test or on
/// Windows where `list_wsl_distros` calls it).
#[cfg(any(windows, test))]
fn parse_wsl_distros(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\u{0}'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Cycle 805: installed WSL distros via `wsl.exe -l -q` (bare names, no header).
/// Output is UTF-16-LE; decode then parse. Empty on any spawn/exit failure so a
/// host without WSL simply offers no WSL entries.
#[cfg(windows)]
fn list_wsl_distros() -> Vec<String> {
    // Cycle 834 (audit): run `wsl.exe -l -q` on a worker thread with a bounded
    // wait. The dropdown that calls this (new-tab `▾`) runs on the UI thread, so
    // a wedged LxssManager — the very `Wsl/Service/E_UNEXPECTED` state that
    // freezes `wsl.exe` — would otherwise hang the whole window ("not
    // responding"). On timeout we abandon the call and report no distros; the
    // worker self-terminates if `wsl.exe` ever returns (its `send` no-ops once
    // the receiver is gone). With the App-side cache (open_new_tab_menu), the
    // worst case is one ~2 s wait on the first dropdown open. `-l -q` only reads
    // the registry/service (it doesn't boot a distro), so 2 s is generous.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            std::process::Command::new("wsl.exe")
                .args(["-l", "-q"])
                .output(),
        );
    });
    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(2)) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let units: Vec<u16> = out
        .stdout
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    parse_wsl_distros(&String::from_utf16_lossy(&units))
}

/// One axis of a `PtySize`, computed without overflow. `cell` is the
/// per-cell pixel extent (1 when computing the row/column count itself);
/// `count` is the grid dimension in cells. The product is evaluated in
/// `u32` and clamped into `u16`, the type `PtySize` requires. The old
/// `cell_w * cols as u16` did the whole multiply in `u16` — a panic in
/// debug and a silent wrap in release once the product passed 65535,
/// reachable with a HiDPI cell on a very wide grid — and `cols as u16`
/// truncated a pathological `usize` before the multiply.
fn clamp_pty_dim(cell: u16, count: usize) -> u16 {
    let count = count.min(u16::MAX as usize) as u32;
    (cell as u32 * count).min(u16::MAX as u32) as u16
}

/// Cycle 743: the default shell `CommandBuilder` when no `command` is
/// configured. Windows prefers pwsh 7 → Windows PowerShell → `%ComSpec%`;
/// every other platform defers to portable_pty (which honors `$SHELL`).
fn default_prog() -> CommandBuilder {
    #[cfg(windows)]
    {
        if let Some(path) = pick_windows_default_shell(find_on_path) {
            return CommandBuilder::new(path);
        }
    }
    CommandBuilder::new_default_prog()
}

/// Cycle 902 (audit): cap on the OSC 133 prompt-mark ring. A long-lived shell
/// session emits one mark per prompt; without a cap the Vec grew unbounded.
const MAX_PROMPT_MARKS: usize = 2048;

/// Cycle 902 (audit): push an absolute prompt-start line into the bounded ring.
/// Dedups against the most-recent mark (some shells emit OSC 133 `A` twice for a
/// single prompt) and trims oldest-first with O(1) `pop_front` — the previous
/// `Vec::drain(0..d)` shifted all ~2048 elements on every prompt once full, on
/// the hot reader-thread path. Pure, so the ring discipline is unit-tested.
fn push_prompt_mark(ring: &mut std::collections::VecDeque<i64>, abs: i64) {
    if ring.back() == Some(&abs) {
        return;
    }
    ring.push_back(abs);
    while ring.len() > MAX_PROMPT_MARKS {
        ring.pop_front();
    }
}

impl Terminal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
    ) -> Result<Terminal> {
        Self::new_with_env(
            argv,
            cwd,
            scrollback,
            cols,
            rows,
            cell_w,
            cell_h,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            "xterm-256color",
            "truecolor",
            false,
            event_tx,
            waker,
        )
    }

    /// Cycle 343 Terminator parity: PTY spawn with explicit `TERM` +
    /// `COLORTERM` env override + `login_shell` flag (prepends `-l`
    /// to the shell argv to get login-shell semantics).
    ///
    /// `term` / `colorterm` correspond to Terminator's per-profile
    /// `term` (terminatorlib/config.py:114) and `colorterm`
    /// (`:115`); `login_shell` is `:122`. Empty strings preserve
    /// kettle's existing default — same shape as the parse-side
    /// fall-through (cycle 335).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        login_shell: bool,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output(
            argv,
            cwd,
            scrollback,
            cols,
            rows,
            cell_w,
            cell_h,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            term_env,
            colorterm_env,
            login_shell,
            event_tx,
            waker,
            None,
        )
    }

    /// Cycle 378 (Terminator plugin parity, plugin sub-cycle 3): same
    /// as `new_with_env` plus an optional sidechannel that ships raw
    /// PTY-output bytes to the App for `LuaEvent::Output` dispatch.
    /// `None` keeps the zero-cost path for non-Lua kettle runs;
    /// `Some(tx)` lets a plugin-runtime caller subscribe.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env_and_output(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        login_shell: bool,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<crossbeam_channel::Sender<Vec<u8>>>,
    ) -> Result<Terminal> {
        let pty = portable_pty::native_pty_system();
        // Cycle 760: clamp the same way `resize()` does. The raw casts
        // (`cols as u16`, `cell_w * cols as u16`) truncate a large grid and can
        // overflow the u16 multiply on a wide / HiDPI layout (e.g. cell_w=20 ×
        // cols=5000 = 100000, wraps); `clamp_pty_dim` saturates to u16::MAX so
        // the ConPTY/openpty always gets sane dimensions.
        let pair = pty.openpty(PtySize {
            rows: clamp_pty_dim(1, rows),
            cols: clamp_pty_dim(1, cols),
            pixel_width: clamp_pty_dim(cell_w, cols),
            pixel_height: clamp_pty_dim(cell_h, rows),
        })?;

        let mut cmd = match argv.split_first() {
            Some((prog, rest)) => {
                let mut c = CommandBuilder::new(prog);
                if login_shell && prog_accepts_login_flag(prog) {
                    // Cycle 343: `-l` (POSIX-defined "shell that
                    // reads /etc/profile + ~/.profile + login dotfiles
                    // before running interactively"). Goes BEFORE
                    // the user's argv args so a config like
                    // `command = bash -i` still works.
                    // Cycle 748/840: skipped for `wsl.exe` (where `-l` lists
                    // distros) and Windows-native shells (pwsh/powershell/cmd
                    // reject it) via `prog_accepts_login_flag`.
                    c.arg("-l");
                }
                for a in rest {
                    c.arg(a);
                }
                c
            }
            None => {
                let mut c = default_prog();
                // Cycle 822 (audit): `-l` is the POSIX login-shell switch. On
                // Windows `default_prog()` resolves to pwsh/powershell/cmd, none
                // of which accept it (powershell.exe errors on an unknown arg,
                // pwsh's `-Login` is reserved/no-op on Windows, cmd ignores it),
                // so `login-shell = true` with no explicit `command` produced a
                // broken/empty pane. The explicit-argv arm already guards the
                // analogous `wsl.exe` footgun; guard the default-shell arm for
                // Windows-native shells via `default_shell_accepts_login_flag`.
                if login_shell && default_shell_accepts_login_flag() {
                    c.arg("-l");
                }
                c
            }
        };
        // Cycle 343: honor cfg.term + cfg.colorterm (empty preserves
        // kettle's default).
        cmd.env(
            "TERM",
            if term_env.is_empty() {
                "xterm-256color"
            } else {
                term_env
            },
        );
        cmd.env(
            "COLORTERM",
            if colorterm_env.is_empty() {
                "truecolor"
            } else {
                colorterm_env
            },
        );
        cmd.env("TERM_PROGRAM", "kettle");
        // `TERM_PROGRAM_VERSION` is the de-facto pair to `TERM_PROGRAM`
        // (iTerm2 / kitty / WezTerm / Ghostty all set it). Neovim's
        // `:checkhealth provider`, fish's prompt themers, and various
        // diagnostic tools key off the pair when probing whether they're
        // running under a known modern terminal. Kettle's own crate
        // version is the obvious answer — populated from Cargo at build
        // time so a bumped `kettle/Cargo.toml` flows through with no
        // separate version string to keep in sync.
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // Cycle 799 (audit): env vars set on the child's *Windows* process do
        // NOT cross into a WSL distro unless listed in `WSLENV`. Without this,
        // `COLORTERM` is silently dropped at the WSL boundary, so a program
        // inside WSL (Ubuntu) that decides truecolor support from `$COLORTERM`
        // — rather than force-enabling it — falls back to 256-color and
        // renders washed-out, mis-mapped colors. Append our terminal-identity
        // vars to WSLENV (preserving any the user already set) with the `/u`
        // flag, i.e. "pass Windows→WSL only". `cmd.env` set them on the
        // Windows side just above, so WSLENV can reference them. Harmless when
        // the child isn't `wsl.exe` — it's just an extra, ignored env var.
        cmd.env(
            "WSLENV",
            augment_wslenv(
                &std::env::var("WSLENV").unwrap_or_default(),
                &["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"],
            ),
        );
        match cwd {
            Some(d) if std::path::Path::new(d).is_dir() => cmd.cwd(d),
            _ => {
                // Recorded cwd is missing or no longer on disk (e.g.,
                // user moved the repo between sessions, or the `-d` arg
                // pointed at a since-deleted path). Fall back to the OS
                // home directory. The previous version only checked
                // `HOME`, which is unset on Windows by default — so
                // Windows users with a stale recorded cwd silently
                // ended up in whatever directory they happened to
                // launch kettle from. `home_dir_fallback` probes
                // `HOME` then `USERPROFILE` then `APPDATA`, in that
                // order, so all three platforms (Linux/macOS/Windows)
                // converge on the same "user-home" intent. Same shape
                // as cycle 159's macOS universal2 fix — Linux+macOS
                // worked, Windows didn't, the env var probe order is
                // the difference.
                // Cycle 185: also gate the fallback on `is_dir`. The env
                // var could be set to something that exists but isn't a
                // directory (an exotic `HOME=/etc/passwd` misconfig, or
                // a path that's a regular file / symlink to a file) —
                // `cmd.cwd` would then hand the OS spawn an invalid
                // target. Treating that the same as "no home" lets
                // `portable_pty` inherit kettle's launch directory
                // (the same recovery as cycles 162/180 when no env
                // var was set or it was empty).
                if let Some(home) = home_dir_fallback(|k| std::env::var_os(k))
                    && home.is_dir()
                {
                    cmd.cwd(home);
                }
            }
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let proxy = EventProxy::new(event_tx, waker.clone());
        // Seed the engine's *default* cursor style from the user config; the
        // engine seeds `cursor_style` lazily from this, and programs can flip
        // both fields at runtime — `?12 h/l` for blinking (honored live via
        // `cursor_blinking()` below) and DECSCUSR `CSI Ps SP q` for shape
        // (honored live via the engine's `renderable_content().cursor.shape`,
        // read by the renderer per-frame).
        let default_cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            blinking: cursor_blink,
            shape: cursor_shape,
        };
        let mut tconf = TermConfig {
            scrolling_history: scrollback,
            // Cycle 798 (audit A2, critical): do NOT advertise the kitty
            // keyboard protocol. With this `true`, alacritty_terminal replies
            // to the `CSI ? u` progressive-enhancement query and honors
            // `CSI > flags u` (setting DISAMBIGUATE_ESC_CODES / REPORT_*
            // TermMode bits) — i.e. it tells programs "I encode keys in the
            // kitty CSI-u format." But kettle's key encoder
            // (`kettle-ui/src/input.rs::encode`) ONLY implements the legacy
            // xterm encoding and never emits CSI-u. So an app that enabled the
            // protocol (e.g. Neovim's kitty keyboard mode) would push its flags,
            // think kettle speaks CSI-u, and then mis-read the legacy bytes it
            // actually receives — broken/ambiguous key input. Until a real
            // CSI-u encoder lands, the robust answer is to not advertise it:
            // programs fall back to the legacy encoding `encode()` implements
            // and tests, which is correct and unambiguous for the common keys.
            kitty_keyboard: false,
            default_cursor_style,
            ..TermConfig::default()
        };
        // Word delimiters drive double-click word selection (and the
        // jump-to-prompt search). An empty config means "use the engine
        // default" — `",│`|:\"' ()[]{}<>\t"` — so users that don't set
        // anything still get sensible word boundaries.
        if let Some(wd) = word_delimiters
            && !wd.is_empty()
        {
            tconf.semantic_escape_chars = wd.to_string();
        }
        let term = Term::new(
            tconf,
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy.clone(),
        );
        let term: SharedTerm = Arc::new(Mutex::new(term));

        let images: Images = Arc::new(Mutex::new(Vec::new()));
        let virtuals: Virtuals = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let anims: Animations = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let relatives: Relatives = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let prompts: Arc<Mutex<std::collections::VecDeque<i64>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        // Cycle 612 (Terminator parity, `command_notify.py`):
        // per-pane OSC 133 OutputStart timestamp + completed-command
        // event queue. Reader thread writes; App polls each tick.
        let output_started_at: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let command_finished: Arc<Mutex<Vec<CommandFinished>>> = Arc::new(Mutex::new(Vec::new()));
        let cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.map(|s| s.to_string())));
        // Cycle 745: latest OSC 9;4 taskbar-progress state from this pane.
        // The reader thread writes it; the App polls the focused pane's value
        // each frame and drives the OS taskbar indicator (pwsh 7 parity).
        let progress_cell: Arc<Mutex<Option<Progress>>> = Arc::new(Mutex::new(None));
        let cell_px = Arc::new(Mutex::new((cell_w.max(1), cell_h.max(1))));
        // Cycle 621 (Terminator parity, `plugins/logger.py`): per-pane
        // session log. Default None; `Action::ToggleSessionLog`
        // opens/closes it at runtime. The reader thread holds a
        // clone and writes raw PTY bytes when Some.
        let log_file: Arc<Mutex<Option<std::fs::File>>> = Arc::new(Mutex::new(None));
        let log_file_for_struct = log_file.clone();
        // Cycle 625 (Terminator parity): when true, strip ANSI
        // escape sequences from the bytes before writing to the
        // log file. Default false preserves cycle-621 raw-stream
        // behavior.
        let log_strip_ansi: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let log_strip_ansi_for_struct = log_strip_ansi.clone();
        // Cycle 742: teardown stop flag (see the `stop` struct field).
        let stop = Arc::new(AtomicBool::new(false));

        let reader_thread = {
            let term = term.clone();
            let images = images.clone();
            let virtuals = virtuals.clone();
            let anims = anims.clone();
            let relatives = relatives.clone();
            let prompts = prompts.clone();
            let output_started_at = output_started_at.clone();
            let command_finished = command_finished.clone();
            let cwd_cell = cwd_cell.clone();
            let progress_cell = progress_cell.clone();
            let cell_px = cell_px.clone();
            let log_file = log_file.clone();
            let log_strip_ansi = log_strip_ansi.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("kettle-pty-reader".into())
                .spawn(move || {
                    let mut processor: Processor = Processor::new();
                    let mut extractor = Extractor::new();
                    let mut buf = [0u8; 65536];
                    loop {
                        // Cycle 742: bail out during teardown (Drop sets
                        // `stop`). This can't interrupt a *currently* blocked
                        // read — only the pseudoconsole closing does that —
                        // but it stops us re-entering a fresh blocking read
                        // after the close, so the detached thread winds down
                        // immediately instead of processing stale output.
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => {
                                proxy.send_event_exit();
                                break;
                            }
                            Ok(n) => {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                // Cycle 621 (Terminator parity, logger.py):
                                // per-pane session log tap. Best-effort —
                                // I/O errors are swallowed so a full disk
                                // doesn't crash the reader. Held lock is
                                // brief (just the write call).
                                if let Ok(mut guard) = log_file.lock()
                                    && let Some(f) = guard.as_mut()
                                {
                                    use std::io::Write as _;
                                    let strip = log_strip_ansi.lock().map(|g| *g).unwrap_or(false);
                                    if strip {
                                        let cleaned = strip_ansi_bytes(&buf[..n]);
                                        let _ = f.write_all(&cleaned);
                                    } else {
                                        let _ = f.write_all(&buf[..n]);
                                    }
                                }
                                // Cycle 378: ship raw PTY bytes to the
                                // App via the output_tx sidechannel
                                // (if any plugin subscriber is listening).
                                // Skips the alloc entirely when no
                                // subscriber. send-or-drop on a full
                                // channel — slow plugins shouldn't
                                // back-pressure the PTY reader.
                                if let Some(tx) = &output_tx {
                                    // Drop on full channel — slow
                                    // plugins shouldn't back-pressure
                                    // the PTY reader.
                                    let _ = tx.try_send(buf[..n].to_vec());
                                }
                                for chunk in extractor.feed(&buf[..n]) {
                                    match chunk {
                                        Chunk::Pass(bytes) => {
                                            if let Ok(mut t) = term.lock() {
                                                processor.advance(&mut *t, &bytes);
                                            }
                                        }
                                        Chunk::Image(placed) => {
                                            place_image(
                                                &term,
                                                &images,
                                                &cell_px,
                                                &mut processor,
                                                placed,
                                            );
                                        }
                                        Chunk::DeleteImages { all, id } => {
                                            if let Ok(mut v) = images.lock() {
                                                if all {
                                                    v.clear();
                                                } else {
                                                    v.retain(|p| {
                                                        id.is_none_or(|x| p.id != Some(x))
                                                    });
                                                }
                                            }
                                            if let Ok(mut vm) = virtuals.lock() {
                                                match (all, id) {
                                                    (true, _) => vm.clear(),
                                                    (false, Some(x)) => {
                                                        vm.remove(&x);
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                            if let Ok(mut am) = anims.lock() {
                                                match (all, id) {
                                                    (true, _) => am.clear(),
                                                    (false, Some(x)) => {
                                                        am.remove(&x);
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                            if let Ok(mut rm) = relatives.lock() {
                                                match (all, id) {
                                                    (true, _) => rm.clear(),
                                                    // Group dies with parent:
                                                    // drop the child and any
                                                    // child parented to it.
                                                    (false, Some(x)) => {
                                                        rm.retain(|&(cimg, _), e| {
                                                            cimg != x && e.parent_img != x
                                                        })
                                                    }
                                                    (false, None) => {}
                                                }
                                            }
                                        }
                                        Chunk::RelativePlacement {
                                            id,
                                            placement,
                                            img,
                                            parent_img,
                                            parent_placement,
                                            h,
                                            v,
                                        } => {
                                            if let Ok(mut rm) = relatives.lock() {
                                                rm.insert(
                                                    (id, placement),
                                                    RelEntry {
                                                        img,
                                                        parent_img,
                                                        parent_placement,
                                                        h,
                                                        v,
                                                    },
                                                );
                                            }
                                            (waker)();
                                        }
                                        Chunk::VirtualImage {
                                            id,
                                            img,
                                            cols,
                                            rows,
                                            z,
                                        } => {
                                            if let Ok(mut vm) = virtuals.lock() {
                                                vm.insert(id, VirtualEntry { img, cols, rows, z });
                                            }
                                            (waker)();
                                        }
                                        Chunk::Animation {
                                            id,
                                            imgs,
                                            gaps,
                                            state,
                                        } => {
                                            if let Ok(mut am) = anims.lock() {
                                                // An empty/single-image, not-
                                                // running snapshot = cleared.
                                                if imgs.len() <= 1 && !state.running {
                                                    am.remove(&id);
                                                } else {
                                                    // Keep the clock unless the
                                                    // run state flipped.
                                                    let started = match am.get(&id) {
                                                        Some(p)
                                                            if p.state.running == state.running =>
                                                        {
                                                            p.started
                                                        }
                                                        _ => std::time::Instant::now(),
                                                    };
                                                    am.insert(
                                                        id,
                                                        AnimEntry {
                                                            imgs,
                                                            gaps,
                                                            state,
                                                            started,
                                                        },
                                                    );
                                                }
                                            }
                                            (waker)();
                                        }
                                        Chunk::Prompt(PromptKind::PromptStart) => {
                                            if let Ok(t) = term.lock() {
                                                let rc = t.renderable_content();
                                                let line = rc.cursor.point.line.0 as i64;
                                                let abs = t.grid().history_size() as i64 + line;
                                                if let Ok(mut m) = prompts.lock() {
                                                    // Cycle 902 (audit): O(1)
                                                    // bounded ring push (dedup +
                                                    // pop_front trim) — see
                                                    // push_prompt_mark.
                                                    push_prompt_mark(&mut m, abs);
                                                }
                                            }
                                        }
                                        // Cycle 612 (Terminator parity, command_notify.py):
                                        // OSC 133 OutputStart (C) marks the moment the
                                        // shell handed control to a user command. Record
                                        // the timestamp so the matching CommandEnd (D)
                                        // can compute the elapsed duration.
                                        Chunk::Prompt(PromptKind::OutputStart) => {
                                            if let Ok(mut t) = output_started_at.lock() {
                                                *t = Some(std::time::Instant::now());
                                            }
                                        }
                                        // Cycle 612: OSC 133 CommandEnd (D). Pop the
                                        // most-recent OutputStart timestamp, compute
                                        // the elapsed duration, push a CommandFinished
                                        // event for the App to drain. Bounded queue at
                                        // 32 entries — a runaway / hostile shell that
                                        // spams CommandEnd would otherwise grow the
                                        // Vec without bound.
                                        Chunk::Prompt(PromptKind::CommandEnd(code)) => {
                                            let started = output_started_at
                                                .lock()
                                                .ok()
                                                .and_then(|mut t| t.take());
                                            if let Some(started) = started
                                                && let Ok(mut q) = command_finished.lock()
                                            {
                                                if q.len() >= 32 {
                                                    let d = q.len() - 31;
                                                    q.drain(0..d);
                                                }
                                                q.push(CommandFinished {
                                                    duration: started.elapsed(),
                                                    exit_code: code,
                                                });
                                            }
                                        }
                                        Chunk::Prompt(_) => {}
                                        Chunk::Cwd(path) => {
                                            if let Ok(mut c) = cwd_cell.lock() {
                                                *c = Some(path);
                                            }
                                        }
                                        // Cycle 745: OSC 9;4 taskbar progress.
                                        // Record the latest; the App polls it
                                        // and drives the OS taskbar indicator.
                                        Chunk::Progress(p) => {
                                            if let Ok(mut g) = progress_cell.lock() {
                                                *g = Some(p);
                                            }
                                            (waker)();
                                        }
                                    }
                                }
                                (waker)();
                            }
                        }
                    }
                })?
        };

        Ok(Terminal {
            term,
            master: Some(pair.master),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            reader_thread: Some(reader_thread),
            stop,
            cols,
            rows,
            images,
            virtuals,
            anims,
            relatives,
            prompts,
            output_started_at,
            command_finished,
            cwd: cwd_cell,
            progress: progress_cell,
            argv: argv.to_vec(),
            log_file: log_file_for_struct,
            log_strip_ansi: log_strip_ansi_for_struct,
            cell_px,
        })
    }

    /// Last working directory reported via OSC 7, if any.
    pub fn current_dir(&self) -> Option<String> {
        self.cwd.lock().ok().and_then(|c| c.clone())
    }

    /// Cycle 745: latest OSC 9;4 taskbar-progress state reported by this pane
    /// (`None` if never reported, or explicitly cleared with state 0). The
    /// App polls the focused pane's value each frame to drive the OS taskbar.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().and_then(|g| *g)
    }

    /// Cycle 639 (Terminator parity, sub-cycle 1 of
    /// [`TERMINATOR-REMOTE-DESIGN.md`](docs/TERMINATOR-REMOTE-DESIGN.md)):
    /// PTY child PID accessor. Returns the OS pid of the shell that
    /// kettle spawned at pane creation. None means either:
    ///   - lock contention (extremely rare; the child mutex is held only
    ///     briefly by `child_exited`'s `try_wait` from `Mux::reap` and, on
    ///     teardown, by the cycle-833 detached reaper thread's `wait` — the
    ///     reader thread does NOT touch the child)
    ///   - the platform doesn't expose pids for this Child type
    ///     (Windows fallback path)
    ///
    /// Used by the upcoming remote-session detector to root the
    /// process-tree walk. Read-only — does not consume the Child.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|c| c.process_id())
    }

    /// Cycle 612 (Terminator parity, `command_notify.py`): pop every
    /// `CommandFinished` event the reader thread queued since the
    /// previous call. The App drains this each tick to fire desktop
    /// notifications for long commands that completed while the
    /// window was unfocused. Empty Vec when the shell hasn't shipped
    /// OSC 133 D events (no shell integration) or no command has
    /// completed since the last drain.
    pub fn drain_command_finished_events(&self) -> Vec<CommandFinished> {
        self.command_finished
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// Live cursor-blink state. Defaults to whatever the config seeded at
    /// pane creation; programs flip it at runtime via DEC private mode 12
    /// (`CSI ?12 h` blink / `?12 l` solid) — the engine raises
    /// `TermEvent::CursorBlinkingChange` and we re-read this on next redraw
    /// so the cursor obeys the running app, not just the config.
    pub fn cursor_blinking(&self) -> bool {
        self.term
            .lock()
            .map(|t| t.cursor_style().blinking)
            .unwrap_or(false)
    }

    /// Absolute prompt-start lines recorded via OSC 133.
    pub fn prompt_marks(&self) -> Vec<i64> {
        // Cycle 902: `prompts` is now a VecDeque — collect to the Vec the
        // callers (jump-to-prompt nav) expect, preserving oldest→newest order.
        self.prompts
            .lock()
            .map(|m| m.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Image placements for this terminal (cloned cheaply; `ImageData` is
    /// `Arc`-backed). For placements whose kitty id has a registered
    /// animation, the image is swapped for the frame the playback clock
    /// selects right now, so animations play wherever the image sits.
    pub fn placements(&self) -> Vec<Placement> {
        let mut v = self.images.lock().map(|v| v.clone()).unwrap_or_default();
        if let Ok(am) = self.anims.lock()
            && !am.is_empty()
        {
            for p in &mut v {
                if let Some(id) = p.id
                    && let Some(e) = am.get(&id)
                    && let Some(frame) = e.current()
                {
                    p.img = frame.clone();
                }
            }
        }
        v
    }

    /// `true` if any registered kitty animation is currently running (so the
    /// UI knows to schedule frame-paced redraws).
    pub fn has_running_animation(&self) -> bool {
        self.anims
            .lock()
            .map(|am| am.values().any(|e| e.state.running))
            .unwrap_or(false)
    }

    /// Per-cell image tiles for the kitty Unicode placeholders (`U+10EEEE`)
    /// currently visible: decode each cell's `(image-id, row, column)` from
    /// its foreground color + combining diacritics, apply the left-
    /// inheritance rules over contiguous runs, and slice the referenced
    /// virtual image into one `Placement` per cell. Recomputed per frame —
    /// cheap: `ImageData` is `Arc`-backed and only the shown tiles are
    /// cropped. The placement id is decoded from the cell's underline
    /// color (used for run grouping / inheritance per the spec); a single
    /// virtual placement is stored per image id, so it also selects it.
    /// Scan the visible grid for `U+10EEEE` placeholder cells and resolve
    /// each one (image id + in-image row/col after diacritic inheritance) to
    /// its absolute line and column. Shared by placeholder + relative tiles.
    fn placeholder_cells(&self) -> Vec<(i64, usize, placeholder::ResolvedCell)> {
        let Ok(t) = self.term.lock() else {
            return Vec::new();
        };
        let top = t.grid().history_size() as i64 - t.grid().display_offset() as i64;
        let content = t.renderable_content();

        // Maximal same-row contiguous runs of placeholder cells.
        let mut runs: Vec<Vec<(RawCell, i64, usize)>> = Vec::new();
        let mut last: Option<(i32, i32)> = None;
        for ind in content.display_iter {
            let (cell, p) = (ind.cell, ind.point);
            if cell.c == placeholder::PLACEHOLDER {
                let contiguous = matches!(
                    last,
                    Some((r, c)) if r == p.line.0 && c + 1 == p.column.0 as i32
                );
                if !contiguous || runs.is_empty() {
                    runs.push(Vec::new());
                }
                let marks: Vec<char> = cell.zerowidth().map(|z| z.to_vec()).unwrap_or_default();
                // Cycle 760: `runs` is non-empty here (we either just pushed an
                // empty Vec or `contiguous` held with runs already non-empty).
                // Use `if let` rather than `expect()` so the invariant can never
                // panic the PTY reader thread (panic=abort) if a future refactor
                // changes the push logic — the run is simply skipped instead.
                if let Some(run) = runs.last_mut() {
                    run.push((
                        RawCell {
                            fg: fg_id_bits(cell.fg),
                            // Underline color carries the placement id (0/absent
                            // ⇒ any placement); spec §"Unicode placeholders".
                            placement_id: cell.underline_color().map(fg_id_bits).unwrap_or(0),
                            diacritics: CellDiacritics::parse(&marks),
                        },
                        top + p.line.0 as i64,
                        p.column.0,
                    ));
                }
                last = Some((p.line.0, p.column.0 as i32));
            } else {
                last = None;
            }
        }

        let mut out = Vec::new();
        for run in &runs {
            let cells: Vec<RawCell> = run.iter().map(|(rc, _, _)| *rc).collect();
            for (res, &(_, abs, col)) in
                placeholder::resolve_run(&cells).into_iter().zip(run.iter())
            {
                out.push((abs, col, res));
            }
        }
        out
    }

    pub fn placeholder_tiles(&self) -> Vec<Placement> {
        let Ok(virtuals) = self.virtuals.lock() else {
            return Vec::new();
        };
        if virtuals.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (abs, col, res) in self.placeholder_cells() {
            let Some(v) = virtuals.get(&res.image_id) else {
                continue;
            };
            let pcols = v.cols.max(1).min(u16::MAX as u32) as u16;
            let prows = v.rows.max(1).min(u16::MAX as u32) as u16;
            if let Some((x, y, w, h)) = placeholder::tile_src_rect(
                v.img.width,
                v.img.height,
                pcols,
                prows,
                res.row,
                res.col,
            ) && let Some(crop) = v.img.crop(x, y, w, h)
            {
                out.push(Placement {
                    abs_line: abs,
                    col,
                    cell_cols: 1,
                    cell_rows: 1,
                    img: crop,
                    id: Some(res.image_id),
                    z: v.z,
                });
            }
        }
        out
    }

    /// Placements for kitty relative placements whose parent is a visible
    /// Unicode-placeholder (virtual) image: the parent's origin is the
    /// top-left of its placeholder cells, and the child image is drawn
    /// `(h, v)` cells from there. Parents that aren't on screen this frame
    /// are skipped (the relative is simply not shown). Non-placeholder /
    /// chained parents are a later sub-item (see ROADMAP).
    pub fn relative_tiles(&self) -> Vec<Placement> {
        // Snapshot the relatives, then drop the lock before taking the
        // grid / images locks (keeps a single lock-acquisition order).
        let entries: Vec<(u32, RelEntry)> = {
            let Ok(rel) = self.relatives.lock() else {
                return Vec::new();
            };
            if rel.is_empty() {
                return Vec::new();
            }
            rel.iter().map(|(&(c, _), e)| (c, e.clone())).collect()
        };
        // Concrete origins: a parent is either a placeholder/virtual image
        // (top-left of its cells) or a regular placement (its abs_line/col).
        let mut origins: std::collections::HashMap<u32, (i64, usize)> =
            std::collections::HashMap::new();
        let mut note = |id: u32, abs: i64, col: usize| {
            origins
                .entry(id)
                .and_modify(|o: &mut (i64, usize)| {
                    o.0 = o.0.min(abs);
                    o.1 = o.1.min(col);
                })
                .or_insert((abs, col));
        };
        for (abs, col, res) in self.placeholder_cells() {
            note(res.image_id, abs, col);
        }
        if let Ok(imgs) = self.images.lock() {
            for p in imgs.iter() {
                if let Some(id) = p.id {
                    note(id, p.abs_line, p.col);
                }
            }
        }
        // child image id -> (parent image id, h, v), for chain walking.
        let rels: std::collections::HashMap<u32, (u32, i32, i32)> = entries
            .iter()
            .map(|(c, e)| (*c, (e.parent_img, e.h, e.v)))
            .collect();
        let (cw, chh) = self.cell_px.lock().map(|p| *p).unwrap_or((8, 16));
        let (cw, chh) = (cw.max(1) as u32, chh.max(1) as u32);
        let mut out = Vec::new();
        for (cimg, e) in &entries {
            // kitty requires a chain depth of at least 8.
            let Some((pa, pc)) = resolve_chain(e.parent_img, &rels, &origins, 8) else {
                continue;
            };
            let (abs, col) = relative_origin(pa, pc, e.h, e.v);
            out.push(Placement {
                abs_line: abs,
                col,
                cell_cols: e.img.width.div_ceil(cw) as usize,
                cell_rows: e.img.height.div_ceil(chh) as usize,
                img: e.img.clone(),
                id: Some(*cimg),
                z: 0,
            });
        }
        out
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: u16, cell_h: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        if let Some(master) = self.master.as_ref() {
            let _ = master.resize(PtySize {
                rows: clamp_pty_dim(1, rows),
                cols: clamp_pty_dim(1, cols),
                pixel_width: clamp_pty_dim(cell_w, cols),
                pixel_height: clamp_pty_dim(cell_h, rows),
            });
        }
        if let Ok(mut p) = self.cell_px.lock() {
            *p = (cell_w.max(1), cell_h.max(1));
        }
        if let Ok(mut t) = self.term.lock() {
            t.resize(TermSize {
                columns: cols,
                screen_lines: rows,
            });
        }
    }

    /// Has the child process exited?
    pub fn child_exited(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .is_some()
    }
}

impl Drop for Terminal {
    /// Cycle 742: tear down the PTY WITHOUT ever blocking the calling thread.
    ///
    /// This runs on the UI thread — closing a pane drops the owned
    /// `Pane.term` (`Mux::close_focused` → `panes.remove`) — so blocking here
    /// freezes the whole window.
    ///
    /// The pre-742 body `join()`ed the reader thread while the master PTY was
    /// still alive. The reader sits in a blocking `read()` on the ConPTY
    /// conout pipe that only returns once the pseudoconsole is *closed* — but
    /// the master (hence `ClosePseudoConsole`) wasn't dropped until after this
    /// function returned, so the join could never complete and the UI thread
    /// deadlocked. Windows then showed the window as "not responding", which
    /// users reported as a crash. (Reproduced on build 26200: close-split left
    /// the process alive with `Responding=false` for as long as it was sampled
    /// — a hang, not a panic. See `target/cycle-742-repro.txt`.)
    ///
    /// The fix mirrors how WezTerm (portable_pty's own author) and Alacritty
    /// drive teardown: signal stop, kill the child, close the writer (conin)
    /// and the master (conout / pseudoconsole) so the reader's `read()` reaches
    /// EOF, then DETACH the reader thread. Every step is non-blocking, so
    /// `Drop` returns in sub-millisecond time and the UI keeps pumping. The
    /// reader owns only `Arc` clones (no borrow of `Terminal`), so it is sound
    /// for it to outlive this `Drop`; it ends the instant conout EOFs and drops
    /// its clones. On Unix the same ordering applies (master fd close → slave
    /// EOF), so there is no platform-specific branch.
    fn drop(&mut self) {
        // 1. Tell the reader to stop looping.
        self.stop.store(true, Ordering::Relaxed);
        // 2. Kill the child (best-effort; already-exited returns Err), then reap
        //    it OFF the UI thread. Cycle 833 (audit): the pre-833 body kill()'d
        //    but never wait()'d on the close/quit path — `std::process::Child`'s
        //    Drop doesn't reap, and the only reaping path (`child_exited`
        //    →`try_wait`) runs from `Mux::reap` for LIVE panes only — so a long
        //    open/close session accumulated `<defunct>` zombies consuming PID
        //    slots on Unix/macOS. A short detached thread `wait()`s the already-
        //    SIGKILL'd child (returns almost immediately) so Drop stays
        //    non-blocking AND no zombie leaks. The child is a `Send + Sync`
        //    `Arc<Mutex<…>>`, and the reaper's clone keeps it alive to wait on.
        //    Windows is unaffected (handle-based) but the same path reaps it.
        let child = self.child.clone();
        if let Ok(mut c) = child.lock() {
            let _ = c.kill();
        }
        std::thread::Builder::new()
            .name("kettle-pty-reaper".into())
            .spawn(move || {
                if let Ok(mut c) = child.lock() {
                    let _ = c.wait();
                }
            })
            .ok();
        // 3. Close the writer (conin / child stdin) by swapping in a discard
        //    sink and dropping the real writer — an EOF nudge for shells that
        //    exit on stdin close.
        if let Ok(mut w) = self.writer.lock() {
            let _ = std::mem::replace(&mut *w, Box::new(NullWrite));
        }
        // 4. Close the master / pseudoconsole NOW so the reader's blocked
        //    read() returns EOF. We hold no lock and do NOT wait on the reader.
        drop(self.master.take());
        // 5. DETACH the reader thread — never join() on the UI thread.
        drop(self.reader_thread.take());
    }
}

impl EventProxy {
    fn send_event_exit(&self) {
        use alacritty_terminal::event::EventListener;
        self.send_event(TermEvent::Exit);
    }
}

/// The kitty image-id bits a placeholder cell's foreground color carries:
/// a 256-palette index is the low byte, a truecolor spec is the low 24
/// Cycle 625 (Terminator parity, `plugins/logger.py` extension):
/// strip ANSI escape sequences from a byte slice. Handles:
///   - CSI (Control Sequence Introducer): `ESC [ params final`
///     where final is in `0x40..=0x7e`.
///   - OSC (Operating System Command): `ESC ] ... terminator`
///     where terminator is BEL (0x07) or ST (`ESC \\`).
///   - Single-char ESC: `ESC X` for any other X.
///   - Bare ESC at end of buffer: dropped (assumes split-across-
///     reads is fine; the next chunk's continuation would be
///     dropped too, but the reader does line-buffered logging
///     so a stray ESC is an acceptable corner case).
///
/// Plain printable bytes + newlines pass through. Pure — no
/// state across calls (good enough for byte-block stripping;
/// callers needing perfect stripping across read boundaries
/// can buffer first).
pub fn strip_ansi_bytes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b != 0x1b {
            out.push(b);
            i += 1;
            continue;
        }
        // ESC at end of buffer → drop.
        if i + 1 >= input.len() {
            break;
        }
        match input[i + 1] {
            b'[' => {
                // CSI: scan until terminator in 0x40..=0x7e.
                i += 2;
                while i < input.len() && !(0x40..=0x7e).contains(&input[i]) {
                    i += 1;
                }
                if i < input.len() {
                    i += 1; // consume terminator
                }
            }
            b']' => {
                // OSC: scan until BEL or ESC\.
                i += 2;
                while i < input.len() {
                    if input[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                // Single-char ESC (like ESC c reset).
                i += 2;
            }
        }
    }
    out
}

/// bits, and the 16 ANSI named colors map to indices 0..=15
/// (`graphics-protocol.rst:589`). Non-id named slots (default fg/bg/cursor)
/// have no id → 0.
fn fg_id_bits(c: AnsiColor) -> u32 {
    use NamedColor::*;
    match c {
        AnsiColor::Indexed(i) => i as u32,
        AnsiColor::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        AnsiColor::Named(n) => match n {
            Black => 0,
            Red => 1,
            Green => 2,
            Yellow => 3,
            Blue => 4,
            Magenta => 5,
            Cyan => 6,
            White => 7,
            BrightBlack | DimBlack => 8,
            BrightRed | DimRed => 9,
            BrightGreen | DimGreen => 10,
            BrightYellow | DimYellow => 11,
            BrightBlue | DimBlue => 12,
            BrightMagenta | DimMagenta => 13,
            BrightCyan | DimCyan => 14,
            BrightWhite | DimWhite => 15,
            _ => 0,
        },
    }
}

/// Anchor a decoded image at the cursor, then push the cursor below it so
/// subsequent shell output flows after the image (kitty/iTerm2/Sixel all
/// place at the cursor and advance).
fn place_image(
    term: &SharedTerm,
    images: &Images,
    cell_px: &Arc<Mutex<(u16, u16)>>,
    processor: &mut Processor,
    placed: kettle_vt::Placed,
) {
    let kettle_vt::Placed { img: data, id, z } = placed;
    let (cw, chh) = cell_px.lock().map(|p| *p).unwrap_or((8, 16));
    let cw = cw.max(1) as u32;
    let chh = chh.max(1) as u32;
    let cell_cols = data.width.div_ceil(cw) as usize;
    let cell_rows = data.height.div_ceil(chh) as usize;

    let Ok(mut t) = term.lock() else {
        return;
    };
    let (abs_line, col) = {
        let rc = t.renderable_content();
        let cur = rc.cursor.point;
        let hist = t.grid().history_size() as i64;
        (hist + cur.line.0 as i64, cur.column.0)
    };
    if let Ok(mut v) = images.lock() {
        v.push(Placement {
            abs_line,
            col,
            cell_cols,
            cell_rows,
            img: data,
            id,
            z,
        });
        if v.len() > 512 {
            let drop = v.len() - 512;
            v.drain(0..drop);
        }
    }
    // Reserve the rows the image occupies.
    let nl = "\r\n".repeat(cell_rows.clamp(1, 256));
    processor.advance(&mut *t, nl.as_bytes());
}

/// End-to-end VT conformance: drives the *same* parser path the PTY reader
/// uses (alacritty_terminal + vte) over a battery of escape sequences and
/// asserts the resulting grid/cursor/mode. This is the automatable,
/// regression-proof core of a `vttest` sweep.
#[cfg(test)]
mod detect_shells_tests {

    /// Cycle 834 (audit) drift guard. `list_wsl_distros` runs on the UI thread
    /// (new-tab `▾`), so its `wsl.exe` call must stay BOUNDED — a wedged
    /// LxssManager (the `Wsl/Service/E_UNEXPECTED` freeze) otherwise hangs the
    /// window. Pin the worker-thread + `recv_timeout` shape at the source level
    /// (a behavioral test would need to hang a real `wsl.exe`).
    #[test]
    fn list_wsl_distros_is_time_bounded() {
        let src = include_str!("term.rs").replace("\r\n", "\n");
        let start = src
            .find("fn list_wsl_distros()")
            .expect("list_wsl_distros present");
        let body = &src[start..start + 1200];
        assert!(
            body.contains("recv_timeout"),
            "list_wsl_distros must bound the wsl.exe call with recv_timeout so a \
             hung LxssManager can't freeze the UI thread"
        );
    }

    #[test]
    fn parse_wsl_distros_strips_bom_nul_blanks_crlf() {
        // Simulated `wsl -l -q` decoded text: a leading UTF-16 BOM, CRLF line
        // endings, a blank line, and a trailing-NUL artifact.
        let text = "\u{feff}Ubuntu\r\nDebian\r\n\r\nkali-linux\u{0}\r\n";
        assert_eq!(
            super::parse_wsl_distros(text),
            vec!["Ubuntu", "Debian", "kali-linux"]
        );
        assert!(super::parse_wsl_distros("").is_empty());
        assert!(super::parse_wsl_distros("\u{feff}\r\n  \r\n").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn detect_shells_windows_lists_available_plus_distros() {
        let avail = |e: &str| matches!(e, "cmd.exe" | "pwsh.exe" | "wsl.exe");
        let distros = || vec!["Ubuntu".to_string()];
        let got = super::detect_shells_windows(avail, distros);
        assert!(
            got.iter()
                .any(|(l, a)| l == "Command Prompt" && a.as_slice() == ["cmd.exe"])
        );
        assert!(got.iter().any(|(l, _)| l == "PowerShell 7"));
        // powershell.exe was NOT "available" → Windows PowerShell is absent.
        assert!(!got.iter().any(|(l, _)| l == "Windows PowerShell"));
        assert!(
            got.iter()
                .any(|(l, a)| l == "WSL: Ubuntu" && a.as_slice() == ["wsl.exe", "-d", "Ubuntu"])
        );
    }

    #[cfg(windows)]
    #[test]
    fn detect_shells_windows_never_empty() {
        let got = super::detect_shells_windows(|_| false, Vec::new);
        assert_eq!(
            got,
            vec![("Command Prompt".to_string(), vec!["cmd.exe".to_string()])]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn detect_shells_unix_shell_env_first_and_dedupes() {
        let avail = |e: &str| matches!(e, "bash" | "zsh" | "fish");
        let got = super::detect_shells_unix(Some("/bin/zsh".to_string()), avail);
        // $SHELL=zsh is first (label = basename); the detected `zsh` isn't a dup.
        assert_eq!(got[0], ("zsh".to_string(), vec!["/bin/zsh".to_string()]));
        assert_eq!(got.iter().filter(|(_, a)| a[0].ends_with("zsh")).count(), 1);
        assert!(got.iter().any(|(l, _)| l == "bash"));
        assert!(got.iter().any(|(l, _)| l == "fish"));
    }

    #[cfg(not(windows))]
    #[test]
    fn detect_shells_unix_never_empty() {
        let got = super::detect_shells_unix(None, |_| false);
        assert_eq!(
            got,
            vec![("Shell".to_string(), vec!["/bin/sh".to_string()])]
        );
    }
}

#[cfg(test)]
mod wslenv_tests {
    use super::augment_wslenv;

    #[test]
    fn appends_with_u_flag_preserves_existing_and_dedups() {
        let vars = ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];
        // Empty existing → just our vars, each `/u`.
        assert_eq!(
            augment_wslenv("", &vars),
            "COLORTERM/u:TERM_PROGRAM/u:TERM_PROGRAM_VERSION/u"
        );
        // The user's existing WSLENV is preserved verbatim, ours appended.
        assert_eq!(
            augment_wslenv("FOO/p:BAR", &["COLORTERM"]),
            "FOO/p:BAR:COLORTERM/u"
        );
        // An entry the user already has (even with a different flag) is not
        // duplicated; matching is on the name before the `/flags`.
        assert_eq!(
            augment_wslenv("COLORTERM/up:X", &["COLORTERM", "TERM_PROGRAM"]),
            "COLORTERM/up:X:TERM_PROGRAM/u"
        );
        assert_eq!(augment_wslenv("COLORTERM", &["COLORTERM"]), "COLORTERM");
    }
}

#[cfg(test)]
mod home_dir_tests {
    use super::home_dir_fallback;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn prefers_home_then_userprofile_then_appdata() {
        // All three set (rare — a WSL user with both env worlds bleeding
        // through) → HOME wins. This is the Linux / macOS branch.
        assert_eq!(
            home_dir_fallback(from(&[
                ("HOME", "/h"),
                ("USERPROFILE", r"C:\u"),
                ("APPDATA", r"C:\a"),
            ])),
            Some(PathBuf::from("/h")),
        );
        // No HOME → USERPROFILE. This is the *Windows* branch — exactly
        // the gap the previous `var_os("HOME")`-only fallback missed.
        assert_eq!(
            home_dir_fallback(from(&[("USERPROFILE", r"C:\u"), ("APPDATA", r"C:\a"),])),
            Some(PathBuf::from(r"C:\u")),
        );
        // Only APPDATA set (very stripped Windows session) → APPDATA.
        assert_eq!(
            home_dir_fallback(from(&[("APPDATA", r"C:\a")])),
            Some(PathBuf::from(r"C:\a")),
        );
        // Nothing set (minimal Linux container without HOME) → None;
        // caller leaves cmd.cwd() untouched.
        assert_eq!(home_dir_fallback(from(&[])), None);
    }

    #[test]
    fn empty_env_var_value_falls_through_to_next() {
        // Cycle 180: `HOME=""` (a deliberately empty env var — happens
        // in stripped-down CI containers and after a misconfigured
        // `unset HOME` / `export HOME=` in a parent shell) used to
        // return `Some(PathBuf::from(""))`. CommandBuilder::cwd("")
        // then fed an invalid empty path to the OS spawn. Now empty
        // values are filtered as if unset, so the probe continues to
        // the next variable. Pinned at every level of the chain.
        //
        // HOME empty, USERPROFILE valid → USERPROFILE wins.
        assert_eq!(
            home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", r"C:\u")])),
            Some(PathBuf::from(r"C:\u")),
        );
        // HOME empty, USERPROFILE empty, APPDATA valid → APPDATA wins.
        assert_eq!(
            home_dir_fallback(from(&[
                ("HOME", ""),
                ("USERPROFILE", ""),
                ("APPDATA", r"C:\a"),
            ])),
            Some(PathBuf::from(r"C:\a")),
        );
        // All three empty → None. Caller leaves cmd.cwd() untouched
        // rather than handing an empty path to the OS spawn.
        assert_eq!(
            home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", ""), ("APPDATA", ""),])),
            None,
        );
    }

    /// Cycle 625 drift guard. `strip_ansi_bytes` is the pure ANSI-
    /// strip helper behind `log_strip_ansi`. Verify:
    ///   - CSI sequences (SGR / cursor moves / etc.) are removed
    ///   - OSC sequences (title, hyperlink, OSC 7) are removed,
    ///     terminated by either BEL or ESC\
    ///   - Single-char ESC (ESC c full-reset) is removed
    ///   - Plain printable bytes + newlines pass through
    #[test]
    fn strip_ansi_bytes_removes_csi_osc_and_single_esc() {
        use super::strip_ansi_bytes;
        // CSI SGR around plain text: "hello world".
        let s = b"\x1b[31mhello\x1b[0m world";
        assert_eq!(strip_ansi_bytes(s), b"hello world");
        // OSC 0 (set title) terminated by BEL.
        let s = b"prefix \x1b]0;my-title\x07 suffix";
        assert_eq!(strip_ansi_bytes(s), b"prefix  suffix");
        // OSC 8 (hyperlink) terminated by ESC\\.
        let s = b"\x1b]8;;http://example/\x1b\\link text\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi_bytes(s), b"link text");
        // Single-char ESC (full reset).
        let s = b"\x1bcclean";
        assert_eq!(strip_ansi_bytes(s), b"clean");
        // Newlines + tabs pass through.
        let s = b"line1\nline2\tindent\n";
        assert_eq!(strip_ansi_bytes(s), b"line1\nline2\tindent\n");
        // Bare ESC at the very end of buffer is dropped (matches
        // the documented split-across-reads limitation).
        let s = b"trail\x1b";
        assert_eq!(strip_ansi_bytes(s), b"trail");
        // Plain ASCII passes through unchanged.
        let s = b"no escapes here";
        assert_eq!(strip_ansi_bytes(s), b"no escapes here");
    }
}

#[cfg(test)]
mod conformance {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::Processor;

    type Rx = crossbeam_channel::Receiver<TermEvent>;

    fn harness_rx(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy,
        );
        (term, Processor::new(), rx)
    }

    fn harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
        let (t, p, _rx) = harness_rx(cols, rows);
        (t, p)
    }

    /// Concatenate everything the terminal wrote back to the PTY.
    fn drain_pty(rx: &Rx) -> String {
        let mut out = String::new();
        while let Ok(ev) = rx.try_recv() {
            if let TermEvent::PtyWrite(s) = ev {
                out.push_str(&s);
            }
        }
        out
    }

    fn feed(term: &mut Term<EventProxy>, p: &mut Processor, bytes: &[u8]) {
        p.advance(term, bytes);
    }

    /// Cycle 882: feed bytes through the SAME two-stage path the PTY reader
    /// thread uses — `Extractor::feed` then each `Chunk::Pass` →
    /// `Processor::advance` — so a test exercises kettle's REAL pipeline (the
    /// Extractor sits in front of the engine at runtime) instead of driving the
    /// alacritty `Processor` in isolation.
    fn feed_ex(term: &mut Term<EventProxy>, p: &mut Processor, ex: &mut Extractor, bytes: &[u8]) {
        for chunk in ex.feed(bytes) {
            if let Chunk::Pass(b) = chunk {
                p.advance(term, &b);
            }
        }
    }

    fn row_text(term: &Term<EventProxy>, row: i32) -> String {
        let g = term.grid();
        (0..g.columns())
            .map(|c| g[Point::new(Line(row), Column(c))].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn text_newline_and_cursor_addressing() {
        let (mut t, mut p) = harness(20, 5);
        feed(&mut t, &mut p, b"hello\r\nworld");
        assert_eq!(row_text(&t, 0), "hello");
        assert_eq!(row_text(&t, 1), "world");
        // CUP: ESC[3;2H then write — 1-based row/col.
        feed(&mut t, &mut p, b"\x1b[3;2HX");
        assert_eq!(row_text(&t, 2), " X");
    }

    /// Cycle 909 (R1): a selection made while scrolled back must read the
    /// VISIBLE (history) row, not the active-screen row at the same viewport
    /// index. This guards alacritty's `Selection` coordinate contract — it
    /// expects GRID-ABSOLUTE points (viewport − display_offset, via
    /// `viewport_to_point`). kettle-ui previously stored the raw viewport line,
    /// so copying while scrolled returned the wrong/empty text. The two branches
    /// below show the bug (raw viewport) vs the fix (`viewport_to_point`) select
    /// different rows — exactly why the conversion is required.
    #[test]
    fn selection_while_scrolled_reads_visible_row_not_active_screen() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        use alacritty_terminal::term::viewport_to_point;
        // 4 visible rows; feed 8 lines so the first 4 spill into scrollback.
        let (mut t, mut p) = harness(20, 4);
        feed(
            &mut t,
            &mut p,
            b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7",
        );
        // Bottom: visible rows are L4..L7. Scroll back 3 → visible top = L1.
        t.scroll_display(Scroll::Delta(3));
        let off = t.grid().display_offset();
        assert_eq!(off, 3, "scrolled back 3 lines");

        // FIXED: convert viewport row 0 (showing "L1") to its grid-absolute line.
        let a = viewport_to_point(off, Point::new(0usize, Column(0)));
        let mut s = Selection::new(SelectionType::Lines, a, Side::Left);
        s.update(a, Side::Right);
        t.selection = Some(s);
        let fixed = t.selection_to_string().unwrap_or_default();
        assert!(
            fixed.contains("L1"),
            "fixed reads the visible row: {fixed:?}"
        );
        assert!(
            !fixed.contains("L4"),
            "fixed must not read the active screen: {fixed:?}"
        );

        // BUGGY: the raw viewport line used as absolute reads the active-screen
        // row "L4" instead — the regression this conversion prevents.
        let b = Point::new(Line(0), Column(0));
        let mut s2 = Selection::new(SelectionType::Lines, b, Side::Left);
        s2.update(b, Side::Right);
        t.selection = Some(s2);
        let buggy = t.selection_to_string().unwrap_or_default();
        assert!(buggy.contains("L4"), "buggy reads active screen: {buggy:?}");
        assert_ne!(
            fixed.trim(),
            buggy.trim(),
            "display_offset conversion must change which row is copied"
        );
    }

    /// Cycle 909 (R1): a real drag-select (Simple selection spanning rows) made
    /// while scrolled to the top of history must copy the VISIBLE history rows,
    /// not the active screen — the exact action a user does when copying an
    /// earlier chunk of a long Claude Code / Codex conversation.
    #[test]
    fn simple_drag_selection_while_scrolled_copies_visible_rows() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        use alacritty_terminal::term::viewport_to_point;
        let (mut t, mut p) = harness(12, 3);
        feed(
            &mut t,
            &mut p,
            b"row-A\r\nrow-B\r\nrow-C\r\nrow-D\r\nrow-E\r\nrow-F",
        );
        // active screen = row-D/E/F; history = row-A/B/C. Scroll to the top.
        t.scroll_display(Scroll::Delta(3));
        let off = t.grid().display_offset();
        assert_eq!(off, 3, "scrolled to the top of a 3-line history");
        // Drag from viewport (0,0) down to (1, end): copies "row-A" + "row-B".
        let start = viewport_to_point(off, Point::new(0usize, Column(0)));
        let end = viewport_to_point(off, Point::new(1usize, Column(11)));
        let mut s = Selection::new(SelectionType::Simple, start, Side::Left);
        s.update(end, Side::Right);
        t.selection = Some(s);
        let copied = t.selection_to_string().unwrap_or_default();
        assert!(
            copied.contains("row-A"),
            "copied the visible top rows: {copied:?}"
        );
        assert!(copied.contains("row-B"), "{copied:?}");
        assert!(
            !copied.contains("row-D"),
            "must not read the active screen while scrolled: {copied:?}"
        );
    }

    /// Cycle 911 (e2e harness): replay an asciicast v2 trace — the format
    /// `dev_record.rs` writes — through the REAL VT pipeline and assert the grid
    /// reflects it. This is the `.cast` record→replay regression path: a captured
    /// Claude Code / Codex / tmux session can be re-fed deterministically (no
    /// PTY, no auth) to guard rendering, selection, and SGR handling.
    #[test]
    fn replays_asciicast_v2_output_into_grid() {
        // A minimal hand-authored trace (no real session data): plain text, an
        // SGR-bold run, and a CRLF — the shapes a Claude Code frame emits.
        let cast = concat!(
            "{\"version\":2,\"width\":20,\"height\":4}\n",
            "[0.10, \"o\", \"hello \"]\n",
            "[0.20, \"o\", \"\\u001b[1mworld\\u001b[0m\"]\n",
            "[0.30, \"o\", \"\\r\\nsecond line\"]\n",
        );
        let (mut t, mut p) = harness(20, 4);
        for line in cast.lines().skip(1) {
            let v: serde_json::Value = serde_json::from_str(line).expect("event is valid JSON");
            if v[1] == "o" {
                feed(&mut t, &mut p, v[2].as_str().unwrap_or("").as_bytes());
            }
        }
        assert_eq!(row_text(&t, 0), "hello world");
        assert_eq!(row_text(&t, 1), "second line");
        // The SGR bold from the trace applied to "world".
        let g = t.grid();
        assert!(
            g[Point::new(Line(0), Column(6))]
                .flags
                .contains(Flags::BOLD),
            "replayed SGR bold must reach the grid"
        );
    }

    #[test]
    fn erase_line_and_display() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"ABCDEFG");
        feed(&mut t, &mut p, b"\x1b[1;4H\x1b[K"); // cursor col4, erase to EOL
        assert_eq!(row_text(&t, 0), "ABC");
        feed(&mut t, &mut p, b"\x1b[2J"); // erase whole display
        assert_eq!(row_text(&t, 0), "");
    }

    #[test]
    fn sgr_truecolor_bold_and_reset() {
        use alacritty_terminal::vte::ansi::Color;
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"\x1b[1;38;2;10;20;30mZ\x1b[0mz");
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert!(z.flags.contains(Flags::BOLD));
        match z.fg {
            Color::Spec(rgb) => assert_eq!((rgb.r, rgb.g, rgb.b), (10, 20, 30)),
            other => panic!("expected truecolor, got {other:?}"),
        }
        // After SGR reset, the next cell is back to default fg + no bold.
        let z2 = &g[Point::new(Line(0), Column(1))];
        assert!(!z2.flags.contains(Flags::BOLD));
    }

    #[test]
    fn tab_stops_and_carriage_return() {
        let (mut t, mut p) = harness(20, 2);
        feed(&mut t, &mut p, b"a\tb");
        let s = row_text(&t, 0);
        assert_eq!(&s[..1], "a");
        assert_eq!(s.chars().nth(8), Some('b')); // default tab stop at col 8
        feed(&mut t, &mut p, b"\rZ");
        assert_eq!(row_text(&t, 0).chars().next(), Some('Z'));
    }

    #[test]
    fn alt_screen_and_bracketed_paste_modes() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"\x1b[?1049h");
        assert!(t.mode().contains(TermMode::ALT_SCREEN));
        feed(&mut t, &mut p, b"\x1b[?2004h");
        assert!(t.mode().contains(TermMode::BRACKETED_PASTE));
        feed(&mut t, &mut p, b"\x1b[?1049l");
        assert!(!t.mode().contains(TermMode::ALT_SCREEN));
    }

    #[test]
    fn scroll_region_and_index() {
        let (mut t, mut p) = harness(6, 4);
        // Restrict scrolling to rows 1..=2 (DECSTBM, 1-based), then make it
        // scroll: cursor to row2, newlines push row1 content up within region.
        feed(
            &mut t,
            &mut p,
            b"\x1b[1;2r\x1b[1;1Hone\x1b[2;1Htwo\r\n\r\nthree",
        );
        // Row 3 (outside the region) stays empty; region scrolled.
        assert_eq!(row_text(&t, 3), "");
    }

    #[test]
    fn dec_special_graphics_charset() {
        let (mut t, mut p) = harness(6, 2);
        // ESC ( 0 = DEC line-drawing into G0; q->─ x->│; ESC ( B back to ASCII.
        feed(&mut t, &mut p, b"\x1b(0qx\x1b(By");
        let s = row_text(&t, 0);
        let cs: Vec<char> = s.chars().collect();
        assert_eq!(cs[0], '\u{2500}', "q -> light horizontal");
        assert_eq!(cs[1], '\u{2502}', "x -> light vertical");
        assert_eq!(cs[2], 'y', "ASCII restored after ESC(B");
    }

    #[test]
    fn insert_and_delete_char() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"abcde");
        // Cursor to col 2 ('b'), delete it (DCH).
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[P");
        assert_eq!(row_text(&t, 0), "acde");
        // Insert a blank at col 1 (ICH), then type at it.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[@Z");
        assert_eq!(row_text(&t, 0), "Zacde");
    }

    #[test]
    fn insert_and_delete_line() {
        let (mut t, mut p) = harness(6, 4);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Delete line 2 (DL): r2 moves up into row 1.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[M");
        assert_eq!(row_text(&t, 0), "r0");
        assert_eq!(row_text(&t, 1), "r2");
        // Insert a line at row 1 (IL): pushes r2 back down.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[L");
        assert_eq!(row_text(&t, 1), "");
        assert_eq!(row_text(&t, 2), "r2");
    }

    #[test]
    fn save_restore_cursor_and_autowrap() {
        let (mut t, mut p) = harness(3, 3);
        // DECSC at row1col1, move away, write, DECRC back, overwrite.
        feed(&mut t, &mut p, b"\x1b7\x1b[3;3HX\x1b8A");
        assert_eq!(row_text(&t, 0).chars().next(), Some('A'));
        // Autowrap (DECAWM, on by default): 4 chars into 3 columns wraps.
        let (mut t2, mut p2) = harness(3, 3);
        feed(&mut t2, &mut p2, b"abcd");
        assert_eq!(row_text(&t2, 0), "abc");
        assert_eq!(row_text(&t2, 1), "d");
    }

    #[test]
    fn origin_mode_addresses_within_margins() {
        let (mut t, mut p) = harness(6, 5);
        // Scroll region rows 2..=4, enable origin mode, then home (1;1) is
        // the region top (absolute row index 1).
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6h\x1b[1;1HO");
        assert_eq!(row_text(&t, 0), "", "row 0 is above the margin");
        assert_eq!(row_text(&t, 1), "O", "origin-mode home = top margin");
    }

    #[test]
    fn dsr_cursor_position_report() {
        let (mut t, mut p, rx) = harness_rx(40, 10);
        // Move to row 3, col 5 (1-based), then DSR 6n.
        feed(&mut t, &mut p, b"\x1b[3;5H\x1b[6n");
        let reply = drain_pty(&rx);
        assert_eq!(reply, "\x1b[3;5R", "CPR must echo the 1-based cursor");
    }

    #[test]
    fn device_attributes_reply() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c"); // Primary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?"),
            "DA1 reply should be a CSI ? … c, got {reply:?}"
        );
        assert!(reply.ends_with('c'));
    }

    #[test]
    fn sgr_underline_dim_strike() {
        let (mut t, mut p) = harness(8, 2);
        // dim + single underline + strikeout, then a curly underline cell.
        feed(&mut t, &mut p, b"\x1b[2;4;9mA\x1b[0m\x1b[4:3mB");
        let g = t.grid();
        let a = &g[Point::new(Line(0), Column(0))];
        assert!(a.flags.contains(Flags::DIM));
        assert!(a.flags.contains(Flags::UNDERLINE));
        assert!(a.flags.contains(Flags::STRIKEOUT));
        let b = &g[Point::new(Line(0), Column(1))];
        assert!(
            b.flags.contains(Flags::UNDERCURL),
            "SGR 4:3 = curly underline"
        );
        assert!(!b.flags.contains(Flags::DIM), "SGR 0 reset cleared dim");
    }

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"\x1b#8"); // DEC screen alignment test
        for r in 0..3 {
            assert_eq!(row_text(&t, r), "EEEE", "DECALN fills row {r}");
        }
    }

    #[test]
    fn rep_repeats_last_graphic_char() {
        let (mut t, mut p) = harness(8, 2);
        // 'A' then REP 3 -> "AAAA".
        feed(&mut t, &mut p, b"A\x1b[3b");
        assert_eq!(row_text(&t, 0), "AAAA");
    }

    #[test]
    fn charset_g1_via_so_si() {
        let (mut t, mut p) = harness(6, 2);
        // Designate DEC special graphics into G1, SO -> G1, SI -> back to G0.
        feed(&mut t, &mut p, b"\x1b)0\x0eqx\x0fy");
        let cs: Vec<char> = row_text(&t, 0).chars().collect();
        assert_eq!(cs[0], '\u{2500}', "G1 q -> horizontal line");
        assert_eq!(cs[1], '\u{2502}', "G1 x -> vertical line");
        assert_eq!(cs[2], 'y', "SI returned to ASCII G0");
    }

    #[test]
    fn ris_full_reset_clears_origin_mode() {
        let (mut t, mut p) = harness(6, 4);
        // Origin mode on + scroll region, then RIS (ESC c) — a full reset —
        // so 1;1 is absolute home again.
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6hzz\x1bc\x1b[1;1HX");
        assert_eq!(row_text(&t, 0), "X", "RIS cleared origin mode + region");
    }

    #[test]
    fn el_erase_to_left() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor to col 3 (1-based), EL 1 = erase start..=cursor.
        feed(&mut t, &mut p, b"\x1b[1;3H\x1b[1K");
        // cols 0..=2 cleared; "DE" remains at cols 3,4.
        assert_eq!(row_text(&t, 0), "   DE");
    }

    #[test]
    fn ed_erase_below() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Cursor to row 2 col 1 (1-based), ED 0 = erase cursor..=end.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[0J");
        assert_eq!(row_text(&t, 0), "r0", "row above the cursor kept");
        assert_eq!(row_text(&t, 1), "", "cursor row erased");
        assert_eq!(row_text(&t, 2), "", "rows below erased");
    }

    #[test]
    fn da2_secondary_device_attributes() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[>c"); // Secondary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[>") && reply.ends_with('c'),
            "DA2 reply should be CSI > … c, got {reply:?}"
        );
    }

    #[test]
    fn ech_erases_in_place() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor col 2 (1-based), ECH 2 clears 2 cells, cursor unmoved.
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[2X");
        assert_eq!(row_text(&t, 0), "A  DE");
    }

    #[test]
    fn ich_shifts_right_off_edge() {
        let (mut t, mut p) = harness(5, 2);
        feed(&mut t, &mut p, b"abcde");
        // ICH 2 at home pushes cells right; the line is 5 wide so d,e fall off.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2@");
        assert_eq!(row_text(&t, 0), "  abc");
    }

    #[test]
    fn absolute_cursor_moves_cha_hpa_vpa() {
        let (mut t, mut p) = harness(6, 4);
        // CHA: column-absolute to col 3 then write.
        feed(&mut t, &mut p, b"abcde\x1b[3GZ");
        assert_eq!(row_text(&t, 0), "abZde");
        // HPA (ESC[`) col 2 on row 1, VPA (ESC[d) row 3.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2`Q\x1b[3dW");
        assert_eq!(row_text(&t, 0).chars().nth(1), Some('Q'), "HPA col 2");
        // VPA changes the row only; column stays where it was (col 3 after Q).
        assert_eq!(row_text(&t, 2).chars().nth(2), Some('W'), "VPA row 3");
    }

    #[test]
    fn decsc_restores_sgr_attributes() {
        let (mut t, mut p) = harness(6, 3);
        // Bold on, DECSC (saves cursor + pen), reset SGR + move away,
        // DECRC restores both, then write — must be bold at the saved cell.
        feed(&mut t, &mut p, b"\x1b[1m\x1b7\x1b[0m\x1b[3;4HZ\x1b8A");
        let a = &t.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(a.c, 'A');
        assert!(
            a.flags.contains(Flags::BOLD),
            "DECRC must restore the saved SGR pen"
        );
    }

    /// Cycle 870: synchronized output (DEC private mode 2026 / BSU·ESU). While a
    /// sync block is open the engine MUST buffer mutations so a renderer that
    /// locks the grid never samples a half-drawn frame; the buffered changes
    /// apply atomically on close. This is the property that lets well-behaved
    /// TUIs avoid the transient mid-repaint tearing a terminal would otherwise
    /// show. The bytes are fed through kettle's REAL pipeline (`feed_ex` →
    /// Extractor → Processor), so this also guards that a future `Extractor`
    /// change cannot swallow the `?2026` toggles (cycle 882: was previously
    /// fed straight to the Processor, bypassing the Extractor it claims to
    /// guard).
    #[test]
    fn synchronized_update_defers_grid_mutation_until_close() {
        let (mut t, mut p) = harness(6, 2);
        let mut ex = Extractor::new();
        feed_ex(&mut t, &mut p, &mut ex, b"A");
        assert_eq!(t.grid()[Point::new(Line(0), Column(0))].c, 'A');
        // Open a synchronized update, return to col 0 and overwrite with 'B',
        // but DO NOT close the block yet.
        feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026h\rB");
        assert_eq!(
            t.grid()[Point::new(Line(0), Column(0))].c,
            'A',
            "grid mutated mid-synchronized-update (mode 2026 not honored)"
        );
        // Close the block — the buffered write now applies atomically.
        feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026l");
        assert_eq!(
            t.grid()[Point::new(Line(0), Column(0))].c,
            'B',
            "synchronized update not flushed on close"
        );
    }

    #[test]
    fn su_sd_scroll_up_and_down() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        feed(&mut t, &mut p, b"\x1b[1S"); // SU 1: content moves up
        assert_eq!(row_text(&t, 0), "r1");
        assert_eq!(row_text(&t, 1), "r2");
        assert_eq!(row_text(&t, 2), "");

        let (mut t2, mut p2) = harness(4, 3);
        feed(&mut t2, &mut p2, b"a\r\nb\r\nc");
        feed(&mut t2, &mut p2, b"\x1b[1T"); // SD 1: content moves down
        assert_eq!(row_text(&t2, 0), "");
        assert_eq!(row_text(&t2, 1), "a");
        assert_eq!(row_text(&t2, 2), "b");
    }

    #[test]
    fn decscusr_sets_cursor_shape() {
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[3 q"); // DECSCUSR 3 = (blinking) underline
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Underline);
        feed(&mut t, &mut p, b"\x1b[5 q"); // 5 = (blinking) bar/beam
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Beam);
        feed(&mut t, &mut p, b"\x1b[1 q"); // 1 = (blinking) block
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
    }

    #[test]
    fn dec_mode_25_hide_collapses_renderable_cursor_to_hidden() {
        // Cursor visibility (DEC ?25) and cursor shape (DECSCUSR `q`) are
        // tracked in different places in the engine; `RenderableContent`
        // *folds* them so the renderer only has to look at one field. This
        // test pins that contract — what the renderer reads is `Hidden` the
        // moment a program clears ?25, even if the shape was set to
        // something else first. Otherwise we'd silently keep drawing a
        // cursor at TUI apps that asked us not to (less, fzf full-screen).
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[1 q"); // shape = block (visible default)
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
        feed(&mut t, &mut p, b"\x1b[?25l"); // ?25 cleared = hide cursor
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Hidden,
            "DEC ?25 l must collapse the renderable cursor to Hidden"
        );
        feed(&mut t, &mut p, b"\x1b[?25h"); // ?25 set = show again
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Block,
            "DEC ?25 h restores the previous shape"
        );
    }

    #[test]
    fn wide_cjk_char_occupies_two_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, "世A".as_bytes());
        let g = t.grid();
        let c0 = &g[Point::new(Line(0), Column(0))];
        assert_eq!(c0.c, '世');
        assert!(c0.flags.contains(Flags::WIDE_CHAR), "CJK = wide");
        assert!(
            g[Point::new(Line(0), Column(1))]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER),
            "second cell is the wide spacer"
        );
        assert_eq!(g[Point::new(Line(0), Column(2))].c, 'A');
    }

    #[test]
    fn wide_char_wraps_when_it_does_not_fit() {
        let (mut t, mut p) = harness(3, 3);
        // 2 narrow + 1 wide: the wide char can't fit in the last column,
        // so it wraps to the next row.
        feed(&mut t, &mut p, "ab世".as_bytes());
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(t.grid()[Point::new(Line(1), Column(0))].c, '世');
    }

    #[test]
    fn combining_mark_is_zero_width() {
        let (mut t, mut p) = harness(6, 2);
        // 'e' + combining acute accent: one cell, mark stored as zerowidth.
        feed(&mut t, &mut p, "e\u{0301}X".as_bytes());
        let g = t.grid();
        let base = &g[Point::new(Line(0), Column(0))];
        assert_eq!(base.c, 'e');
        assert_eq!(
            base.zerowidth(),
            Some(&['\u{0301}'][..]),
            "combining mark attaches to the base cell"
        );
        assert_eq!(
            g[Point::new(Line(0), Column(1))].c,
            'X',
            "next glyph is in the very next cell (mark took no column)"
        );
    }

    #[test]
    fn osc4_palette_query_emits_color_request() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Query palette entry 1.
        feed(&mut t, &mut p, b"\x1b]4;1;?\x07");
        let got_idx = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ColorRequest(idx, _) => Some(idx),
            _ => None,
        });
        assert_eq!(got_idx, Some(1), "OSC 4 ; 1 ; ? requests palette index 1");
    }

    #[test]
    fn sgr_underline_style_variants_set_distinct_flags() {
        // Five style bits in the engine: `\e[4m` (single), `\e[21m` or
        // `\e[4:2m` (double), `\e[4:3m` (curl), `\e[4:4m` (dotted),
        // `\e[4:5m` (dashed). The renderer (cycle 81) reads these and
        // draws differently per style — this pins each one reaching the
        // engine's cell flags so a future engine bump can't silently
        // drop a variant.
        let (mut t, mut p) = harness(20, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me",
        );
        let g = t.grid();
        let f = |c: usize| g[Point::new(Line(0), Column(c))].flags;
        assert!(f(0).contains(Flags::UNDERLINE), "[4m → UNDERLINE on `a`");
        assert!(
            f(1).contains(Flags::DOUBLE_UNDERLINE),
            "[4:2m → DOUBLE_UNDERLINE on `b`"
        );
        assert!(f(2).contains(Flags::UNDERCURL), "[4:3m → UNDERCURL on `c`");
        assert!(
            f(3).contains(Flags::DOTTED_UNDERLINE),
            "[4:4m → DOTTED_UNDERLINE on `d`"
        );
        assert!(
            f(4).contains(Flags::DASHED_UNDERLINE),
            "[4:5m → DASHED_UNDERLINE on `e`"
        );
        // Each variant is mutually-exclusive: setting DOUBLE clears the
        // previous UNDERLINE bit (alacritty single-underline-flag model).
        // Confirm by checking `b` doesn't still carry plain UNDERLINE.
        assert!(
            !f(1).contains(Flags::UNDERLINE),
            "[4:2m must clear plain UNDERLINE"
        );
    }

    #[test]
    fn sgr_58_sets_per_cell_underline_color() {
        // Neovim spell-check / git diff / lsp diagnostics emit per-cell
        // underline color via SGR 58. The engine stores it on the cell;
        // the renderer reads it (cycle 80) so the squiggle color follows
        // the request instead of using the text fg. Confirms truecolor
        // form `\e[58;2;r;g;bm` reaches `cell.underline_color()`.
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};
        let (mut t, mut p) = harness(8, 2);
        // Underline on + red underline color, then write a glyph.
        feed(&mut t, &mut p, b"\x1b[4m\x1b[58;2;200;30;30mX");
        let grid = t.grid();
        let cell = &grid[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'X');
        assert!(
            cell.flags.contains(Flags::UNDERLINE),
            "SGR 4 must set UNDERLINE"
        );
        assert_eq!(
            cell.underline_color(),
            Some(AnsiColor::Spec(AnsiRgb {
                r: 200,
                g: 30,
                b: 30
            })),
            "SGR 58 must store the per-cell underline color"
        );
        // SGR 59 resets to default (None), leaving UNDERLINE intact.
        feed(&mut t, &mut p, b"\x1b[59mY");
        let cell2 = &t.grid()[Point::new(Line(0), Column(1))];
        assert_eq!(cell2.c, 'Y');
        assert!(cell2.flags.contains(Flags::UNDERLINE));
        assert_eq!(
            cell2.underline_color(),
            None,
            "SGR 59 must clear the per-cell underline color"
        );
    }

    #[test]
    fn osc4_multi_index_query_emits_one_request_per_index() {
        // vte's OSC 4 handler chunks the params in pairs (`;idx;val`), so
        // a single `OSC 4 ; 1 ; ? ; 7 ; ?` should ask for *two* colors in
        // one go. tmux, neovim 0.10+ and base16-shell-hook all batch
        // palette probes this way — without per-pair dispatch they'd see
        // only the first reply and assume the rest of the palette equals
        // the engine default, breaking the dark/light detection they rely
        // on.
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b]4;1;?;7;?\x07");
        let mut indices: Vec<usize> = rx
            .try_iter()
            .filter_map(|ev| match ev {
                TermEvent::ColorRequest(idx, _) => Some(idx),
                _ => None,
            })
            .collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            vec![1, 7],
            "multi-index OSC 4 must fire one ColorRequest per `;idx;?` pair"
        );
    }

    #[test]
    fn osc_10_11_12_set_populate_default_color_slots() {
        // OSC 10/11/12 SET should populate the engine's `Colors[256..=258]`
        // slots (default fg, default bg, cursor) so the renderer's
        // `resolve_query` reflects the override on the next frame. Without
        // this round-trip, OSC 12 (set cursor color) was a silent drop in
        // the render path — see commit notes for cycle 56. Confirms the
        // pair: OSC 4 set was tested in cycle 47; OSC 10/11/12 are the
        // close siblings that use the same Colors slots.
        for (input, idx) in &[
            (b"\x1b]10;rgb:11/22/33\x07" as &[u8], 256usize),
            (b"\x1b]11;rgb:44/55/66\x07", 257),
            (b"\x1b]12;rgb:77/88/99\x07", 258),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, input);
            let c = t.colors()[*idx].unwrap_or_else(|| panic!("slot {idx} unset after {input:?}"));
            // The exact values from the xparsecolor input (engine packs
            // each `RR` byte pair into a single u8).
            let want = match idx {
                256 => (0x11, 0x22, 0x33),
                257 => (0x44, 0x55, 0x66),
                258 => (0x77, 0x88, 0x99),
                _ => unreachable!(),
            };
            assert_eq!((c.r, c.g, c.b), want, "wrong color for slot {idx}");
        }
    }

    #[test]
    fn osc_104_no_params_resets_all_256_palette_slots() {
        // OSC 104 with no parameters (just `\e]104\a` or `\e]104;\a`)
        // resets *every* palette index (0..256), not just one. xterm
        // documents this: "OSC 104 ; c → reset color number c (default
        // restore palette)." Tools like `colorls`/`zsh-colorize`'s
        // theme-changers emit it to undo their session-wide palette
        // overrides on exit. The kettle conformance test from cycle 47
        // covered only the indexed form (`\e]104;1\a`); pin the
        // no-arg-resets-all branch too so it can't quietly regress
        // (e.g. if alacritty/vte upstream change the dispatch table).
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Populate three slots so we have something to confirm reset against.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        feed(&mut t, &mut p, b"\x1b]4;2;rgb:44/55/66\x07");
        feed(&mut t, &mut p, b"\x1b]4;200;rgb:77/88/99\x07");
        assert!(t.colors()[1].is_some(), "slot 1 should be set");
        assert!(t.colors()[2].is_some(), "slot 2 should be set");
        assert!(t.colors()[200].is_some(), "slot 200 should be set");
        // OSC 104 with no parameters → reset all 256 palette indices.
        feed(&mut t, &mut p, b"\x1b]104\x07");
        for idx in 0..256 {
            assert!(
                t.colors()[idx].is_none(),
                "slot {idx} should be cleared after OSC 104 (no params)"
            );
        }
    }

    #[test]
    fn osc_110_111_112_reset_default_fg_bg_cursor_slots() {
        // OSC 110 / 111 / 112 are the reset siblings of OSC 10/11/12 (set
        // default fg/bg/cursor). They tell the engine to throw away any
        // override the user-program set so the renderer falls back to the
        // theme's defaults. Kettle's render path reads `t.colors()[256..=258]`
        // each frame; if the engine didn't honor these resets, a program
        // that did `\e]10;rgb:11/22/33\a` then `\e]110\a` to undo would
        // leave the (red) override in place — a real bug class that
        // matches the cycle-56/65/66 "set went through but reset was
        // silently dropped" shape (cycles fixed the set path; this test
        // pins the reset path so it can't regress in the other
        // direction). Same loop covers all three indices in one
        // declarative table.
        for (idx, set, reset) in &[
            (
                256usize,
                &b"\x1b]10;rgb:11/22/33\x07"[..],
                &b"\x1b]110\x07"[..],
            ),
            (
                257usize,
                &b"\x1b]11;rgb:44/55/66\x07"[..],
                &b"\x1b]111\x07"[..],
            ),
            (
                258usize,
                &b"\x1b]12;rgb:77/88/99\x07"[..],
                &b"\x1b]112\x07"[..],
            ),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, set);
            assert!(
                t.colors()[*idx].is_some(),
                "slot {idx} should be populated after OSC set"
            );
            feed(&mut t, &mut p, reset);
            assert!(
                t.colors()[*idx].is_none(),
                "slot {idx} should be cleared after the OSC reset sibling"
            );
        }
    }

    #[test]
    fn osc_color_set_query_reset_round_trip_through_engine() {
        // Round-trip companion to the OSC query test: confirm OSC 4 set +
        // OSC 104 reset actually move the engine's `Colors` slot (so our
        // `kettle_render::resolve_query` will reflect changes live —
        // tested separately in kettle-render). This guards against an
        // upstream regression silently disconnecting the set/reset path
        // from the OSC 4 query path we ship.
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Initially the engine has no override for palette 1.
        assert!(t.colors()[1].is_none(), "expected no override pre-set");
        // OSC 4 ; 1 ; rgb:11/22/33  → set.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        let after_set = t.colors()[1].expect("OSC 4 set must populate slot 1");
        assert_eq!(
            (after_set.r, after_set.g, after_set.b),
            (0x11, 0x22, 0x33),
            "engine must store the override exactly"
        );
        // OSC 104 ; 1  → reset that index only.
        feed(&mut t, &mut p, b"\x1b]104;1\x07");
        assert!(t.colors()[1].is_none(), "OSC 104 ; 1 must clear slot 1");
    }

    #[test]
    fn xtwinops_text_area_size_pixels_formats_csi4_reply() {
        // CSI 14 t — text-area pixel size. The engine raises
        // TextAreaSizeRequest(fmt) and expects the caller to plug in cell
        // dimensions + grid size; the formatter then produces the standard
        // `CSI 4 ; h ; w t` xtwinops reply (h = rows × cell_h, w = cols ×
        // cell_w). Sixel/kitty/iTerm2 image apps depend on this to compute
        // pixel-accurate placements.
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[14t");
        let fmt = rx
            .try_iter()
            .find_map(|ev| match ev {
                TermEvent::TextAreaSizeRequest(f) => Some(f),
                _ => None,
            })
            .expect("CSI 14 t must raise a TextAreaSizeRequest");
        // 9 px wide × 18 px tall cells on a 40×10 grid → 360 × 180 px.
        let reply = fmt(alacritty_terminal::event::WindowSize {
            num_lines: 10,
            num_cols: 40,
            cell_width: 9,
            cell_height: 18,
        });
        assert_eq!(
            reply, "\x1b[4;180;360t",
            "CSI 14 t reply must be CSI 4 ; <height-px> ; <width-px> t"
        );
    }

    #[test]
    fn osc_color_queries_carry_index_and_format_xparsecolor_reply() {
        // OSC 10 / 11 / 12 (default fg / bg / cursor) and OSC 4 ; n ; ? are
        // the four queries shells and TUIs use to detect light-vs-dark and
        // theme colors. Each must (a) emit a `ColorRequest` carrying the
        // correct index — 256 / 257 / 258 / palette-idx — and (b) hand back
        // an engine-supplied formatter that renders the canonical xparsecolor
        // reply `\e]<prefix>;rgb:RRRR/GGGG/BBBB\` so apps that probe for the
        // exact wire format (mc / neovim / gnome-terminal probes) accept it.
        let cases: &[(&[u8], usize, &str)] = &[
            (b"\x1b]10;?\x07", 256, "\x1b]10;rgb:"), // OSC 10 — fg
            (b"\x1b]11;?\x07", 257, "\x1b]11;rgb:"), // OSC 11 — bg
            (b"\x1b]12;?\x07", 258, "\x1b]12;rgb:"), // OSC 12 — cursor
            (b"\x1b]4;7;?\x07", 7, "\x1b]4;7;rgb:"), // OSC 4 ; 7 — palette
        ];
        for (input, want_idx, want_prefix) in cases {
            let (mut t, mut p, rx) = harness_rx(8, 2);
            feed(&mut t, &mut p, input);
            let (idx, fmt) = rx
                .try_iter()
                .find_map(|ev| match ev {
                    TermEvent::ColorRequest(i, f) => Some((i, f)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no ColorRequest for {input:?}"));
            assert_eq!(idx, *want_idx, "wrong index for {input:?}");

            // Format with a known value and verify the wire shape. The
            // 8-bit channels are doubled to 16-bit per xparsecolor.
            let reply = fmt(alacritty_terminal::vte::ansi::Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            });
            let want_payload = format!("{want_prefix}1212/3434/5656");
            assert!(
                reply.starts_with(&want_payload),
                "{input:?} reply must start with {want_payload:?}, got {reply:?}"
            );
        }
    }

    #[test]
    fn decrqm_reports_mode_state() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Enable bracketed paste, then DECRQM-query it (CSI ? 2004 $ p).
        feed(&mut t, &mut p, b"\x1b[?2004h\x1b[?2004$p");
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2004;1") && reply.ends_with("$y"),
            "DECRPM should report mode 2004 as set, got {reply:?}"
        );
    }

    #[test]
    fn osc52_copy_emits_clipboard_store() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // base64("hi") = "aGk=" ; OSC 52 ; c ; <b64> ST
        feed(&mut t, &mut p, b"\x1b]52;c;aGk=\x07");
        let stored = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ClipboardStore(_, s) => Some(s),
            _ => None,
        });
        assert_eq!(stored.as_deref(), Some("hi"), "OSC 52 c sets clipboard");
    }

    #[test]
    fn osc8_hyperlink_carries_on_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b]8;;https://x.example\x07Z\x1b]8;;\x07W",
        );
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert_eq!(z.c, 'Z');
        assert_eq!(
            z.hyperlink().map(|h| h.uri().to_string()).as_deref(),
            Some("https://x.example"),
            "OSC 8 URI attaches to the cell"
        );
        // After the closing OSC 8 ; ; the link is cleared.
        assert!(g[Point::new(Line(0), Column(1))].hyperlink().is_none());
    }

    #[test]
    fn alt_screen_preserves_primary_content() {
        let (mut t, mut p) = harness(8, 3);
        feed(&mut t, &mut p, b"main");
        feed(&mut t, &mut p, b"\x1b[?1049h"); // enter alt screen
        assert_eq!(row_text(&t, 0), "", "alt screen starts blank");
        feed(&mut t, &mut p, b"\x1b[2J\x1b[1;1Halt");
        assert_eq!(row_text(&t, 0), "alt");
        feed(&mut t, &mut p, b"\x1b[?1049l"); // back to primary
        assert_eq!(row_text(&t, 0), "main", "primary content restored");
    }

    #[test]
    fn synchronized_output_applies_content() {
        let (mut t, mut p) = harness(8, 2);
        // DECSET 2026 brackets an atomic update; the content must be present
        // and correct once the synchronized update ends.
        feed(&mut t, &mut p, b"\x1b[?2026hhello\x1b[?2026l");
        assert_eq!(row_text(&t, 0), "hello");
    }

    #[test]
    fn decrqm_reports_synchronized_output_mode() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?2026$p"); // DECRQM query of mode 2026
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2026;") && reply.ends_with("$y"),
            "DECRPM should report mode 2026, got {reply:?}"
        );
    }

    #[test]
    fn nel_index_reverse_index() {
        // NEL (ESC E): CR+LF to the next line.
        let (mut t, mut p) = harness(6, 3);
        feed(&mut t, &mut p, b"ab\x1bEcd");
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(row_text(&t, 1), "cd");

        // IND (ESC D): down one line, column preserved.
        let (mut t2, mut p2) = harness(6, 3);
        feed(&mut t2, &mut p2, b"X\x1bDY");
        assert_eq!(row_text(&t2, 0), "X");
        assert_eq!(row_text(&t2, 1), " Y", "IND keeps the column");

        // RI (ESC M): up one line, column preserved.
        let (mut t3, mut p3) = harness(6, 3);
        feed(&mut t3, &mut p3, b"\x1b[2;1Hb\x1bMa");
        assert_eq!(row_text(&t3, 0), " a", "RI moved up, kept column");
        assert_eq!(row_text(&t3, 1), "b");
    }

    #[test]
    fn decid_replies_like_da1() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1bZ"); // DECID
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?") && reply.ends_with('c'),
            "DECID should reply like DA1 (CSI ? … c), got {reply:?}"
        );
    }

    #[test]
    fn cursor_blink_mode_emits_event() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?12h"); // DECSET 12 = cursor blink on
        let got = rx
            .try_iter()
            .any(|ev| matches!(ev, TermEvent::CursorBlinkingChange));
        assert!(got, "?12h should signal a cursor-blink change");
    }

    #[test]
    fn dec_mode_12_toggles_engine_cursor_blink_state() {
        // The companion to the event test above: confirm the engine actually
        // tracks the blink state on `cursor_style().blinking` so the UI can
        // read it live (we honor the *running* program's wish for solid vs.
        // blinking cursor, not just the static config). This is what
        // `Terminal::cursor_blinking()` returns — exercised through the real
        // vte parser so the mode-flip path is real.
        let (mut t, mut p) = harness(8, 2);
        let initial = t.cursor_style().blinking;
        feed(&mut t, &mut p, b"\x1b[?12h"); // request blink
        assert!(
            t.cursor_style().blinking,
            "DEC mode 12 set must turn cursor blink on (was {initial})"
        );
        feed(&mut t, &mut p, b"\x1b[?12l"); // request solid
        assert!(
            !t.cursor_style().blinking,
            "DEC mode 12 reset must turn cursor blink off"
        );
    }

    #[test]
    fn cht_cbt_tab_navigation() {
        // CHT (CSI I): forward N tab stops (default stops every 8).
        let (mut t, mut p) = harness(40, 2);
        feed(&mut t, &mut p, b"\x1b[3I*");
        assert_eq!(
            row_text(&t, 0).chars().nth(24),
            Some('*'),
            "CHT 3 → column 24"
        );
        // CBT (CSI Z): backward N tab stops.
        let (mut t2, mut p2) = harness(40, 2);
        feed(&mut t2, &mut p2, b"\x1b[1;21H\x1b[1ZB");
        assert_eq!(
            row_text(&t2, 0).chars().nth(16),
            Some('B'),
            "CBT 1 from col 20 → column 16"
        );
    }

    #[test]
    fn xtwinops_text_area_size_chars() {
        // XTWINOPS CSI 18 t → report text area size in characters as
        // CSI 8 ; rows ; cols t (DA-style, deterministic — no window needed).
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[18t");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[8;10;40t",
            "CSI 18 t must report 8;<rows>;<cols>t"
        );
    }

    #[test]
    fn dsr_device_status_ok() {
        // DSR CSI 5 n → "terminal OK" = CSI 0 n (no malfunction).
        let (mut t, mut p, rx) = harness_rx(8, 3);
        feed(&mut t, &mut p, b"\x1b[5n");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[0n",
            "CSI 5 n must reply CSI 0 n (ready)"
        );
    }

    #[test]
    fn da1_primary_attributes_exact_params() {
        // Primary DA (CSI c) must reply exactly CSI ? 6 c — VT2xx-class id
        // with no extensions — so apps don't probe for features we lack.
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[?6c",
            "DA1 reply must be exactly CSI ? 6 c"
        );
        // CSI 0 c is an explicit-parameter alias for the same query.
        let (mut t2, mut p2, rx2) = harness_rx(10, 3);
        feed(&mut t2, &mut p2, b"\x1b[0c");
        assert_eq!(drain_pty(&rx2), "\x1b[?6c", "CSI 0 c == CSI c");
    }

    #[test]
    fn irm_insert_mode_shifts_right() {
        // Default (replace) vs IRM (CSI 4 h): inserting pushes text right.
        let (mut t, mut p) = harness(10, 2);
        feed(&mut t, &mut p, b"ABCD\x1b[1;1H\x1b[4hX");
        assert_eq!(row_text(&t, 0), "XABCD", "IRM inserts, shifting right");
        feed(&mut t, &mut p, b"\x1b[4l\x1b[1;1HZ");
        assert_eq!(row_text(&t, 0), "ZABCD", "4 l → back to replace");
    }

    #[test]
    fn dectcem_cursor_visibility_mode() {
        let (mut t, mut p) = harness(6, 2);
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "shown by default");
        feed(&mut t, &mut p, b"\x1b[?25l");
        assert!(!t.mode().contains(TermMode::SHOW_CURSOR), "?25 l hides");
        feed(&mut t, &mut p, b"\x1b[?25h");
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "?25 h shows");
    }

    #[test]
    fn lnm_newline_mode_sets_flag() {
        // CSI 20 h sets LNM; 20 l clears it. (alacritty_terminal tracks the
        // mode but does not itself translate LF→CRLF on output, so only the
        // mode bit — the conformant, observable part — is asserted here.)
        let (mut t, mut p) = harness(8, 2);
        assert!(!t.mode().contains(TermMode::LINE_FEED_NEW_LINE));
        feed(&mut t, &mut p, b"\x1b[20h");
        assert!(t.mode().contains(TermMode::LINE_FEED_NEW_LINE), "20 h sets");
        feed(&mut t, &mut p, b"\x1b[20l");
        assert!(
            !t.mode().contains(TermMode::LINE_FEED_NEW_LINE),
            "20 l clears LNM"
        );
    }

    #[test]
    fn sgr_individual_attribute_resets() {
        // Cycle 238: VT conformance gap. SGR `set` codes are well
        // tested (`sgr_truecolor_bold_and_reset`,
        // `sgr_underline_dim_strike`, …) but the individual
        // attribute-*off* codes weren't:
        //   * SGR 22 — normal intensity (clears bold *and* dim)
        //   * SGR 23 — not italic
        //   * SGR 24 — not underlined (clears all underline styles)
        //   * SGR 27 — not reversed
        //   * SGR 29 — not strikethrough
        // These matter for tools that emit nested styling: nvim /
        // tmux / less / `git diff --color` set an attribute, write,
        // unset just that attribute, and continue with the rest of
        // their accumulated SGR state. Without these we'd silently
        // diverge from xterm behavior (cells AFTER the `not X`
        // would carry residual flags).
        //
        // Note on SGR 25 / blink: `alacritty_terminal`'s `Cell::flags`
        // bitfield deliberately doesn't track BLINK (blink is a
        // render-time concern, not a cell attribute). SGR 5 / 25 are
        // accepted at the parser layer but produce no cell-flag
        // change; we don't assert on them here.
        let (mut t, mut p) = harness(20, 2);
        // Stack: bold + dim + italic + underline + reverse + strike.
        // (Skip blink — see note above.)
        feed(&mut t, &mut p, b"\x1b[1;2;3;4;7;9mA");
        let a = &t.grid()[Point::new(Line(0), Column(0))];
        assert!(a.flags.contains(Flags::BOLD), "SGR 1 set");
        assert!(a.flags.contains(Flags::DIM), "SGR 2 set");
        assert!(a.flags.contains(Flags::ITALIC), "SGR 3 set");
        assert!(a.flags.contains(Flags::UNDERLINE), "SGR 4 set");
        assert!(a.flags.contains(Flags::INVERSE), "SGR 7 set");
        assert!(a.flags.contains(Flags::STRIKEOUT), "SGR 9 set");

        // SGR 22 → clears BOTH bold and dim (normal intensity).
        feed(&mut t, &mut p, b"\x1b[22mB");
        let b = &t.grid()[Point::new(Line(0), Column(1))];
        assert!(!b.flags.contains(Flags::BOLD), "SGR 22 clears bold");
        assert!(!b.flags.contains(Flags::DIM), "SGR 22 clears dim");
        // The other flags must still be set.
        assert!(b.flags.contains(Flags::ITALIC), "SGR 22 keeps italic");
        assert!(b.flags.contains(Flags::UNDERLINE), "SGR 22 keeps underline");
        assert!(b.flags.contains(Flags::INVERSE), "SGR 22 keeps inverse");
        assert!(b.flags.contains(Flags::STRIKEOUT), "SGR 22 keeps strikeout");

        // SGR 23 → italic off only.
        feed(&mut t, &mut p, b"\x1b[23mC");
        let c = &t.grid()[Point::new(Line(0), Column(2))];
        assert!(!c.flags.contains(Flags::ITALIC), "SGR 23 clears italic");
        assert!(c.flags.contains(Flags::UNDERLINE), "SGR 23 keeps underline");

        // SGR 24 → underline off (any style).
        feed(&mut t, &mut p, b"\x1b[24mD");
        let d = &t.grid()[Point::new(Line(0), Column(3))];
        assert!(
            !d.flags.contains(Flags::UNDERLINE),
            "SGR 24 clears underline"
        );
        assert!(d.flags.contains(Flags::INVERSE), "SGR 24 keeps inverse");

        // SGR 27 → inverse off.
        feed(&mut t, &mut p, b"\x1b[27mE");
        let e = &t.grid()[Point::new(Line(0), Column(4))];
        assert!(!e.flags.contains(Flags::INVERSE), "SGR 27 clears inverse");
        assert!(e.flags.contains(Flags::STRIKEOUT), "SGR 27 keeps strikeout");

        // SGR 29 → strikeout off.
        feed(&mut t, &mut p, b"\x1b[29mF");
        let f = &t.grid()[Point::new(Line(0), Column(5))];
        assert!(
            !f.flags.contains(Flags::STRIKEOUT),
            "SGR 29 clears strikeout"
        );
    }

    #[test]
    fn app_cursor_and_keypad_modes() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1h");
        assert!(t.mode().contains(TermMode::APP_CURSOR), "DECCKM set");
        feed(&mut t, &mut p, b"\x1b=");
        assert!(t.mode().contains(TermMode::APP_KEYPAD), "DECKPAM set");
        feed(&mut t, &mut p, b"\x1b[?1l\x1b>");
        assert!(!t.mode().contains(TermMode::APP_CURSOR));
        assert!(!t.mode().contains(TermMode::APP_KEYPAD), "DECKPNM clears");
    }

    #[test]
    fn mouse_tracking_modes_set_and_clear_flags() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1000h");
        assert!(t.mode().contains(TermMode::MOUSE_REPORT_CLICK));
        feed(&mut t, &mut p, b"\x1b[?1002h\x1b[?1006h");
        assert!(t.mode().contains(TermMode::MOUSE_DRAG), "?1002 = drag");
        assert!(t.mode().contains(TermMode::SGR_MOUSE), "?1006 = SGR enc");
        feed(&mut t, &mut p, b"\x1b[?1003h");
        assert!(
            t.mode().contains(TermMode::MOUSE_MOTION),
            "?1003 = any-motion"
        );
        feed(&mut t, &mut p, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        assert!(
            !t.mode().intersects(TermMode::MOUSE_MODE),
            "all tracking off"
        );
    }

    // SS2/SS3 single-shift (ESC N / ESC O), HTS (ESC H, custom tab
    // stops), DECSCA/DECSEL selective-erase and LNM LF→CRLF *output*
    // translation are not applied by alacritty_terminal, so no conformance
    // test asserts those behaviors (only LNM's mode bit) — see ROADMAP.
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use std::time::Duration;

    /// Cycle 902 (audit): the OSC 133 prompt-mark ring must (a) dedup against
    /// the most-recent mark, (b) preserve insertion order, and (c) cap at
    /// `MAX_PROMPT_MARKS` by dropping the OLDEST — all with O(1) `pop_front`,
    /// not an O(n) `Vec::drain` on every prompt (the hot reader-thread path).
    #[test]
    fn prompt_mark_ring_dedups_and_caps_oldest_first() {
        use std::collections::VecDeque;
        let mut ring: VecDeque<i64> = VecDeque::new();

        // Dedup: pushing the same most-recent mark twice keeps one.
        push_prompt_mark(&mut ring, 10);
        push_prompt_mark(&mut ring, 10);
        assert_eq!(ring.len(), 1);
        // A different mark appends; a non-adjacent repeat is allowed (the shell
        // genuinely re-prompted at a line it used earlier after scrollback).
        push_prompt_mark(&mut ring, 20);
        push_prompt_mark(&mut ring, 10);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![10, 20, 10]);

        // Cap: push well past the limit; length pins at MAX, oldest dropped,
        // newest retained, order preserved.
        let mut ring: VecDeque<i64> = VecDeque::new();
        for i in 0..(MAX_PROMPT_MARKS as i64 + 500) {
            push_prompt_mark(&mut ring, i);
        }
        assert_eq!(ring.len(), MAX_PROMPT_MARKS);
        assert_eq!(*ring.front().unwrap(), 500); // oldest 500 dropped
        assert_eq!(
            *ring.back().unwrap(),
            MAX_PROMPT_MARKS as i64 + 499 // newest kept
        );
    }

    /// Cycle 742 regression guard (runtime). Dropping a `Terminal` whose
    /// child is alive and whose PTY reader is parked in a blocking `read()`
    /// must return PROMPTLY. Pre-742 `Drop` `join()`ed the reader while the
    /// master was still open; on Windows ConPTY that join could never
    /// complete, so the UI thread (which owns the drop on a pane close)
    /// deadlocked and the window went "not responding". We run the drop on a
    /// worker thread and require it to finish far inside the old hang window.
    #[test]
    fn drop_is_prompt_with_blocked_reader() {
        // A child that stays alive and quiet, so the reader is parked in a
        // blocking read at drop time: `cmd.exe` waits on stdin; `cat` (no
        // args) blocks reading stdin and emits nothing.
        #[cfg(windows)]
        let argv = vec!["cmd.exe".to_string()];
        #[cfg(unix)]
        let argv = vec!["/bin/cat".to_string()];

        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let term = match Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        ) {
            Ok(t) => t,
            // A sandbox without a usable PTY (rare on the CI runners) — soft
            // skip rather than red the suite. The deterministic source drift
            // guard below pins the invariant without needing a real PTY.
            Err(e) => {
                eprintln!("skipping drop_is_prompt_with_blocked_reader: no PTY ({e})");
                return;
            }
        };

        // Let the child start and the reader settle into a blocking read.
        std::thread::sleep(Duration::from_millis(300));

        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            drop(term);
            let _ = done_tx.send(());
        });

        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "Terminal::Drop blocked >5s — the cycle-742 reader-thread join \
             deadlock has regressed (Drop must detach the reader, never join)"
        );
    }

    /// Cycle 742 regression guard (source, deterministic / cross-platform).
    /// `Terminal::Drop` must DETACH the reader thread, never `join()` it: a
    /// future refactor that re-adds `.join()` reintroduces the Windows
    /// UI-thread deadlock. Inspect just the `fn drop` body so doc comments
    /// and surrounding code can't skew the check.
    #[test]
    fn drop_detaches_reader_never_joins() {
        // Normalize CRLF→LF first: the repo checks out with Windows line
        // endings, so byte patterns must not assume bare `\n`.
        let src = include_str!("term.rs").replace("\r\n", "\n");
        let start = src
            .find("fn drop(&mut self) {")
            .expect("Terminal::Drop present");
        let rest = &src[start..];
        // The fn body closes at a 4-space-indented `}`; every nested block
        // inside closes at >=8 spaces, so the first `\n    }` is unambiguous.
        let end = rest.find("\n    }").map(|e| e + 5).expect("drop fn close");
        let body = &rest[..end];
        assert!(
            body.contains("reader_thread.take()"),
            "Drop must take() (detach) the reader thread handle"
        );
        assert!(
            !body.contains(".join("),
            "Terminal::Drop must NOT join the PTY reader — joining on the UI \
             thread deadlocks on a blocked ConPTY read (cycle 742)"
        );
        // Cycle 833: Drop must also REAP the killed child off-thread so it
        // doesn't leak a <defunct> zombie on Unix/macOS — and must do so in a
        // detached reaper (no blocking wait on the UI thread).
        assert!(
            body.contains("kettle-pty-reaper") && body.contains("c.wait()"),
            "Drop must reap the killed child in a detached reaper thread (cycle 833)"
        );
    }
}

#[cfg(test)]
mod login_flag_tests {
    use super::{default_shell_accepts_login_flag, prog_accepts_login_flag};

    /// Cycle 840 (audit): an explicit `command = …` only gets `-l` for a POSIX
    /// shell — never wsl.exe (where `-l` lists distros) or a Windows-native
    /// shell (pwsh/powershell/cmd reject it).
    #[test]
    fn prog_accepts_login_flag_excludes_wsl_and_windows_shells() {
        // POSIX shells (and unknown progs) honor -l.
        assert!(prog_accepts_login_flag("bash"));
        assert!(prog_accepts_login_flag("/bin/zsh"));
        assert!(prog_accepts_login_flag("/usr/bin/fish"));
        // Windows-native shells reject -l (path + .exe + case variants).
        assert!(!prog_accepts_login_flag("pwsh.exe"));
        assert!(!prog_accepts_login_flag(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        ));
        assert!(!prog_accepts_login_flag("powershell.exe"));
        assert!(!prog_accepts_login_flag("CMD.EXE"));
        assert!(!prog_accepts_login_flag("cmd"));
        // wsl.exe is excluded (-l there means "list distros").
        assert!(!prog_accepts_login_flag("wsl.exe"));
        assert!(!prog_accepts_login_flag("wsl"));
    }

    /// Cycle 822 (audit) drift guard. The spawn path gates the default-shell
    /// `-l` injection on this fn, so pinning its value pins the behavior: `-l`
    /// is POSIX-only and must never reach the Windows default shell.
    #[test]
    fn default_shell_login_flag_is_posix_only() {
        assert_eq!(default_shell_accepts_login_flag(), !cfg!(windows));
        #[cfg(windows)]
        assert!(
            !default_shell_accepts_login_flag(),
            "Windows default shell (pwsh/powershell/cmd) must not get -l"
        );
        #[cfg(not(windows))]
        assert!(
            default_shell_accepts_login_flag(),
            "POSIX default shell honors -l when login-shell=true"
        );
    }
}

#[cfg(all(test, windows))]
mod default_shell_tests {
    use super::pick_windows_default_shell;
    use std::path::PathBuf;

    const PWSH: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";
    const WPS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    /// Cycle 743: pwsh 7 wins when both it and Windows PowerShell are present
    /// (matches Windows Terminal's default).
    #[test]
    fn prefers_pwsh_over_windows_powershell() {
        let pick = pick_windows_default_shell(|e| match e {
            "pwsh.exe" => Some(PathBuf::from(PWSH)),
            "powershell.exe" => Some(PathBuf::from(WPS)),
            _ => None,
        });
        assert_eq!(pick, Some(PathBuf::from(PWSH)));
    }

    /// Falls back to Windows PowerShell 5.1 when pwsh 7 is not installed.
    #[test]
    fn falls_back_to_windows_powershell() {
        let pick = pick_windows_default_shell(|e| match e {
            "powershell.exe" => Some(PathBuf::from(WPS)),
            _ => None,
        });
        assert_eq!(pick, Some(PathBuf::from(WPS)));
    }

    /// Neither present → None, so the caller falls back to %ComSpec% / cmd.exe.
    #[test]
    fn none_when_neither_present() {
        assert_eq!(pick_windows_default_shell(|_| None), None);
    }
}

#[cfg(test)]
mod wsl_launcher_tests {
    use super::is_wsl_launcher;

    /// Cycle 748: the `login_shell` `-l` injection must be suppressed for the
    /// WSL launcher (bare name, `.exe`, full path, any case) because
    /// `wsl.exe -l` lists distros instead of opening a shell.
    #[test]
    fn recognizes_wsl_launcher_forms() {
        assert!(is_wsl_launcher("wsl"));
        assert!(is_wsl_launcher("wsl.exe"));
        assert!(is_wsl_launcher("WSL.EXE"));
        assert!(is_wsl_launcher(r"C:\Windows\System32\wsl.exe"));
        assert!(is_wsl_launcher("/mnt/c/Windows/System32/wsl.exe"));
    }

    /// Real shells must NOT be treated as wsl — they still get `-l`.
    #[test]
    fn does_not_match_other_shells() {
        for p in [
            "bash",
            "/bin/zsh",
            "pwsh.exe",
            "powershell.exe",
            "cmd.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "wsltty.exe", // stem is "wsltty", not "wsl"
        ] {
            assert!(!is_wsl_launcher(p), "{p} should not match wsl");
        }
    }
}

#[cfg(test)]
mod pty_dim_tests {
    use super::clamp_pty_dim;

    #[test]
    fn ordinary_sizes_pass_through() {
        // A typical 4K-wide grid: 8px cells × 480 cols = 3840px, well
        // within u16. The row/col count case uses cell = 1.
        assert_eq!(clamp_pty_dim(1, 200), 200);
        assert_eq!(clamp_pty_dim(8, 480), 3840);
        assert_eq!(clamp_pty_dim(20, 100), 2000); // HiDPI cell
    }

    #[test]
    fn overflowing_product_saturates_instead_of_wrapping() {
        // 30px HiDPI cell × 5000 cols = 150_000 — overflows u16. The old
        // `cell_w * cols as u16` panicked here in debug / wrapped to 18928
        // in release; we clamp to u16::MAX instead.
        assert_eq!(clamp_pty_dim(30, 5000), u16::MAX);
        // Pathological count that would truncate in the old `cols as u16`.
        assert_eq!(clamp_pty_dim(1, usize::MAX), u16::MAX);
        assert_eq!(clamp_pty_dim(10, usize::MAX), u16::MAX);
    }

    #[test]
    fn zero_inputs_are_benign() {
        assert_eq!(clamp_pty_dim(0, 80), 0);
        assert_eq!(clamp_pty_dim(8, 0), 0);
    }
}
