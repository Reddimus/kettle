//! kettle-remote — SSH / Docker / Podman / kubectl session detection.
//!
//! Cycle 643 (sub-cycle 2 of
//! [`TERMINATOR-REMOTE-DESIGN.md`](../../../docs/TERMINATOR-REMOTE-DESIGN.md)):
//! crate skeleton + `RemoteContext` type + `detect_remote` stub.
//!
//! Sub-cycle ledger (closed):
//!
//! - Cycle 644 (sub-cycle 3) — SSH detector
//!   (`detect_ssh` covering 11 argv shapes; see `tests` module).
//! - Cycle 645 (sub-cycle 4) — Container detector
//!   (`detect_container` for Docker / Podman / kubectl / lxc;
//!   11 argv shapes).
//! - Cycle 646 (sub-cycle 5) — process-tree BFS via sysinfo
//!   (`detect_remote_with(child_pid, &mut System)`).
//! - Cycle 658 (sub-cycle 7) — `clone_session_command` +
//!   `clone_session_label` (Clone Session menu item).
//! - Cycle 720 (2026-05-23): re-wrote the original
//!   "sub-cycle 3 *will* ship" forward-looking comments now that
//!   the foundations all landed at cycles 644-658.

#![forbid(unsafe_code)]

/// Cycle 656: re-export `sysinfo::System` so kettle-ui can own one
/// (and pass it to `detect_remote_with`) without pulling sysinfo
/// in as a direct dep. Keeps sysinfo a transitive-only dep that
/// kettle-ui doesn't need to track its version of.
pub use sysinfo::System as SysinfoSystem;

/// Cycle 643: a detected remote-session context.
///
/// Returned by [`detect_remote`] when the pane's process tree
/// contains a recognized remote-client process (`ssh`, `docker
/// exec`, `podman exec`, `kubectl exec`, `lxc-attach`).
///
/// Drives the cycle-647-target right-click "Clone session" menu
/// item and the pane-title update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteContext {
    /// SSH session. `host` is the target host (e.g. `box.example.com`);
    /// `user` is the optional username if the argv had `user@host`.
    Ssh { host: String, user: Option<String> },
    /// Container session (Docker / Podman / kubectl exec / lxc-attach).
    /// `container` is the target name/id from the argv.
    Container {
        runtime: ContainerRuntime,
        container: String,
    },
}

/// Cycle 643: which container runtime the detected `docker exec` /
/// `podman exec` / `kubectl exec` / `lxc-attach` command is using.
/// Drives the cycle-647-target "Clone session" command construction
/// (matches the same argv shape for the new pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Kubectl,
    Lxc,
}

/// Cycle 730: process-tree abstraction so the BFS body of
/// [`detect_remote_with`] is testable against a synthetic fixture
/// instead of needing real OS processes.
///
/// Pre-729, the BFS read `sysinfo::System` directly; the only test
/// that could exist was the `detect_remote_returns_none_for_invalid_pids`
/// smoke (`detect_remote(0).is_none()`). Two-hop ssh, depth-3
/// container, closer-wins-on-tie — none of those could be unit-
/// tested without spawning real ssh / docker processes from CI,
/// which the cycle-646 author specifically called out as too
/// fragile (see the comment on that test).
///
/// Implementations:
/// - [`sysinfo::System`](https://docs.rs/sysinfo) — built-in via
///   the impl below; used by [`detect_remote_with`].
/// - `tests::MockProcessTree` — `#[cfg(test)]`-only fixture in the
///   test module; powers the 8 cycle-730 BFS tests.
///
/// The trait is intentionally minimal: four read-only methods +
/// one `refresh`, all `u32`-pid typed (no `sysinfo::Pid` leak).
/// External implementations are unusual but supported — a future
/// e.g. `/proc`-only or seccomp-restricted impl would slot in
/// here without changing kettle-remote's API.
pub trait ProcessTree {
    /// Refresh the snapshot. The BFS calls this once at the top of
    /// each detection pass so a single-threaded poll loop sees a
    /// consistent process map. For sysinfo this re-reads the OS;
    /// for a fixture this is a no-op.
    fn refresh(&mut self);
    /// Parent PID of `pid`, or `None` if `pid` is missing / has no
    /// parent recorded (top-level / scheduler).
    fn parent_of(&self, pid: u32) -> Option<u32>;
    /// Argv of `pid` as lossy UTF-8 strings, or `None` if the
    /// process is gone or never existed. Lossy conversion mirrors
    /// the pre-729 `to_string_lossy` behavior — non-UTF8 argv is
    /// exotic and the detectors only care about `argv[0]` + flags
    /// which are always ASCII in practice.
    fn argv_of(&self, pid: u32) -> Option<Vec<String>>;
    /// Cycle 888: the working directory of `pid` (lossy UTF-8), or `None` if
    /// unknown. Default `None` so an external impl that can't report a cwd
    /// degrades gracefully (shell-detection just inherits no cwd); the sysinfo
    /// impl overrides it. Used to carry the dir of a detected running shell into
    /// a split.
    fn cwd_of(&self, _pid: u32) -> Option<String> {
        None
    }
    /// All known PIDs in the current snapshot. Used to build the
    /// children-by-parent map exactly once per detection pass.
    fn all_pids(&self) -> Vec<u32>;
}

/// Cycle 730: the production `ProcessTree` impl. Wraps sysinfo's
/// cmd-refresh + `processes()` map behind the trait's u32-pid API.
///
/// The refresh strategy matches the pre-729 in-line code: cmd-only
/// refresh (not memory / disk / network), all PIDs, full refresh
/// of any that disappeared. sysinfo's internal cache makes this
/// cheap on the second + later calls (~hundreds of µs on a typical
/// 200-process machine).
impl ProcessTree for sysinfo::System {
    fn refresh(&mut self) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        let refresh_kind = ProcessRefreshKind::new().with_cmd(sysinfo::UpdateKind::Always);
        self.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
    }

    fn parent_of(&self, pid: u32) -> Option<u32> {
        self.process(sysinfo::Pid::from_u32(pid))
            .and_then(|p| p.parent())
            .map(|p| p.as_u32())
    }

    fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
        // Lossy conversion is fine — non-UTF8 argv is exotic and
        // would still be detected at the exe-name level (the
        // detectors only inspect argv[0]'s last path component +
        // ASCII flags).
        self.process(sysinfo::Pid::from_u32(pid)).map(|p| {
            p.cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        })
    }

    fn cwd_of(&self, pid: u32) -> Option<String> {
        self.process(sysinfo::Pid::from_u32(pid))
            .and_then(|p| p.cwd())
            .map(|c| c.to_string_lossy().into_owned())
    }

    fn all_pids(&self) -> Vec<u32> {
        self.processes().keys().map(|p| p.as_u32()).collect()
    }
}

/// Cycle 646 (sub-cycle 5 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): detect a remote-
/// session context for the pane rooted at `child_pid`.
///
/// Walks the process tree starting from `child_pid` (the shell
/// kettle spawned), looks at each descendant's argv, and returns
/// the first match from `detect_ssh` / `detect_container` (closest
/// descendant of `child_pid` wins on tie).
///
/// Returns `None` when:
///   - no descendant matches a known remote-client argv
///   - `child_pid` itself is gone (process exited)
///   - sysinfo can't enumerate processes (rare; permission denied
///     on hardened systems)
///
/// Allocates a fresh `sysinfo::System` per call. For app-loop use
/// (~10 Hz poll), prefer [`detect_remote_with`] which takes a
/// caller-owned `System` so the refreshes amortize.
pub fn detect_remote(child_pid: u32) -> Option<RemoteContext> {
    let mut sys = sysinfo::System::new();
    detect_remote_with(child_pid, &mut sys)
}

/// Cycle 646: same as [`detect_remote`] but reuses a caller-owned
/// `sysinfo::System`. The App's poll loop will own one of these
/// across ticks so the process-list refresh amortizes (sysinfo's
/// internal cache survives between calls).
///
/// Cycle 730: now a thin wrapper around the generic
/// `detect_in_tree` helper (private — see the cycle-730 doc on
/// `ProcessTree` for why the BFS body got extracted) so the
/// detection logic is testable. Signature preserved —
/// `kettle-ui::App` still passes `&mut self.remote_sysinfo` (a
/// `SysinfoSystem`) and gets back the same `Option<RemoteContext>`.
pub fn detect_remote_with(child_pid: u32, sys: &mut sysinfo::System) -> Option<RemoteContext> {
    detect_in_tree(child_pid, sys)
}

/// Cycle 851 (audit): a shared process snapshot for multi-pane polling.
///
/// `detect_remote_with` refreshes the OS-wide process list **and** rebuilds the
/// parent→children index on every call. The app's poll loop calls it once per
/// pane, so an N-pane window did N full process walks + N map builds every
/// 200 ms tick. `RemoteScanner` splits that: [`refresh`](Self::refresh) does the
/// one OS walk + one index build per tick, then [`detect_root`](Self::detect_root)
/// answers each pane from the shared index (a cheap BFS + cache-hit argv reads).
///
/// `detect_remote` / `detect_remote_with` are kept for one-shot callers and
/// existing tests.
pub struct RemoteScanner {
    sys: sysinfo::System,
    index: std::collections::HashMap<u32, Vec<u32>>,
}

impl Default for RemoteScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteScanner {
    pub fn new() -> Self {
        Self {
            sys: sysinfo::System::new(),
            index: std::collections::HashMap::new(),
        }
    }

    /// Refresh the process snapshot and rebuild the parent→children index.
    /// Call once per poll tick, before querying panes.
    pub fn refresh(&mut self) {
        self.sys.refresh();
        self.index = build_children_index(&self.sys);
    }

    /// Resolve the remote context for the pane rooted at `child_pid`, using the
    /// index built by the last [`refresh`](Self::refresh). No OS walk, no map
    /// rebuild — safe to call once per pane.
    pub fn detect_root(&self, child_pid: u32) -> Option<RemoteContext> {
        detect_root_in_index(child_pid, &self.sys, &self.index)
    }

    /// Cycle 888: the deepest known-shell descendant of the pane rooted at
    /// `child_pid` (its argv + cwd), using the index from the last
    /// [`refresh`](Self::refresh). Lets a Split / Duplicate clone the shell the
    /// user actually entered (e.g. `wsl` typed inside pwsh) instead of the
    /// pane's original launch command. `None` for a plain pane.
    pub fn foreground_shell(&self, child_pid: u32) -> Option<ShellLaunch> {
        find_foreground_shell_in_index(child_pid, &self.sys, &self.index)
    }
}

/// Cycle 730: generic BFS over any [`ProcessTree`]. Walks descendants
/// of `child_pid` breadth-first; closest descendants checked first
/// so a `bash → docker → ssh` tree resolves to the docker context
/// (the directly-spawned remote client), not the deeper-but-also-
/// matching ssh.
///
/// Private — the public entry points ([`detect_remote`] /
/// [`detect_remote_with`]) wrap it with a sysinfo `&mut System`.
/// External impls of `ProcessTree` can still call it (the trait is
/// `pub`), but routing through the sysinfo wrapper is the documented
/// path. Tests use this directly with `MockProcessTree`.
fn detect_in_tree<T: ProcessTree + ?Sized>(child_pid: u32, tree: &mut T) -> Option<RemoteContext> {
    tree.refresh();
    let children_by_parent = build_children_index(tree);
    detect_root_in_index(child_pid, tree, &children_by_parent)
}

/// Group every PID by its parent, so a BFS over descendants is O(N) to build
/// the index plus O(D) to walk. The pre-729 sysinfo BFS used `sysinfo::Pid`
/// keys; the trait abstraction is `u32` so the same map works for both
/// `sysinfo::System` and `MockProcessTree`.
///
/// Cycle 851 (audit): extracted so a multi-pane poll can build this **once**
/// per tick (via [`RemoteScanner`]) instead of once per pane.
fn build_children_index<T: ProcessTree + ?Sized>(
    tree: &T,
) -> std::collections::HashMap<u32, Vec<u32>> {
    let pids = tree.all_pids();
    let mut children_by_parent: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::with_capacity(pids.len());
    for pid in &pids {
        if let Some(parent) = tree.parent_of(*pid) {
            children_by_parent.entry(parent).or_default().push(*pid);
        }
    }
    // Cycle 916 (file-by-file audit): all_pids() comes from sysinfo's HashMap, so
    // sibling order is non-deterministic. BFS over it made equal-depth tie-breaks
    // (which shell a Split clones; which remote client the pane title shows) flap
    // run-to-run. Sort each sibling list so the lowest PID deterministically wins.
    for kids in children_by_parent.values_mut() {
        kids.sort_unstable();
    }
    children_by_parent
}

/// BFS from `child_pid` over a **prebuilt** parent→children index, resolving
/// the closest descendant whose argv matches a known remote client. Does no
/// refresh and no map build — cheap enough to call per pane against a shared
/// index (cycle 851, audit). `argv_of` lookups still go to `tree`, but those
/// hit sysinfo's already-refreshed cache (no OS walk).
fn detect_root_in_index<T: ProcessTree + ?Sized>(
    child_pid: u32,
    tree: &T,
    children_by_parent: &std::collections::HashMap<u32, Vec<u32>>,
) -> Option<RemoteContext> {
    let pids_len = children_by_parent.len();
    // BFS from child_pid; closer descendants checked first. Loop
    // bound: each pid is enqueued ≤ 1 time (a Pid only has one
    // parent), so termination is guaranteed even on a cyclic
    // children_by_parent (which shouldn't happen but the bound
    // protects against a future fixture bug).
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<u32> =
        std::collections::HashSet::with_capacity(pids_len);
    if let Some(initial) = children_by_parent.get(&child_pid) {
        for &pid in initial {
            if visited.insert(pid) {
                queue.push_back(pid);
            }
        }
    }
    while let Some(pid) = queue.pop_front() {
        if let Some(argv) = tree.argv_of(pid) {
            if let Some(ctx) = detect_ssh(&argv) {
                return Some(ctx);
            }
            if let Some(ctx) = detect_container(&argv) {
                return Some(ctx);
            }
        }
        if let Some(grand) = children_by_parent.get(&pid) {
            for &gpid in grand {
                if visited.insert(gpid) {
                    queue.push_back(gpid);
                }
            }
        }
    }
    None
}

/// Cycle 888: a shell session detected running inside a pane — the argv to
/// relaunch it with and its working directory. Returned by
/// [`RemoteScanner::foreground_shell`] so a Split / Duplicate can reproduce the
/// shell the user is actually in (e.g. they opened pwsh then typed `wsl`) rather
/// than the pane's original launch command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunch {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

/// Cycle 888: is `prog` (an argv[0]) a known interactive shell a split should
/// reproduce? Matched on the basename (path- and `.exe`-insensitive, via
/// [`argv0_basename`]). Deliberately an allowlist — a split should clone
/// `wsl` / `bash` / `pwsh`, but NOT an arbitrary foreground program like `vim`.
fn is_known_shell(prog: &str) -> bool {
    matches!(
        argv0_basename(prog).as_str(),
        "wsl"
            | "bash"
            | "zsh"
            | "fish"
            | "sh"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "nu"
            | "elvish"
            | "xonsh"
    )
}

/// Cycle 917 (#2, user-reported on native Ubuntu): is this shell invocation a
/// ONE-SHOT / non-interactive command rather than an interactive session? A
/// foreground `node` (Claude Code) or `nvim` routinely spawns transient
/// `sh -c "…"` / `bash -c "…"` helpers; cloning one into a split spawns a shell
/// that runs the command and exits immediately, leaving a blank/dead pane
/// ("new pane but no terminal would load"). Only an interactive shell should be
/// cloned. Matched per shell family because the one-shot flag grammar differs.
fn is_noninteractive_shell(argv: &[String]) -> bool {
    let base = argv0_basename(argv.first().map(String::as_str).unwrap_or(""));
    let rest = argv.get(1..).unwrap_or(&[]);
    match base.as_str() {
        // POSIX-ish: `-c`, a combined short cluster containing `c` (`-ic`,
        // `-lc`), or `--command`/`--commands`. `-i`/`-l`/`-il` stay interactive.
        "bash" | "zsh" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "fish" | "nu" | "elvish"
        | "xonsh" => rest.iter().any(|a| {
            a == "-c"
                || a == "--command"
                || a == "--commands"
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && a.len() >= 2
                    && a[1..].contains('c'))
        }),
        // PowerShell: -Command / -c, -File, or -EncodedCommand / -e (each
        // prefix-abbreviated, case-insensitive) all run and exit — UNLESS
        // -NoExit keeps the session open. Cycle 919 (audit L3) added
        // -EncodedCommand (`pwsh -e <base64>` is how tools spawn one-shots).
        "pwsh" | "powershell" => {
            let norm = |a: &String| {
                a.strip_prefix('-')
                    .or_else(|| a.strip_prefix('/'))
                    .unwrap_or(a)
                    .to_ascii_lowercase()
            };
            // Cycle 918: `-NoExit` keeps the session interactive even alongside
            // `-Command`/`-File`, so such an invocation is NOT one-shot. Match its
            // prefix-abbreviations (`-noe`…`-noexit`) without colliding with
            // `-NoLogo`/`-NoProfile` (which differ at the 3rd letter).
            let noexit = rest.iter().any(|a| {
                let s = norm(a);
                s.len() >= 3 && "noexit".starts_with(&s)
            });
            !noexit
                && rest.iter().any(|a| {
                    let s = norm(a);
                    !s.is_empty()
                        && ("command".starts_with(&s)
                            || "file".starts_with(&s)
                            || "encodedcommand".starts_with(&s))
                })
        }
        // cmd: `/c` runs then exits; `/k` runs then STAYS interactive (allowed).
        "cmd" => rest
            .iter()
            .any(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("-c")),
        // wsl: bare or option-only is interactive; a positional command, `-e`,
        // or `--` followed by a command runs and exits.
        "wsl" => wsl_runs_command(rest),
        _ => false,
    }
}

/// Whether a `wsl …` argv tail (everything after argv[0]) carries a command to
/// run (→ exits) rather than launching an interactive login shell. Value-taking
/// options (`-d`/`-u`/`--cd`/`--shell-type`) consume their argument so a distro
/// name or directory isn't mistaken for a command — notably kettle's own
/// injected `wsl --cd <dir>` (see `launch_cwd`) stays interactive.
fn wsl_runs_command(rest: &[String]) -> bool {
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if a == "--" || a == "-e" || a == "--exec" {
            return i + 1 < rest.len();
        }
        if matches!(
            a,
            "-d" | "--distribution"
                | "--distribution-id"
                | "-u"
                | "--user"
                | "--cd"
                | "--shell-type"
        ) {
            i += 2; // skip the option AND its value
            continue;
        }
        if a.starts_with('-') {
            i += 1; // a boolean flag (e.g. --system)
            continue;
        }
        // `wsl ~` (and `~/…`) selects the home directory for an INTERACTIVE
        // shell, not a command to run — skip it rather than treating it as a
        // one-shot. (`wsl ~` alone then falls through to `false` = interactive.)
        if a == "~" || a.starts_with("~/") || a.starts_with("~\\") {
            i += 1;
            continue;
        }
        return true; // first bare positional = a command to run
    }
    false
}

/// Cycle 917 (#2): is `argv` a clonable INTERACTIVE shell? A split clones the
/// pane's detected foreground shell only when this holds; otherwise the caller
/// falls back to the pane's own launch shell, so a split can never spawn a
/// dead/one-shot pane. Public so the UI can assert the same contract at the
/// split boundary.
pub fn shell_launch_is_interactive(argv: &[String]) -> bool {
    argv.first().map(|p| is_known_shell(p)).unwrap_or(false) && !is_noninteractive_shell(argv)
}

/// Cycle 888: find the DEEPEST known-shell descendant of `child_pid` — the shell
/// the user has effectively entered (e.g. `pwsh → wsl.exe`). Returns its argv +
/// cwd to relaunch in a split. BFS by depth; the deepest shell wins (the most
/// nested ≈ the current foreground). `None` when no descendant is a known shell
/// (a plain pane, or one running a non-shell program) — the caller then falls
/// back to cloning the pane's own launch command.
fn find_foreground_shell_in_index<T: ProcessTree + ?Sized>(
    child_pid: u32,
    tree: &T,
    children_by_parent: &std::collections::HashMap<u32, Vec<u32>>,
) -> Option<ShellLaunch> {
    let mut queue: std::collections::VecDeque<(u32, u32)> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    if let Some(initial) = children_by_parent.get(&child_pid) {
        for &pid in initial {
            if visited.insert(pid) {
                queue.push_back((pid, 1));
            }
        }
    }
    let mut best: Option<(u32, u32)> = None; // (depth, pid) of the deepest shell
    while let Some((pid, depth)) = queue.pop_front() {
        // Cycle 917 (#2): a candidate must be a known shell AND an INTERACTIVE
        // invocation — a deeper `sh -c "…"` helper (spawned by node/claude/nvim)
        // is rejected so the split never clones a one-shot that exits instantly.
        let is_shell = tree
            .argv_of(pid)
            .map(|a| {
                a.first().map(|p| is_known_shell(p)).unwrap_or(false)
                    && !is_noninteractive_shell(&a)
            })
            .unwrap_or(false);
        if is_shell && best.map(|(d, _)| depth > d).unwrap_or(true) {
            best = Some((depth, pid));
        }
        if let Some(grand) = children_by_parent.get(&pid) {
            for &gpid in grand {
                if visited.insert(gpid) {
                    queue.push_back((gpid, depth + 1));
                }
            }
        }
    }
    let (_, pid) = best?;
    let argv = tree.argv_of(pid).filter(|a| !a.is_empty())?;
    Some(ShellLaunch {
        cwd: tree.cwd_of(pid),
        argv,
    })
}

/// Cycle 888: one-shot [`find_foreground_shell_in_index`] over a fresh snapshot
/// (mirrors [`detect_in_tree`]). Test-only — the app uses
/// [`RemoteScanner::foreground_shell`] for the amortized shared-index path.
#[cfg(test)]
fn find_foreground_shell<T: ProcessTree + ?Sized>(
    child_pid: u32,
    tree: &mut T,
) -> Option<ShellLaunch> {
    tree.refresh();
    let children_by_parent = build_children_index(tree);
    find_foreground_shell_in_index(child_pid, tree, &children_by_parent)
}

/// Cross-platform basename of an `argv[0]` for matching against bare command
/// names: drop any `/`- or `\`-separated path, a trailing (case-insensitive)
/// `.exe`, and lowercase the rest.
///
/// Cycle 823 (audit): the detectors split only on `/` and kept `.exe`, so on
/// Windows `argv[0]` is a backslash path with extension
/// (`C:\Windows\System32\OpenSSH\ssh.exe`) — `split('/')` returned the whole
/// path, and even a bare `ssh.exe` failed the `== "ssh"` check. The entire
/// Terminator-parity remote feature (pane-title `ssh box`, right-click
/// Reconnect / Re-attach) was silently dead on Windows 11, a primary target.
fn argv0_basename(prog: &str) -> String {
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Cycle 644 (sub-cycle 3 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): SSH-session
/// detector. Takes a process's argv (as the sysinfo walk in
/// sub-cycle 5 will supply it) and returns `Some(Ssh { host, user })`
/// if the argv shape matches an `ssh` invocation, else `None`.
///
/// Recognized `argv[0]` values: `ssh`, `sshpass`. (`autossh` is a
/// reasonable extension; deferred to follow-up.)
///
/// Host extraction:
///   - `ssh host`                            → host=host, user=None
///   - `ssh user@host`                       → host=host, user=Some(user)
///   - `ssh -p 22 user@host`                 → same
///   - `ssh -o StrictHostKeyChecking=no host` → host=host
///   - `sshpass -p secret ssh user@host`     → host=host, user=Some(user)
///
/// The detector skips `-flag value` and `-flag=value` and `--flag=value`
/// prefixes to find the first non-option argv element. That element is
/// the target (potentially `user@host`).
///
/// Pure — takes a `&[String]` slice; unit-testable without spawning
/// anything.
pub fn detect_ssh(argv: &[String]) -> Option<RemoteContext> {
    let exe = argv0_basename(argv.first()?);
    if exe != "ssh" && exe != "sshpass" {
        return None;
    }
    // sshpass wraps ssh — find the `ssh` inside its argv.
    let inner_start = if exe == "sshpass" {
        argv.iter().position(|a| argv0_basename(a) == "ssh")? + 1
    } else {
        1
    };
    let mut i = inner_start;
    let mut target: Option<&str> = None;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(s) = a.strip_prefix("--")
            && s.contains('=')
        {
            i += 1;
            continue;
        }
        if let Some(s) = a.strip_prefix('-')
            && !s.is_empty()
        {
            // `-o foo=bar` / `-p 22` / `-l user` / `-J jump` style: skip a value.
            // Cycle 836 (audit): this is the COMPLETE OpenSSH value-taking
            // single-char option set (ssh(1)). The old subset omitted `-J`
            // (ProxyJump, common in bastion setups) and `-w/-e/-m/-O/-Q/-S/-B/
            // -E/-I`, so e.g. `ssh -J jump host` skipped nothing and took `jump`
            // as the target → reconnected to the bastion. The joined form
            // (`-Jjump`) is a single multi-char token and is already skipped as
            // one below.
            let needs_value = matches!(
                s,
                "B" | "b"
                    | "c"
                    | "D"
                    | "E"
                    | "e"
                    | "F"
                    | "I"
                    | "i"
                    | "J"
                    | "L"
                    | "l"
                    | "m"
                    | "O"
                    | "o"
                    | "p"
                    | "Q"
                    | "R"
                    | "S"
                    | "W"
                    | "w"
            );
            if needs_value && i + 1 < argv.len() {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        target = Some(a.as_str());
        break;
    }
    let raw = target?;
    let (user, host) = match raw.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (Some(u.to_string()), h.to_string()),
        _ => (None, raw.to_string()),
    };
    if host.is_empty() {
        return None;
    }
    Some(RemoteContext::Ssh { host, user })
}

/// Cycle 645 (sub-cycle 4 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): Container-session
/// detector. Recognizes the four common container-exec argv shapes:
///
///   - `docker exec [-it] <container> <cmd> [args …]`
///   - `podman exec [-it] <container> <cmd> [args …]`
///   - `kubectl exec [-it] <pod> -- <cmd> [args …]`
///     (also `kubectl exec [-it] -n ns <pod> -- <cmd>`)
///   - `lxc-attach [-n] <container>`
///
/// The container token is the first non-option argv element after
/// the `exec` / `attach` subcommand (skipping `-flag value` pairs).
///
/// Pure — argv-in, `Option<RemoteContext>`-out. Unit-testable.
pub fn detect_container(argv: &[String]) -> Option<RemoteContext> {
    let exe = argv0_basename(argv.first()?);
    let runtime = match exe.as_str() {
        "docker" => ContainerRuntime::Docker,
        "podman" => ContainerRuntime::Podman,
        "kubectl" => ContainerRuntime::Kubectl,
        "lxc-attach" => ContainerRuntime::Lxc,
        _ => return None,
    };
    let mut i = 1; // skip argv[0] (the exe)
    if runtime != ContainerRuntime::Lxc {
        // Cycle 836 (audit): find the `exec` subcommand, allowing GLOBAL options
        // before it (`kubectl -n ns exec …`, `docker --context foo exec …`)
        // rather than pinning it at argv[1] (which silently returned None for
        // those). Scan for the first literal `exec` token; a container/namespace
        // named "exec" is contrived enough to ignore.
        match argv.iter().skip(1).position(|a| a == "exec") {
            Some(pos) => i = 1 + pos + 1,
            None => return None,
        }
    }
    // Skip flags + their values. Container CLIs share the same
    // shape — `-it` is a stacked short-flag bundle (no value),
    // `-n ns` is a flag + value (kubectl namespace), `-u user`
    // is a flag + value (docker/podman user). Be conservative:
    // single-char flags with known value-taking ones get +=2;
    // bundled short-flags (`-it`, `-rm`) and `--flag=value` are +=1.
    //
    // Lxc special case: `lxc-attach -n NAME` is the *idiomatic*
    // form. The value of `-n` IS the container name — capture
    // it directly instead of skipping it.
    let needs_value = |s: &str| matches!(s, "n" | "u" | "c" | "w" | "e");
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            continue;
        }
        if let Some(stripped) = a.strip_prefix("--") {
            // Cycle 836 (audit): a bare `--flag` is VALUELESS by default — most
            // docker/podman/kubectl exec long flags are booleans
            // (--privileged/--interactive/--tty/--detach). The old `i += 2`
            // treated `docker exec --privileged alpine sh` as `--privileged
            // alpine`, skipping the container and returning `sh`. Only a small
            // allowlist of long flags takes a separate value.
            let long_needs_value = !stripped.contains('=')
                && i + 1 < argv.len()
                && matches!(
                    stripped,
                    "env"
                        | "user"
                        | "workdir"
                        | "namespace"
                        | "detach-keys"
                        | "cidfile"
                        | "name"
                        | "context"
                        | "kubeconfig"
                );
            i += if long_needs_value { 2 } else { 1 };
            continue;
        }
        if let Some(stripped) = a.strip_prefix('-')
            && !stripped.is_empty()
        {
            // Lxc: -n VALUE is the container name.
            if runtime == ContainerRuntime::Lxc && stripped == "n" && i + 1 < argv.len() {
                return Some(RemoteContext::Container {
                    runtime,
                    container: argv[i + 1].clone(),
                });
            }
            let single_char_needs_value =
                stripped.len() == 1 && needs_value(stripped) && i + 1 < argv.len();
            if single_char_needs_value {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-option positional. For Lxc the lxc-attach
        // form is `lxc-attach -n name`; without `-n` the first
        // positional IS the name. So either way, this is the
        // container token.
        return Some(RemoteContext::Container {
            runtime,
            container: a.clone(),
        });
    }
    None
}

/// Cycle 658 (sub-cycle 7 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md)): format a
/// `RemoteContext` as a shell command string the user can re-run.
/// Drives the right-click "Reconnect to …" / "Re-attach …" menu
/// entry — clicking writes this string to the focused pane's PTY
/// (one shell-line away from re-establishing the session).
///
/// - `Ssh { user: None, host: "box" }` → `"ssh box"`
/// - `Ssh { user: Some("me"), host: "box" }` → `"ssh me@box"`
/// - `Container { Docker, c }` → `"docker exec -it c $SHELL"`
/// - `Container { Kubectl, c }` → `"kubectl exec -it c -- $SHELL"`
///
/// Pure — no `&self`, no env. Unit-testable. The "$SHELL"
/// placeholder leaves shell-choice to the user's environment
/// (the running pane's shell resolves it at command time).
pub fn clone_session_command(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("ssh {u}@{host}"),
            None => format!("ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => match runtime {
            ContainerRuntime::Docker => format!("docker exec -it {container} $SHELL"),
            ContainerRuntime::Podman => format!("podman exec -it {container} $SHELL"),
            ContainerRuntime::Kubectl => format!("kubectl exec -it {container} -- $SHELL"),
            ContainerRuntime::Lxc => format!("lxc-attach -n {container}"),
        },
    }
}

/// Cycle 658: short user-friendly label for the right-click menu
/// entry that reconnects to a detected remote session. The cycle-
/// 611 `ContextMenuItem::ConfigItem { label, command }` consumes
/// the pair `(clone_session_label(ctx), clone_session_command(ctx))`.
pub fn clone_session_label(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("Reconnect ssh {u}@{host}"),
            None => format!("Reconnect ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => {
            let runtime_name = match runtime {
                ContainerRuntime::Docker => "docker",
                ContainerRuntime::Podman => "podman",
                ContainerRuntime::Kubectl => "kubectl",
                ContainerRuntime::Lxc => "lxc",
            };
            format!("Re-attach {runtime_name} {container}")
        }
    }
}

/// Cycle 643: format a `RemoteContext` as a one-line title string
/// for use in the pane-title surface (Terminator's pattern).
///
///   - `Ssh { user: None, host: "box" }`             → `"ssh box"`
///   - `Ssh { user: Some("me"), host: "box" }`       → `"ssh me@box"`
///   - `Container { runtime: Docker, container: c }` → `"docker: c"`
///   - `Container { runtime: Kubectl, container: c }` → `"kubectl: c"`
///
/// Pure — no `&self` parameter (the enum is the input + the format
/// is the output). Unit-testable without disk.
pub fn format_remote_title(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user } => match user {
            Some(u) => format!("ssh {u}@{host}"),
            None => format!("ssh {host}"),
        },
        RemoteContext::Container { runtime, container } => {
            let runtime_name = match runtime {
                ContainerRuntime::Docker => "docker",
                ContainerRuntime::Podman => "podman",
                ContainerRuntime::Kubectl => "kubectl",
                ContainerRuntime::Lxc => "lxc",
            };
            format!("{runtime_name}: {container}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycle 643 drift guard. `format_remote_title` is the pure
    /// formatter behind the per-pane title update path.
    #[test]
    fn format_remote_title_covers_ssh_and_container_shapes() {
        // SSH without user.
        assert_eq!(
            format_remote_title(&RemoteContext::Ssh {
                host: "box.example.com".to_string(),
                user: None,
            }),
            "ssh box.example.com"
        );
        // SSH with user.
        assert_eq!(
            format_remote_title(&RemoteContext::Ssh {
                host: "box".to_string(),
                user: Some("me".to_string()),
            }),
            "ssh me@box"
        );
        // Container — Docker.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu-2204".to_string(),
            }),
            "docker: ubuntu-2204"
        );
        // Container — Podman.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".to_string(),
            }),
            "podman: fedora"
        );
        // Container — kubectl.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod-deadbeef".to_string(),
            }),
            "kubectl: my-pod-deadbeef"
        );
        // Container — LXC.
        assert_eq!(
            format_remote_title(&RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".to_string(),
            }),
            "lxc: alpine"
        );
    }

    /// Cycle 658 drift guard. `clone_session_command` is the pure
    /// formatter for the right-click "Reconnect to …" menu entry's
    /// dispatched command. Sub-cycle 7 of remote.py design.
    #[test]
    fn clone_session_command_for_all_shapes() {
        // SSH without user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            }),
            "ssh box"
        );
        // SSH with user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            }),
            "ssh me@box"
        );
        // Docker.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu".into(),
            }),
            "docker exec -it ubuntu $SHELL"
        );
        // Podman.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".into(),
            }),
            "podman exec -it fedora $SHELL"
        );
        // Kubectl (note the `--` separator).
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            }),
            "kubectl exec -it my-pod -- $SHELL"
        );
        // LXC.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            }),
            "lxc-attach -n alpine"
        );
    }

    /// Cycle 658 drift guard: `clone_session_label` is the menu
    /// label paired with `clone_session_command`.
    #[test]
    fn clone_session_label_for_all_shapes() {
        assert_eq!(
            clone_session_label(&RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            }),
            "Reconnect ssh me@box"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            }),
            "Reconnect ssh box"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "foo".into(),
            }),
            "Re-attach docker foo"
        );
        assert_eq!(
            clone_session_label(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            }),
            "Re-attach kubectl my-pod"
        );
    }

    /// Cycle 646 drift guard: `detect_remote` returns None for
    /// pids that aren't real (or have no descendants matching a
    /// remote-client argv). Real-process testing isn't feasible
    /// here — we'd need to actually spawn ssh, which adds CI
    /// fragility. The argv-side detectors (detect_ssh +
    /// detect_container) get exhaustive coverage above; this
    /// test just locks the no-op no-match contract.
    #[test]
    fn detect_remote_returns_none_for_invalid_pids() {
        // PID 0 is the kernel scheduler on Linux — never has the
        // shape we're looking for. u32::MAX is reserved / unused.
        assert!(detect_remote(0).is_none());
        assert!(detect_remote(u32::MAX).is_none());
    }

    /// Cycle 645 drift guard. `detect_container` walks the four
    /// container-runtime argv shapes.
    #[test]
    fn detect_container_recognizes_docker_podman_kubectl_lxc() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // docker exec -it <container> bash
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "-it", "alpine", "bash"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "alpine".into(),
            })
        );
        // podman exec foo sh
        assert_eq!(
            detect_container(&argv(&["podman", "exec", "fedora", "sh"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".into(),
            })
        );
        // kubectl exec -it -n my-ns my-pod -- bash
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "exec", "-it", "-n", "my-ns", "my-pod", "--", "bash"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            })
        );
        // lxc-attach -n alpine
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "-n", "alpine"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            })
        );
        // lxc-attach alpine (no -n)
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "alpine"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            })
        );
        // Absolute path.
        assert_eq!(
            detect_container(&argv(&["/usr/bin/docker", "exec", "foo"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "foo".into(),
            })
        );
        // Non-container argv → None.
        assert!(detect_container(&argv(&["docker", "ps"])).is_none());
        assert!(detect_container(&argv(&["docker", "build", "."])).is_none());
        assert!(detect_container(&argv(&["bash"])).is_none());
        assert!(detect_container(&argv(&[])).is_none());
        // docker exec with no container arg → None.
        assert!(detect_container(&argv(&["docker", "exec"])).is_none());
        assert!(detect_container(&argv(&["docker", "exec", "-it"])).is_none());
    }

    /// Cycle 644 drift guard. `detect_ssh` walks argv shapes that
    /// match real-world ssh invocations.
    #[test]
    fn detect_ssh_recognizes_common_argv_shapes() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // ssh host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "box.example.com"])),
            Some(RemoteContext::Ssh {
                host: "box.example.com".into(),
                user: None,
            })
        );
        // ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "me@box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            })
        );
        // ssh -p 22 user@host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-p", "22", "alice@h.example"])),
            Some(RemoteContext::Ssh {
                host: "h.example".into(),
                user: Some("alice".into()),
            })
        );
        // ssh -o StrictHostKeyChecking=no host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-o", "StrictHostKeyChecking=no", "h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: None,
            })
        );
        // ssh -l user host (kettle-supplied user via -l)
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: None, // -l user goes into ssh-internal state; we don't extract it
            })
        );
        // sshpass -p secret ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-p", "secret", "ssh", "carol@h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: Some("carol".into()),
            })
        );
        // Absolute-path argv[0].
        assert_eq!(
            detect_ssh(&argv(&["/usr/bin/ssh", "box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            })
        );
        // Non-SSH argv → None.
        assert!(detect_ssh(&argv(&["vim", "ssh.txt"])).is_none());
        assert!(detect_ssh(&argv(&["bash"])).is_none());
        assert!(detect_ssh(&argv(&[])).is_none());
        // ssh with no target (just flags) → None.
        assert!(detect_ssh(&argv(&["ssh", "-V"])).is_none());
    }

    /// Cycle 823 (audit) drift guard: argv[0] in the Windows shape — a
    /// backslash path WITH a `.exe` extension — must still be recognized. The
    /// detectors split only on `/` and kept `.exe`, so the whole remote feature
    /// was silently dead on Windows 11.
    #[test]
    fn detect_recognizes_windows_argv0_shape() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Windows OpenSSH: backslash path + .exe (+ a capitalized .EXE variant).
        assert_eq!(
            detect_ssh(&argv(&[
                r"C:\Windows\System32\OpenSSH\ssh.exe",
                "alice@host"
            ])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("alice".into()),
            })
        );
        assert_eq!(
            detect_ssh(&argv(&["ssh.EXE", "box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            })
        );
        // Docker Desktop on Windows: backslash path + .exe.
        assert_eq!(
            detect_container(&argv(&[
                r"C:\Program Files\Docker\Docker\resources\bin\docker.exe",
                "exec",
                "-it",
                "alpine",
                "sh"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "alpine".into(),
            })
        );
        // sshpass wrapping a Windows-path ssh.exe still finds the inner ssh.
        assert_eq!(
            detect_ssh(&argv(&[
                "sshpass",
                "-p",
                "secret",
                r"C:\OpenSSH\ssh.exe",
                "carol@h"
            ])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: Some("carol".into()),
            })
        );
    }

    /// Cycle 836 (audit): each of these argv shapes used to drive the WRONG
    /// reconnect target/command.
    #[test]
    fn detect_handles_proxyjump_bool_flags_and_global_flags() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // (a) ssh -J jump host → host (was: 'jump', the bastion).
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-J", "jump.example", "me@host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("me".into()),
            })
        );
        // Joined -Jjump form is one token; the host still wins.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-Jjump.example", "host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: None,
            })
        );
        // (b) docker exec --privileged <c> sh → c (was: 'sh').
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "--privileged", "alpine", "sh"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "alpine".into(),
            })
        );
        // A value-taking long flag still skips its value.
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "--user", "root", "alpine", "sh"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "alpine".into(),
            })
        );
        // (c) global flags before `exec` (kubectl -n ns exec pod) → pod.
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "-n", "prod", "exec", "my-pod", "--", "sh"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            })
        );
        assert_eq!(
            detect_container(&argv(&[
                "docker",
                "--context",
                "remote",
                "exec",
                "web",
                "bash"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "web".into(),
            })
        );
    }

    // === Cycle 730: ProcessTree fixture + mocked BFS tests =========
    //
    // Pre-729 the only `detect_remote_with` test was the
    // `detect_remote_returns_none_for_invalid_pids` smoke (above)
    // — it called the real sysinfo against pid 0 / u32::MAX. The
    // BFS body (descendant walk, closer-wins-on-tie, refresh
    // contract) was untested because spawning real ssh from CI
    // is too fragile. Cycle 730 extracted [`ProcessTree`] so the
    // BFS body is now testable with a synthetic process tree.

    /// Cycle 730 fixture: a `ProcessTree` impl backed by a hashmap.
    /// `add(pid, parent, argv)` builds the tree; `ProcessTree` reads
    /// it. `refresh()` is a no-op (the fixture is already-built).
    struct MockProcessTree {
        procs: std::collections::HashMap<u32, MockProc>,
    }

    struct MockProc {
        parent: Option<u32>,
        argv: Vec<String>,
        cwd: Option<String>,
    }

    impl MockProcessTree {
        fn new() -> Self {
            Self {
                procs: std::collections::HashMap::new(),
            }
        }

        fn add(&mut self, pid: u32, parent: Option<u32>, argv: &[&str]) {
            self.procs.insert(
                pid,
                MockProc {
                    parent,
                    argv: argv.iter().map(|s| (*s).to_string()).collect(),
                    cwd: None,
                },
            );
        }

        /// Cycle 888: like `add` but with a reported working directory (for the
        /// foreground-shell tests).
        fn add_cwd(&mut self, pid: u32, parent: Option<u32>, argv: &[&str], cwd: &str) {
            self.procs.insert(
                pid,
                MockProc {
                    parent,
                    argv: argv.iter().map(|s| (*s).to_string()).collect(),
                    cwd: Some(cwd.to_string()),
                },
            );
        }
    }

    impl ProcessTree for MockProcessTree {
        fn refresh(&mut self) {
            // Fixture is already-built; refresh is a no-op.
        }

        fn parent_of(&self, pid: u32) -> Option<u32> {
            self.procs.get(&pid).and_then(|p| p.parent)
        }

        fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
            self.procs.get(&pid).map(|p| p.argv.clone())
        }

        fn cwd_of(&self, pid: u32) -> Option<String> {
            self.procs.get(&pid).and_then(|p| p.cwd.clone())
        }

        fn all_pids(&self) -> Vec<u32> {
            self.procs.keys().copied().collect()
        }
    }

    /// Cycle 888 drift guard: `pwsh → wsl.exe` (the user's exact case — open
    /// PowerShell, type `wsl`, split) resolves to the WSL shell + its dir, so a
    /// split can clone WSL in the same directory instead of a fresh pwsh.
    #[test]
    fn find_foreground_shell_clones_typed_wsl_under_pwsh() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["pwsh.exe"]); // the pane's launch shell
        tree.add_cwd(200, Some(100), &["wsl.exe"], "C:\\Users\\me\\Repos\\proj");
        assert_eq!(
            find_foreground_shell(100, &mut tree),
            Some(ShellLaunch {
                argv: vec!["wsl.exe".to_string()],
                cwd: Some("C:\\Users\\me\\Repos\\proj".to_string()),
            })
        );
    }

    /// Cycle 888: the DEEPEST shell wins (most-nested ≈ current foreground), and
    /// a non-shell foreground (e.g. vim) is never cloned.
    #[test]
    fn find_foreground_shell_picks_deepest_and_ignores_non_shells() {
        // Deepest shell wins: pwsh → wsl → bash → (returns bash).
        let mut tree = MockProcessTree::new();
        tree.add(1, None, &["pwsh.exe"]);
        tree.add(2, Some(1), &["wsl.exe"]);
        tree.add_cwd(3, Some(2), &["bash"], "/home/me");
        assert_eq!(
            find_foreground_shell(1, &mut tree),
            Some(ShellLaunch {
                argv: vec!["bash".to_string()],
                cwd: Some("/home/me".to_string()),
            })
        );

        // A non-shell descendant (vim) is NOT cloned → None (caller falls back).
        let mut tree = MockProcessTree::new();
        tree.add(1, None, &["pwsh.exe"]);
        tree.add(2, Some(1), &["vim", "file.rs"]);
        assert_eq!(find_foreground_shell(1, &mut tree), None);

        // A plain pane with no descendants → None.
        let mut tree = MockProcessTree::new();
        tree.add(1, None, &["pwsh.exe"]);
        assert_eq!(find_foreground_shell(1, &mut tree), None);
    }

    /// Cycle 917 (#2, user-reported on native Ubuntu): a foreground `node`
    /// (Claude Code) or `nvim` spawns transient `sh -c "…"` helpers. The detector
    /// must NOT clone a one-shot helper into a split — doing so spawns a shell
    /// that runs the command and exits immediately, leaving a blank/dead pane
    /// ("new pane but no terminal would load"). With no INTERACTIVE shell
    /// descendant, it returns None so the caller clones the pane's real shell.
    #[test]
    fn foreground_shell_ignores_node_spawned_sh_dash_c_helper() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash", "-l"]); // pane's login shell (BFS starts at its children)
        tree.add(200, Some(100), &["node", "/usr/bin/claude"]); // Claude Code CLI
        tree.add(300, Some(200), &["sh", "-c", "rg --json foo"]); // transient tool helper
        assert_eq!(
            find_foreground_shell(100, &mut tree),
            None,
            "a node-spawned `sh -c` helper must not be cloned into a split"
        );
    }

    /// Combined short-flag cluster `-ic` is still one-shot (the `c` runs a
    /// command); `-i`/`-l`/`-il` alone stay interactive (covered in the table).
    #[test]
    fn foreground_shell_ignores_combined_dash_ic_oneshot() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["zsh"]);
        tree.add(200, Some(100), &["nvim"]);
        tree.add(300, Some(200), &["bash", "-ic", "lazygit"]);
        assert_eq!(find_foreground_shell(100, &mut tree), None);
    }

    /// A genuinely interactive nested shell is still detected, and a DEEPER
    /// one-shot helper under it is skipped in favor of the interactive ancestor.
    #[test]
    fn foreground_shell_skips_deeper_oneshot_for_interactive() {
        let mut tree = MockProcessTree::new();
        tree.add(1, None, &["pwsh.exe"]);
        tree.add_cwd(2, Some(1), &["wsl.exe"], "C:\\proj"); // interactive (depth 1)
        tree.add(3, Some(2), &["bash", "-c", "git status"]); // one-shot (depth 2, skipped)
        assert_eq!(
            find_foreground_shell(1, &mut tree),
            Some(ShellLaunch {
                argv: vec!["wsl.exe".to_string()],
                cwd: Some("C:\\proj".to_string()),
            }),
            "the interactive wsl ancestor wins over a deeper one-shot bash -c"
        );
    }

    /// Truth table for the one-shot predicate across shell families.
    #[test]
    fn is_noninteractive_shell_truth_table() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // One-shot → rejected.
        for a in [
            &["sh", "-c", "x"][..],
            &["bash", "-c", "x"],
            &["bash", "-lc", "x"],
            &["bash", "-ic", "x"],
            &["zsh", "--command", "x"],
            &["fish", "--command", "x"],
            &["nu", "-c", "x"],
            &["pwsh", "-Command", "x"],
            &["pwsh", "-c", "x"],
            &["pwsh", "-File", "s.ps1"],
            &["powershell.exe", "-co", "x"],
            // Cycle 919 (audit L3): -EncodedCommand / -e / -enc run a one-shot.
            &["pwsh", "-EncodedCommand", "AGUA"],
            &["pwsh", "-e", "AGUA"],
            &["pwsh", "-enc", "AGUA"],
            &["cmd", "/c", "x"],
            &["wsl", "ls"],
            &["wsl", "-e", "bash", "-c", "x"],
            &["wsl", "-d", "Ubuntu", "--", "htop"],
        ] {
            assert!(
                is_noninteractive_shell(&argv(a)),
                "{a:?} should be one-shot/non-interactive"
            );
        }
        // Interactive → kept.
        for a in [
            &["bash"][..],
            &["bash", "-i"],
            &["bash", "-l"],
            &["bash", "-il"],
            &["zsh"],
            &["pwsh"],
            &["pwsh", "-NoLogo"],
            // -NoExit keeps the session interactive even with -Command/-File.
            &["pwsh", "-NoExit", "-Command", "x"],
            &["pwsh", "-noe", "-c", "x"],
            &["pwsh", "-NoExit", "-EncodedCommand", "AGUA"],
            // -ExecutionPolicy (-ep/-ex) is NOT one-shot — it shares the leading
            // 'e' with -EncodedCommand but diverges at index 1 ("ex"/"ep" are not
            // a prefix of "encodedcommand"), so a bare `pwsh -ExecutionPolicy
            // Bypass` stays interactive.
            &["pwsh", "-ExecutionPolicy", "Bypass"],
            &["pwsh", "-ep", "Bypass"],
            &["cmd"],
            &["cmd", "/k", "x"],
            &["wsl"],
            &["wsl", "-d", "Ubuntu"],
            &["wsl", "--cd", "/home/me"],
            &["wsl", "--cd", "/home/me", "-d", "Ubuntu"],
            // Cycle 936 (review): `~` selects the home dir for an INTERACTIVE
            // shell (not a command); `--distribution-id` takes a GUID value
            // that must be consumed, not mistaken for a command.
            &["wsl", "~"],
            &[
                "wsl",
                "--distribution-id",
                "{12345678-1234-1234-1234-123456789abc}",
            ],
        ] {
            assert!(
                !is_noninteractive_shell(&argv(a)),
                "{a:?} should be interactive"
            );
        }
    }

    /// Cycle 730 drift guard: ssh as a direct child of the pane's
    /// shell is the most common shape. `detect_in_tree` must reach
    /// it in a single BFS hop.
    #[test]
    fn detect_in_tree_direct_child_ssh() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["ssh", "alice@server.example.com"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Ssh {
                host: "server.example.com".into(),
                user: Some("alice".into()),
            })
        );
    }

    /// Cycle 851 drift guard: the shared-index path (`build_children_index`
    /// once + per-root `detect_root_in_index`) — how `RemoteScanner` amortizes a
    /// multi-pane poll — must answer each root identically to the one-shot
    /// `detect_in_tree`, and one index build must serve several distinct roots.
    #[test]
    fn shared_index_matches_per_pane_detect() {
        let mut tree = MockProcessTree::new();
        // Three independent pane shells: ssh, docker, and a plain local shell.
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["ssh", "alice@a.example"]);
        tree.add(300, None, &["zsh"]);
        tree.add(400, Some(300), &["docker", "exec", "-it", "web", "sh"]);
        tree.add(500, None, &["fish"]);

        tree.refresh();
        let idx = build_children_index(&tree);
        for root in [100u32, 300, 500] {
            assert_eq!(
                detect_root_in_index(root, &tree, &idx),
                detect_in_tree(root, &mut tree),
                "shared-index result must match one-shot for root {root}"
            );
        }
        // One index build answers all three distinctly.
        assert!(
            detect_root_in_index(100, &tree, &idx).is_some(),
            "ssh root detected"
        );
        assert!(
            detect_root_in_index(300, &tree, &idx).is_some(),
            "docker root detected"
        );
        assert_eq!(
            detect_root_in_index(500, &tree, &idx),
            None,
            "plain local shell → no remote context"
        );
    }

    /// Cycle 730 drift guard: `ssh-with-credentials` wrappers
    /// (e.g., `sshpass`, `assume-role`, corporate VPN wrappers)
    /// spawn ssh one or more levels deep. The BFS must walk past
    /// the non-matching intermediate process.
    #[test]
    fn detect_in_tree_two_hops_ssh_via_wrapper() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["/usr/local/bin/run-with-creds"]);
        tree.add(200, Some(150), &["ssh", "bob@deep.example"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Ssh {
                host: "deep.example".into(),
                user: Some("bob".into()),
            })
        );
    }

    /// Cycle 730 drift guard: container exec at depth 3 (shell →
    /// tmux session → window → `docker exec`). Matches the
    /// terminator-parity assumption that a pane can have arbitrary-
    /// depth descendants.
    #[test]
    fn detect_in_tree_container_at_depth_3() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["tmux", "new-session"]);
        tree.add(160, Some(150), &["zsh"]);
        tree.add(
            200,
            Some(160),
            &["docker", "exec", "-it", "ubuntu-2204", "bash"],
        );
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu-2204".into(),
            })
        );
    }

    /// Cycle 730 drift guard: when two descendants both match a
    /// remote-client argv, the *closer* (depth-1) wins over the
    /// deeper (depth-2). This pins the BFS-is-breadth-first
    /// contract — a future swap to DFS would silently change
    /// behavior on this tree, breaking which container the right-
    /// click "Reconnect" menu offers.
    #[test]
    fn detect_in_tree_closer_descendant_wins_on_tie() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["docker", "exec", "near", "sh"]);
        tree.add(201, Some(200), &["ssh", "far.example"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "near".into(),
            })
        );
    }

    /// Cycle 730 drift guard: when `child_pid` has no entry in the
    /// tree AND nothing claims it as parent, return `None` without
    /// looping forever or panicking.
    #[test]
    fn detect_in_tree_missing_root_returns_none() {
        let mut tree = MockProcessTree::new();
        tree.add(50, Some(40), &["bash"]); // unrelated tree
        tree.add(60, Some(50), &["ssh", "elsewhere"]);
        assert!(detect_in_tree(999, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: an empty tree returns `None`.
    /// Boundary case for the BFS init.
    #[test]
    fn detect_in_tree_empty_tree_returns_none() {
        let mut tree = MockProcessTree::new();
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: descendants that don't match a
    /// remote-client argv return `None` even though the walk
    /// completes successfully. Catches the "grep ssh log.txt"
    /// false-positive class.
    #[test]
    fn detect_in_tree_non_remote_descendants_return_none() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(150, Some(100), &["vim", "file.txt"]);
        tree.add(151, Some(100), &["python", "-c", "print('hi')"]);
        // grep "ssh" is not an ssh client — argv[0] gates the
        // detector. Pre-729 this couldn't be tested without
        // spawning a real grep.
        tree.add(200, Some(150), &["grep", "ssh", "log.txt"]);
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Cycle 730 drift guard: a cycle in the parent chain
    /// (impossible in a real OS, but possible in a buggy fixture
    /// or future trait impl) must not loop the BFS forever. The
    /// `visited` set added in cycle 730 protects against this;
    /// this test fails if someone removes that defensive code.
    #[test]
    fn detect_in_tree_handles_parent_cycle_without_looping() {
        let mut tree = MockProcessTree::new();
        // Pathological: A is parent of B, B is parent of A.
        tree.add(100, Some(200), &["bash"]);
        tree.add(200, Some(100), &["zsh"]);
        // No remote client in the cycle → None, but the test
        // would hang forever pre-`visited` if there's a regression.
        assert!(detect_in_tree(100, &mut tree).is_none());
    }
}
