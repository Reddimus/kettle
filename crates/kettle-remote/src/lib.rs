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
    /// `user` is the optional username if the argv had `user@host` or `-l user`;
    /// `options` carries the connection options that decide which endpoint the
    /// host name actually resolves to.
    Ssh {
        host: String,
        user: Option<String>,
        options: SshOptions,
    },
    /// Container session (Docker / Podman / kubectl exec / lxc-attach).
    /// `container` is the target name/id from the argv; `options` carries the
    /// client-side context that decides which daemon, cluster, or namespace
    /// that name lives in.
    Container {
        runtime: ContainerRuntime,
        container: String,
        options: ContainerOptions,
    },
}

/// The `ssh` connection options that decide WHICH endpoint a session reached.
///
/// A host name on its own does not identify a service: `ssh -p 2222 -J bastion
/// -i key box` and `ssh box` are two different machines behind one word. The
/// detector therefore carries these alongside the host so
/// [`clone_session_command`] can reproduce the original endpoint instead of a
/// plausible-looking neighbour.
///
/// Values are validated at parse time (same conservative-charset rule as the
/// host/user fields) and single-quoted at build time. An endpoint-selecting
/// option that cannot be reproduced sets [`unreproducible`](Self::unreproducible)
/// instead, which suppresses the Reconnect entry entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshOptions {
    /// `-p PORT` (or the joined `-pPORT`): the port the session connected to.
    pub port: Option<String>,
    /// `-J DESTINATION`: the ProxyJump chain the session tunnelled through. A
    /// reconnect without it either fails or lands on a same-named host in the
    /// local network instead.
    pub jump: Option<String>,
    /// `-i IDENTITY_FILE`: the key the endpoint authenticated. Dropping it can
    /// silently select a different account on the same host.
    pub identity: Option<String>,
    /// `-F CONFIG_FILE`: the config that resolves host aliases, and therefore
    /// what `host` names at all.
    pub config: Option<String>,
    /// An endpoint-selecting option was present that this crate does not
    /// reproduce — `-o ProxyCommand=…` (an arbitrary shell command), `-W`
    /// (the session is a stdio forward, not a shell), or a value that escaped
    /// its charset. The pane still gets a remote title; the Reconnect entry is
    /// dropped rather than pointed somewhere else.
    pub unreproducible: bool,
}

/// The client-side context that decides WHICH daemon, cluster, or namespace an
/// `exec` attached to.
///
/// As with [`SshOptions`], the container name alone is not an endpoint:
/// `docker --context remote exec web` and `docker exec web` are containers on
/// two different machines, and `kubectl -n prod exec api` and `kubectl exec
/// api` are two different pods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerOptions {
    /// Which named connection the client used: Docker `--context`/`-c`,
    /// Podman `--connection`/`-c`, kubectl `--context`.
    pub context: Option<String>,
    /// The daemon / API-server address given directly: Docker `--host`/`-H`,
    /// Podman `--url`, kubectl `--server`/`-s`.
    pub endpoint: Option<String>,
    /// kubectl `--namespace`/`-n`.
    pub namespace: Option<String>,
    /// The file or directory that resolves everything else: Docker `--config`,
    /// kubectl `--kubeconfig`, `lxc-attach --lxcpath`/`-P`.
    pub config: Option<String>,
    /// `kubectl exec --container`/`-c`: which container inside the pod. A pod
    /// is not one shell — reconnecting without this lands in the pod's default
    /// container.
    pub pod_container: Option<String>,
    /// An endpoint-selecting option was present that this crate does not
    /// reproduce — a credential that must never be re-emitted (`--token`,
    /// `--password`), a selector with no single-flag equivalent, or a value
    /// that escaped its charset. Reconnect is suppressed rather than sent to
    /// the local daemon.
    pub unreproducible: bool,
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
    detect_queue: std::collections::VecDeque<u32>,
    detect_visited: std::collections::HashSet<u32>,
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
            detect_queue: std::collections::VecDeque::new(),
            detect_visited: std::collections::HashSet::new(),
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
    /// Linux walks only those roots' bounded `/proc/<pid>/task/*/children`
    /// trees, including children created by non-leader threads. That avoids an
    /// OS-wide process walk. Platforms without that rooted procfs interface
    /// retain the cross-platform sysinfo snapshot.
    pub fn refresh_roots(&mut self, roots: &[u32]) -> bool {
        #[cfg(target_os = "linux")]
        {
            let complete = self.procfs.refresh_roots(roots);
            self.index = build_children_index(&self.procfs);
            self.use_procfs = true;
            complete
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = roots;
            self.refresh();
            true
        }
    }

    /// Resolve the remote context for the pane rooted at `child_pid`, using the
    /// index built by the last [`refresh`](Self::refresh). No OS walk, no map
    /// rebuild — safe to call once per pane.
    pub fn detect_root(&mut self, child_pid: u32) -> Option<RemoteContext> {
        self.detect_queue.clear();
        self.detect_visited.clear();
        self.detect_visited.reserve(self.index.len());

        #[cfg(target_os = "linux")]
        let tree: &dyn ProcessTree = if self.use_procfs {
            &self.procfs
        } else {
            &self.sys
        };
        #[cfg(not(target_os = "linux"))]
        let tree: &dyn ProcessTree = &self.sys;

        detect_root_in_index_with_scratch(
            child_pid,
            tree,
            &self.index,
            &mut self.detect_queue,
            &mut self.detect_visited,
        )
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

/// One pane root submitted to the background remote-context scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteProbeTarget {
    pub pid: u32,
    /// False for launch shapes whose host cwd is meaningless (for example
    /// `wsl.exe` or `ssh.exe`).
    pub allow_native_cwd: bool,
}

/// Remote and native-cwd state resolved from one consistent process snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteProbe {
    pub remote: Option<RemoteContext>,
    pub native_cwd: Option<String>,
    pub foreground_shell: Option<ShellLaunch>,
}

/// Latest complete background scan.
#[derive(Debug, Clone)]
pub struct RemoteProbeSnapshot {
    pub probes: std::sync::Arc<std::collections::HashMap<u32, RemoteProbe>>,
}

/// Coalescing, bounded background owner for [`RemoteScanner`].
///
/// Process enumeration and procfs reads never run on the window event loop.
/// Submitting replaces any queued roots with the newest set and emits at most
/// one wake token. Results likewise replace an unconsumed older snapshot.
/// A Linux scan that reaches its byte/node/task/deadline ceiling is not
/// published, so a partial hostile subtree cannot erase the last good UI state.
pub struct RemoteScanWorker {
    pending: std::sync::Arc<std::sync::Mutex<Option<Vec<RemoteProbeTarget>>>>,
    latest: std::sync::Arc<std::sync::Mutex<Option<RemoteProbeSnapshot>>>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl RemoteScanWorker {
    pub fn spawn() -> std::io::Result<Self> {
        Self::spawn_with_notifier(|| {})
    }

    pub fn spawn_with_notifier(notify: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(None));
        let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (wake, wakes) = std::sync::mpsc::sync_channel(1);
        let worker_pending = pending.clone();
        let worker_latest = latest.clone();
        std::thread::Builder::new()
            .name("kettle-remote-scan".into())
            .spawn(move || {
                let mut scanner = RemoteScanner::new();
                let mut roots = Vec::new();
                while wakes.recv().is_ok() {
                    loop {
                        let Some(mut targets) = worker_pending
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        else {
                            break;
                        };
                        normalize_probe_targets(&mut targets);
                        roots.clear();
                        roots.extend(targets.iter().map(|target| target.pid));
                        if !scanner.refresh_roots(&roots) {
                            continue;
                        }
                        let mut probes = std::collections::HashMap::with_capacity(targets.len());
                        for target in targets {
                            let remote = scanner.detect_root(target.pid);
                            let foreground_shell = scanner.foreground_shell(target.pid);
                            let nested_wsl = foreground_shell.as_ref().is_some_and(|shell| {
                                shell
                                    .argv
                                    .first()
                                    .is_some_and(|argv0| argv0_basename(argv0) == "wsl")
                            });
                            let native_cwd =
                                if target.allow_native_cwd && remote.is_none() && !nested_wsl {
                                    scanner.foreground_cwd(target.pid)
                                } else {
                                    None
                                };
                            probes.insert(
                                target.pid,
                                RemoteProbe {
                                    remote,
                                    native_cwd,
                                    foreground_shell,
                                },
                            );
                        }
                        let probes = std::sync::Arc::new(probes);
                        *worker_latest
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(RemoteProbeSnapshot { probes });
                        notify();
                    }
                }
            })?;
        Ok(Self {
            pending,
            latest,
            wake,
        })
    }

    pub fn submit(&self, mut targets: Vec<RemoteProbeTarget>) {
        normalize_probe_targets(&mut targets);
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(targets);
        let _ = self.wake.try_send(());
    }

    pub fn take_latest(&self) -> Option<RemoteProbeSnapshot> {
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

fn normalize_probe_targets(targets: &mut Vec<RemoteProbeTarget>) {
    targets.sort_unstable_by_key(|target| target.pid);
    let mut write = 0_usize;
    for read in 0..targets.len() {
        if write != 0 && targets[write - 1].pid == targets[read].pid {
            targets[write - 1].allow_native_cwd &= targets[read].allow_native_cwd;
        } else {
            targets.swap(write, read);
            write += 1;
        }
    }
    targets.truncate(write.min(MAX_REMOTE_PROBE_TARGETS));
}

#[cfg(target_os = "linux")]
const MAX_PROC_FILE_BYTES: u64 = 1 << 20;
const MAX_REMOTE_PROBE_TARGETS: usize = 4096;
#[cfg(target_os = "linux")]
const MAX_PROC_TREE_NODES: usize = MAX_REMOTE_PROBE_TARGETS;
#[cfg(target_os = "linux")]
const MAX_PROC_TASKS_PER_PROCESS: usize = 1024;
#[cfg(target_os = "linux")]
const MAX_PROC_TASK_FILE_READS: usize = 1024;
#[cfg(target_os = "linux")]
const MAX_PROC_SCAN_DURATION: std::time::Duration = std::time::Duration::from_millis(25);
#[cfg(any(target_os = "linux", test))]
const MAX_PROC_ARGS_PER_PROCESS: usize = 256;
#[cfg(any(target_os = "linux", test))]
const MAX_PROC_ARG_DECODED_BYTES: usize = 64 * 1024;
/// Audit (robustness): aggregate ceiling, in bytes, on ALL `cmdline` +
/// `children` file content read across a single [`LinuxProcessTree::refresh_from`]
/// walk. `MAX_PROC_FILE_BYTES` only bounds a SINGLE file's size; without an
/// aggregate cap, up to `MAX_PROC_TREE_NODES` (4096) descendants each near
/// that 1 MiB per-file ceiling could retain multiple GiB in `self.entries`
/// and cost multiple GiB of file I/O on one `refresh_roots` tick. Four MiB is
/// generous next to legitimate shell argv/child-list totals while keeping a
/// hostile pane's background scan bounded. The app consumes these snapshots
/// asynchronously; it never performs this walk on the event-loop thread.
#[cfg(target_os = "linux")]
const MAX_PROC_TREE_TOTAL_BYTES: u64 = 4 * MAX_PROC_FILE_BYTES;

#[cfg(any(target_os = "linux", test))]
fn parse_proc_children(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|pid| !pid.is_empty())
        .filter_map(|pid| std::str::from_utf8(pid).ok()?.parse().ok())
}

#[cfg(any(target_os = "linux", test))]
struct ParsedProcArgv {
    argv: Vec<String>,
    complete: bool,
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_argv(bytes: &[u8]) -> ParsedProcArgv {
    let mut argv = Vec::new();
    let mut decoded_bytes = 0_usize;
    for arg in bytes.split(|byte| *byte == 0).filter(|arg| !arg.is_empty()) {
        if argv.len() >= MAX_PROC_ARGS_PER_PROCESS {
            return ParsedProcArgv {
                argv,
                complete: false,
            };
        }
        let arg = String::from_utf8_lossy(arg);
        let Some(next_decoded_bytes) = decoded_bytes.checked_add(arg.len()) else {
            return ParsedProcArgv {
                argv,
                complete: false,
            };
        };
        if next_decoded_bytes > MAX_PROC_ARG_DECODED_BYTES {
            return ParsedProcArgv {
                argv,
                complete: false,
            };
        }
        decoded_bytes = next_decoded_bytes;
        argv.push(arg.into_owned());
    }
    ParsedProcArgv {
        argv,
        complete: true,
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct LinuxProcessTree {
    entries: std::collections::HashMap<u32, LinuxProcessEntry>,
    proc_root: std::path::PathBuf,
    bytes_read: u64,
    task_files_read: usize,
}

#[cfg(target_os = "linux")]
struct LinuxProcessEntry {
    parent: Option<u32>,
    argv: Option<Vec<String>>,
}

#[cfg(target_os = "linux")]
impl LinuxProcessTree {
    fn refresh_roots(&mut self, roots: &[u32]) -> bool {
        self.refresh_from(std::path::Path::new("/proc"), roots)
    }

    /// Audit (robustness): synchronous procfs walk backing `refresh_roots`,
    /// which kettle-ui's remote scanner worker calls at most once per
    /// coalesced request. `MAX_PROC_TREE_NODES` bounds node COUNT and
    /// `MAX_PROC_FILE_BYTES` bounds a single file's size, but neither bounds
    /// the SUM of every node's argv/children bytes — see
    /// `MAX_PROC_TREE_TOTAL_BYTES`, which this walk now also enforces:
    /// once the running total reaches that ceiling, further `cmdline`/
    /// `children` reads are skipped for the rest of the walk (the node
    /// itself and its already-known parent are still recorded — only the
    /// potentially-large file payloads are dropped). Cwd is read on demand
    /// for the selected foreground process instead of being retained for
    /// every entry, so a pathological or hostile descendant subtree can no
    /// longer balloon this to multiple GiB of retained memory or synchronous
    /// I/O.
    fn refresh_from(&mut self, proc_root: &std::path::Path, roots: &[u32]) -> bool {
        use std::collections::{HashSet, VecDeque};

        let deadline = std::time::Instant::now() + MAX_PROC_SCAN_DURATION;
        self.entries.clear();
        self.proc_root.clear();
        self.proc_root.push(proc_root);
        let mut queue: VecDeque<_> = roots.iter().copied().map(|pid| (pid, None)).collect();
        let mut scheduled: HashSet<_> = roots.iter().copied().collect();
        let mut total_bytes: u64 = 0;
        let mut task_files_read = 0_usize;
        let mut complete = true;
        while let Some((pid, parent)) = queue.pop_front() {
            if std::time::Instant::now() >= deadline {
                complete = false;
                break;
            }
            if self.entries.len() >= MAX_PROC_TREE_NODES {
                complete = false;
                continue;
            }
            let process_dir = proc_root.join(pid.to_string());
            let argv = match read_proc_file_charged(&process_dir.join("cmdline"), &mut total_bytes)
            {
                ProcFileRead::Complete(bytes) => {
                    let parsed = parse_proc_argv(&bytes);
                    complete &= parsed.complete;
                    Some(parsed.argv)
                }
                ProcFileRead::Unavailable => None,
                ProcFileRead::Incomplete => {
                    complete = false;
                    None
                }
            };
            let task_dir = process_dir.join("task");
            let mut child_metadata_read = false;
            // Always inspect the leader task first so a process with more than
            // the per-process task cap still preserves the conventional edge.
            let (read, within_limits) = read_proc_task_children(
                &task_dir,
                pid,
                pid,
                &mut total_bytes,
                &mut task_files_read,
                &mut scheduled,
                &mut queue,
                deadline,
            );
            child_metadata_read |= read;
            complete &= within_limits;
            if task_files_read < MAX_PROC_TASK_FILE_READS
                && total_bytes < MAX_PROC_TREE_TOTAL_BYTES
                && std::time::Instant::now() < deadline
            {
                match std::fs::read_dir(&task_dir) {
                    Ok(tasks) => {
                        for (task_entries, task) in tasks.enumerate() {
                            let task = match task {
                                Ok(task) => task,
                                Err(_) => {
                                    complete = false;
                                    break;
                                }
                            };
                            if task_entries >= MAX_PROC_TASKS_PER_PROCESS {
                                complete = false;
                                break;
                            }
                            let Some(task_pid) = task
                                .file_name()
                                .to_str()
                                .and_then(|name| name.parse::<u32>().ok())
                            else {
                                continue;
                            };
                            if task_pid != pid {
                                let (read, within_limits) = read_proc_task_children(
                                    &task_dir,
                                    task_pid,
                                    pid,
                                    &mut total_bytes,
                                    &mut task_files_read,
                                    &mut scheduled,
                                    &mut queue,
                                    deadline,
                                );
                                child_metadata_read |= read;
                                complete &= within_limits;
                            }
                            if task_files_read >= MAX_PROC_TASK_FILE_READS
                                || total_bytes >= MAX_PROC_TREE_TOTAL_BYTES
                                || std::time::Instant::now() >= deadline
                            {
                                complete = false;
                                break;
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => complete = false,
                }
            }
            // A descendant already named by its parent's bounded `children`
            // file remains useful process-tree structure even when its own
            // metadata disappeared or the aggregate byte budget is spent.
            // Only an unreadable requested root has no authenticated edge to
            // retain.
            if argv.is_none() && !child_metadata_read && parent.is_none() {
                continue;
            }
            self.entries.insert(pid, LinuxProcessEntry { parent, argv });
        }
        self.bytes_read = total_bytes;
        self.task_files_read = task_files_read;
        complete
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn read_proc_task_children(
    task_dir: &std::path::Path,
    task_pid: u32,
    parent_pid: u32,
    total_bytes: &mut u64,
    task_files_read: &mut usize,
    scheduled: &mut std::collections::HashSet<u32>,
    queue: &mut std::collections::VecDeque<(u32, Option<u32>)>,
    deadline: std::time::Instant,
) -> (bool, bool) {
    if *task_files_read >= MAX_PROC_TASK_FILE_READS
        || *total_bytes >= MAX_PROC_TREE_TOTAL_BYTES
        || std::time::Instant::now() >= deadline
    {
        return (false, false);
    }
    *task_files_read += 1;
    let path = task_dir.join(task_pid.to_string()).join("children");
    let bytes = match read_proc_file_charged(&path, total_bytes) {
        ProcFileRead::Complete(bytes) => bytes,
        ProcFileRead::Unavailable => return (false, true),
        ProcFileRead::Incomplete => return (false, false),
    };
    let mut complete = true;
    for child in parse_proc_children(&bytes) {
        if scheduled.len() >= MAX_PROC_TREE_NODES {
            complete = false;
            break;
        }
        if scheduled.insert(child) {
            queue.push_back((child, Some(parent_pid)));
        }
    }
    (true, complete)
}

#[cfg(target_os = "linux")]
enum ProcFileRead {
    Complete(Vec<u8>),
    /// The proc entry disappeared or could not be opened before any bytes were
    /// consumed. Process exit/permission races are normal and do not make the
    /// rest of the snapshot internally partial.
    Unavailable,
    /// The read reached a byte ceiling or failed after opening. Publishing
    /// this scan could erase state based on truncated evidence.
    Incomplete,
}

#[cfg(target_os = "linux")]
fn read_proc_file_charged(path: &std::path::Path, total_bytes: &mut u64) -> ProcFileRead {
    use std::io::Read;

    let remaining = MAX_PROC_TREE_TOTAL_BYTES.saturating_sub(*total_bytes);
    let limit = remaining.min(MAX_PROC_FILE_BYTES);
    if limit == 0 {
        return ProcFileRead::Incomplete;
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ProcFileRead::Unavailable;
        }
        Err(_) => return ProcFileRead::Incomplete,
    };
    let mut bytes = Vec::with_capacity(usize::try_from(limit.min(8192)).unwrap_or(8192));
    let mut reader = file.take(limit);
    let result = reader.read_to_end(&mut bytes);
    *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
    if result.is_err() || bytes.len() as u64 == limit {
        // Reaching the limit does not prove EOF without reading another byte.
        // Reject the unproven/truncated file and charge every byte already
        // consumed to the aggregate budget.
        ProcFileRead::Incomplete
    } else {
        ProcFileRead::Complete(bytes)
    }
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
        self.entries.get(&pid)?;
        std::fs::read_link(self.proc_root.join(pid.to_string()).join("cwd"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
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
    let mut queue = std::collections::VecDeque::new();
    let mut visited = std::collections::HashSet::with_capacity(children_by_parent.len());
    detect_root_in_index_with_scratch(
        child_pid,
        tree,
        children_by_parent,
        &mut queue,
        &mut visited,
    )
}

fn detect_root_in_index_with_scratch<T: ProcessTree + ?Sized>(
    child_pid: u32,
    tree: &T,
    children_by_parent: &std::collections::HashMap<u32, Vec<u32>>,
    queue: &mut std::collections::VecDeque<u32>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<RemoteContext> {
    queue.clear();
    visited.clear();
    // BFS from child_pid; closer processes checked first. Loop bound: each pid is
    // enqueued ≤ 1 time (a Pid only has one parent, and `visited` dedupes), so
    // termination is guaranteed even on a cyclic children_by_parent (which
    // shouldn't happen but the bound protects against a future fixture bug).
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
    visited.insert(child_pid);
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
    for _ in 0..=children_by_parent.len() {
        match children_by_parent.get(&node).map(Vec::as_slice) {
            // Linear step: descend to the sole child.
            Some([only]) => {
                // Defensive against a cyclic fixture/index (a pid can normally
                // have only one parent, so this should never fire): stop rather
                // than loop forever.
                deepest = Some(*only);
                node = *only;
            }
            // Fork: >1 child at this level → ambiguous foreground, bail to None.
            Some(_) => return None,
            // Leaf: end of a linear chain (or `root` itself had no children).
            None => break,
        }
    }
    // If a malformed index cycles for more steps than it contains nodes, the
    // foreground is not trustworthy.
    if children_by_parent.get(&node).is_some() {
        None
    } else {
        deepest
    }
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
        && s.len() <= MAX_FIELD_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// Longest argv-derived value any field will carry into the auto-executed
/// Reconnect line. Nothing legitimate comes close — the longest realistic case
/// is a Windows extended-length path, and even those stay well under this — but
/// the values come from a descendant process's argv, which the user did not
/// necessarily type. Without a cap, one field can make the emitted line longer
/// than the tty's canonical input buffer (4096 bytes on Linux), and the shell
/// then sees a truncated line: harmless in practice, because truncation lands
/// inside a `'…'` and leaves an unterminated quote rather than a different
/// command, but a bounded value is the property to have on a line kettle types
/// into a PTY for the user.
const MAX_FIELD_LEN: usize = 512;

/// SSH host: DNS labels (`a-z0-9.-`), IPv6 literals (`:`, optional `[ ]`
/// brackets and `%zone`), and the `user@host` split already consumed the `@`.
const SSH_HOST_EXTRA: &str = ".-_:%[]";
/// SSH login user: POSIX usernames plus the small set real-world accounts use.
const SSH_USER_EXTRA: &str = ".-_$\\@";
/// SSH ProxyJump destination (`-J`): a host, plus the `user@` prefix and the
/// `,` that separates a multi-hop chain.
const SSH_JUMP_EXTRA: &str = ".-_:%[]@,";
/// A filesystem path given as an option value (`ssh -i`/`-F`, `docker
/// --config`, `kubectl --kubeconfig`, `lxc-attach --lxcpath`). Every character
/// here is literal inside the POSIX single quotes the value is emitted in
/// (including `'` itself, which [`shell_single_quote`] closes/escapes/reopens
/// with the canonical `'\''` idiom), so the set is as wide as real paths need:
/// `C:\Program Files (x86)\…` is the ordinary shape of a Windows install path,
/// and a set without `(`/`)` silently removed the whole Reconnect entry for it
/// rather than reproducing it.
///
/// Still excluded — and the reason this is an allowlist at all — is everything
/// that would matter if the quoting layer were ever wrong: `$`, backtick, `"`,
/// `;`, `|`, `&&`-style control, redirection, glob, and any control character.
/// A path needing one of those is not reproduced (see
/// [`SshOptions::unreproducible`]).
const FILE_PATH_EXTRA: &str = "./\\-_~: +@()[]{},=#!%'";
/// Container name/id: Docker/Podman/kubectl/lxc allow `[A-Za-z0-9][A-Za-z0-9_.-]`
/// plus `/` (kubectl `type/name`, registry-qualified refs) and `:` (tags).
const CONTAINER_EXTRA: &str = ".-_:/";
/// A daemon / API-server address (`docker -H`, `podman --url`, `kubectl -s`):
/// `tcp://host:2375`, `unix:///var/run/docker.sock`, `ssh://user@host`.
const CONTAINER_ENDPOINT_EXTRA: &str = ".-_:/@+%[]";

/// Record an endpoint-selecting option value, or — when the value escapes its
/// charset — mark the context unreproducible. Losing the value silently is the
/// one outcome that is not allowed: it would leave a Reconnect that looks
/// right and connects elsewhere.
fn capture_option(slot: &mut Option<String>, unreproducible: &mut bool, value: &str, safe: bool) {
    if safe {
        *slot = Some(value.to_string());
    } else {
        *unreproducible = true;
    }
}

fn capture_first_option(
    slot: &mut Option<String>,
    seen: &mut bool,
    unreproducible: &mut bool,
    value: &str,
    safe: bool,
) {
    if *seen {
        return;
    }
    *seen = true;
    capture_option(slot, unreproducible, value, safe);
}

/// One short-option occurrence pulled out of a `-abc`-style argv token.
struct ShortOption<'a> {
    /// The first value-taking letter in the bundle, if the bundle has one.
    flag: Option<char>,
    /// Its value: the remainder of the token (`-p2222`), or the next argv
    /// element (`-p 2222`). `None` when the bundle ends on a value-taking
    /// letter with nothing left to consume.
    value: Option<&'a str>,
    /// Whether `value` came from the NEXT argv element, so the caller advances
    /// two tokens instead of one.
    consumed_next: bool,
    /// The boolean letters in front of [`flag`](Self::flag) — the whole bundle
    /// when it has no value-taking letter at all. A boolean is not always
    /// nothing: `podman -r` selects the remote service, so a caller that read
    /// only the value-taking letter reconnected to the local socket.
    booleans: &'a str,
}

/// Walk one `-abc` token the way getopt(3) — and Go's pflag, which
/// docker/podman/kubectl use — does: every letter is an option, boolean
/// letters bundle freely (`-it`, `-Nf`), and the first value-taking letter
/// closes the bundle by taking the rest of the token as its value or, failing
/// that, the next argv element.
///
/// Treating a bundle as one opaque token is what let `ssh -vp 2222 box` read
/// `2222` as the destination and `kubectl exec -itc sidecar pod` read
/// `sidecar` as the pod.
fn parse_short_bundle<'a>(
    bundle: &'a str,
    next: Option<&'a str>,
    mut takes_value: impl FnMut(char) -> bool,
) -> ShortOption<'a> {
    let mut rest = bundle;
    let mut booleans_len = 0;
    while let Some(flag) = rest.chars().next() {
        rest = &rest[flag.len_utf8()..];
        if !takes_value(flag) {
            booleans_len += flag.len_utf8();
            continue;
        }
        let booleans = &bundle[..booleans_len];
        return if rest.is_empty() {
            ShortOption {
                flag: Some(flag),
                value: next,
                consumed_next: next.is_some(),
                booleans,
            }
        } else {
            ShortOption {
                flag: Some(flag),
                value: Some(rest),
                consumed_next: false,
                booleans,
            }
        };
    }
    ShortOption {
        flag: None,
        value: None,
        consumed_next: false,
        booleans: bundle,
    }
}

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
///   - `ssh -p 22 user@host`                 → same, port=Some("22")
///   - `ssh -o StrictHostKeyChecking=no host` → host=host
///   - `sshpass -p secret ssh user@host`     → host=host, user=Some(user)
///
/// Options are walked the way getopt(3) parses them (see the crate-private
/// `parse_short_bundle`) to find the first non-option argv element, which is
/// the target (potentially `user@host`). Along the way the options that decide
/// which endpoint that target names — port, ProxyJump, identity, config file,
/// login name — are captured into [`SshOptions`] instead of merely skipped,
/// and one that cannot be reproduced marks the context so no Reconnect is
/// offered.
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
    // the login user. OpenSSH keeps the first command-line value obtained, so
    // an earlier `-l` also wins over a later target's `user@` component.
    let mut flag_user: Option<&str> = None;
    // The remaining endpoint-selecting options travel with the host: `box` on
    // its own is not a service, and a Reconnect that dropped `-p`/`-J`/`-i`
    // would open a session on a different one.
    let mut options = SshOptions::default();
    let mut port_seen = false;
    let mut jump_seen = false;
    let mut config_seen = false;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with("--") {
            // OpenSSH takes no long options — a `--`-prefixed token is the
            // end-of-options marker or something this detector cannot read.
            i += 1;
            continue;
        }
        let Some(bundle) = a.strip_prefix('-').filter(|rest| !rest.is_empty()) else {
            target = Some(a.as_str());
            break;
        };
        let short = parse_short_bundle(bundle, argv.get(i + 1).map(String::as_str), |flag| {
            SSH_VALUE_OPTIONS.contains(flag)
        });
        if let (Some(flag), Some(value)) = (short.flag, short.value) {
            match flag {
                'l' => {
                    if flag_user.is_none() {
                        flag_user = Some(value);
                    }
                }
                'p' => capture_first_option(
                    &mut options.port,
                    &mut port_seen,
                    &mut options.unreproducible,
                    value,
                    ssh_port_is_safe(value),
                ),
                'J' => capture_first_option(
                    &mut options.jump,
                    &mut jump_seen,
                    &mut options.unreproducible,
                    value,
                    field_is_safe(value, SSH_JUMP_EXTRA),
                ),
                'i' => {
                    // `-i` is the one repeatable endpoint option: OpenSSH tries
                    // every identity it was given, in order. This crate has one
                    // slot, so a second, different key is not reproducible —
                    // re-emitting only the last one can authenticate as a
                    // different account on the same host, which is the exact
                    // outcome `unreproducible` exists to prevent.
                    if options
                        .identity
                        .as_deref()
                        .is_some_and(|first| first != value)
                    {
                        options.unreproducible = true;
                    }
                    capture_option(
                        &mut options.identity,
                        &mut options.unreproducible,
                        value,
                        field_is_safe(value, FILE_PATH_EXTRA),
                    );
                }
                'F' => capture_first_option(
                    &mut options.config,
                    &mut config_seen,
                    &mut options.unreproducible,
                    value,
                    field_is_safe(value, FILE_PATH_EXTRA),
                ),
                'o' => options.unreproducible |= ssh_option_selects_endpoint(value),
                // `-W host:port` forwards stdio to a third host instead of
                // opening a shell on the destination, so `ssh destination`
                // would be a different session entirely.
                'W' => options.unreproducible = true,
                // Everything else (ciphers, forwards, log files, control
                // sockets) leaves the endpoint where it was.
                _ => {}
            }
        }
        i += if short.consumed_next { 2 } else { 1 };
    }
    let raw = target?;
    let (target_user, host) = match raw.split_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (Some(u), h.to_string()),
        _ => (None, raw.to_string()),
    };
    let user = flag_user.or(target_user).map(str::to_string);
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
    Some(RemoteContext::Ssh {
        host,
        user,
        options,
    })
}

/// OpenSSH's complete set of value-taking single-char options (the ssh(1)
/// synopsis). Every other option letter is a boolean and bundles freely, so
/// `-vp 2222` is `-v` followed by `-p 2222`.
///
/// Getting this set wrong misreads the destination in both directions: a
/// missing letter leaves an option VALUE looking like the host (this is how
/// `-J jump` used to reconnect to the bastion), while a spurious one eats the
/// host as a value. `P` is `-P tag`, not the long-removed boolean.
const SSH_VALUE_OPTIONS: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// A TCP port as `-p` accepts it.
fn ssh_port_is_safe(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.chars().all(|c| c.is_ascii_digit())
}

/// Does this `-o Keyword=value` decide where the session lands?
///
/// `-o` opens the whole ssh_config grammar on the command line, and a handful
/// of keywords replace the destination outright (`HostName`), tunnel it
/// through another machine (`ProxyCommand`, `ProxyJump`), rewrite the name
/// before it is looked up (`CanonicalizeHostname`), move the port or login, or
/// pick the credential that decides which account the host authenticates
/// (`IdentityFile`, `CertificateFile`, `IdentityAgent` — the `-o` spellings of
/// the same choice `-i` makes). None of them is re-emitted: a `ProxyCommand`
/// value is itself a shell command, and the ones that do have a flag
/// equivalent (`Port`, `User`, `IdentityFile`) resolve against `-p`/`-l`/`-i`
/// by rules subtle enough that guessing is worse than offering nothing. So the
/// Reconnect entry is dropped instead. Keywords that only tune an existing
/// connection (`StrictHostKeyChecking`, `ServerAliveInterval`, …) are ignored,
/// as they always were.
fn ssh_option_selects_endpoint(option: &str) -> bool {
    // ssh accepts both `-o Keyword=value` and the quoted `-o "Keyword value"`,
    // and OpenSSH's own config reader (`process_config_line`) skips leading
    // whitespace before the keyword — so `-o " ProxyJump=bastion"` is honoured
    // by ssh and must be honoured here too. Splitting without the trim yielded
    // an empty keyword, which matched nothing and let the bastion be dropped.
    let keyword = option
        .trim_start_matches([' ', '\t'])
        .split(['=', ' ', '\t'])
        .next()
        .unwrap_or(option)
        .to_ascii_lowercase();
    matches!(
        keyword.as_str(),
        "hostname"
            | "port"
            | "user"
            | "proxycommand"
            | "proxyjump"
            | "hostkeyalias"
            | "bindaddress"
            | "bindinterface"
            | "remotecommand"
            | "include"
            | "identityfile"
            | "certificatefile"
            | "identityagent"
            | "canonicalizehostname"
            | "canonicaldomains"
    )
}

/// Phase 4 of [`TERMINATOR-REMOTE-DESIGN.md`](
/// ../../../docs/TERMINATOR-REMOTE-DESIGN.md): Container-session
/// detector. Recognizes the four common container-exec argv shapes:
///
///   - `docker exec [-it] <container> <cmd> [args …]`
///   - `podman exec [-it] <container> <cmd> [args …]`
///   - `kubectl exec [-it] <pod> -- <cmd> [args …]`
///     (also `kubectl exec [-it] -n ns <pod> [-c container] -- <cmd>`)
///   - `lxc-attach [-n|--name] <container>`
///
/// The container token is the first non-option argv element after
/// the `exec` / `attach` subcommand (skipping `-flag value` pairs), and the
/// options that name the daemon, cluster, or namespace it lives in are
/// carried in [`ContainerOptions`] — the name alone would reconnect against
/// whatever the client's defaults happen to be.
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
    // The client-side context travels with the name: `web` on the local daemon
    // and `web` under `--context remote` are containers on different machines.
    let mut parse = ContainerParse::default();
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
        loop {
            let a = argv.get(i)?;
            if let Some(flag) = a.strip_prefix("--") {
                if flag.is_empty() {
                    // Bare "--" before the subcommand isn't meaningful for
                    // any of these CLIs; skip rather than treat as positional.
                    i += 1;
                    continue;
                }
                i += apply_long_option(
                    flag,
                    argv.get(i + 1).map(String::as_str),
                    |name| container_global_option(runtime, name),
                    &mut parse,
                );
                continue;
            }
            if let Some(bundle) = a.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                i += apply_short_option(
                    bundle,
                    argv.get(i + 1).map(String::as_str),
                    |name| container_global_option(runtime, name),
                    &mut parse,
                );
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
    // Walk the subcommand's own options. `-it` is a boolean bundle, `-n ns` is
    // kubectl's namespace, `-u user` is a uid inside the container: whichever
    // of those names part of the ENDPOINT is captured, the rest are consumed
    // so their values never pass for the container itself.
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            // What follows `--` is NOT the same thing for every CLI, and
            // guessing costs an endpoint:
            //
            // - docker/podman: `--` only ends flag parsing. The positionals
            //   after it are still `CONTAINER COMMAND …`, so `docker exec --
            //   web sh` really does name `web` — keep walking.
            // - kubectl: `exec` reads cobra's `ArgsLenAtDash`, and when no
            //   positional preceded the `--` it takes EVERYTHING after it as
            //   the command and the pod from `-f`/stdin instead. `kubectl exec
            //   -f pod.yaml -- sh` runs `sh` in the manifest's pod; reading
            //   past the `--` made the COMMAND the pod, so the menu offered to
            //   re-attach to a container called `sh`.
            // - either CLI once the container is known, or once a flag has
            //   said the name lives outside the argv: the rest is the command
            //   the session was running, never an option this walk should read.
            if parse.container.is_some()
                || parse.implicit_name
                || runtime == ContainerRuntime::Kubectl
            {
                break;
            }
            i += 1;
            continue;
        }
        if let Some(flag) = a.strip_prefix("--") {
            i += apply_long_option(
                flag,
                argv.get(i + 1).map(String::as_str),
                |name| container_exec_option(runtime, name),
                &mut parse,
            );
            continue;
        }
        if let Some(bundle) = a.strip_prefix('-').filter(|rest| !rest.is_empty()) {
            i += apply_short_option(
                bundle,
                argv.get(i + 1).map(String::as_str),
                |name| container_exec_option(runtime, name),
                &mut parse,
            );
            continue;
        }
        // First non-option positional. For Lxc the lxc-attach form is
        // `lxc-attach -n name`; without `-n` the first positional IS the name.
        // `implicit_name` means an earlier flag already claimed the container
        // slot from outside the argv (`podman exec --latest bash`), so this
        // token is the command, not the name.
        if parse.container.is_none() && !parse.implicit_name {
            parse.container = Some(a.clone());
        } else {
            break;
        }
        // kubectl keeps parsing its own flags past the pod
        // (`kubectl exec pod -c sidecar -- sh`); for the others the token
        // after the container starts the command.
        if runtime != ContainerRuntime::Kubectl {
            break;
        }
        i += 1;
    }
    // The container was named by a manifest file or by "whichever ran last":
    // there is no name in this argv, and inventing one from the command that
    // followed is precisely the defect this walk exists to avoid. A positional
    // alongside such a flag (`kubectl exec pod -f pod.yaml`) is contradictory
    // enough that the CLIs disagree about which wins, so that shape fails
    // closed here too — an absent menu entry, never a guessed one.
    if parse.implicit_name {
        return None;
    }
    // H1 (audit v2.32.0, SECURITY): reject a container token that carries a
    // control char or escapes the conservative charset, so it can never
    // become a RemoteContext whose Reconnect command the caller auto-execs.
    // (clone_session_command additionally single-quotes — layer 2.)
    let container = parse.container?;
    if !field_is_safe(&container, CONTAINER_EXTRA) {
        return None;
    }
    Some(RemoteContext::Container {
        runtime,
        container,
        options: parse.options,
    })
}

/// Everything [`detect_container`]'s argv walk accumulates.
///
/// Bundled into one accumulator because the option appliers below have to be
/// able to touch all three: an option can name the endpoint, name the
/// container, or say that the container is named somewhere this detector
/// cannot look.
#[derive(Default)]
struct ContainerParse {
    /// The client-side context that decides which daemon/cluster/namespace the
    /// name resolves in.
    options: ContainerOptions,
    /// The container name — from `lxc-attach -n`/`--name`, or from the first
    /// positional after the subcommand.
    container: Option<String>,
    /// The invocation names its container through something outside the argv:
    /// `kubectl exec -f pod.yaml` reads the pod out of a manifest, and `podman
    /// exec --latest`/`-l` means "whichever container ran last". Both make
    /// every remaining positional part of the COMMAND, so no token here is the
    /// name — and with no name there is nothing to title or reconnect to, so
    /// detection yields `None` rather than a plausible-looking wrong answer.
    implicit_name: bool,
}

/// Where one recognized container-CLI option lands in [`ContainerOptions`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerOptionSlot {
    Context,
    Endpoint,
    Namespace,
    Config,
    PodContainer,
    /// The container name itself (`lxc-attach -n`/`--name`).
    Name,
    /// The container is named outside the argv — `kubectl exec -f pod.yaml`
    /// (inside the manifest) or `podman exec --latest` (whichever ran last).
    /// See [`ContainerParse::implicit_name`].
    ImplicitName,
    /// Endpoint-selecting, but never reproduced: a credential kettle must not
    /// echo into a command line, or a selector with no single-flag equivalent.
    Unreproducible,
    /// Recognized so that its value is consumed, but it leaves the endpoint
    /// alone (`--user`, `--workdir`, `--log-level`, …).
    Ignored,
}

/// Global options — those before the `exec` subcommand — that decide which
/// daemon or cluster the client talks to. Returns the slot plus whether the
/// option consumes a separate value; `None` means unrecognized, which skips
/// the token without consuming a value (the conservative default this walk has
/// always used for booleans).
fn container_global_option(
    runtime: ContainerRuntime,
    name: &str,
) -> Option<(ContainerOptionSlot, bool)> {
    use ContainerOptionSlot as Slot;
    use ContainerRuntime as Runtime;
    Some(match (runtime, name) {
        // Docker and Podman spell "which daemon" differently, and `-c` means
        // a different flag in each.
        (Runtime::Docker, "context" | "c") => (Slot::Context, true),
        (Runtime::Podman, "connection" | "c") => (Slot::Context, true),
        (Runtime::Docker, "host" | "H") => (Slot::Endpoint, true),
        (Runtime::Podman, "url") => (Slot::Endpoint, true),
        // The config dir holds Docker's current context, so it selects the
        // daemon just as `--context` does.
        (Runtime::Docker, "config") => (Slot::Config, true),
        // `--remote`, and its documented `-r` alias, switch Podman to its
        // default remote service and carry no value, so there is nothing to
        // re-emit. Recognizing only the long spelling left `podman -r exec web`
        // reconnecting to the LOCAL socket.
        (Runtime::Podman, "remote" | "r") => (Slot::Unreproducible, false),
        // kubectl: the cluster, the namespace, and the file that resolves both.
        (Runtime::Kubectl, "context") => (Slot::Context, true),
        (Runtime::Kubectl, "server" | "s") => (Slot::Endpoint, true),
        (Runtime::Kubectl, "namespace" | "n") => (Slot::Namespace, true),
        (Runtime::Kubectl, "kubeconfig") => (Slot::Config, true),
        // `--cluster` names an API server inside the kubeconfig with no
        // one-flag equivalent, and a token or password must never be written
        // back out into a command line.
        (Runtime::Kubectl, "cluster" | "token" | "password") => (Slot::Unreproducible, true),
        // Identity and credential selectors: these decide WHO the client
        // authenticates as, and therefore which cluster/daemon the same name
        // resolves against. Consuming them silently — which is what `Ignored`
        // did — meant `kubectl --user prod-admin exec api-0` came back as a
        // plain `kubectl exec api-0`, run against the DEFAULT kubeconfig user,
        // and `docker --tlscacert … -H tcp://host:2376 exec web` came back
        // without any of the TLS material the endpoint needs. None of them is
        // re-emittable (a secret must never be echoed into a command line, and
        // the rest have no single-flag equivalent that resolves the same way),
        // so they suppress the Reconnect entry the way `--token` already did.
        //
        // NB: this is the GLOBAL `--user` — the kubeconfig user. The `--user`
        // AFTER `exec` is a uid inside the container and stays ignorable.
        (
            _,
            "user"
            | "username"
            | "as"
            | "as-group"
            | "certificate-authority"
            | "client-certificate"
            | "client-key"
            | "tlscacert"
            | "tlscert"
            | "tlskey"
            | "identity",
        ) => (Slot::Unreproducible, true),
        // A flag whose very NAME says endpoint, on a runtime whose own arm
        // above did not claim it. There is no mapping to re-emit and no reason
        // to believe the default is the same place, so fail closed instead of
        // consuming it and reconnecting to the local daemon.
        (
            _,
            "host" | "context" | "config" | "namespace" | "connection" | "url" | "server"
            | "kubeconfig" | "cluster",
        ) => (Slot::Unreproducible, true),
        // Recognized only so their values are consumed instead of being
        // mistaken for the subcommand. None of them moves the endpoint.
        (
            _,
            "log-level" | "request-timeout" | "cache-dir" | "root" | "runroot" | "storage-driver"
            | "storage-opt" | "tmpdir" | "runtime" | "l",
        ) => (Slot::Ignored, true),
        _ => return None,
    })
}

/// Options after the `exec` / `attach` subcommand. Same contract as
/// [`container_global_option`].
fn container_exec_option(
    runtime: ContainerRuntime,
    name: &str,
) -> Option<(ContainerOptionSlot, bool)> {
    use ContainerOptionSlot as Slot;
    use ContainerRuntime as Runtime;
    Some(match (runtime, name) {
        // `lxc-attach -n NAME` / `--name NAME` IS the container.
        (Runtime::Lxc, "name" | "n") => (Slot::Name, true),
        // A different container root holds a different set of containers.
        (Runtime::Lxc, "lxcpath" | "P") => (Slot::Config, true),
        // `-e` is `--elevated-privileges` here, a boolean — unlike the `-e`
        // that sets an environment variable for docker/podman.
        (Runtime::Lxc, "e") => (Slot::Ignored, false),
        // lxc-attach's remaining value-taking options: `-u`/`--uid` and
        // `-g`/`--gid` are its own, and `-o`/`--logfile` + `-l`/`--logpriority`
        // come from the option set every lxc-* tool shares. A name missing
        // here is read as a boolean, which leaves its VALUE looking like a
        // positional — `lxc-attach --uid 1000 -n web` reported the container as
        // `1000` and offered to re-attach to it.
        (Runtime::Lxc, "uid" | "gid" | "u" | "g" | "logfile" | "o" | "logpriority" | "l") => {
            (Slot::Ignored, true)
        }
        // `kubectl exec -f pod.yaml -- sh` takes the pod from a manifest this
        // detector cannot open, and `podman exec --latest`/`-l` takes it from
        // "whichever container ran last". Neither leaves a name in the argv,
        // and both make every following positional part of the COMMAND.
        (Runtime::Kubectl, "filename" | "f") => (Slot::ImplicitName, true),
        (Runtime::Podman, "latest" | "l") => (Slot::ImplicitName, false),
        // A pod is not one shell: `-c`/`--container` picks which of its
        // containers the session entered.
        (Runtime::Kubectl, "container" | "c") => (Slot::PodContainer, true),
        // kubectl's cluster-selecting flags are accepted after the subcommand
        // as well as before it.
        (Runtime::Kubectl, "namespace" | "n") => (Slot::Namespace, true),
        (Runtime::Kubectl, "context") => (Slot::Context, true),
        (Runtime::Kubectl, "kubeconfig") => (Slot::Config, true),
        (Runtime::Kubectl, "server" | "s") => (Slot::Endpoint, true),
        // Value-taking options that name something INSIDE the container — a
        // uid, an env file, a working directory. Consumed, never captured: a
        // missing entry here is what made `docker exec --env-file vars web`
        // report `vars` as the container.
        (
            _,
            "env"
            | "env-file"
            | "user"
            | "workdir"
            | "detach-keys"
            | "cidfile"
            | "name"
            | "context"
            | "kubeconfig"
            | "namespace"
            | "filename"
            | "pod-running-timeout"
            | "preserve-fds"
            | "arch"
            | "namespaces"
            | "set-var"
            | "keep-var"
            | "rcfile"
            | "logfile"
            | "logpriority"
            | "e"
            | "u"
            | "w"
            | "c"
            | "n"
            | "f"
            | "a"
            | "s"
            | "v"
            | "l"
            | "L",
        ) => (Slot::Ignored, true),
        _ => return None,
    })
}

/// Store one recognized option's value in the slot it belongs to, validating
/// it against the charset that slot can safely re-emit.
fn apply_container_option(slot: ContainerOptionSlot, value: &str, parse: &mut ContainerParse) {
    let options = &mut parse.options;
    let unreproducible = &mut options.unreproducible;
    match slot {
        ContainerOptionSlot::Context => capture_option(
            &mut options.context,
            unreproducible,
            value,
            field_is_safe(value, CONTAINER_EXTRA),
        ),
        ContainerOptionSlot::Endpoint => capture_option(
            &mut options.endpoint,
            unreproducible,
            value,
            field_is_safe(value, CONTAINER_ENDPOINT_EXTRA),
        ),
        ContainerOptionSlot::Namespace => capture_option(
            &mut options.namespace,
            unreproducible,
            value,
            field_is_safe(value, CONTAINER_EXTRA),
        ),
        ContainerOptionSlot::Config => capture_option(
            &mut options.config,
            unreproducible,
            value,
            field_is_safe(value, FILE_PATH_EXTRA),
        ),
        ContainerOptionSlot::PodContainer => capture_option(
            &mut options.pod_container,
            unreproducible,
            value,
            field_is_safe(value, CONTAINER_EXTRA),
        ),
        // Validated with the container name itself once parsing finishes.
        ContainerOptionSlot::Name => parse.container = Some(value.to_string()),
        // The value is the manifest that holds the name (`-f pod.yaml`), not
        // the name: consumed, and recorded as "no name in this argv".
        ContainerOptionSlot::ImplicitName => parse.implicit_name = true,
        ContainerOptionSlot::Unreproducible => *unreproducible = true,
        ContainerOptionSlot::Ignored => {}
    }
}

/// Apply one recognized option that carries NO value. Most are inert, but two
/// slots mean something on their own: an endpoint switch with nothing to
/// re-emit (`podman --remote`/`-r`) and an implicit container name (`podman
/// exec --latest`/`-l`). Reading only value-taking letters dropped both.
fn apply_boolean_container_option(slot: ContainerOptionSlot, parse: &mut ContainerParse) {
    match slot {
        ContainerOptionSlot::Unreproducible => parse.options.unreproducible = true,
        ContainerOptionSlot::ImplicitName => parse.implicit_name = true,
        _ => {}
    }
}

/// Apply one `--name` / `--name=value` token, returning how many argv tokens
/// it consumed.
fn apply_long_option(
    flag: &str,
    next: Option<&str>,
    lookup: impl Fn(&str) -> Option<(ContainerOptionSlot, bool)>,
    parse: &mut ContainerParse,
) -> usize {
    let (name, joined) = match flag.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (flag, None),
    };
    // An unrecognized long flag is assumed boolean — most exec flags are
    // (`--privileged`, `--tty`), and consuming a value would swallow the
    // container name.
    let (slot, takes_value) = lookup(name).unwrap_or((ContainerOptionSlot::Ignored, false));
    let (value, consumed) = match (joined, takes_value) {
        (Some(value), _) => (Some(value), 1),
        (None, true) => match next {
            Some(value) => (Some(value), 2),
            None => (None, 1),
        },
        (None, false) => (None, 1),
    };
    match value {
        Some(value) => apply_container_option(slot, value, parse),
        // The boolean form of an endpoint-selecting flag (`podman --remote`)
        // or of an implicit name (`podman exec --latest`).
        None => apply_boolean_container_option(slot, parse),
    }
    consumed
}

/// Apply one `-abc`-style token, returning how many argv tokens it consumed.
fn apply_short_option(
    bundle: &str,
    next: Option<&str>,
    lookup: impl Fn(&str) -> Option<(ContainerOptionSlot, bool)>,
    parse: &mut ContainerParse,
) -> usize {
    let mut probe = [0_u8; 4];
    let short = parse_short_bundle(bundle, next, |flag| {
        lookup(flag.encode_utf8(&mut probe)).is_some_and(|(_, takes_value)| takes_value)
    });
    let mut found = [0_u8; 4];
    // The letters BEFORE the value-taking one are not all inert: `podman -r`
    // and `podman exec -l` each say something about the endpoint with no value
    // attached, and both were silently skipped while only their long spellings
    // worked.
    for flag in short.booleans.chars() {
        if let Some((slot, false)) = lookup(flag.encode_utf8(&mut found)) {
            apply_boolean_container_option(slot, parse);
        }
    }
    if let (Some(flag), Some(value)) = (short.flag, short.value)
        && let Some((slot, _)) = lookup(flag.encode_utf8(&mut found))
    {
        apply_container_option(slot, value, parse);
    }
    if short.consumed_next { 2 } else { 1 }
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
/// The endpoint-selecting options detected alongside the host/container are
/// re-emitted too — `ssh -p '2222' -J 'bastion' 'box'`, `docker --context
/// 'remote' exec -it 'web' $SHELL` — because the bare name reaches a different
/// service. When the original session used an option this crate cannot
/// reproduce (`SshOptions::unreproducible` / `ContainerOptions::unreproducible`)
/// the answer is `None`: no Reconnect at all beats a Reconnect somewhere else.
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
        RemoteContext::Ssh {
            host,
            user,
            options,
        } => {
            if options.unreproducible || has_control_char(host) {
                return None;
            }
            let mut cmd = String::from("ssh");
            // Fixed order (config, identity, jump, port) so the emitted line is
            // deterministic regardless of how the user happened to type it.
            for (flag, value) in [
                ("-F", &options.config),
                ("-i", &options.identity),
                ("-J", &options.jump),
                ("-p", &options.port),
            ] {
                if let Some(value) = value {
                    push_quoted_option(&mut cmd, flag, value)?;
                }
            }
            cmd.push(' ');
            if let Some(u) = user {
                if has_control_char(u) {
                    return None;
                }
                cmd.push_str(&shell_single_quote(u));
                cmd.push('@');
            }
            cmd.push_str(&shell_single_quote(host));
            Some(cmd)
        }
        RemoteContext::Container {
            runtime,
            container,
            options,
        } => {
            if options.unreproducible || has_control_char(container) {
                return None;
            }
            let mut cmd = String::from(match runtime {
                ContainerRuntime::Docker => "docker",
                ContainerRuntime::Podman => "podman",
                ContainerRuntime::Kubectl => "kubectl",
                ContainerRuntime::Lxc => "lxc-attach",
            });
            // Each runtime spells the same concept differently, so re-emit the
            // flag this runtime understands rather than the one that was typed.
            let globals: &[(&str, &Option<String>)] = match runtime {
                ContainerRuntime::Docker => &[
                    ("--config", &options.config),
                    ("--context", &options.context),
                    ("--host", &options.endpoint),
                ],
                ContainerRuntime::Podman => &[
                    ("--connection", &options.context),
                    ("--url", &options.endpoint),
                ],
                ContainerRuntime::Kubectl => &[
                    ("--kubeconfig", &options.config),
                    ("--context", &options.context),
                    ("--server", &options.endpoint),
                    ("--namespace", &options.namespace),
                ],
                ContainerRuntime::Lxc => &[("--lxcpath", &options.config)],
            };
            for (flag, value) in globals {
                if let Some(value) = value {
                    push_quoted_option(&mut cmd, flag, value)?;
                }
            }
            let c = shell_single_quote(container);
            match runtime {
                ContainerRuntime::Docker | ContainerRuntime::Podman => {
                    cmd.push_str(&format!(" exec -it {c} $SHELL"));
                }
                ContainerRuntime::Kubectl => {
                    cmd.push_str(&format!(" exec -it {c}"));
                    if let Some(value) = &options.pod_container {
                        push_quoted_option(&mut cmd, "-c", value)?;
                    }
                    cmd.push_str(" -- $SHELL");
                }
                ContainerRuntime::Lxc => cmd.push_str(&format!(" -n {c}")),
            }
            Some(cmd)
        }
    }
}

/// Append ` FLAG 'VALUE'` to a command line being built, or `None` when the
/// value carries a control char — the caller propagates that with `?`, dropping
/// the whole Reconnect entry rather than emitting a line that a newline could
/// split into extra auto-executed commands (same contract as the host/user/
/// container fields in [`clone_session_command`]).
fn push_quoted_option(cmd: &mut String, flag: &str, value: &str) -> Option<()> {
    if has_control_char(value) {
        return None;
    }
    cmd.push(' ');
    cmd.push_str(flag);
    cmd.push(' ');
    cmd.push_str(&shell_single_quote(value));
    Some(())
}

/// Short user-friendly label for the right-click menu
/// entry that reconnects to a detected remote session. The
/// `ContextMenuItem::ConfigItem { label, command }` variant consumes
/// the pair `(clone_session_label(ctx), clone_session_command(ctx))`.
pub fn clone_session_label(ctx: &RemoteContext) -> String {
    match ctx {
        RemoteContext::Ssh { host, user, .. } => match user {
            Some(u) => format!("Reconnect ssh {u}@{host}"),
            None => format!("Reconnect ssh {host}"),
        },
        RemoteContext::Container {
            runtime, container, ..
        } => {
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
        RemoteContext::Ssh { host, user, .. } => match user {
            Some(u) => format!("ssh {u}@{host}"),
            None => format!("ssh {host}"),
        },
        RemoteContext::Container {
            runtime, container, ..
        } => {
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

    /// The `Ssh` context a plain `ssh [user@]host` argv must produce: host,
    /// login user, and NO connection options. Comparing against this asserts
    /// both halves — a stray `-p`/`-J`/`-i` capture or an `unreproducible`
    /// verdict fails the equality just as a wrong host would. Shapes that do
    /// carry options spell out `SshOptions` explicitly.
    fn ssh_ctx(host: &str, user: Option<&str>) -> RemoteContext {
        RemoteContext::Ssh {
            host: host.to_string(),
            user: user.map(str::to_string),
            options: SshOptions::default(),
        }
    }

    /// The `Container` context an `exec` against the client's default daemon
    /// must produce — no context / endpoint / namespace captured. Same
    /// two-sided assertion as [`ssh_ctx`].
    fn container_ctx(runtime: ContainerRuntime, container: &str) -> RemoteContext {
        RemoteContext::Container {
            runtime,
            container: container.to_string(),
            options: ContainerOptions::default(),
        }
    }

    #[test]
    fn proc_parsers_are_bounded_to_valid_pids_and_preserve_lossy_argv() {
        assert_eq!(
            parse_proc_children(b"12 34\ninvalid 4294967296 56").collect::<Vec<_>>(),
            [12, 34, 56]
        );
        let parsed = parse_proc_argv(b"ssh\0alice@host\0bad-\xff\0\0");
        assert_eq!(parsed.argv, ["ssh", "alice@host", "bad-\u{fffd}"]);
        assert!(parsed.complete);

        let exact_arg_count = [b'x', 0]
            .into_iter()
            .cycle()
            .take(MAX_PROC_ARGS_PER_PROCESS * 2)
            .collect::<Vec<_>>();
        let parsed = parse_proc_argv(&exact_arg_count);
        assert_eq!(parsed.argv.len(), MAX_PROC_ARGS_PER_PROCESS);
        assert!(parsed.complete, "an exact count-boundary EOF is complete");

        let one_extra_arg = [b'x', 0]
            .into_iter()
            .cycle()
            .take((MAX_PROC_ARGS_PER_PROCESS + 1) * 2)
            .collect::<Vec<_>>();
        let parsed = parse_proc_argv(&one_extra_arg);
        assert_eq!(parsed.argv.len(), MAX_PROC_ARGS_PER_PROCESS);
        assert!(!parsed.complete, "a dropped argument must mark truncation");

        let exact_decoded = vec![b'x'; MAX_PROC_ARG_DECODED_BYTES];
        let parsed = parse_proc_argv(&exact_decoded);
        assert_eq!(parsed.argv.len(), 1);
        assert!(parsed.complete, "an exact byte-boundary EOF is complete");
        let oversized_decoded = vec![b'x'; MAX_PROC_ARG_DECODED_BYTES + 1];
        let parsed = parse_proc_argv(&oversized_decoded);
        assert!(parsed.argv.is_empty());
        assert!(!parsed.complete);
    }

    #[test]
    fn proc_argv_limit_reports_a_destination_in_argument_257_as_truncated() {
        let mut cmdline = b"ssh\0".to_vec();
        for _ in 0..255 {
            cmdline.extend_from_slice(b"-n\0");
        }
        cmdline.extend_from_slice(b"host.example\0");

        let parsed = parse_proc_argv(&cmdline);
        assert_eq!(parsed.argv.len(), MAX_PROC_ARGS_PER_PROCESS);
        assert!(!parsed.complete);
        assert!(!parsed.argv.iter().any(|arg| arg == "host.example"));
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
        assert!(tree.refresh_from(&root, &[10]));
        let index = build_children_index(&tree);
        assert_eq!(tree.all_pids().len(), 2);
        assert!(!tree.all_pids().contains(&99));
        assert_eq!(tree.parent_of(20), Some(10));
        assert_eq!(tree.cwd_of(20).as_deref(), Some("/tmp"));
        std::fs::remove_file(root.join("20").join("cwd")).unwrap();
        symlink("/var", root.join("20").join("cwd")).unwrap();
        assert_eq!(
            tree.cwd_of(20).as_deref(),
            Some("/var"),
            "cwd is read only for the chosen foreground pid and stays live between refreshes"
        );
        assert_eq!(
            detect_root_in_index(10, &tree, &index),
            Some(ssh_ctx("box.example", Some("alice")))
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_rejects_an_argv_truncated_before_the_ssh_destination() {
        let root = std::env::temp_dir().join(format!(
            "kettle-proc-tree-argv-truncation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let process_dir = root.join("10");
        std::fs::create_dir_all(process_dir.join("task/10")).unwrap();
        let mut cmdline = b"ssh\0".to_vec();
        for _ in 0..255 {
            cmdline.extend_from_slice(b"-n\0");
        }
        cmdline.extend_from_slice(b"host.example\0");
        std::fs::write(process_dir.join("cmdline"), cmdline).unwrap();
        std::fs::write(process_dir.join("task/10/children"), b"").unwrap();

        let mut tree = LinuxProcessTree::default();
        assert!(
            !tree.refresh_from(&root, &[10]),
            "argv parser truncation must prevent publishing the scan"
        );
        assert!(detect_in_tree(10, &mut tree).is_none());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_follows_children_owned_by_non_leader_tasks() {
        let root = std::env::temp_dir().join(format!(
            "kettle-proc-tree-thread-child-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root_dir = root.join("10");
        std::fs::create_dir_all(root_dir.join("task/10")).unwrap();
        std::fs::create_dir_all(root_dir.join("task/11")).unwrap();
        std::fs::write(root_dir.join("cmdline"), b"bash\0").unwrap();
        std::fs::write(root_dir.join("task/10/children"), b"").unwrap();
        std::fs::write(root_dir.join("task/11/children"), b"20\n").unwrap();
        let child_dir = root.join("20");
        std::fs::create_dir_all(child_dir.join("task/20")).unwrap();
        std::fs::write(child_dir.join("cmdline"), b"ssh\0threaded.example\0").unwrap();
        std::fs::write(child_dir.join("task/20/children"), b"").unwrap();

        let mut tree = LinuxProcessTree::default();
        assert!(tree.refresh_from(&root, &[10]));
        let index = build_children_index(&tree);
        assert_eq!(tree.parent_of(20), Some(10));
        assert_eq!(
            detect_root_in_index(10, &tree, &index),
            Some(ssh_ctx("threaded.example", None))
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_charges_oversized_files_to_the_aggregate_budget() {
        let root = std::env::temp_dir().join(format!(
            "kettle-proc-tree-oversize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scan_root = 10_u32;
        let root_dir = root.join(scan_root.to_string());
        std::fs::create_dir_all(root_dir.join("task").join(scan_root.to_string())).unwrap();
        let children = (20_u32..40)
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root_dir.join("cmdline"), b"bash\0").unwrap();
        std::fs::write(
            root_dir
                .join("task")
                .join(scan_root.to_string())
                .join("children"),
            children,
        )
        .unwrap();
        let oversized = vec![b'x'; MAX_PROC_FILE_BYTES as usize + 1];
        for pid in 20_u32..40 {
            let process_dir = root.join(pid.to_string());
            std::fs::create_dir_all(process_dir.join("task").join(pid.to_string())).unwrap();
            std::fs::write(process_dir.join("cmdline"), &oversized).unwrap();
            std::fs::write(
                process_dir
                    .join("task")
                    .join(pid.to_string())
                    .join("children"),
                b"",
            )
            .unwrap();
        }

        let mut tree = LinuxProcessTree::default();
        assert!(!tree.refresh_from(&root, &[scan_root]));
        assert_eq!(tree.bytes_read, MAX_PROC_TREE_TOTAL_BYTES);
        assert!(
            tree.task_files_read <= MAX_PROC_TASK_FILE_READS,
            "task-file reads must retain their independent operation bound"
        );
        assert!(
            (20_u32..40).all(|pid| tree.argv_of(pid).is_none()),
            "oversized argv must never be parsed or retained"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_marks_single_oversized_metadata_files_incomplete() {
        for oversized_children in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "kettle-proc-single-oversize-{}-{}-{}",
                std::process::id(),
                oversized_children,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let process_dir = root.join("10");
            std::fs::create_dir_all(process_dir.join("task/10")).unwrap();
            let oversized = vec![b'x'; MAX_PROC_FILE_BYTES as usize + 1];
            std::fs::write(
                process_dir.join("cmdline"),
                if oversized_children {
                    b"bash\0".as_slice()
                } else {
                    &oversized
                },
            )
            .unwrap();
            std::fs::write(
                process_dir.join("task/10/children"),
                if oversized_children {
                    &oversized
                } else {
                    b"".as_slice()
                },
            )
            .unwrap();

            let mut tree = LinuxProcessTree::default();
            assert!(
                !tree.refresh_from(&root, &[10]),
                "oversized {} must prevent publishing a partial scan",
                if oversized_children {
                    "children"
                } else {
                    "cmdline"
                }
            );

            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_scanner_does_not_publish_an_unreadable_task_topology() {
        let root = std::env::temp_dir().join(format!(
            "kettle-proc-bad-task-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let process_dir = root.join("10");
        std::fs::create_dir_all(&process_dir).unwrap();
        std::fs::write(process_dir.join("cmdline"), b"bash\0").unwrap();
        std::fs::write(process_dir.join("task"), b"not a directory").unwrap();

        let mut tree = LinuxProcessTree::default();
        assert!(!tree.refresh_from(&root, &[10]));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_task_reads_stop_at_the_operation_budget_even_for_empty_files() {
        let root = std::env::temp_dir().join(format!(
            "kettle-proc-task-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut total_bytes = 0_u64;
        let mut task_files_read = 0_usize;
        let mut scheduled = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

        for task_pid in 1..=MAX_PROC_TASK_FILE_READS as u32 {
            let (_, within_limits) = read_proc_task_children(
                &root,
                task_pid,
                1,
                &mut total_bytes,
                &mut task_files_read,
                &mut scheduled,
                &mut queue,
                deadline,
            );
            assert!(within_limits);
        }
        let (_, within_limits) = read_proc_task_children(
            &root,
            u32::MAX,
            1,
            &mut total_bytes,
            &mut task_files_read,
            &mut scheduled,
            &mut queue,
            deadline,
        );
        assert!(!within_limits);
        assert_eq!(task_files_read, MAX_PROC_TASK_FILE_READS);
        assert_eq!(total_bytes, 0);

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
    /// descendant's parent (structure survives the budget; only the large
    /// payloads are dropped). Cwd remains available through its on-demand
    /// procfs read.
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
        assert!(!tree.refresh_from(&root, &[scan_root]));

        // Structure survives for every descendant regardless of the aggregate
        // budget, and cwd remains available through an on-demand read.
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
            format_remote_title(&ssh_ctx("box.example.com", None)),
            "ssh box.example.com"
        );
        // SSH with user.
        assert_eq!(
            format_remote_title(&ssh_ctx("box", Some("me"))),
            "ssh me@box"
        );
        // Container — Docker.
        assert_eq!(
            format_remote_title(&container_ctx(ContainerRuntime::Docker, "ubuntu-2204")),
            "docker: ubuntu-2204"
        );
        // Container — Podman.
        assert_eq!(
            format_remote_title(&container_ctx(ContainerRuntime::Podman, "fedora")),
            "podman: fedora"
        );
        // Container — kubectl.
        assert_eq!(
            format_remote_title(&container_ctx(ContainerRuntime::Kubectl, "my-pod-deadbeef")),
            "kubectl: my-pod-deadbeef"
        );
        // Container — LXC.
        assert_eq!(
            format_remote_title(&container_ctx(ContainerRuntime::Lxc, "alpine")),
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
            clone_session_command(&ssh_ctx("box", None)),
            Some("ssh 'box'".to_string())
        );
        // SSH with user.
        assert_eq!(
            clone_session_command(&ssh_ctx("box", Some("me"))),
            Some("ssh 'me'@'box'".to_string())
        );
        // Docker.
        assert_eq!(
            clone_session_command(&container_ctx(ContainerRuntime::Docker, "ubuntu")),
            Some("docker exec -it 'ubuntu' $SHELL".to_string())
        );
        // Podman.
        assert_eq!(
            clone_session_command(&container_ctx(ContainerRuntime::Podman, "fedora")),
            Some("podman exec -it 'fedora' $SHELL".to_string())
        );
        // Kubectl (note the `--` separator).
        assert_eq!(
            clone_session_command(&container_ctx(ContainerRuntime::Kubectl, "my-pod")),
            Some("kubectl exec -it 'my-pod' -- $SHELL".to_string())
        );
        // LXC.
        assert_eq!(
            clone_session_command(&container_ctx(ContainerRuntime::Lxc, "alpine")),
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
        let cmd = clone_session_command(&ssh_ctx("h; rm -rf ~", None))
            .expect("no control char → Some, just quoted");
        // The metacharacters live entirely inside one quoted argument — there is
        // no UNQUOTED `;`/`$`/`(` that the shell could act on. (The exact-string
        // compare below pins this fully; the helper double-checks the property.)
        assert_eq!(cmd, "ssh 'h; rm -rf ~'");
        assert!(
            !has_unquoted_metachar(&cmd),
            "metachars must stay quoted: {cmd}"
        );

        let cmd = clone_session_command(&container_ctx(ContainerRuntime::Docker, "$(reboot)"))
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
        let cmd = clone_session_command(&ssh_ctx("a'b", None)).unwrap();
        assert_eq!(cmd, "ssh 'a'\\''b'");

        // A control char (newline) at build time → None (never a multi-line cmd).
        assert_eq!(clone_session_command(&ssh_ctx("h\nrm -rf ~", None)), None);
        assert_eq!(clone_session_command(&ssh_ctx("h", Some("u\nx"))), None);
        assert_eq!(
            clone_session_command(&container_ctx(ContainerRuntime::Kubectl, "p\nx")),
            None
        );
        // Whatever clone_session_command returns, it is always a single line.
        for ctx in [
            ssh_ctx("ok-host", Some("me")),
            container_ctx(ContainerRuntime::Podman, "ok_container"),
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
    /// end — both the remote title and the Reconnect command render `bob@h`.
    /// OpenSSH keeps that first user even if the later target spells another.
    #[test]
    fn ssh_dash_l_user_reaches_title_and_reconnect() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ctx = detect_ssh(&argv(&["ssh", "-l", "bob", "h"])).unwrap();
        assert_eq!(ctx, ssh_ctx("h", Some("bob")));
        assert_eq!(format_remote_title(&ctx), "ssh bob@h");
        assert_eq!(
            clone_session_command(&ctx),
            Some("ssh 'bob'@'h'".to_string())
        );
        assert_eq!(clone_session_label(&ctx), "Reconnect ssh bob@h");

        // The earlier -l wins over the later target's user@ component.
        let ctx = detect_ssh(&argv(&["ssh", "-l", "bob", "alice@h"])).unwrap();
        assert_eq!(ctx, ssh_ctx("h", Some("bob")));
        assert_eq!(format_remote_title(&ctx), "ssh bob@h");
    }

    /// Drift guard: `clone_session_label` is the menu
    /// label paired with `clone_session_command`.
    #[test]
    fn clone_session_label_for_all_shapes() {
        assert_eq!(
            clone_session_label(&ssh_ctx("box", Some("me"))),
            "Reconnect ssh me@box"
        );
        assert_eq!(
            clone_session_label(&ssh_ctx("box", None)),
            "Reconnect ssh box"
        );
        assert_eq!(
            clone_session_label(&container_ctx(ContainerRuntime::Docker, "foo")),
            "Re-attach docker foo"
        );
        assert_eq!(
            clone_session_label(&container_ctx(ContainerRuntime::Kubectl, "my-pod")),
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
            Some(container_ctx(ContainerRuntime::Docker, "alpine"))
        );
        // podman exec foo sh
        assert_eq!(
            detect_container(&argv(&["podman", "exec", "fedora", "sh"])),
            Some(container_ctx(ContainerRuntime::Podman, "fedora"))
        );
        // kubectl exec -it -n my-ns my-pod -- bash — the namespace is part of
        // the endpoint, so it is carried rather than merely skipped.
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "exec", "-it", "-n", "my-ns", "my-pod", "--", "bash"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
                options: ContainerOptions {
                    namespace: Some("my-ns".into()),
                    ..ContainerOptions::default()
                },
            })
        );
        // lxc-attach -n alpine
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "-n", "alpine"])),
            Some(container_ctx(ContainerRuntime::Lxc, "alpine"))
        );
        // lxc-attach alpine (no -n)
        assert_eq!(
            detect_container(&argv(&["lxc-attach", "alpine"])),
            Some(container_ctx(ContainerRuntime::Lxc, "alpine"))
        );
        // Absolute path.
        assert_eq!(
            detect_container(&argv(&["/usr/bin/docker", "exec", "foo"])),
            Some(container_ctx(ContainerRuntime::Docker, "foo"))
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
            Some(ssh_ctx("box.example.com", None))
        );
        // ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["ssh", "me@box"])),
            Some(ssh_ctx("box", Some("me")))
        );
        // ssh -p 22 user@host — the port is part of the endpoint, so it is
        // carried rather than merely skipped.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-p", "22", "alice@h.example"])),
            Some(RemoteContext::Ssh {
                host: "h.example".into(),
                user: Some("alice".into()),
                options: SshOptions {
                    port: Some("22".into()),
                    ..SshOptions::default()
                },
            })
        );
        // ssh -o StrictHostKeyChecking=no host — an `-o` that only tunes an
        // existing connection leaves the endpoint alone.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-o", "StrictHostKeyChecking=no", "h"])),
            Some(ssh_ctx("h", None))
        );
        // ssh -l user host — H2 (audit v2.32.0): `-l bob` now populates the user
        // so Reconnect / the remote title reproduce `ssh bob@h` (previously the
        // login user was silently dropped).
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "h"])),
            Some(ssh_ctx("h", Some("bob")))
        );
        // ssh -l bob alice@h — OpenSSH keeps the first obtained user, `bob`.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-l", "bob", "alice@h"])),
            Some(ssh_ctx("h", Some("bob")))
        );
        // sshpass -p secret ssh user@host
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-p", "secret", "ssh", "carol@h"])),
            Some(ssh_ctx("h", Some("carol")))
        );
        // Absolute-path argv[0].
        assert_eq!(
            detect_ssh(&argv(&["/usr/bin/ssh", "box"])),
            Some(ssh_ctx("box", None))
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
            Some(ssh_ctx("host", Some("alice")))
        );
        assert_eq!(
            detect_ssh(&argv(&["ssh.EXE", "box"])),
            Some(ssh_ctx("box", None))
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
            Some(container_ctx(ContainerRuntime::Docker, "alpine"))
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
            Some(ssh_ctx("h", Some("carol")))
        );
    }

    /// Each of these argv shapes used to drive the WRONG
    /// reconnect target/command.
    #[test]
    fn detect_handles_proxyjump_bool_flags_and_global_flags() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // (a) ssh -J jump host → host (was: 'jump', the bastion), with the
        // bastion carried so the reconnect still tunnels through it.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-J", "jump.example", "me@host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("me".into()),
                options: SshOptions {
                    jump: Some("jump.example".into()),
                    ..SshOptions::default()
                },
            })
        );
        // Joined -Jjump form is one token; the host still wins.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-Jjump.example", "host"])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: None,
                options: SshOptions {
                    jump: Some("jump.example".into()),
                    ..SshOptions::default()
                },
            })
        );
        // (b) docker exec --privileged <c> sh → c (was: 'sh').
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "--privileged", "alpine", "sh"])),
            Some(container_ctx(ContainerRuntime::Docker, "alpine"))
        );
        // A value-taking long flag still skips its value. `--user` names a
        // uid inside the container, not the endpoint, so nothing is carried.
        assert_eq!(
            detect_container(&argv(&["docker", "exec", "--user", "root", "alpine", "sh"])),
            Some(container_ctx(ContainerRuntime::Docker, "alpine"))
        );
        // (c) global flags before `exec` (kubectl -n ns exec pod) → pod, in
        // that namespace.
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "-n", "prod", "exec", "my-pod", "--", "sh"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "my-pod".into(),
                options: ContainerOptions {
                    namespace: Some("prod".into()),
                    ..ContainerOptions::default()
                },
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
                options: ContainerOptions {
                    context: Some("remote".into()),
                    ..ContainerOptions::default()
                },
            })
        );
    }

    #[test]
    fn ssh_scalar_options_keep_the_first_obtained_value() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            detect_ssh(&argv(&[
                "ssh",
                "-l",
                "bob",
                "-l",
                "carol",
                "-p",
                "2222",
                "-p22",
                "-J",
                "first-jump",
                "-Jsecond-jump",
                "-F",
                "/first/config",
                "-F/second/config",
                "alice@host"
            ])),
            Some(RemoteContext::Ssh {
                host: "host".into(),
                user: Some("bob".into()),
                options: SshOptions {
                    port: Some("2222".into()),
                    jump: Some("first-jump".into()),
                    config: Some("/first/config".into()),
                    ..SshOptions::default()
                },
            })
        );

        let unsafe_first = detect_ssh(&argv(&[
            "ssh",
            "-F",
            "/bad/$config",
            "-F",
            "/safe/config",
            "host",
        ]))
        .unwrap();
        let RemoteContext::Ssh { options, .. } = unsafe_first else {
            unreachable!();
        };
        assert!(options.unreproducible);
        assert_eq!(
            options.config, None,
            "a later value cannot replace the first"
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
            Some(container_ctx(ContainerRuntime::Docker, "exec"))
        );
    }

    /// The options that decide WHICH host `ssh` reached were parsed past and
    /// dropped, so Reconnect offered plain `ssh host` — a different service on
    /// a different port, reached without the bastion and authenticated by a
    /// different key. They now travel with the host and come back out.
    #[test]
    fn ssh_endpoint_options_are_reproduced_by_the_reconnect_command() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ctx = detect_ssh(&argv(&[
            "ssh",
            "-p",
            "2222",
            "-J",
            "bastion.example",
            "-i",
            "/home/me/.ssh/id_ed25519",
            "box",
        ]))
        .expect("a fully-specified ssh invocation is still an ssh session");
        assert_eq!(
            ctx,
            RemoteContext::Ssh {
                host: "box".into(),
                user: None,
                options: SshOptions {
                    port: Some("2222".into()),
                    jump: Some("bastion.example".into()),
                    identity: Some("/home/me/.ssh/id_ed25519".into()),
                    ..SshOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&ctx),
            Some(
                "ssh -i '/home/me/.ssh/id_ed25519' -J 'bastion.example' -p '2222' 'box'"
                    .to_string()
            )
        );

        // The joined spellings of the same options carry exactly as far.
        let joined = detect_ssh(&argv(&[
            "ssh",
            "-p2222",
            "-Jbastion.example",
            "-i/home/me/.ssh/id_ed25519",
            "-lroot",
            "box",
        ]))
        .expect("joined option forms are still an ssh session");
        assert_eq!(
            joined,
            RemoteContext::Ssh {
                host: "box".into(),
                user: Some("root".into()),
                options: SshOptions {
                    port: Some("2222".into()),
                    jump: Some("bastion.example".into()),
                    identity: Some("/home/me/.ssh/id_ed25519".into()),
                    ..SshOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&joined),
            Some(
                "ssh -i '/home/me/.ssh/id_ed25519' -J 'bastion.example' -p '2222' 'root'@'box'"
                    .to_string()
            )
        );

        // `-F` picks the config that resolves the alias, so the alias alone
        // does not name the same machine.
        let aliased = detect_ssh(&argv(&["ssh", "-F", "/etc/ssh/work.conf", "build"])).unwrap();
        assert_eq!(
            clone_session_command(&aliased),
            Some("ssh -F '/etc/ssh/work.conf' 'build'".to_string())
        );
    }

    /// ssh parses `-abc` the way getopt does, so a boolean letter in front of
    /// a value-taking one leaves the value in the same token or the next argv
    /// element. Reading the bundle as one opaque token made `ssh -vp 2222 box`
    /// report the PORT as the host.
    #[test]
    fn ssh_short_option_bundles_do_not_swallow_the_destination() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-vp", "2222", "box"])),
            Some(RemoteContext::Ssh {
                host: "box".into(),
                user: None,
                options: SshOptions {
                    port: Some("2222".into()),
                    ..SshOptions::default()
                },
            })
        );
        // A bundle of booleans still consumes nothing.
        assert_eq!(
            detect_ssh(&argv(&["ssh", "-tt", "box"])),
            Some(ssh_ctx("box", None))
        );
    }

    /// Some options cannot be re-emitted faithfully: an `-o ProxyCommand` is
    /// an arbitrary shell command, `-W` makes the session a stdio forward
    /// rather than a shell, and a path outside the reproducible charset cannot
    /// be quoted back with confidence. The pane still gets its remote title —
    /// only the Reconnect entry goes away, because a Reconnect that silently
    /// skipped the proxy would open a session on a different machine.
    #[test]
    fn ssh_options_that_cannot_be_reproduced_suppress_the_reconnect() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for unreproducible in [
            &["ssh", "-o", "ProxyCommand=nc -X5 proxy 1080 %h %p", "box"][..],
            &["ssh", "-o", "ProxyJump=bastion.example", "box"],
            &["ssh", "-o", "HostName=10.0.0.9", "box"],
            &["ssh", "-o", "Port=2222", "box"],
            &["ssh", "-W", "internal.example:22", "box"],
            // An identity path with a shell metacharacter is not reproduced.
            &["ssh", "-i", "/keys/$(id -u)", "box"],
        ] {
            let ctx = detect_ssh(&argv(unreproducible))
                .unwrap_or_else(|| panic!("{unreproducible:?} is still an ssh session"));
            assert_eq!(
                format_remote_title(&ctx),
                "ssh box",
                "the pane must still be labelled as remote: {unreproducible:?}"
            );
            assert_eq!(
                clone_session_command(&ctx),
                None,
                "no reconnect beats one that lands elsewhere: {unreproducible:?}"
            );
        }

        // Positive controls, in the same shape as the loop above: suppression
        // has to be selective, or "no reconnect beats a wrong one" would be
        // satisfied by never offering one at all.
        for (reproducible, expected) in [
            (
                &["ssh", "-o", "ServerAliveInterval=30", "box"][..],
                "ssh 'box'",
            ),
            (
                &["ssh", "-o", "StrictHostKeyChecking=no", "box"],
                "ssh 'box'",
            ),
            (&["ssh", "-p", "2222", "box"], "ssh -p '2222' 'box'"),
            (
                &["ssh", "-J", "bastion.example", "box"],
                "ssh -J 'bastion.example' 'box'",
            ),
            (
                &["ssh", "-i", "/keys/id_ed25519", "box"],
                "ssh -i '/keys/id_ed25519' 'box'",
            ),
            (&["ssh", "-L", "8080:localhost:80", "box"], "ssh 'box'"),
        ] {
            let ctx = detect_ssh(&argv(reproducible))
                .unwrap_or_else(|| panic!("{reproducible:?} is still an ssh session"));
            assert_eq!(
                clone_session_command(&ctx),
                Some(expected.to_string()),
                "{reproducible:?} names one endpoint this crate can rebuild"
            );
        }
    }

    /// `-o` is the whole ssh_config grammar on the command line, so the gate
    /// that decides whether a keyword moves the endpoint has to read keywords
    /// the way OpenSSH does. It missed the `-o` spellings of choices this crate
    /// already treats as endpoint-defining — `IdentityFile` is `-i` under
    /// another name, and `CanonicalizeHostname` rewrites the destination before
    /// it is looked up — and it read the keyword without skipping the leading
    /// whitespace `process_config_line` skips, so `-o " ProxyJump=bastion"`
    /// yielded an EMPTY keyword, matched nothing, and offered a reconnect that
    /// bypassed the bastion.
    #[test]
    fn ssh_dash_o_keywords_are_read_the_way_openssh_reads_them() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for suppressed in [
            "IdentityFile=/keys/other_ed25519",
            "CertificateFile=/keys/other-cert.pub",
            "IdentityAgent=/run/other-agent.sock",
            "CanonicalizeHostname=yes",
            "CanonicalDomains=example.com",
            // Leading whitespace, in both the `=` and the space spelling.
            " ProxyJump=bastion.example",
            "\tProxyJump=bastion.example",
            "  HostName=10.0.0.9",
            " HostName 10.0.0.9",
            "ProxyJump bastion.example",
        ] {
            let ctx = detect_ssh(&argv(&["ssh", "-o", suppressed, "box"]))
                .unwrap_or_else(|| panic!("`-o {suppressed}` is still an ssh session"));
            assert_eq!(format_remote_title(&ctx), "ssh box");
            assert_eq!(
                clone_session_command(&ctx),
                None,
                "`-o {suppressed}` reaches an endpoint this crate cannot rebuild"
            );
        }
        // Keywords that only tune the connection keep Reconnect, leading
        // whitespace and all.
        for kept in [
            "ServerAliveInterval=30",
            " StrictHostKeyChecking=no",
            "\tCompression yes",
        ] {
            let ctx = detect_ssh(&argv(&["ssh", "-o", kept, "box"])).unwrap();
            assert_eq!(
                clone_session_command(&ctx),
                Some("ssh 'box'".to_string()),
                "`-o {kept}` leaves the endpoint where it was"
            );
        }
    }

    /// A Windows install path is `C:\Program Files (x86)\…` and a POSIX home
    /// can hold an apostrophe. Both are ordinary paths, and both are inert in
    /// the POSIX single quotes the value is emitted in — `'` via the `'\''`
    /// idiom — so rejecting them at parse time deleted a Reconnect entry that
    /// used to work and bought nothing.
    #[test]
    fn ordinary_windows_and_posix_paths_keep_the_reconnect_entry() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let windows = detect_ssh(&argv(&[
            "ssh",
            "-i",
            r"C:\Program Files (x86)\OpenSSH\id_ed25519",
            "box",
        ]))
        .expect("a Windows identity path is still an ssh session");
        let cmd = clone_session_command(&windows)
            .expect("an ordinary Windows path keeps the reconnect entry");
        assert_eq!(
            cmd,
            r"ssh -i 'C:\Program Files (x86)\OpenSSH\id_ed25519' 'box'"
        );
        assert!(
            !has_unquoted_metachar(&cmd),
            "the path's parentheses must stay inside the quotes: {cmd}"
        );

        // The same charset drives every path-shaped option, not just `-i`.
        let kubeconfig = detect_container(&argv(&[
            "kubectl",
            "--kubeconfig",
            r"C:\Program Files (x86)\kube\config",
            "exec",
            "api-0",
            "--",
            "sh",
        ]))
        .expect("a Windows kubeconfig path is still a kubectl session");
        assert_eq!(
            clone_session_command(&kubeconfig),
            Some(
                r"kubectl --kubeconfig 'C:\Program Files (x86)\kube\config' exec -it 'api-0' -- $SHELL"
                    .to_string()
            )
        );

        // An apostrophe is reproduced with the close/escape/reopen idiom.
        let apostrophe = detect_ssh(&argv(&["ssh", "-i", "/home/o'brien/.ssh/id", "box"])).unwrap();
        assert_eq!(
            clone_session_command(&apostrophe),
            Some(r"ssh -i '/home/o'\''brien/.ssh/id' 'box'".to_string())
        );

        // A path that genuinely needs a shell metacharacter is still dropped.
        for hostile in [
            &["ssh", "-i", "/keys/`id -u`", "box"][..],
            &["ssh", "-i", "/keys/$HOME/id", "box"],
            &["ssh", "-i", "/keys/a\"b", "box"],
            &["ssh", "-i", "/keys/a|b", "box"],
        ] {
            assert_eq!(
                clone_session_command(&detect_ssh(&argv(hostile)).unwrap()),
                None,
                "{hostile:?} is not a path this crate re-emits"
            );
        }
    }

    /// The Reconnect line is typed into a live PTY, and its values come from a
    /// descendant process's argv rather than from anything the user typed
    /// here. So a value is bounded, and `-i` — the one endpoint option OpenSSH
    /// accepts more than once, trying each key in order — is not reproduced by
    /// keeping the last of several: the key that authenticated may not be the
    /// key re-emitted, which is a different account on the same host.
    #[test]
    fn oversized_and_repeated_option_values_suppress_the_reconnect() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let oversized = format!("/keys/{}", "a".repeat(6000));
        let huge = detect_ssh(&argv(&["ssh", "-i", &oversized, "box"]))
            .expect("an oversized identity path is still an ssh session");
        assert_eq!(format_remote_title(&huge), "ssh box");
        assert_eq!(clone_session_command(&huge), None);
        // A field that IS the endpoint never becomes a context at all.
        assert_eq!(
            detect_container(&argv(&["docker", "exec", &"c".repeat(6000), "sh"])),
            None
        );
        assert_eq!(detect_ssh(&argv(&["ssh", &"h".repeat(6000)])), None);

        // Two different identities: one slot cannot carry both.
        let two_keys =
            detect_ssh(&argv(&["ssh", "-i", "/keys/a", "-i", "/keys/b", "box"])).unwrap();
        assert_eq!(format_remote_title(&two_keys), "ssh box");
        assert_eq!(clone_session_command(&two_keys), None);
        // The same identity twice is still one identity.
        let same_key =
            detect_ssh(&argv(&["ssh", "-i", "/keys/a", "-i", "/keys/a", "box"])).unwrap();
        assert_eq!(
            clone_session_command(&same_key),
            Some("ssh -i '/keys/a' 'box'".to_string())
        );
    }

    /// `docker --context remote exec web` runs on another machine entirely;
    /// dropping the context reconnected against the LOCAL daemon, to whatever
    /// container happened to share the name. Same for podman's connection,
    /// an explicit daemon address, and kubectl's namespace + in-pod container.
    #[test]
    fn container_client_context_is_reproduced_by_the_reconnect_command() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let docker = detect_container(&argv(&[
            "docker",
            "--context",
            "remote",
            "exec",
            "-it",
            "web",
            "bash",
        ]))
        .unwrap();
        assert_eq!(
            docker,
            RemoteContext::Container {
                runtime: ContainerRuntime::Docker,
                container: "web".into(),
                options: ContainerOptions {
                    context: Some("remote".into()),
                    ..ContainerOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&docker),
            Some("docker --context 'remote' exec -it 'web' $SHELL".to_string())
        );
        // The `--flag=value` spelling of the same option.
        assert_eq!(
            detect_container(&argv(&[
                "docker",
                "--context=remote",
                "exec",
                "web",
                "bash"
            ])),
            Some(docker)
        );

        // An explicit daemon address, in docker's own flag.
        let remote_daemon = detect_container(&argv(&[
            "docker",
            "-H",
            "tcp://build.example:2375",
            "exec",
            "web",
            "sh",
        ]))
        .unwrap();
        assert_eq!(
            clone_session_command(&remote_daemon),
            Some("docker --host 'tcp://build.example:2375' exec -it 'web' $SHELL".to_string())
        );

        // Podman names the same concept `--connection`, so that is what comes
        // back out.
        let podman = detect_container(&argv(&[
            "podman",
            "--connection",
            "prod",
            "exec",
            "api",
            "sh",
        ]))
        .unwrap();
        assert_eq!(
            podman,
            RemoteContext::Container {
                runtime: ContainerRuntime::Podman,
                container: "api".into(),
                options: ContainerOptions {
                    context: Some("prod".into()),
                    ..ContainerOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&podman),
            Some("podman --connection 'prod' exec -it 'api' $SHELL".to_string())
        );

        // kubectl: the namespace picks the pod, and `-c` picks which of the
        // pod's containers the shell ran in.
        let kubectl = detect_container(&argv(&[
            "kubectl", "-n", "prod", "exec", "-it", "api-0", "-c", "sidecar", "--", "sh",
        ]))
        .unwrap();
        assert_eq!(
            kubectl,
            RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "api-0".into(),
                options: ContainerOptions {
                    namespace: Some("prod".into()),
                    pod_container: Some("sidecar".into()),
                    ..ContainerOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&kubectl),
            Some("kubectl --namespace 'prod' exec -it 'api-0' -c 'sidecar' -- $SHELL".to_string())
        );

        // A kubeconfig defines the clusters every other selector resolves in.
        let kubeconfig = detect_container(&argv(&[
            "kubectl",
            "--kubeconfig",
            "/home/me/.kube/staging",
            "exec",
            "api-0",
            "--",
            "sh",
        ]))
        .unwrap();
        assert_eq!(
            clone_session_command(&kubeconfig),
            Some(
                "kubectl --kubeconfig '/home/me/.kube/staging' exec -it 'api-0' -- $SHELL"
                    .to_string()
            )
        );
    }

    /// The post-subcommand option tables were incomplete, so an option VALUE
    /// was reported as the container: `--container sidecar` named the pod,
    /// `--env-file vars` named the container. Bundled shorts hid the same
    /// bug (`-itc sidecar`).
    #[test]
    fn container_option_values_are_never_read_as_the_container() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The pod is `api-0`; `sidecar` is which container inside it.
        assert_eq!(
            detect_container(&argv(&[
                "kubectl",
                "exec",
                "--container",
                "sidecar",
                "api-0",
                "--",
                "sh"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "api-0".into(),
                options: ContainerOptions {
                    pod_container: Some("sidecar".into()),
                    ..ContainerOptions::default()
                },
            })
        );
        // Same option inside a short bundle.
        assert_eq!(
            detect_container(&argv(&[
                "kubectl", "exec", "-itc", "sidecar", "api-0", "--", "sh"
            ])),
            Some(RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "api-0".into(),
                options: ContainerOptions {
                    pod_container: Some("sidecar".into()),
                    ..ContainerOptions::default()
                },
            })
        );
        // `vars` is an env file, not a container.
        assert_eq!(
            detect_container(&argv(&[
                "docker",
                "exec",
                "--env-file",
                "vars",
                "web",
                "sh"
            ])),
            Some(container_ctx(ContainerRuntime::Docker, "web"))
        );
    }

    /// `lxc-attach` accepts `--name`, `--name=`, and the joined `-n` spelling
    /// of the same option. Only the separated `-n NAME` form was recognized —
    /// the long forms consumed the name as an anonymous flag value and the
    /// pane got no remote context at all.
    #[test]
    fn lxc_attach_name_option_forms_are_all_detected() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for form in [
            &["lxc-attach", "--name", "web"][..],
            &["lxc-attach", "--name=web"],
            &["lxc-attach", "-nweb"],
            &["lxc-attach", "-n", "web"],
        ] {
            assert_eq!(
                detect_container(&argv(form)),
                Some(container_ctx(ContainerRuntime::Lxc, "web")),
                "{form:?} names container `web`"
            );
        }
        // A non-default container root holds a different set of containers, so
        // it has to come back out with the name.
        let rooted =
            detect_container(&argv(&["lxc-attach", "-P", "/srv/lxc", "-n", "web"])).unwrap();
        assert_eq!(
            rooted,
            RemoteContext::Container {
                runtime: ContainerRuntime::Lxc,
                container: "web".into(),
                options: ContainerOptions {
                    config: Some("/srv/lxc".into()),
                    ..ContainerOptions::default()
                },
            }
        );
        assert_eq!(
            clone_session_command(&rooted),
            Some("lxc-attach --lxcpath '/srv/lxc' -n 'web'".to_string())
        );
    }

    /// A credential can never be echoed back into a command line, and
    /// `podman --remote` has no value to carry. Both still produce a remote
    /// title; both refuse to produce a Reconnect, because the one that could
    /// be built would attach to the local daemon instead.
    #[test]
    fn container_options_that_cannot_be_reproduced_suppress_the_reconnect() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let with_token = detect_container(&argv(&[
            "kubectl",
            "--token",
            "s3cr3t-bearer-token",
            "exec",
            "api-0",
            "--",
            "sh",
        ]))
        .expect("still a kubectl exec session");
        assert_eq!(format_remote_title(&with_token), "kubectl: api-0");
        assert_eq!(clone_session_command(&with_token), None);

        let remote_podman =
            detect_container(&argv(&["podman", "--remote", "exec", "web", "sh"])).unwrap();
        assert_eq!(format_remote_title(&remote_podman), "podman: web");
        assert_eq!(clone_session_command(&remote_podman), None);

        // `-r` is podman's documented short spelling of `--remote`. Gating
        // only the long one reconnected to the LOCAL socket for a session that
        // ran against the remote service.
        for short_remote in [
            &["podman", "-r", "exec", "web", "sh"][..],
            &["podman", "--remote", "exec", "web", "sh"],
        ] {
            let ctx = detect_container(&argv(short_remote)).unwrap();
            assert_eq!(format_remote_title(&ctx), "podman: web");
            assert_eq!(
                clone_session_command(&ctx),
                None,
                "{short_remote:?} ran somewhere the local socket is not"
            );
        }

        // An identity or credential selector decides WHICH account — and often
        // which cluster — the same name resolves against. Consuming it and
        // saying nothing produced a reconnect that runs as the DEFAULT
        // kubeconfig user, or one that drops the TLS material the endpoint
        // needs.
        for credentialed in [
            &[
                "kubectl",
                "--user",
                "prod-admin",
                "exec",
                "api-0",
                "--",
                "sh",
            ][..],
            &["kubectl", "--as", "sre", "exec", "api-0", "--", "sh"],
            &["kubectl", "--as-group", "sre", "exec", "api-0", "--", "sh"],
            &[
                "kubectl",
                "--client-key",
                "/keys/admin.key",
                "exec",
                "api-0",
                "--",
                "sh",
            ],
            &[
                "kubectl",
                "--certificate-authority",
                "/certs/ca.pem",
                "exec",
                "api-0",
                "--",
                "sh",
            ],
        ] {
            let ctx = detect_container(&argv(credentialed))
                .unwrap_or_else(|| panic!("{credentialed:?} is still a kubectl session"));
            assert_eq!(format_remote_title(&ctx), "kubectl: api-0");
            assert_eq!(
                clone_session_command(&ctx),
                None,
                "{credentialed:?} authenticates as someone the bare command would not"
            );
        }
        let tls = detect_container(&argv(&[
            "docker",
            "--tlscacert",
            "/certs/ca.pem",
            "-H",
            "tcp://build.example:2376",
            "exec",
            "web",
            "sh",
        ]))
        .unwrap();
        assert_eq!(format_remote_title(&tls), "docker: web");
        assert_eq!(clone_session_command(&tls), None);

        // A flag whose NAME says endpoint, on a runtime whose own table did not
        // claim it: there is nothing to re-emit and no reason to believe the
        // default is the same place, so it fails closed instead of quietly
        // reconnecting to the local daemon.
        for foreign in [
            &[
                "podman",
                "--host",
                "tcp://build.example:2375",
                "exec",
                "web",
                "sh",
            ][..],
            &["podman", "--context", "remote", "exec", "web", "sh"],
            &[
                "kubectl",
                "--host",
                "https://api.example",
                "exec",
                "api-0",
                "--",
                "sh",
            ],
        ] {
            let ctx = detect_container(&argv(foreign))
                .unwrap_or_else(|| panic!("{foreign:?} is still a container session"));
            assert_eq!(
                clone_session_command(&ctx),
                None,
                "{foreign:?} names an endpoint this crate cannot map back"
            );
        }

        // A context name outside the reproducible charset is likewise dropped
        // rather than guessed at.
        let hostile = detect_container(&argv(&[
            "docker",
            "--context",
            "$(curl evil)",
            "exec",
            "web",
            "sh",
        ]))
        .unwrap();
        assert_eq!(clone_session_command(&hostile), None);

        // Positive controls: suppression is selective. `--user` AFTER `exec` is
        // a uid inside the container, not a credential for the endpoint, and a
        // plain exec still reconnects.
        for (reproducible, expected) in [
            (
                &["podman", "exec", "-it", "web", "sh"][..],
                "podman exec -it 'web' $SHELL",
            ),
            (
                &["docker", "exec", "--user", "root", "web", "sh"],
                "docker exec -it 'web' $SHELL",
            ),
            (
                &["kubectl", "exec", "-it", "api-0", "--", "sh"],
                "kubectl exec -it 'api-0' -- $SHELL",
            ),
        ] {
            let ctx = detect_container(&argv(reproducible))
                .unwrap_or_else(|| panic!("{reproducible:?} is still a container session"));
            assert_eq!(
                clone_session_command(&ctx),
                Some(expected.to_string()),
                "{reproducible:?} names one endpoint this crate can rebuild"
            );
        }
    }

    /// `--` does not mean the same thing to every CLI, and guessing costs an
    /// endpoint. docker/podman use it to end flag parsing, so the positional
    /// after it really is the container. kubectl's `exec` reads how many
    /// positionals preceded it (cobra's `ArgsLenAtDash`) and, when none did,
    /// takes EVERYTHING after it as the command with the pod coming from
    /// `-f`/stdin; `podman exec --latest` moves the container out of the argv
    /// the same way. Skipping the `--` and scanning on made the COMMAND the
    /// endpoint: `kubectl exec -f pod.yaml -- sh` titled the pane `kubectl: sh`
    /// and offered to re-attach to a container called `sh`.
    #[test]
    fn a_command_after_a_double_dash_is_never_read_as_the_container() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for command_only in [
            &["kubectl", "exec", "-f", "pod.yaml", "--", "sh"][..],
            &["kubectl", "exec", "--filename", "p.yaml", "--", "bash"],
            &["kubectl", "exec", "--filename=p.yaml", "--", "bash"],
            &["kubectl", "exec", "-itf", "pod.yaml", "--", "sh"],
            // Nothing before the `--` at all: the pod came from `-f` or stdin,
            // so every token after it is the command.
            &["kubectl", "exec", "--", "sh"],
            &["kubectl", "exec", "-it", "--", "web", "sh"],
            &["podman", "exec", "--latest", "--", "bash"],
            &["podman", "exec", "-l", "--", "bash"],
            // Without a `--` the same flags still move the container out of the
            // argv, so the next positional is the command, not the name.
            &["podman", "exec", "-l", "bash"],
            &["podman", "exec", "-it", "--latest", "bash"],
            &["kubectl", "exec", "-f", "pod.yaml", "sh"],
        ] {
            assert_eq!(
                detect_container(&argv(command_only)),
                None,
                "the container is not in this argv, so nothing may be offered: {command_only:?}"
            );
        }

        // Positive controls: for docker/podman `--` only ends flag parsing, and
        // a name given before the `--` is still the name.
        for (named, expected) in [
            (
                &["docker", "exec", "--", "web", "sh"][..],
                container_ctx(ContainerRuntime::Docker, "web"),
            ),
            (
                &["podman", "exec", "-it", "--", "web", "sh"],
                container_ctx(ContainerRuntime::Podman, "web"),
            ),
            (
                &["kubectl", "exec", "api-0", "--", "sh"],
                container_ctx(ContainerRuntime::Kubectl, "api-0"),
            ),
            (
                &["lxc-attach", "-n", "web", "--", "sh"],
                container_ctx(ContainerRuntime::Lxc, "web"),
            ),
        ] {
            assert_eq!(
                detect_container(&argv(named)),
                Some(expected),
                "{named:?} names its container in the argv"
            );
        }
    }

    /// lxc-attach's own value-taking options were missing from the table, so
    /// each one's VALUE was read as the container: `lxc-attach --uid 1000 -n
    /// web` titled the pane `lxc: 1000` and offered `lxc-attach -n '1000'`.
    /// `-o`/`--logfile` and `-l`/`--logpriority` come from the option set every
    /// lxc-* tool shares; `-u`/`--uid` and `-g`/`--gid` are lxc-attach's own.
    #[test]
    fn lxc_attach_option_values_are_never_read_as_the_container() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for form in [
            &["lxc-attach", "--uid", "1000", "-n", "web"][..],
            &["lxc-attach", "--uid=1000", "-n", "web"],
            &["lxc-attach", "--gid", "1000", "-n", "web"],
            &["lxc-attach", "-u", "1000", "-n", "web"],
            &["lxc-attach", "-g", "1000", "-n", "web"],
            &["lxc-attach", "-o", "/tmp/lxc.log", "-n", "web"],
            &["lxc-attach", "--logfile", "/tmp/lxc.log", "-n", "web"],
            &["lxc-attach", "-l", "DEBUG", "-n", "web"],
            &["lxc-attach", "-n", "web", "--uid", "1000", "--gid", "1000"],
        ] {
            assert_eq!(
                detect_container(&argv(form)),
                Some(container_ctx(ContainerRuntime::Lxc, "web")),
                "{form:?} names container `web`"
            );
        }
    }

    /// Structural guard for the two option tables and the emit tables, which
    /// are kept in step by hand: an option captured into a slot that the
    /// runtime's `clone_session_command` arm never emits would be silently
    /// dropped, which is exactly the "reconnect lands elsewhere" defect the
    /// slots exist to prevent. Nothing in the type system links the two, so
    /// assert the link.
    #[test]
    fn every_slot_a_runtime_can_capture_is_emitted_by_its_reconnect_command() {
        // The union of every option name either table recognizes, long and
        // short. Written out so the guard reads as data, and checked against
        // the tables themselves below — the list claimed to be complete "by
        // construction" while being hand-maintained, so a table entry landing
        // in a slot the emitter never writes could be added without any test
        // noticing.
        const NAMES: &[&str] = &[
            "context",
            "c",
            "connection",
            "host",
            "H",
            "url",
            "config",
            "remote",
            "r",
            "server",
            "s",
            "namespace",
            "n",
            "kubeconfig",
            "cluster",
            "token",
            "password",
            "user",
            "username",
            "as",
            "as-group",
            "certificate-authority",
            "client-certificate",
            "client-key",
            "tlscacert",
            "tlscert",
            "tlskey",
            "identity",
            "log-level",
            "request-timeout",
            "cache-dir",
            "root",
            "runroot",
            "storage-driver",
            "storage-opt",
            "tmpdir",
            "runtime",
            "l",
            "name",
            "lxcpath",
            "P",
            "e",
            "uid",
            "gid",
            "g",
            "o",
            "logfile",
            "logpriority",
            "container",
            "filename",
            "f",
            "latest",
            "env",
            "env-file",
            "workdir",
            "detach-keys",
            "cidfile",
            "pod-running-timeout",
            "preserve-fds",
            "arch",
            "namespaces",
            "set-var",
            "keep-var",
            "rcfile",
            "u",
            "w",
            "a",
            "v",
            "L",
        ];
        // Every option name the two tables actually match on, read out of
        // their source. `NAMES` must equal this exactly, or the sweep below is
        // silently skipping a table entry.
        let source = include_str!("lib.rs");
        let start = source
            .find("fn container_global_option(")
            .expect("the global option table");
        let exec = source
            .find("fn container_exec_option(")
            .expect("the exec option table");
        let end = exec
            + source[exec..]
                .find(
                    "
}
",
                )
                .expect("the exec option table ends");
        let mut in_tables = std::collections::BTreeSet::new();
        for line in source[start..end].lines() {
            // Prose in the comments is full of quoted words.
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(open) = rest.find('"') {
                let after = &rest[open + 1..];
                let Some(close) = after.find('"') else { break };
                in_tables.insert(after[..close].to_string());
                rest = &after[close + 1..];
            }
        }
        // If the slicing above ever stops matching, this catches it rather than
        // leaving an empty set to compare equal to an empty list.
        assert!(
            in_tables.len() > 50 && in_tables.contains("kubeconfig") && in_tables.contains("n"),
            "the table scan found {} names, which is not the tables",
            in_tables.len()
        );
        let declared = NAMES.iter().map(|n| (*n).to_string()).collect();
        assert_eq!(
            in_tables, declared,
            "NAMES and the option tables disagree; every name either table              matches on must be swept here"
        );

        const PROBE: &str = "slot-probe";
        for runtime in [
            ContainerRuntime::Docker,
            ContainerRuntime::Podman,
            ContainerRuntime::Kubectl,
            ContainerRuntime::Lxc,
        ] {
            for name in NAMES {
                for (table, slot) in [
                    ("global", container_global_option(runtime, name)),
                    ("exec", container_exec_option(runtime, name)),
                ]
                .into_iter()
                .filter_map(|(table, entry)| entry.map(|(slot, _)| (table, slot)))
                {
                    let mut options = ContainerOptions::default();
                    match slot {
                        ContainerOptionSlot::Context => options.context = Some(PROBE.into()),
                        ContainerOptionSlot::Endpoint => options.endpoint = Some(PROBE.into()),
                        ContainerOptionSlot::Namespace => options.namespace = Some(PROBE.into()),
                        ContainerOptionSlot::Config => options.config = Some(PROBE.into()),
                        ContainerOptionSlot::PodContainer => {
                            options.pod_container = Some(PROBE.into());
                        }
                        // These reach the user through the container name or
                        // through suppression, not through an emitted flag.
                        ContainerOptionSlot::Name
                        | ContainerOptionSlot::ImplicitName
                        | ContainerOptionSlot::Unreproducible
                        | ContainerOptionSlot::Ignored => continue,
                    }
                    let cmd = clone_session_command(&RemoteContext::Container {
                        runtime,
                        container: "c".into(),
                        options,
                    })
                    .unwrap_or_else(|| {
                        panic!("{runtime:?} {table} `{name}` captures a value but emits nothing")
                    });
                    assert!(
                        cmd.contains(PROBE),
                        "{runtime:?} {table} `{name}` lands in a slot its reconnect \
                         command never emits: {cmd}"
                    );
                }
            }
        }
    }

    /// The option values reach the same auto-executed line as the host and
    /// container names, so they get the same two layers: single-quoted on the
    /// way out, and a control character anywhere in them means no command at
    /// all rather than one a newline could split.
    #[test]
    fn reconnect_option_values_are_quoted_and_control_chars_rejected() {
        let quoted = clone_session_command(&RemoteContext::Ssh {
            host: "box".into(),
            user: None,
            options: SshOptions {
                identity: Some("/keys/a b; rm -rf ~".into()),
                ..SshOptions::default()
            },
        })
        .expect("no control char → Some, just quoted");
        assert_eq!(quoted, "ssh -i '/keys/a b; rm -rf ~' 'box'");
        assert!(
            !has_unquoted_metachar(&quoted),
            "option-value metachars must stay quoted: {quoted}"
        );

        assert_eq!(
            clone_session_command(&RemoteContext::Ssh {
                host: "box".into(),
                user: None,
                options: SshOptions {
                    port: Some("22\nreboot".into()),
                    ..SshOptions::default()
                },
            }),
            None
        );
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "api-0".into(),
                options: ContainerOptions {
                    namespace: Some("prod\nreboot".into()),
                    ..ContainerOptions::default()
                },
            }),
            None
        );
        assert_eq!(
            clone_session_command(&RemoteContext::Container {
                runtime: ContainerRuntime::Kubectl,
                container: "api-0".into(),
                options: ContainerOptions {
                    pod_container: Some("side\ncar".into()),
                    ..ContainerOptions::default()
                },
            }),
            None
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
            Some(ssh_ctx("host", Some("user")))
        );
        // Same for a `-f` password-file path literally named "ssh".
        assert_eq!(
            detect_ssh(&argv(&["sshpass", "-f", "ssh", "ssh", "user@host"])),
            Some(ssh_ctx("host", Some("user")))
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

    #[test]
    fn deepest_descendant_rejects_a_cyclic_index() {
        let index = std::collections::HashMap::from([(10, vec![11]), (11, vec![10])]);
        assert_eq!(deepest_descendant_in_index(10, &index), None);
    }

    #[test]
    fn foreground_shell_does_not_reselect_the_root_through_a_cycle() {
        let mut tree = MockProcessTree::new();
        tree.add(10, Some(11), &["bash"]);
        tree.add(11, Some(10), &["sleep", "1"]);
        let index = build_children_index(&tree);
        assert_eq!(find_foreground_shell_in_index(10, &tree, &index), None);
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
            Some(ssh_ctx("server.example.com", Some("alice")))
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
            Some(ssh_ctx("direct.example.com", Some("carol")))
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
            Some(container_ctx(ContainerRuntime::Docker, "api-1"))
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

    #[test]
    fn shared_index_detection_reuses_and_resets_bfs_scratch() {
        let mut tree = MockProcessTree::new();
        tree.add(100, None, &["bash"]);
        tree.add(200, Some(100), &["ssh", "alice@a.example"]);
        tree.add(300, None, &["fish"]);
        let index = build_children_index(&tree);
        let mut queue = std::collections::VecDeque::from([999]);
        let mut visited = std::collections::HashSet::from([999]);

        assert_eq!(
            detect_root_in_index_with_scratch(100, &tree, &index, &mut queue, &mut visited,),
            Some(ssh_ctx("a.example", Some("alice")))
        );
        assert!(!visited.contains(&999));

        assert_eq!(
            detect_root_in_index_with_scratch(300, &tree, &index, &mut queue, &mut visited,),
            None
        );
        assert_eq!(visited, std::collections::HashSet::from([300]));
        assert!(queue.is_empty());
    }

    #[test]
    fn background_probe_targets_are_bounded_deduplicated_and_fail_closed() {
        let mut targets = (1..=(MAX_REMOTE_PROBE_TARGETS as u32 + 10))
            .map(|pid| RemoteProbeTarget {
                pid,
                allow_native_cwd: true,
            })
            .collect::<Vec<_>>();
        targets.push(RemoteProbeTarget {
            pid: 7,
            allow_native_cwd: false,
        });
        targets.reverse();
        normalize_probe_targets(&mut targets);

        assert_eq!(targets.len(), MAX_REMOTE_PROBE_TARGETS);
        assert!(targets.windows(2).all(|pair| pair[0].pid < pair[1].pid));
        assert_eq!(
            targets.iter().find(|target| target.pid == 7),
            Some(&RemoteProbeTarget {
                pid: 7,
                allow_native_cwd: false,
            })
        );
    }

    #[test]
    fn background_probe_worker_returns_a_current_process_snapshot() {
        let worker = RemoteScanWorker::spawn().unwrap();
        let pid = std::process::id();
        worker.submit(vec![RemoteProbeTarget {
            pid,
            allow_native_cwd: false,
        }]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(snapshot) = worker.take_latest() {
                assert!(snapshot.probes.contains_key(&pid));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background remote scan did not publish within five seconds"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
            Some(ssh_ctx("deep.example", Some("bob")))
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
            Some(container_ctx(ContainerRuntime::Docker, "ubuntu-2204"))
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
            Some(container_ctx(ContainerRuntime::Docker, "near"))
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
