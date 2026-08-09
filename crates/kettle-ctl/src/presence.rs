//! Multi-window cycle (Peacock accents): the cross-process window-presence
//! registry.
//!
//! Every live kettle WINDOW (across every kettle process) writes a
//! `<pid>-w<seq>.json` entry recording the accent color it claimed, so a new
//! window — in this process or another — can pick a hue no live window is
//! using (VS Code Peacock vibes, but deduped LIVE). Unlike the `discovery`
//! registry this has no endpoint and is always on: it costs one tiny file
//! per window and a directory listing per window-open.
//!
//! Failure policy: every operation is best-effort. A claim that can't be
//! written, a directory that can't be created, an unreadable entry — all
//! degrade to "no dedupe information", never to a startup failure.
//!
//! Staleness: entries from dead pids are pruned on every read (pid-liveness
//! via `kill(pid, 0)` on Unix / `OpenProcess` + `GetExitCodeProcess` on
//! Windows), and a record also carries the owning process's start-time token
//! so a *recycled* pid cannot resurrect it — see [`process_start_token`]. Two
//! processes claiming simultaneously can race to the same hue (no locking by
//! design); the worst case is two same-colored windows, which the next
//! window-open won't repeat.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Presence records contain only fixed-size metadata. Bound leaf reads so a
/// corrupt same-user file cannot force an unbounded allocation during window
/// creation.
const MAX_PRESENCE_ENTRY_BYTES: usize = 4 * 1024;
/// Presence is one tiny record per live Kettle window. Stop a polluted
/// same-user directory from turning window creation into unbounded work.
const MAX_PRESENCE_DIR_ENTRIES: usize = 1024;

/// One live window's accent claim.
///
/// `#[non_exhaustive]`: a claim is only trustworthy while it names the process
/// instance that made it, so outside this crate one is built by
/// [`PresenceEntry::claiming`] (which resolves the token) or by deserializing
/// one that already exists — never by a struct literal that can leave the
/// binding out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PresenceEntry {
    /// Format version.
    pub v: u32,
    /// Owning process.
    pub pid: u32,
    /// The window's per-process sequence number.
    pub win: u64,
    /// Claimed accent as `#rrggbb`.
    pub rgb: String,
    /// True when the claim came from the Peacock auto pool (a pinned
    /// `accent-color = <hex>` window still registers, so auto windows can
    /// avoid colliding with it).
    pub auto: bool,
    /// Unix seconds at claim time (diagnostics only).
    pub started_unix: u64,
    /// The owner's [`process_start_token`] at claim time, so the record is
    /// bound to one process *instance* and not just to its pid. `None` for a
    /// record written by a build that predates the token, or on a platform
    /// that cannot report one; readers then fall back to bare pid liveness.
    #[serde(default)]
    pub start_token: Option<u64>,
}

impl PresenceEntry {
    /// A claim by the *current* process for its window `win`.
    ///
    /// The start token and claim time describe the claiming process, so they
    /// are resolved here rather than passed in — a call site cannot forget the
    /// binding that keeps this record from outliving its owner's pid.
    pub fn claiming(pid: u32, win: u64, rgb: String, auto: bool) -> Self {
        Self {
            v: 1,
            pid,
            win,
            rgb,
            auto,
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or(0),
            start_token: process_start_token(pid),
        }
    }

    fn is_valid(&self) -> bool {
        self.v == 1
            && self.pid != 0
            && self.rgb.len() == 7
            && self.rgb.starts_with('#')
            && self.rgb[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

/// Resolve the presence directory, environment injected for tests. Sibling
/// of the ctl discovery registry: `<runtime base>/kettle/instances`.
pub fn presence_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    // Discovery owns runtime-base validation and the private fallback policy.
    // Derive the sibling instead of duplicating that security boundary here.
    let mut dir = crate::discovery::registry_dir_from(get);
    dir.set_file_name("instances");
    dir
}

/// The default presence directory from the real environment.
pub fn presence_dir() -> PathBuf {
    presence_dir_from(|k| std::env::var(k).ok())
}

fn entry_path(dir: &Path, pid: u32, win: u64) -> PathBuf {
    dir.join(format!("{pid}-w{win}.json"))
}

/// Is `pid` a live process?
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `kill` reads its first argument as a SIGNED pid, and the special
        // values are all <= 0: `0` is "every process in my group", `-1` is
        // "every process I may signal", and any other negative is a process
        // group. A record is attacker-influenced data on disk, so casting a
        // `u32` straight through meant `u32::MAX` became `-1` and probed
        // everything — reporting a dead owner as live, forever, and keeping
        // its stale claim alive with it.
        //
        // Reject anything that cannot be a real pid before asking the kernel.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // kill(pid, 0): no signal sent, just the permission/existence check.
        // ESRCH = gone; EPERM = alive but not ours (still alive).
        let r = unsafe { libc::kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            // A reused pid whose process exited reports the real exit code;
            // STILL_ACTIVE (259) means it is genuinely running. (A process
            // that exits WITH code 259 is a documented Windows footgun we
            // accept — the entry just lives until the next reboot cleanup.)
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// A token naming the process *instance* behind `pid`: its OS-reported start
/// time.
///
/// Pids are recycled — Windows returns the number to the pool once the last
/// handle to the process closes, and Linux wraps at `pid_max` (32768 by
/// default) — so `pid_alive` alone says "some process has this number", not
/// "the process that wrote this record is still running". Any unrelated
/// program inheriting the number keeps a dead window's record (and its accent
/// claim) alive forever. The start time is the cheapest value an OS attests
/// for a running process that a recycled pid cannot reproduce.
///
/// `None` means "cannot tell": no such process, an unsupported platform, or a
/// query this process is not allowed to make. Callers must degrade to bare
/// liveness rather than treat that as proof of staleness. On macOS
/// `proc_pidinfo` refuses a process owned by another user, so a token there is
/// only available for this user's processes; on Windows and Linux it is
/// effectively always available.
///
/// What a token is comparable *across* differs by platform, and the guarantee
/// this code needs is only the weaker one. Windows reports an absolute
/// creation `FILETIME` and macOS an absolute wall-clock start, but Linux
/// reports ticks since **boot**, so two Linux processes from different boots
/// can share a token. Records live under a base directory that may itself
/// survive a reboot (`XDG_STATE_HOME` / `~/.local/state`), so a leftover Linux
/// record can collide with a same-pid, same-start-tick process from a later
/// boot. That failure is in the safe direction — the record is kept, which is
/// exactly the pre-token behavior — and pids and boot-relative start ticks
/// rarely coincide; nothing here treats the token as a durable cross-boot
/// identity.
pub fn process_start_token(pid: u32) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: the handle is closed on every path below, and all four
        // FILETIME out-parameters are valid for the duration of the call.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut created = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut exited, mut kernel, mut user) = (created, created, created);
            let ok =
                GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
            CloseHandle(handle);
            ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Field 22 of /proc/<pid>/stat is the start time in clock ticks since
        // boot. `comm` (field 2) is unescaped and may itself contain spaces
        // and ')', so the fixed-position fields only begin after its LAST
        // ')' — counting from the left would misread a process named
        // "sh (mine)".
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_comm = stat.rsplit_once(')')?.1;
        // `state` is the first field after `comm`, so starttime sits 19 fields
        // further along.
        after_comm.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let pid = libc::pid_t::try_from(pid).ok()?;
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, correctly sized buffer for the
        // PROC_PIDTBSDINFO flavor; the call writes at most `size` bytes into
        // it and reports how many it wrote.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size as libc::c_int,
            )
        };
        if written != size as libc::c_int {
            return None;
        }
        // Microseconds since the epoch: one value, still ordered like the
        // wall-clock start time it came from.
        Some(
            info.pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
        )
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Liveness for a record written by `pid`: the pid is running *and*, when both
/// the record and the OS can name the instance, it is still the same one.
///
/// A mismatch is the pid-reuse case and means the record is stale. An
/// unavailable token on either side means "cannot tell", which stays with the
/// historical bare-pid answer: never prune a record that may still be live.
pub fn owner_alive(pid: u32, start_token: Option<u64>) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match (start_token, process_start_token(pid)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

/// RAII claim: the entry file lives as long as the guard. Dropped on window
/// close (or process exit via OS cleanup + the next reader's pruning).
#[derive(Debug)]
pub struct PresenceGuard {
    path: PathBuf,
    entry: PresenceEntry,
}

impl PresenceGuard {
    /// The currently-claimed color.
    pub fn rgb(&self) -> &str {
        &self.entry.rgb
    }

    /// Re-claim with a new color (theme switch re-resolves the pool slot).
    /// Best-effort, like everything here.
    pub fn set_rgb(&mut self, rgb: &str) {
        if self.entry.rgb == rgb {
            return;
        }
        let mut replacement = self.entry.clone();
        replacement.rgb = rgb.to_string();
        if !replacement.is_valid() {
            return;
        }
        if let Ok(json) =
            crate::protocol::to_json_vec_bounded(&replacement, MAX_PRESENCE_ENTRY_BYTES)
            && kettle_state::atomic_replace(
                &self.path,
                &json,
                kettle_state::AtomicWriteOptions::PRIVATE,
            )
            .is_ok()
        {
            self.entry = replacement;
        }
    }
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write this window's claim. Returns `None` (silently — see the module
/// failure policy) when the directory or file can't be created.
pub fn claim(dir: &Path, entry: PresenceEntry) -> Option<PresenceGuard> {
    if !entry.is_valid() {
        return None;
    }
    ensure_private_dir(dir).ok()?;
    let path = entry_path(dir, entry.pid, entry.win);
    let json = crate::protocol::to_json_vec_bounded(&entry, MAX_PRESENCE_ENTRY_BYTES).ok()?;
    kettle_state::atomic_replace(&path, &json, kettle_state::AtomicWriteOptions::PRIVATE).ok()?;
    Some(PresenceGuard { path, entry })
}

/// Every live claim, stale entries pruned as a side effect. Unreadable or
/// unparseable files are skipped (and removed — they can only be leftovers).
pub fn live_entries(dir: &Path) -> Vec<PresenceEntry> {
    let mut out = Vec::new();
    if !private_dir_is_valid(dir) {
        return out;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.take(MAX_PRESENCE_DIR_ENTRIES).flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = read_entry(&path);
        match parsed {
            Some(e)
                if entry_path(dir, e.pid, e.win) == path
                    && e.is_valid()
                    && owner_alive(e.pid, e.start_token) =>
            {
                out.push(e);
            }
            // Dead owner (including a pid now belonging to someone else) —
            // prune so the dir can't grow forever, but only while the file is
            // still the record we judged.
            Some(stale) => prune_stale(&path, &stale),
            // Garbage: nothing readable to preserve, and a claim this reader
            // cannot parse can never take part in color selection.
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out
}

/// Delete a record judged stale — but only while the file still *is* that
/// record.
///
/// A record is named on disk by pid and window sequence, and both are reused:
/// the process that made this claim is gone, so its pid can already belong to a
/// new kettle whose first window claims the same name. Re-reading keeps the
/// delete from taking a live window's color out of the pool on the strength of
/// a judgement made about a different record. Mirrors
/// [`crate::discovery::prune_stale`].
fn prune_stale(path: &Path, judged: &PresenceEntry) {
    if read_entry(path).is_some_and(|current| current == *judged) {
        let _ = std::fs::remove_file(path);
    }
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        kettle_state::create_private_dirs(dir)?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "presence directory is not owned by the current user",
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    if !private_dir_is_valid(dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "presence directory is not private",
        ));
    }
    Ok(())
}

fn private_dir_is_valid(dir: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if !metadata.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn read_entry(path: &Path) -> Option<PresenceEntry> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        if std::fs::symlink_metadata(path)
            .ok()
            .is_none_or(|metadata| metadata.file_type().is_symlink())
        {
            return None;
        }
    }
    let mut file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PRESENCE_ENTRY_BYTES as u64 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return None;
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_PRESENCE_ENTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_PRESENCE_ENTRY_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = crate::test_scratch_root().join(format!(
            "kettle-presence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn entry(pid: u32, win: u64, rgb: &str) -> PresenceEntry {
        PresenceEntry {
            v: 1,
            pid,
            win,
            rgb: rgb.into(),
            auto: true,
            started_unix: 0,
            start_token: process_start_token(pid),
        }
    }

    /// Put `entry` on disk exactly the way `claim` would, without taking a
    /// guard that would delete it again at end of scope.
    fn write_entry(dir: &Path, entry: &PresenceEntry) {
        kettle_state::atomic_replace(
            &entry_path(dir, entry.pid, entry.win),
            &serde_json::to_vec(entry).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
    }

    #[test]
    fn claim_live_release_round_trip() {
        let dir = tmp("roundtrip");
        let me = std::process::id();
        let g1 = claim(&dir, entry(me, 1, "#cba6f7")).expect("claim 1");
        let _g2 = claim(&dir, entry(me, 2, "#89b4fa")).expect("claim 2");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(entry_path(&dir, me, 1))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let mut rgbs: Vec<String> = live_entries(&dir).into_iter().map(|e| e.rgb).collect();
        rgbs.sort();
        assert_eq!(rgbs, vec!["#89b4fa".to_string(), "#cba6f7".to_string()]);
        // Dropping a guard releases its claim.
        drop(g1);
        let rgbs: Vec<String> = live_entries(&dir).into_iter().map(|e| e.rgb).collect();
        assert_eq!(rgbs, vec!["#89b4fa".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_pid_entries_are_pruned_on_read() {
        let dir = tmp("stale");
        ensure_private_dir(&dir).unwrap();
        // A pid that can't be alive: u32::MAX is far past any real pid table
        // on Windows and Linux alike.
        let stale = entry(u32::MAX - 1, 1, "#ff0000");
        std::fs::write(
            entry_path(&dir, stale.pid, stale.win),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("garbage.json"), "not json").unwrap();
        assert!(
            live_entries(&dir).is_empty(),
            "dead-pid + garbage entries are dropped"
        );
        assert!(
            std::fs::read_dir(&dir).unwrap().flatten().next().is_none(),
            "pruned from disk too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_rgb_updates_the_claim_in_place() {
        let dir = tmp("update");
        let me = std::process::id();
        let mut g = claim(&dir, entry(me, 7, "#cba6f7")).expect("claim");
        g.set_rgb("#a6e3a1");
        let live = live_entries(&dir);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].rgb, "#a6e3a1");
        assert_eq!(live[0].win, 7);
        drop(g);
        assert!(live_entries(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_claims_and_updates_are_rejected() {
        let dir = tmp("invalid-claims");
        let me = std::process::id();
        assert!(claim(&dir, entry(me, 1, "red")).is_none());
        assert!(claim(&dir, entry(0, 1, "#cba6f7")).is_none());

        let mut guard = claim(&dir, entry(me, 2, "#cba6f7")).expect("valid claim");
        guard.set_rgb("not-a-color");
        assert_eq!(guard.rgb(), "#cba6f7");
        assert_eq!(live_entries(&dir)[0].rgb, "#cba6f7");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_or_oversized_records_are_pruned_without_loading_them() {
        let dir = tmp("bounded-records");
        ensure_private_dir(&dir).unwrap();
        let me = std::process::id();

        let mismatch = entry(me, 1, "#cba6f7");
        kettle_state::atomic_replace(
            &dir.join("wrong-name.json"),
            &serde_json::to_vec(&mismatch).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        let mut invalid = entry(me, 2, "#123456");
        invalid.v = 99;
        kettle_state::atomic_replace(
            &entry_path(&dir, me, 2),
            &serde_json::to_vec(&invalid).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        kettle_state::atomic_replace(
            &dir.join("oversized.json"),
            &vec![b'x'; MAX_PRESENCE_ENTRY_BYTES + 1],
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();

        assert!(live_entries(&dir).is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_record_is_pruned_without_following_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tmp("record-symlink");
        ensure_private_dir(&dir).unwrap();
        let target = dir.with_extension("target");
        kettle_state::atomic_replace(
            &target,
            &serde_json::to_vec(&entry(std::process::id(), 9, "#cba6f7")).unwrap(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        symlink(&target, entry_path(&dir, std::process::id(), 9)).unwrap();

        assert!(live_entries(&dir).is_empty());
        assert!(target.is_file(), "the external target must not be removed");
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn own_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
        // `kill` takes a SIGNED pid and every special value is <= 0: `0` means
        // "my whole process group", `-1` means "everything I may signal", and
        // other negatives mean a process group. Casting a `u32` straight
        // through turned `u32::MAX` into `-1`, so a crafted record probed
        // every process and came back "alive" — keeping a dead owner's claim
        // permanently. These must all be rejected before the kernel sees them.
        assert!(!pid_alive(u32::MAX), "u32::MAX would cast to -1");
        assert!(!pid_alive(0), "0 addresses the caller's own process group");
        for pid in [u32::MAX - 2, 0x8000_0000, 0xffff_fffe] {
            assert!(
                !pid_alive(pid),
                "{pid:#x} cannot be a live pid and must not be probed"
            );
        }
        // A pid that genuinely exists is still reported live.
        assert!(
            pid_alive(std::process::id()),
            "this process must be recognised as alive"
        );
    }

    /// Every CI target can name a process instance; the fallback arm exists
    /// only for platforms kettle does not ship.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn start_token_identifies_the_process_instance_behind_a_pid() {
        let me = std::process::id();
        let token = process_start_token(me).expect("a supported platform reports a start token");
        assert_eq!(
            Some(token),
            process_start_token(me),
            "the token must not change while the process runs"
        );
        assert_eq!(
            process_start_token(u32::MAX - 1),
            None,
            "a pid with no process behind it has no token"
        );

        assert!(owner_alive(me, Some(token)));
        assert!(
            !owner_alive(me, Some(token.wrapping_add(1))),
            "a live pid running a DIFFERENT instance than the record names is not the owner"
        );
        assert!(
            owner_alive(me, None),
            "a record without a token keeps the historical bare-pid answer"
        );
        assert!(!owner_alive(u32::MAX - 1, None));

        assert_eq!(
            PresenceEntry::claiming(me, 1, "#cba6f7".into(), true).start_token,
            Some(token),
            "a claim built for this process carries its own instance token"
        );
    }

    /// A pid the OS handed to somebody else must not keep a dead window's
    /// accent claim alive. The stale record names a live pid — our own — so
    /// bare pid liveness cannot tell it apart from the healthy one beside it.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn claim_from_a_recycled_pid_is_pruned_although_the_pid_is_live() {
        let dir = tmp("pid-reuse");
        ensure_private_dir(&dir).unwrap();
        let me = std::process::id();
        let token = process_start_token(me).expect("a supported platform reports a start token");

        let mut recycled = entry(me, 1, "#ff0000");
        recycled.start_token = Some(token.wrapping_add(1));
        write_entry(&dir, &recycled);
        let mine = entry(me, 2, "#00ff00");
        write_entry(&dir, &mine);

        let live = live_entries(&dir);
        assert_eq!(
            live.iter().map(|e| e.rgb.as_str()).collect::<Vec<_>>(),
            vec!["#00ff00"],
            "only the claim written by THIS instance stays live"
        );
        assert!(
            !entry_path(&dir, me, 1).exists(),
            "the recycled-pid record is pruned from disk"
        );
        assert!(
            entry_path(&dir, me, 2).exists(),
            "this instance's own record is left alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same rule the ctl registry follows: a record is deleted only while
    /// the file still *is* the record that was judged stale. A claim is named
    /// by pid and window sequence and both are reused, so a delete aimed at a
    /// dead window's claim must not take a live window's color out of the pool.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn pruning_a_stale_claim_spares_the_live_one_that_replaced_it() {
        let dir = tmp("prune-race");
        ensure_private_dir(&dir).unwrap();
        let me = std::process::id();
        let token = process_start_token(me).expect("a supported platform reports a start token");

        let mut stale = entry(me, 1, "#ff0000");
        stale.start_token = Some(token.wrapping_add(1));
        write_entry(&dir, &stale);
        // The recycle completes before the delete lands: a new process owns
        // the pid and its first window claimed the very same name.
        let live = entry(me, 1, "#00ff00");
        write_entry(&dir, &live);

        prune_stale(&entry_path(&dir, me, 1), &stale);
        assert_eq!(
            live_entries(&dir),
            vec![live.clone()],
            "a delete aimed at the stale claim must not take the live one with it"
        );

        prune_stale(&entry_path(&dir, me, 1), &live);
        assert!(
            live_entries(&dir).is_empty(),
            "a record still unchanged on disk is pruned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presence_dir_is_sibling_of_ctl_registry() {
        let fake = |k: &str| {
            (k == if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_RUNTIME_DIR"
            })
            .then(|| "/base".to_string())
        };
        let p = presence_dir_from(fake);
        let d = crate::discovery::registry_dir_from(fake);
        assert_eq!(p.parent(), d.parent(), "shared <base>/kettle parent");
        assert_eq!(
            p.file_name().and_then(|name| name.to_str()),
            Some("instances")
        );
        assert_eq!(d.file_name().and_then(|name| name.to_str()), Some("ctl"));
    }
}
