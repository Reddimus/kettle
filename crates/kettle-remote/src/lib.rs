//! kettle-remote — SSH / Docker / Podman / kubectl session detection.
//!
//! Phase 2 of
//! [`TERMINATOR-REMOTE-DESIGN.md`](../../../docs/TERMINATOR-REMOTE-DESIGN.md):
//! crate skeleton + `RemoteContext` type + `detect_remote` stub.
//!
//! Implementation history (closed):
//!
//! - Phase 3 — SSH detector
//!   (`detect_ssh` covering 11 argv shapes; see `tests` module).
//! - Phase 4 — Container detector
//!   (`detect_container` for Docker / Podman / kubectl / lxc;
//!   11 argv shapes).
//! - Phase 5 — process-tree BFS via sysinfo
//!   (`detect_remote_with(child_pid, &mut System)`).
//! - Phase 7 — `clone_session_command` +
//!   `clone_session_label` (Clone Session menu item).
//! - (2026-05-23): re-wrote the original
//!   "the SSH detector *will* ship" forward-looking comments now that
//!   the phases above had all landed.

#![forbid(unsafe_code)]

/// Re-export `sysinfo::System` so kettle-ui can own one
/// (and pass it to `detect_remote_with`) without pulling sysinfo
/// in as a direct dep. Keeps sysinfo a transitive-only dep that
/// kettle-ui doesn't need to track its version of.
pub use sysinfo::System as SysinfoSystem;

/// A detected remote-session context.
///
/// Returned by [`detect_remote`] when the pane's process tree
/// contains a recognized remote-client process (`ssh`, `docker
/// exec`, `podman exec`, `kubectl exec`, `lxc-attach`).
///
/// Drives the `clone_session_command`/`clone_session_label` right-click
/// "Clone session" menu item and the pane-title update.
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

/// Which container runtime the detected `docker exec` /
/// `podman exec` / `kubectl exec` / `lxc-attach` command is using.
/// Drives the `clone_session_command` "Clone session" command construction
/// (matches the same argv shape for the new pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Kubectl,
    Lxc,
}

/// Process-tree abstraction so the BFS body of
/// [`detect_remote_with`] is testable against a synthetic fixture
/// instead of needing real OS processes.
///
/// Previously, the BFS read `sysinfo::System` directly; the only test
/// that could exist was the `detect_remote_returns_none_for_invalid_pids`
/// smoke (`detect_remote(0).is_none()`). Two-hop ssh, depth-3
/// container, closer-wins-on-tie — none of those could be unit-
/// tested without spawning real ssh / docker processes from CI,
/// which is flagged as too fragile in the comment on that test.
///
/// Implementations:
/// - [`sysinfo::System`](https://docs.rs/sysinfo) — built-in via
///   the impl below; used by [`detect_remote_with`].
/// - `tests::MockProcessTree` — `#[cfg(test)]`-only fixture in the
///   test module; powers the 8 BFS tests below.
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
    /// the original `to_string_lossy` behavior — non-UTF8 argv is
    /// exotic and the detectors only care about `argv[0]` + flags
    /// which are always ASCII in practice.
    fn argv_of(&self, pid: u32) -> Option<Vec<String>>;
    /// The working directory of `pid` (lossy UTF-8), or `None` if
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

/// The production `ProcessTree` impl. Wraps sysinfo's
/// cmd-refresh + `processes()` map behind the trait's u32-pid API.
///
/// The refresh strategy matches the original in-line code: cmd-only
/// refresh (not memory / disk / network), all PIDs, full refresh
/// of any that disappeared. sysinfo's internal cache makes this
/// cheap on the second + later calls (~hundreds of µs on a typical
/// 200-process machine).
impl ProcessTree for sysinfo::System {
    fn refresh(&mut self) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        // v2.29.0: also request cwd so `cwd_of` is populated — powers the
        // native cwd fallback for tab/window labels when a shell emits no
        // OSC 7/9;9 (stock Windows pwsh/cmd). On Windows sysinfo reads it from
        // the process PEB; it degrades to None for elevated/cross-arch targets.
        let refresh_kind = ProcessRefreshKind::new()
            .with_cmd(sysinfo::UpdateKind::Always)
            .with_cwd(sysinfo::UpdateKind::Always);
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

/// Phase 5 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): detect a remote-
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

/// Same as [`detect_remote`] but reuses a caller-owned
/// `sysinfo::System`. The App's poll loop will own one of these
/// across ticks so the process-list refresh amortizes (sysinfo's
/// internal cache survives between calls).
///
/// Now a thin wrapper around the generic
/// `detect_in_tree` helper (private — see the doc on
/// `ProcessTree` for why the BFS body got extracted) so the
/// detection logic is testable. Signature preserved —
/// `kettle-ui::App` still passes `&mut self.remote_sysinfo` (a
/// `SysinfoSystem`) and gets back the same `Option<RemoteContext>`.
pub fn detect_remote_with(child_pid: u32, sys: &mut sysinfo::System) -> Option<RemoteContext> {
    detect_in_tree(child_pid, sys)
}

/// A shared process snapshot for multi-pane polling.
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
    #[cfg(target_os = "linux")]
    procfs: LinuxProcessTree,
    #[cfg(target_os = "linux")]
    use_procfs: bool,
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
            #[cfg(target_os = "linux")]
            procfs: LinuxProcessTree::default(),
            #[cfg(target_os = "linux")]
            use_procfs: false,
            index: std::collections::HashMap::new(),
        }
    }

    /// Refresh the cross-platform full process snapshot and rebuild the
    /// parent→children index. Preserved for one-shot callers that do not know
    /// their roots; Kettle's app loop uses [`Self::refresh_roots`] instead.
    pub fn refresh(&mut self) {
        self.sys.refresh();
        self.index = build_children_index(&self.sys);
        #[cfg(target_os = "linux")]
        {
            self.use_procfs = false;
        }
    }

    /// Refresh the process snapshot for the pane roots that will be queried.
    ///
    /// Linux walks only those roots' `/proc/<pid>/task/<pid>/children` trees.
    /// That keeps a focused cursor blink from synchronously rereading every
    /// process and thread on the machine. Platforms without that rooted procfs
    /// interface retain the cross-platform sysinfo snapshot.
    pub fn refresh_roots(&mut self, roots: &[u32]) {
        #[cfg(target_os = "linux")]
        {
            self.procfs.refresh_roots(roots);
            self.index = build_children_index(&self.procfs);
            self.use_procfs = true;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = roots;
            self.refresh();
        }
    }

    /// Resolve the remote context for the pane rooted at `child_pid`, using the
    /// index built by the last [`refresh`](Self::refresh). No OS walk, no map
    /// rebuild — safe to call once per pane.
    pub fn detect_root(&self, child_pid: u32) -> Option<RemoteContext> {
        detect_root_in_index(child_pid, self.tree(), &self.index)
    }

    /// The deepest known-shell descendant of the pane rooted at
    /// `child_pid` (its argv + cwd), using the index from the last
    /// [`refresh`](Self::refresh). Lets a Split / Duplicate clone the shell the
    /// user actually entered (e.g. `wsl` typed inside pwsh) instead of the
    /// pane's original launch command. `None` for a plain pane.
    pub fn foreground_shell(&self, child_pid: u32) -> Option<ShellLaunch> {
        find_foreground_shell_in_index(child_pid, self.tree(), &self.index)
    }

    /// v2.29.0: the cwd of the pane's foreground process — the DEEPEST live
    /// descendant of `child_pid` (e.g. `pwsh → git status`), or `child_pid`
    /// itself when it has no children (a shell idling at a prompt; its own
    /// process cwd tracks builtin `cd`). Backs the native cwd fallback used to
    /// label a pane whose shell emits no OSC 7/9;9. Unlike
    /// [`foreground_shell`](Self::foreground_shell) the descendant need NOT be a
    /// known interactive shell — any foreground program inherits the shell's cwd,
    /// so the deepest one is "where the user is". `None` if the cwd can't be read
    /// (elevated / cross-arch / WSL-relay target — sysinfo returns None).
    pub fn foreground_cwd(&self, child_pid: u32) -> Option<String> {
        let pid = deepest_descendant_in_index(child_pid, &self.index).unwrap_or(child_pid);
        self.tree().cwd_of(pid)
    }

    fn tree(&self) -> &dyn ProcessTree {
        #[cfg(target_os = "linux")]
        {
            if self.use_procfs {
                &self.procfs
            } else {
                &self.sys
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            &self.sys
        }
    }
}

#[cfg(target_os = "linux")]
const MAX_PROC_FILE_BYTES: u64 = 1 << 20;
#[cfg(target_os = "linux")]
const MAX_PROC_TREE_NODES: usize = 4096;
/// Audit (robustness): aggregate ceiling, in bytes, on ALL `cmdline` +
/// `children` file content read across a single [`LinuxProcessTree::refresh_from`]
/// walk. `MAX_PROC_FILE_BYTES` only bounds a SINGLE file's size; without an
/// aggregate cap, up to `MAX_PROC_TREE_NODES` (4096) descendants each near
/// that 1 MiB per-file ceiling could retain multiple GiB in `self.entries`
/// and cost multiple GiB of synchronous file I/O on one `refresh_roots` tick
/// (called directly on kettle-ui's poll loop — see the doc on
/// `LinuxProcessTree::refresh_from`). 32 MiB is generous next to any
/// legitimate shell-argv/child-list total (a few hundred KiB in practice
/// even for a wide process tree) while keeping the worst case bounded to
/// tens of MiB instead of GiB.
#[cfg(target_os = "linux")]
const MAX_PROC_TREE_TOTAL_BYTES: u64 = 32 * MAX_PROC_FILE_BYTES;

#[cfg(any(target_os = "linux", test))]
fn parse_proc_children(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|pid| !pid.is_empty())
        .filter_map(|pid| std::str::from_utf8(pid).ok()?.parse().ok())
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_argv(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct LinuxProcessTree {
    entries: std::collections::HashMap<u32, LinuxProcessEntry>,
}

#[cfg(target_os = "linux")]
struct LinuxProcessEntry {
    parent: Option<u32>,
    argv: Option<Vec<String>>,
    cwd: Option<String>,
}

#[cfg(target_os = "linux")]
impl LinuxProcessTree {
    fn refresh_roots(&mut self, roots: &[u32]) {
        self.refresh_from(std::path::Path::new("/proc"), roots);
    }

    /// Audit (robustness): synchronous procfs walk backing `refresh_roots`,
    /// which kettle-ui's `poll_remote_contexts` calls directly on the UI
    /// thread every ~200ms (no worker thread) — so both the memory this
    /// retains and the wall-clock time this takes bound how long the whole
    /// window can stall. `MAX_PROC_TREE_NODES` bounds node COUNT and
    /// `MAX_PROC_FILE_BYTES` bounds a single file's size, but neither bounds
    /// the SUM of every node's argv/children bytes — see
    /// `MAX_PROC_TREE_TOTAL_BYTES`, which this walk now also enforces:
    /// once the running total reaches that ceiling, further `cmdline`/
    /// `children` reads are skipped for the rest of the walk (the node
    /// itself, and its already-known parent/cwd, are still recorded — only
    /// the potentially-large file payloads are dropped), so a pathological
    /// or hostile descendant subtree can no longer balloon this to
    /// multiple GiB of retained memory or synchronous I/O.
    fn refresh_from(&mut self, proc_root: &std::path::Path, roots: &[u32]) {
        use std::collections::{HashSet, VecDeque};

        self.entries.clear();
        let mut queue: VecDeque<_> = roots.iter().copied().map(|pid| (pid, None)).collect();
        let mut visited = HashSet::with_capacity(roots.len());
        let mut total_bytes: u64 = 0;
        while let Some((pid, parent)) = queue.pop_front() {
            if self.entries.len() >= MAX_PROC_TREE_NODES || !visited.insert(pid) {
                continue;
            }
            let process_dir = proc_root.join(pid.to_string());
            let argv = if total_bytes < MAX_PROC_TREE_TOTAL_BYTES {
                read_proc_file_bounded(&process_dir.join("cmdline")).map(|b| {
                    total_bytes += b.len() as u64;
                    parse_proc_argv(&b)
                })
            } else {
                None
            };
            let cwd = std::fs::read_link(process_dir.join("cwd"))
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
            let children = if total_bytes < MAX_PROC_TREE_TOTAL_BYTES {
                read_proc_file_bounded(
                    &process_dir
                        .join("task")
                        .join(pid.to_string())
                        .join("children"),
                )
                .inspect(|b| {
                    total_bytes += b.len() as u64;
                })
            } else {
                None
            };
            if argv.is_none() && cwd.is_none() && children.is_none() {
                continue;
            }
            if let Some(children) = &children {
                for child in parse_proc_children(children) {
                    if queue.len() + self.entries.len() >= MAX_PROC_TREE_NODES {
                        break;
                    }
                    queue.push_back((child, Some(pid)));
                }
            }
            self.entries
                .insert(pid, LinuxProcessEntry { parent, argv, cwd });
        }
    }
}

#[cfg(target_os = "linux")]
fn read_proc_file_bounded(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;

    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROC_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_PROC_FILE_BYTES).then_some(bytes)
}

#[cfg(target_os = "linux")]
impl ProcessTree for LinuxProcessTree {
    fn refresh(&mut self) {}

    fn parent_of(&self, pid: u32) -> Option<u32> {
        self.entries.get(&pid)?.parent
    }

    fn argv_of(&self, pid: u32) -> Option<Vec<String>> {
        self.entries.get(&pid)?.argv.clone()
    }

    fn cwd_of(&self, pid: u32) -> Option<String> {
        self.entries.get(&pid)?.cwd.clone()
    }

    fn all_pids(&self) -> Vec<u32> {
        self.entries.keys().copied().collect()
    }
}

/// Generic BFS over any [`ProcessTree`]. Walks descendants
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
/// the index plus O(D) to walk. The original sysinfo BFS used `sysinfo::Pid`
/// keys; the trait abstraction is `u32` so the same map works for both
/// `sysinfo::System` and `MockProcessTree`.
///
/// Extracted so a multi-pane poll can build this **once**
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
    // all_pids() comes from sysinfo's HashMap, so
    // sibling order is non-deterministic. BFS over it made equal-depth tie-breaks
    // (which shell a Split clones; which remote client the pane title shows) flap
    // run-to-run. Sort each sibling list so the lowest PID deterministically wins.
    for kids in children_by_parent.values_mut() {
        kids.sort_unstable();
    }
    children_by_parent
}

/// BFS from `child_pid` over a **prebuilt** parent→children index, resolving
/// the closest process (starting with the pane root itself) whose argv matches a
/// known remote client. Does no refresh and no map build — cheap enough to call
/// per pane against a shared index. `argv_of` lookups still go
/// to `tree`, but those hit sysinfo's already-refreshed cache (no OS walk).
///
/// v2.32.0 (audit, low): the BFS now seeds at depth 0 with `child_pid` ITSELF, so
/// a pane that launched a remote client DIRECTLY (`command = ssh box`, no
/// intervening shell) is detected. The pre-fix walk only enqueued `child_pid`'s
/// children, so a directly-launched `ssh`/`docker` pane got no [`RemoteContext`]
/// (no Reconnect menu, no remote pane title). Existing trees that root at a plain
/// shell are unaffected — a shell argv matches neither detector.
fn detect_root_in_index<T: ProcessTree + ?Sized>(
    child_pid: u32,
    tree: &T,
    children_by_parent: &std::collections::HashMap<u32, Vec<u32>>,
) -> Option<RemoteContext> {
    let pids_len = children_by_parent.len();
    // BFS from child_pid; closer processes checked first. Loop bound: each pid is
    // enqueued ≤ 1 time (a Pid only has one parent, and `visited` dedupes), so
    // termination is guaranteed even on a cyclic children_by_parent (which
    // shouldn't happen but the bound protects against a future fixture bug).
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<u32> =
        std::collections::HashSet::with_capacity(pids_len);
    // Seed depth 0: the pane root pid itself, so a directly-launched remote
    // client (no shell in between) is considered before its descendants.
    if visited.insert(child_pid) {
        queue.push_back(child_pid);
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

/// A shell session detected running inside a pane — the argv to
/// relaunch it with and its working directory. Returned by
/// [`RemoteScanner::foreground_shell`] so a Split / Duplicate can reproduce the
/// shell the user is actually in (e.g. they opened pwsh then typed `wsl`) rather
/// than the pane's original launch command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunch {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

/// Is `prog` (an argv[0]) a known interactive shell a split should
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

/// Is this shell invocation a ONE-SHOT / non-interactive command rather than
/// an interactive session (user-reported on native Ubuntu)? A
/// foreground agent/editor (`claude`/`codex`/`nvim`) routinely spawns transient
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
        // -NoExit keeps the session open. -EncodedCommand support was added
        // (`pwsh -e <base64>` is how tools spawn one-shots).
        "pwsh" | "powershell" => {
            let norm = |a: &String| {
                a.strip_prefix('-')
                    .or_else(|| a.strip_prefix('/'))
                    .unwrap_or(a)
                    .to_ascii_lowercase()
            };
            // `-NoExit` keeps the session interactive even alongside
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

/// Is `argv` a clonable INTERACTIVE shell? A split clones the
/// pane's detected foreground shell only when this holds; otherwise the caller
/// falls back to the pane's own launch shell, so a split can never spawn a
/// dead/one-shot pane. Public so the UI can assert the same contract at the
/// split boundary.
pub fn shell_launch_is_interactive(argv: &[String]) -> bool {
    argv.first().map(|p| is_known_shell(p)).unwrap_or(false) && !is_noninteractive_shell(argv)
}

/// Find the DEEPEST known-shell descendant of `child_pid` — the shell
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
        // A candidate must be a known shell AND an INTERACTIVE
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

/// v2.29.0: the deepest live descendant pid of `root` — but ONLY along a LINEAR
/// chain (each level has ≤1 child). `None` when `root` has no descendants OR the
/// tree forks (some level has >1 child); the caller then reads `root`'s own cwd.
/// Used by [`RemoteScanner::foreground_cwd`] to find the foreground process whose
/// cwd is "where the user is". Unlike [`find_foreground_shell_in_index`] it does
/// not filter to known shells — any descendant inherits the shell's cwd.
///
/// v2.32.0 (audit, medium): the pre-fix walk took the deepest descendant across
/// ALL branches, so when the pane shell had two children (e.g. a backgrounded
/// `sleep 999 &` alongside an idle foreground prompt, or a long-running build in
/// one branch while the user `cd`s in the shell) the cwd label tracked whichever
/// branch happened to be deeper — a BACKGROUND job, not the foreground. "Deepest
/// descendant" is only a valid foreground signal on a single (linear) chain like
/// `pwsh → wsl → bash → git`; the moment the tree forks there is no unambiguous
/// foreground from the process tree alone, so we bail to `None` and let
/// `foreground_cwd` fall back to the root shell's own process cwd (which tracks
/// the shell's builtin `cd` correctly regardless of background jobs).
fn deepest_descendant_in_index(
    root: u32,
    children_by_parent: &std::collections::HashMap<u32, Vec<u32>>,
) -> Option<u32> {
    // Walk straight down the chain. At each level the node must have exactly one
    // child to continue; >1 child means a fork (ambiguous foreground → None), 0
    // children means we've reached the deepest node of a linear chain.
    let mut node = root;
    let mut deepest: Option<u32> = None;
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    visited.insert(root);
    loop {
        match children_by_parent.get(&node).map(Vec::as_slice) {
            // Linear step: descend to the sole child.
            Some([only]) => {
                // Defensive against a cyclic fixture/index (a pid can normally
                // have only one parent, so this should never fire): stop rather
                // than loop forever.
                if !visited.insert(*only) {
                    break;
                }
                deepest = Some(*only);
                node = *only;
            }
            // Fork: >1 child at this level → ambiguous foreground, bail to None.
            Some(_) => return None,
            // Leaf: end of a linear chain (or `root` itself had no children).
            None => break,
        }
    }
    deepest
}

/// One-shot [`find_foreground_shell_in_index`] over a fresh snapshot
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
/// The detectors used to split only on `/` and keep `.exe`, so on
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

/// v2.32.0 (audit H1, SECURITY): does `s` contain a control character (newline,
/// carriage return, NUL, tab, ESC, …)? A control char in an argv-derived
/// host/user/container token is the highest-severity case: a newline would split
/// [`clone_session_command`]'s output into extra shell lines that the caller
/// auto-executes. Such a token must NEVER become a [`RemoteContext`] (rejected at
/// parse time) and, defensively, must never be emitted (rejected at build time).
fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// v2.32.0 (audit H1, SECURITY): parse-time validation of a dynamic field
/// (ssh host, ssh user, container name) that was extracted from a descendant
/// process's argv and will later be interpolated into an auto-executed shell
/// command by [`clone_session_command`]. Rejects (returns `false`) any token
/// that is empty, carries a control char, or contains a character outside a
/// conservative per-field allowlist — so a malformed/hostile token never becomes
/// a [`RemoteContext`] in the first place (layer 1 of the defense; the build-time
/// single-quoting in [`clone_session_command`] is layer 2).
///
/// `extra` is the field-specific set of punctuation permitted on top of the
/// common `[A-Za-z0-9]`. These sets are deliberately tight: real SSH hosts
/// (DNS names, IPv4/IPv6, `%`-zone, `[bracketed]` literals), usernames, and
/// container names/ids never need shell metacharacters (`;`, `$`, backticks,
/// quotes, `&`, `|`, `<`, `>`, `(`, `)`, spaces, …), so excluding them costs
/// nothing and closes the injection surface.
fn field_is_safe(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// SSH host: DNS labels (`a-z0-9.-`), IPv6 literals (`:`, optional `[ ]`
/// brackets and `%zone`), and the `user@host` split already consumed the `@`.
const SSH_HOST_EXTRA: &str = ".-_:%[]";
/// SSH login user: POSIX usernames plus the small set real-world accounts use.
const SSH_USER_EXTRA: &str = ".-_$\\@";
/// Container name/id: Docker/Podman/kubectl/lxc allow `[A-Za-z0-9][A-Za-z0-9_.-]`
/// plus `/` (kubectl `type/name`, registry-qualified refs) and `:` (tags).
const CONTAINER_EXTRA: &str = ".-_:/";

/// v2.32.0 (audit H1, SECURITY): POSIX single-quote a dynamic field so it is
/// inert when interpolated into a shell command — every character between the
/// quotes is literal, and an embedded single-quote is closed/escaped/reopened
/// via the canonical `'\''` idiom. Belt-and-suspenders with the parse-time
/// [`field_is_safe`] check: even a value that somehow slipped through cannot
/// break out of the quotes.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Phase 3 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): SSH-session
/// detector. Takes a process's argv (as the sysinfo walk in
/// phase 5 will supply it) and returns `Some(Ssh { host, user })`
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
    //
    // Audit (correctness): this used to be a bare
    // `argv.iter().position(|a| argv0_basename(a) == "ssh")` — an unanchored
    // scan of the WHOLE argv, including sshpass's own flag VALUES. If any
    // earlier token (e.g. the `-p`/`-f` value) happened to basename to
    // "ssh" (a password or password-file path literally named "ssh"), that
    // token was mistaken for the real ssh invocation and everything after
    // it (the actual target) was silently discarded. Instead, walk
    // sshpass's OWN known flags from argv[1] (mirroring the disciplined
    // flag-skipping loop ssh's own options get below) and take the first
    // remaining non-flag token as the wrapped command's argv[0].
    //
    // sshpass(1) short options: `-p password` / `-f filename` / `-d fd` /
    // `-P prompt` each consume exactly one separate value; `-e` (password
    // from the `SSHPASS` env var), `-v`, `-h`, `-V` are boolean.
    let inner_start = if exe == "sshpass" {
        let mut j = 1;
        let inner = loop {
            let a = argv.get(j)?;
            if let Some(stripped) = a.strip_prefix('-')
                && !stripped.is_empty()
            {
                let needs_value = matches!(stripped, "p" | "f" | "d" | "P") && j + 1 < argv.len();
                j += if needs_value { 2 } else { 1 };
                continue;
            }
            break j;
        };
        // The wrapped command must actually be ssh — sshpass can wrap any
        // password-prompting program, but this detector only recognizes the
        // ssh case (the pre-existing contract of this function).
        if argv0_basename(&argv[inner]) != "ssh" {
            return None;
        }
        inner + 1
    } else {
        1
    };
    let mut i = inner_start;
    let mut target: Option<&str> = None;
    // H2 (audit v2.32.0): capture `-l user` so Reconnect / the remote title keep
    // the login user. An explicit `user@host` (parsed from `target` below) wins
    // per OpenSSH precedence, so `-l` only fills `user` when `user@host` didn't.
    let mut flag_user: Option<&str> = None;
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
            // This is the COMPLETE OpenSSH value-taking
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
                // H2 (audit v2.32.0): `-l user` (login name) — capture the value
                // instead of merely skipping it, so a later Reconnect / title
                // reproduces it. Only the bare `-l` form carries the user here;
                // the joined `-luser` form is a single multi-char token that
                // falls to the `else` (skipped, no separate value) — matching the
                // pre-existing behavior for every other value-taking flag.
                if s == "l" {
                    flag_user = Some(argv[i + 1].as_str());
                }
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
        // No `user@` in the target — fall back to any `-l user` we captured
        // (OpenSSH precedence: an explicit `user@host` would have won above).
        _ => (flag_user.map(str::to_string), raw.to_string()),
    };
    if host.is_empty() {
        return None;
    }
    // H1 (audit v2.32.0, SECURITY): reject any host/user that carries a control
    // char or escapes the conservative per-field charset, so a token that could
    // break out of the auto-executed Reconnect command never becomes a
    // RemoteContext. (clone_session_command additionally single-quotes — layer 2.)
    if !field_is_safe(&host, SSH_HOST_EXTRA) {
        return None;
    }
    if let Some(u) = &user
        && !field_is_safe(u, SSH_USER_EXTRA)
    {
        return None;
    }
    Some(RemoteContext::Ssh { host, user })
}

/// Phase 4 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): Container-session
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
        // Find the `exec` subcommand, allowing GLOBAL options
        // before it (`kubectl -n ns exec …`, `docker --context foo exec …`)
        // rather than pinning it at argv[1] (which silently returned None for
        // those).
        //
        // Audit (correctness): this used to be
        // `argv.iter().skip(1).position(|a| a == "exec")` — an UNANCHORED
        // scan of the ENTIRE argv. Any later positional argument that
        // happened to literally be "exec" (a build tag, an npm/script arg, a
        // k8s object literally named `exec`, …) was mistaken for the
        // subcommand boundary, and the argv element right after IT got
        // parsed as the container name — even for commands that aren't an
        // `exec` invocation at all (e.g. `docker build -t exec .`).
        // Anchor instead: walk only recognized GLOBAL option flags (and
        // their values) from argv[1], and require the FIRST non-flag
        // token to be exactly "exec" — any other subcommand there
        // (`build`, `ps`, `run`, `inspect`, …) correctly yields `None`
        // instead of being scanned past.
        const GLOBAL_LONG_NEEDS_VALUE: &[&str] = &[
            "host",
            "context",
            "config",
            "log-level",
            "namespace",
            "kubeconfig",
            "cluster",
            "user",
            "server",
            "token",
            "as",
            "as-group",
            "request-timeout",
            "tlscacert",
            "tlscert",
            "tlskey",
        ];
        loop {
            let a = argv.get(i)?;
            if let Some(stripped) = a.strip_prefix("--") {
                if stripped.is_empty() {
                    // Bare "--" before the subcommand isn't meaningful for
                    // any of these CLIs; skip rather than treat as positional.
                    i += 1;
                    continue;
                }
                // Once we know `stripped` has no `=`, it IS the flag name
                // (a `--flag=value` token never reaches this check — the
                // `!stripped.contains('=')` short-circuits first).
                let needs_value = !stripped.contains('=')
                    && i + 1 < argv.len()
                    && GLOBAL_LONG_NEEDS_VALUE.contains(&stripped);
                i += if needs_value { 2 } else { 1 };
                continue;
            }
            if let Some(stripped) = a.strip_prefix('-')
                && !stripped.is_empty()
            {
                // Single-char global flags that take a value: docker/podman
                // `-H`/`-c`, kubectl `-n`/`-s`. Bundled/boolean shorts (`-D`,
                // `-v`) just skip one token — same conservative default the
                // post-`exec` flag loop below uses.
                let needs_value = stripped.len() == 1
                    && matches!(stripped, "H" | "c" | "n" | "l" | "s")
                    && i + 1 < argv.len();
                i += if needs_value { 2 } else { 1 };
                continue;
            }
            // First non-flag token: it MUST be the `exec` subcommand — no
            // scanning past it for a later, coincidental match.
            if a != "exec" {
                return None;
            }
            i += 1;
            break;
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
            // A bare `--flag` is VALUELESS by default — most
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
                let container = &argv[i + 1];
                // H1 (audit v2.32.0, SECURITY): see the final return below.
                if !field_is_safe(container, CONTAINER_EXTRA) {
                    return None;
                }
                return Some(RemoteContext::Container {
                    runtime,
                    container: container.clone(),
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
        // H1 (audit v2.32.0, SECURITY): reject a container token that carries a
        // control char or escapes the conservative charset, so it can never
        // become a RemoteContext whose Reconnect command the caller auto-execs.
        // (clone_session_command additionally single-quotes — layer 2.)
        if !field_is_safe(a, CONTAINER_EXTRA) {
            return None;
        }
        return Some(RemoteContext::Container {
            runtime,
            container: a.clone(),
        });
    }
    None
}

/// Phase 7 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): format a
/// `RemoteContext` as a shell command string the user can re-run.
/// Drives the right-click "Reconnect to …" / "Re-attach …" menu
/// entry — clicking writes this string to the focused pane's PTY
/// (one shell-line away from re-establishing the session).
///
/// - `Ssh { user: None, host: "box" }` → `Some("ssh 'box'")`
/// - `Ssh { user: Some("me"), host: "box" }` → `Some("ssh 'me'@'box'")`
/// - `Container { Docker, c }` → `Some("docker exec -it 'c' $SHELL")`
/// - `Container { Kubectl, c }` → `Some("kubectl exec -it 'c' -- $SHELL")`
///
/// Pure — no `&self`, no env. Unit-testable. The "$SHELL"
/// placeholder leaves shell-choice to the user's environment
/// (the running pane's shell resolves it at command time).
///
/// v2.32.0 (audit H1, SECURITY): the host/user/container fields are
/// argv-derived (from a descendant process's command line) and the caller
/// AUTO-EXECUTES this string by writing it to the pane's PTY with a trailing
/// newline. To keep the data→code boundary safe this function:
///
/// 1. POSIX single-quotes (`'…'`) every dynamic field via
///    `shell_single_quote`, so even a value that slipped past parse-time
///    validation (`field_is_safe`) is inert (no `;`/`$()`/space splits it);
/// 2. returns `None` if any field still contains a control char (a newline
///    would split the auto-exec into extra shell lines) — the caller then
///    omits the Reconnect menu item rather than emit an unsafe line.
///
/// This is layer 2; layer 1 is the parse-time rejection in
/// `detect_ssh` / `detect_container`. Returning `Option` lets the UI drop
/// the menu entry entirely when no safe command can be built.
pub fn clone_session_command(ctx: &RemoteContext) -> Option<String> {
    match ctx {
        RemoteContext::Ssh { host, user } => {
            if has_control_char(host) {
                return None;
            }
            let h = shell_single_quote(host);
            match user {
                Some(u) => {
                    if has_control_char(u) {
                        return None;
                    }
                    Some(format!("ssh {}@{h}", shell_single_quote(u)))
                }
                None => Some(format!("ssh {h}")),
            }
        }
        RemoteContext::Container { runtime, container } => {
            if has_control_char(container) {
                return None;
            }
            let c = shell_single_quote(container);
            Some(match runtime {
                ContainerRuntime::Docker => format!("docker exec -it {c} $SHELL"),
                ContainerRuntime::Podman => format!("podman exec -it {c} $SHELL"),
                ContainerRuntime::Kubectl => format!("kubectl exec -it {c} -- $SHELL"),
                ContainerRuntime::Lxc => format!("lxc-attach -n {c}"),
            })
        }
    }
}

/// Short user-friendly label for the right-click menu
/// entry that reconnects to a detected remote session. The
/// `ContextMenuItem::ConfigItem { label, command }` variant consumes
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

/// Format a `RemoteContext` as a one-line title string
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

    #[test]
    fn proc_parsers_are_bounded_to_valid_pids_and_preserve_lossy_argv() {
        assert_eq!(
            parse_proc_children(b"12 34\ninvalid 4294967296 56").collect::<Vec<_>>(),
            [12, 34, 56]
        );
        assert_eq!(
            parse_proc_argv(b"ssh\0alice@host\0bad-\xff\0\0"),
            ["ssh", "alice@host", "bad-\u{fffd}"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_walks_only_requested_descendants() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "kettle-proc-tree-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let make_process = |pid: u32, argv: &[u8], children: &[u8]| {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(dir.join("task").join(pid.to_string())).unwrap();
            std::fs::write(dir.join("cmdline"), argv).unwrap();
            std::fs::write(
                dir.join("task").join(pid.to_string()).join("children"),
                children,
            )
            .unwrap();
            symlink("/tmp", dir.join("cwd")).unwrap();
        };
        make_process(10, b"bash\0", b"20\n");
        make_process(20, b"ssh\0alice@box.example\0", b"");
        make_process(99, b"docker\0exec\0unrelated\0sh\0", b"");

        let mut tree = LinuxProcessTree::default();
        tree.refresh_from(&root, &[10]);
        let index = build_children_index(&tree);
        assert_eq!(tree.all_pids().len(), 2);
        assert!(!tree.all_pids().contains(&99));
        assert_eq!(tree.parent_of(20), Some(10));
        assert_eq!(tree.cwd_of(20).as_deref(), Some("/tmp"));
        assert_eq!(
            detect_root_in_index(10, &tree, &index),
            Some(RemoteContext::Ssh {
                host: "box.example".into(),
                user: Some("alice".into()),
            })
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Audit (robustness): `refresh_from` used to bound only a SINGLE
    /// file's size (`MAX_PROC_FILE_BYTES`), not the sum across the whole
    /// walk — a wide subtree of large-argv descendants could retain
    /// multiple GiB in `entries`. Build ~40 descendants each with a
    /// near-1-MiB `cmdline` (comfortably over `MAX_PROC_TREE_TOTAL_BYTES`
    /// in aggregate, even though each individually passes the per-file
    /// cap) and assert the walk stops accumulating argv bytes well before
    /// the naive `N * per-file-cap` total, while still recording every
    /// descendant's parent/cwd (structure survives the budget; only the
    /// large payloads are dropped).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_caps_aggregate_argv_bytes_across_the_whole_walk() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "kettle-proc-tree-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Just under MAX_PROC_FILE_BYTES (1 MiB) so the per-file cap alone
        // would happily accept every one of these.
        const BLOB_LEN: usize = 1_048_000;
        const N: u32 = 40;
        let scan_root = 1000u32;
        let mut root_children = String::new();
        for pid in 1..=N {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(dir.join("task").join(pid.to_string())).unwrap();
            let mut argv = vec![b'x'; BLOB_LEN];
            *argv.last_mut().unwrap() = 0; // NUL-terminate: one argv token.
            std::fs::write(dir.join("cmdline"), &argv).unwrap();
            std::fs::write(dir.join("task").join(pid.to_string()).join("children"), b"").unwrap();
            symlink("/tmp", dir.join("cwd")).unwrap();
            root_children.push_str(&pid.to_string());
            root_children.push('\n');
        }
        let root_dir = root.join(scan_root.to_string());
        std::fs::create_dir_all(root_dir.join("task").join(scan_root.to_string())).unwrap();
        std::fs::write(root_dir.join("cmdline"), b"bash\0").unwrap();
        std::fs::write(
            root_dir
                .join("task")
                .join(scan_root.to_string())
                .join("children"),
            root_children.as_bytes(),
        )
        .unwrap();
        symlink("/tmp", root_dir.join("cwd")).unwrap();

        let mut tree = LinuxProcessTree::default();
        tree.refresh_from(&root, &[scan_root]);

        // Structure (parent + cwd) survives for every descendant regardless
        // of the aggregate budget.
        for pid in 1..=N {
            assert_eq!(tree.parent_of(pid), Some(scan_root));
            assert_eq!(tree.cwd_of(pid).as_deref(), Some("/tmp"));
        }
        // But the total retained argv payload is capped well below the
        // naive N * BLOB_LEN worst case, and at least one descendant's argv
        // was skipped once the aggregate budget was spent.
        let total_argv_bytes: usize = (1..=N)
            .filter_map(|pid| tree.argv_of(pid))
            .map(|argv| argv.iter().map(|s| s.len()).sum::<usize>())
            .sum();
        assert!(
            total_argv_bytes < (N as usize) * BLOB_LEN,
            "aggregate byte budget did not cap total retained argv bytes: {total_argv_bytes}"
        );
        assert!(
            (1..=N).any(|pid| tree.argv_of(pid).is_none()),
            "expected at least one descendant's argv to be skipped past the aggregate budget"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Drift guard. `format_remote_title` is the pure
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

    /// Drift guard. `clone_session_command` is the pure
    /// formatter for the right-click "Reconnect to …" menu entry's
    /// dispatched command (phase 7 of the remote-session design).
    #[test]
    fn clone_session_command_for_all_shapes() {
        // v2.32.0 (audit H1): dynamic fields are POSIX single-quoted and the
        // return is `Option` (None only for an unsafe/control-char field).
        // SSH without user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
            }),
            Some("ssh 'box'".to_string())
        );
        // SSH with user.
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: Some("me".into()),
            }),
            Some("ssh 'me'@'box'".to_string())
        );
        // Docker.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "ubuntu".into(),
            }),
            Some("docker exec -it 'ubuntu' $SHELL".to_string())
        );
        // Podman.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "fedora".into(),
            }),
            Some("podman exec -it 'fedora' $SHELL".to_string())
        );
        // Kubectl (note the `--` separator).
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
            }),
            Some("kubectl exec -it 'my-pod' -- $SHELL".to_string())
        );
        // LXC.
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "alpine".into(),
            }),
            Some("lxc-attach -n 'alpine'".to_string())
        );
    }

    /// H1 (audit v2.32.0, SECURITY): the host/user/container fields are
    /// argv-derived and the caller AUTO-EXECUTES `clone_session_command`'s output
    /// by writing it to the PTY with a trailing newline. A hostile token must
    /// either (a) never become a RemoteContext (parse-time rejection in
    /// detect_ssh/detect_container) or (b) be rendered inert by single-quoting,
    /// and a control char (esp. newline) must yield None — never a multi-line
    /// command. This test exercises BOTH layers.
    #[test]
    fn clone_session_command_neutralizes_shell_injection() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // --- Layer 1: parse-time rejection ---------------------------------
        // A host carrying a `;`/`$()` shell metachar never becomes a context.
        assert_eq!(detect_ssh(&argv(&["ssh", "h; rm -rf ~"])), None);
        assert_eq!(detect_ssh(&argv(&["ssh", "$(reboot)@h"])), None);
        // A container named `$(reboot)` is likewise rejected at parse time.
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "$(reboot)", "sh"])),
            None
        );
        assert_eq!(detect_container(&argv(&["lxc-attach", "-n", "a;b"])), None);
        // A NEWLINE in the token (worst case — would split into extra exec'd
        // lines) is rejected outright: it can never produce a RemoteContext.
        assert_eq!(detect_ssh(&argv(&["ssh", "h\nrm -rf ~"])), None);
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "c\nreboot", "sh"])),
            None
        );

        // --- Layer 2: build-time single-quoting + control-char None --------
        // Even if a metachar value were constructed directly (bypassing the
        // detectors), single-quoting makes it inert — the `;`/`$()` are literal.
        let cmd = clone_session_command(&RemoteContext::Ssh {
            host: "h; rm -rf ~".into(),
            user: None,
        })
        .expect("no control char → Some, just quoted");
        // The metacharacters live entirely inside one quoted argument — there is
        // no UNQUOTED `;`/`$`/`(` that the shell could act on. (The exact-string
        // compare below pins this fully; the helper double-checks the property.)
        assert_eq!(cmd, "ssh 'h; rm -rf ~'");
        assert!(
            !has_unquoted_metachar(&cmd),
            "metachars must stay quoted: {cmd}"
        );

        let cmd = clone_session_command(&RemoteContext::Container {
            runtime: ContainerRuntime::Docker,
            container: "$(reboot)".into(),
        })
        .expect("no control char → Some");
        // NOTE: the trailing literal `$SHELL` placeholder is intentional (the
        // user's pane shell resolves it), so the metachar property is asserted on
        // just the quoted container token, not the whole line.
        assert_eq!(cmd, "docker exec -it '$(reboot)' $SHELL");
        assert!(
            !has_unquoted_metachar("docker exec -it '$(reboot)'"),
            "container token metachars must stay quoted: {cmd}"
        );

        // An embedded single-quote is escaped via the `'\''` idiom (no break-out).
        let cmd = clone_session_command(&RemoteContext::Ssh {
            host: "a'b".into(),
            user: None,
        })
        .unwrap();
        assert_eq!(cmd, "ssh 'a'\\''b'");

        // A control char (newline) at build time → None (never a multi-line cmd).
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "h\nrm -rf ~".into(),
                user: None,
            }),
            None
        );
        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "h".into(),
                user: Some("u\nx".into()),
            }),
            None
        );
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "p\nx".into(),
            }),
            None
        );
        // Whatever clone_session_command returns, it is always a single line.
        for ctx in [
            RemoteContext::Ssh {
                host: "ok-host".into(),
                user: Some("me".into()),
            },
            RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "ok_container".into(),
            },
        ] {
            if let Some(cmd) = clone_session_command(&ctx) {
                assert!(!cmd.contains('\n'), "command must be one line: {cmd}");
            }
        }
    }

    /// Test helper: is there a shell metacharacter OUTSIDE single quotes in
    /// `cmd`? Used to assert the dynamic fields are fully quoted. Tracks a simple
    /// in/out-of-`'…'` state (kettle's quoting never nests quotes — a literal
    /// quote is rendered as the `'\''` break-out idiom, which this still reads
    /// correctly because the inner `\'` is itself outside quotes but is a
    /// backslash-escape, not one of the metachars we flag).
    fn has_unquoted_metachar(cmd: &str) -> bool {
        let mut in_quote = false;
        for c in cmd.chars() {
            match c {
                '\'' => in_quote = !in_quote,
                ';' | '$' | '`' | '&' | '|' | '<' | '>' | '(' | ')' if !in_quote => return true,
                _ => {}
            }
        }
        false
    }

    /// H2 (audit v2.32.0): `ssh -l bob h` must reproduce the login user end to
    /// end — both the remote title and the Reconnect command render `bob@h`. An
    /// explicit `user@host` still wins over `-l` (OpenSSH precedence).
    #[test]
    fn ssh_dash_l_user_reaches_title_and_reconnect() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ctx = detect_ssh(&argv(&["ssh", "-l", "bob", "h"])).unwrap();
        assert_eq!(
            ctx,
            RemoteContext::Ssh {
                host: "h".into(),
                user: Some("bob".into()),
            }
        );
        assert_eq!(format_remote_title(&ctx), "ssh bob@h");
        assert_eq!(
            clone_session_command(&ctx),
            Some("ssh 'bob'@'h'".to_string())
        );
        assert_eq!(clone_session_label(&ctx), "Reconnect ssh bob@h");

        // user@host wins over -l.
        let ctx = detect_ssh(&argv(&["ssh", "-l", "bob", "alice@h"])).unwrap();
        assert_eq!(
            ctx,
            RemoteContext::Ssh {
                host: "h".into(),
                user: Some("alice".into()),
            }
        );
        assert_eq!(format_remote_title(&ctx), "ssh alice@h");
    }

    /// Drift guard: `clone_session_label` is the menu
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

    /// Drift guard: `detect_remote` returns None for
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

    /// Drift guard. `detect_container` walks the four
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

    /// Drift guard. `detect_ssh` walks argv shapes that
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
        // ssh -l user host — H2 (audit v2.32.0): `-l bob` now populates the user
        // so Reconnect / the remote title reproduce `ssh bob@h` (previously the
        // login user was silently dropped).
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: Some("bob".into()),
            })
        );
        // ssh -l bob alice@h — an explicit user@host wins over -l (OpenSSH
        // precedence); the login user stays `alice`.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "alice@h"])),
            Some(RemoteContext::Ssh {
                host: "h".into(),
                user: Some("alice".into()),
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

    /// Drift guard: argv[0] in the Windows shape — a
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

    /// Each of these argv shapes used to drive the WRONG
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

    /// Audit (correctness): `detect_container` used to locate the `exec`
    /// subcommand with an unanchored `.position()` scan of the WHOLE argv,
    /// so a later positional value that happened to literally be "exec"
    /// (a build tag, a k8s object name, …) was mistaken for the subcommand
    /// boundary and the argv element right after it became a phantom
    /// container. The subcommand must now be exactly the first non-flag
    /// token (after skipping only recognized global option flags).
    #[test]
    fn detect_container_does_not_scan_past_the_subcommand_for_a_coincidental_exec() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `docker build -t exec .` — "exec" is the IMAGE TAG, not a
        // subcommand. Must NOT become a phantom `Container` context.
        assert_eq!(
            detect_container(&argv(&["docker", "build", "-t", "exec", "."])),
            None
        );
        // Same shape for podman / kubectl (any non-"exec" first positional
        // must reject, regardless of a later "exec"-shaped token).
        assert_eq!(
            detect_container(&argv(&["podman", "run", "--name", "exec", "alpine"])),
            None
        );
        assert_eq!(
            detect_container(&argv(&["kubectl", "get", "pods", "exec"])),
            None
        );
        // A container/pod genuinely named "exec" still works when it's the
        // ACTUAL argument to a real `exec` subcommand.
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "exec", "sh"])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "exec".into(),
            })
        );
    }

    /// Audit (correctness): for `sshpass`, the inner `ssh` used to be found
    /// via `argv.iter().position(|a| argv0_basename(a) == "ssh")`, an
    /// unanchored scan of the WHOLE argv — including sshpass's own flag
    /// VALUES. A `-p`/`-f` value that happened to basename to "ssh" (a
    /// password or password-file path literally named "ssh") was mistaken
    /// for the real ssh invocation, silently discarding the real target.
    #[test]
    fn detect_ssh_sshpass_does_not_match_a_flag_value_named_ssh() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Password literally "ssh": the -p VALUE must not be mistaken for
        // the wrapped ssh binary — the real ssh + target come after it.
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-p", "ssh", "ssh", "user@host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("user".into()),
            })
        );
        // Same for a `-f` password-file path literally named "ssh".
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-f", "ssh", "ssh", "user@host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("user".into()),
            })
        );
        // sshpass wrapping a non-ssh command is not an ssh session.
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-p", "secret", "scp", "f", "host:/f"])),
            None
        );
    }

    // === ProcessTree fixture + mocked BFS tests =========
    //
    // Previously the only `detect_remote_with` test was the
    // `detect_remote_returns_none_for_invalid_pids` smoke (above)
    // — it called the real sysinfo against pid 0 / u32::MAX. The
    // BFS body (descendant walk, closer-wins-on-tie, refresh
    // contract) was untested because spawning real ssh from CI
    // is too fragile. Extracting [`ProcessTree`] made the
    // BFS body testable with a synthetic process tree.

    /// Fixture: a `ProcessTree` impl backed by a hashmap.
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

        /// Like `add` but with a reported working directory (for the
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

    /// Drift guard: `pwsh → wsl.exe` (the user's exact case — open
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

    /// v2.29.0: the native-cwd foreground walk picks the DEEPEST descendant
    /// (where the user is) regardless of whether it's a known shell — `pwsh →
    /// git status` tracks git's pid (which inherits the shell's cwd); a bare
    /// shell with no children returns None so the caller reads the shell's own
    /// process cwd. (Contrast with `find_foreground_shell`, which filters to
    /// interactive shells.)
    #[test]
    fn deepest_descendant_tracks_foreground_for_native_cwd() {
        // pwsh → git (an external command, NOT a shell): deepest = git's pid.
        let mut tree = MockProcessTree::new();
        tree.add_cwd(1, None, &["pwsh.exe"], "C:\\proj");
        tree.add_cwd(2, Some(1), &["git", "status"], "C:\\proj");
        let idx = build_children_index(&tree);
        assert_eq!(deepest_descendant_in_index(1, &idx), Some(2));

        // Deeper chain wins: pwsh → wsl → bash.
        let mut tree = MockProcessTree::new();
        tree.add(10, None, &["pwsh.exe"]);
        tree.add(11, Some(10), &["wsl.exe"]);
        tree.add(12, Some(11), &["bash"]);
        let idx = build_children_index(&tree);
        assert_eq!(deepest_descendant_in_index(10, &idx), Some(12));

        // No descendants → None (caller reads the root's own cwd).
        let mut tree = MockProcessTree::new();
        tree.add(20, None, &["pwsh.exe"]);
        let idx = build_children_index(&tree);
        assert_eq!(deepest_descendant_in_index(20, &idx), None);
    }

    /// v2.32.0 (audit, medium): once the tree FORKS, "deepest descendant" is no
    /// longer a valid foreground signal — a background job in another branch can
    /// be deeper than the real foreground. So a root with >1 child returns None,
    /// and `foreground_cwd` falls back to the root shell's own cwd (which tracks
    /// the shell's builtin `cd`). Pre-fix this walked into the deeper background
    /// branch and labelled the pane with the wrong dir.
    #[test]
    fn deepest_descendant_forked_tree_returns_none() {
        // pwsh → { idle foreground prompt (leaf), backgrounded `sleep 999 &`
        // chain that happens to be deeper }. The fork at the root means we cannot
        // tell the foreground apart, so bail to None (root-cwd fallback).
        let mut tree = MockProcessTree::new();
        tree.add(1, None, &["pwsh.exe"]); // root shell (two children = fork)
        tree.add(2, Some(1), &["nvim"]); // foreground at depth 1
        tree.add(3, Some(1), &["sleep", "999"]); // background at depth 1
        tree.add(4, Some(3), &["sleep-helper"]); // deeper background at depth 2
        let idx = build_children_index(&tree);
        assert_eq!(
            deepest_descendant_in_index(1, &idx),
            None,
            "a forked root is ambiguous → None so foreground_cwd uses the root's own cwd"
        );

        // A fork DEEPER in an otherwise-linear chain also bails: pwsh → wsl →
        // { bash, htop } — the linear prefix is fine but the fork at wsl is not.
        let mut tree = MockProcessTree::new();
        tree.add(10, None, &["pwsh.exe"]);
        tree.add(11, Some(10), &["wsl.exe"]);
        tree.add(12, Some(11), &["bash"]);
        tree.add(13, Some(11), &["htop"]);
        let idx = build_children_index(&tree);
        assert_eq!(
            deepest_descendant_in_index(10, &idx),
            None,
            "a fork at any level is ambiguous → None"
        );
    }

    /// v2.29.1: end-to-end check that the sysinfo-backed native cwd read actually
    /// works on this Windows host — spawns a real `pwsh`, `Set-Location`s it to a
    /// known dir, and reads that dir back via `RemoteScanner::foreground_cwd`.
    /// `#[ignore]`d (spawns a process + is Windows-only); run explicitly:
    /// `cargo test -p kettle-remote -- --ignored foreground_cwd_reads_real_pwsh`.
    #[test]
    #[ignore = "spawns a real pwsh; Windows-only manual verification of the sysinfo PEB cwd read"]
    #[cfg(windows)]
    fn foreground_cwd_reads_real_pwsh_cwd() {
        use std::process::Command;
        let mut child = Command::new("pwsh")
            .args([
                "-NoProfile",
                "-Command",
                "Set-Location C:\\Windows; Start-Sleep 6",
            ])
            .spawn()
            .expect("spawn pwsh");
        let pid = child.id();
        // Give pwsh a moment to start + run Set-Location before reading its cwd.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let mut sc = RemoteScanner::new();
        sc.refresh_roots(&[pid]);
        let cwd = sc.foreground_cwd(pid);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            cwd.as_deref(),
            Some("C:\\Windows"),
            "sysinfo should read the live pwsh process cwd (got {cwd:?})"
        );
    }

    /// The DEEPEST shell wins (most-nested ≈ current foreground), and
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

    /// User-reported on native Ubuntu: a foreground agent/editor
    /// (`claude`/`codex`/`nvim`) spawns transient `sh -c "…"` helpers. The
    /// detector must NOT clone a one-shot helper into a split — doing so spawns a
    /// shell that runs the command and exits immediately, leaving a blank/dead
    /// pane ("new pane but no terminal would load"). With no INTERACTIVE shell
    /// descendant, it returns None so the caller clones the pane's real shell.
    #[test]
    fn foreground_shell_ignores_agent_and_editor_spawned_oneshot_helpers() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash", "-l"]); // pane's login shell (BFS starts at its children)
        tree.add(200, Some(100), &["node", "/usr/bin/claude"]); // Claude Code CLI
        tree.add(300, Some(200), &["sh", "-c", "rg --json foo"]); // transient tool helper
        assert_eq!(
            find_foreground_shell(100, &mut tree),
            None,
            "a node-spawned `sh -c` helper must not be cloned into a split"
        );

        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["zsh", "-l"]);
        tree.add(200, Some(100), &["codex", "exec"]);
        tree.add(300, Some(200), &["bash", "-lc", "cargo test -p kettle"]);
        assert_eq!(
            find_foreground_shell(100, &mut tree),
            None,
            "a Codex-spawned `bash -lc` helper must not be cloned into a split"
        );

        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["fish"]);
        tree.add(200, Some(100), &["nvim"]);
        tree.add(300, Some(200), &["fish", "--command", "lazygit"]);
        assert_eq!(
            find_foreground_shell(100, &mut tree),
            None,
            "an editor-spawned one-shot shell helper must not be cloned into a split"
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
            // -EncodedCommand / -e / -enc run a one-shot.
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
            // `~` selects the home dir for an INTERACTIVE
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

    /// Drift guard: ssh as a direct child of the pane's
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

    /// v2.32.0 (audit, low): a pane that launches a remote client DIRECTLY
    /// (`command = ssh box`, no intervening shell) — the pane root pid IS the
    /// `ssh` process. The BFS must inspect the root itself (depth 0), not only its
    /// children, or the pane gets no RemoteContext (no Reconnect, no remote title).
    #[test]
    fn detect_in_tree_root_pid_is_ssh() {
        let mut tree = MockProcessTree::new();
        // No shell — the pane's child_pid argv is ssh itself.
        tree.add(100, None, &["ssh", "carol@direct.example.com"]);
        assert_eq!(
            detect_in_tree(100, &mut tree),
            Some(RemoteContext::Ssh {
                host: "direct.example.com".into(),
                user: Some("carol".into()),
            })
        );
    }

    /// v2.32.0 (audit, low): same root-pid detection through the shared-index
    /// path that `RemoteScanner::detect_root` actually uses, with a directly-
    /// launched `docker exec` pane root.
    #[test]
    fn detect_root_in_index_root_pid_is_container() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["docker", "exec", "-it", "api-1", "bash"]);
        tree.refresh();
        let idx = build_children_index(&tree);
        assert_eq!(
            detect_root_in_index(100, &tree, &idx),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "api-1".into(),
            })
        );
    }

    /// Drift guard: the shared-index path (`build_children_index`
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

    /// Drift guard: `ssh-with-credentials` wrappers
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

    /// Drift guard: container exec at depth 3 (shell →
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

    /// Drift guard: when two descendants both match a
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

    /// Drift guard: when `child_pid` has no entry in the
    /// tree AND nothing claims it as parent, return `None` without
    /// looping forever or panicking.
    #[test]
    fn detect_in_tree_missing_root_returns_none() {
        let mut tree = MockProcessTree::new();
        tree.add(50, Some(40), &["bash"]); // unrelated tree
        tree.add(60, Some(50), &["ssh", "elsewhere"]);
        assert!(detect_in_tree(999, &mut tree).is_none());
    }

    /// Drift guard: an empty tree returns `None`.
    /// Boundary case for the BFS init.
    #[test]
    fn detect_in_tree_empty_tree_returns_none() {
        let mut tree = MockProcessTree::new();
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Drift guard: descendants that don't match a
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
        // detector. Previously this couldn't be tested without
        // spawning a real grep.
        tree.add(200, Some(150), &["grep", "ssh", "log.txt"]);
        assert!(detect_in_tree(100, &mut tree).is_none());
    }

    /// Drift guard: a cycle in the parent chain
    /// (impossible in a real OS, but possible in a buggy fixture
    /// or future trait impl) must not loop the BFS forever. The
    /// `visited` set protects against this;
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
