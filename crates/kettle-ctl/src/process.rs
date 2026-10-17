//! Who a process is right now, read from the OS.
//!
//! Control-caller verification walks from a connected peer up through its
//! ancestors, so every read here is bounded, never follows a guess, and
//! reports [`InspectError`] instead of a partial answer. A [`ProcessIdentity`]
//! pairs a pid with the instant that process started, so a recycled pid is a
//! different identity.
//!
//! Start instants are only comparable on one machine within one boot: Linux
//! reports clock ticks since boot, macOS microseconds since the epoch and
//! Windows a creation `FILETIME`. Nothing here treats one as a durable or
//! cross-machine identity.

/// A process instance: its pid and when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessIdentity {
    pid: u32,
    start: u64,
}

impl ProcessIdentity {
    pub const fn pid(self) -> u32 {
        self.pid
    }

    /// The platform's start instant (see the module docs for its unit).
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Only the OS readers and tests make identities; a wire claim is parsed
    /// into a separate, untrusted type and compared with one of these.
    pub(crate) const fn new(pid: u32, start: u64) -> Self {
        Self { pid, start }
    }

    /// For other crates' tests, which cannot read a scripted process table.
    #[doc(hidden)]
    pub const fn new_for_tests(pid: u32, start: u64) -> Self {
        Self { pid, start }
    }
}

/// One observation of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub identity: ProcessIdentity,
    /// `None` for a process with no parent the OS will name (pid 1, the
    /// kernel's own tasks, or a parent outside this pid namespace).
    pub parent_pid: Option<u32>,
    /// Exited but not yet reaped. Such a process can no longer act, so it
    /// never verifies anything. macOS refuses to describe such a process at
    /// all, so there it reads as [`InspectError::Unavailable`] instead.
    pub exited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectError {
    /// No such process, or it is not this user's to inspect.
    Unavailable,
    /// The OS answered with something this reader does not accept.
    InvalidData,
    /// This platform has no reader.
    Unsupported,
}

/// Inspect `pid` now.
pub fn inspect(pid: u32) -> Result<ProcessSnapshot, InspectError> {
    if pid == 0 {
        return Err(InspectError::Unavailable);
    }
    imp::inspect(pid)
}

/// This process.
pub fn current() -> Result<ProcessSnapshot, InspectError> {
    inspect(std::process::id())
}

/// `pid`'s identity without its parent, which on Windows costs a scan of every
/// process. For liveness checks that only compare start instants.
pub fn identity(pid: u32) -> Result<ProcessIdentity, InspectError> {
    if pid == 0 {
        return Err(InspectError::Unavailable);
    }
    #[cfg(windows)]
    return imp::identity(pid);
    #[cfg(not(windows))]
    return imp::inspect(pid).map(|snapshot| snapshot.identity);
}

/// The executable `identity` runs, as the operating system names it, while
/// it is still that process: the identity is read again after the path, so a
/// pid reused meanwhile reads as unavailable. The path is the kernel's, never
/// anything the process says about itself; display it only after sanitizing.
pub fn executable(identity: ProcessIdentity) -> Result<std::path::PathBuf, InspectError> {
    if identity.pid == 0 {
        return Err(InspectError::Unavailable);
    }
    #[cfg(windows)]
    return imp::executable(identity);
    #[cfg(not(windows))]
    {
        let path = imp::executable(identity.pid)?;
        if self::identity(identity.pid)? != identity {
            return Err(InspectError::Unavailable);
        }
        Ok(path)
    }
}

/// Where a process's standard output goes, as the kernel names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardOutput {
    /// A character device, such as a terminal: its number, as
    /// [`device_number`] gives it.
    Device(u64),
    /// Anything else: a pipe, a socket, a file, or nothing open.
    Other,
}

/// Where `identity`'s standard output (descriptor 1) goes, while it is still
/// that process: the identity is read again after, so a pid reused meanwhile
/// reads as unavailable. Read from the kernel, never from the process.
pub fn standard_output(identity: ProcessIdentity) -> Result<StandardOutput, InspectError> {
    if identity.pid == 0 {
        return Err(InspectError::Unavailable);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let output = imp::standard_output(identity.pid)?;
        if self::identity(identity.pid)? != identity {
            return Err(InspectError::Unavailable);
        }
        Ok(output)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Err(InspectError::Unsupported)
}

/// `identity`'s process group, while it is still that process.
pub fn process_group(identity: ProcessIdentity) -> Result<u32, InspectError> {
    if identity.pid == 0 {
        return Err(InspectError::Unavailable);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let group = imp::process_group(identity.pid)?;
        if self::identity(identity.pid)? != identity {
            return Err(InspectError::Unavailable);
        }
        Ok(group)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Err(InspectError::Unsupported)
}

/// A character device's number as [`StandardOutput::Device`] gives it, from
/// the device file's metadata: Linux's 64-bit `dev_t`, macOS's 32-bit one.
#[cfg(unix)]
pub fn device_number(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    #[cfg(target_os = "macos")]
    return u64::from(metadata.rdev() as u32);
    #[cfg(not(target_os = "macos"))]
    return metadata.rdev();
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{InspectError, ProcessIdentity, ProcessSnapshot, StandardOutput};
    use std::io::Read as _;

    /// What `/proc/<pid>/fd/1` opens, by its target's metadata. No such
    /// descriptor is no device; a process gone is told apart by the caller,
    /// which reads its identity again.
    pub(super) fn standard_output(pid: u32) -> Result<StandardOutput, InspectError> {
        use std::os::unix::fs::FileTypeExt as _;
        match std::fs::metadata(format!("/proc/{pid}/fd/1")) {
            Ok(metadata) if metadata.file_type().is_char_device() => {
                Ok(StandardOutput::Device(super::device_number(&metadata)))
            }
            Ok(_) => Ok(StandardOutput::Other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StandardOutput::Other),
            Err(_) => Err(InspectError::Unavailable),
        }
    }

    pub(super) fn process_group(pid: u32) -> Result<u32, InspectError> {
        let stat = read_stat(pid)?;
        let fields = fixed_fields(pid, &stat)?;
        fields
            .get(5 - 3)
            .and_then(|group| group.parse().ok())
            .ok_or(InspectError::InvalidData)
    }

    pub(super) fn executable(pid: u32) -> Result<std::path::PathBuf, InspectError> {
        std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|_| InspectError::Unavailable)
    }

    /// `/proc/<pid>/stat` is one short line; `comm` is at most 64 bytes.
    const MAX_STAT_BYTES: u64 = 4096;

    pub(super) fn inspect(pid: u32) -> Result<ProcessSnapshot, InspectError> {
        parse_stat(pid, &read_stat(pid)?)
    }

    fn read_stat(pid: u32) -> Result<Vec<u8>, InspectError> {
        let mut stat = Vec::new();
        std::fs::File::open(format!("/proc/{pid}/stat"))
            .and_then(|file| file.take(MAX_STAT_BYTES + 1).read_to_end(&mut stat))
            .map_err(|_| InspectError::Unavailable)?;
        if stat.len() as u64 > MAX_STAT_BYTES {
            return Err(InspectError::InvalidData);
        }
        Ok(stat)
    }

    /// The fields after `comm`, numbered from 3 as in proc_pid_stat(5), at
    /// index `number - 3`. `comm` (field 2) is raw bytes, unescaped, and may
    /// hold spaces, `)` or bytes that are not UTF-8, so the fixed fields begin
    /// after its LAST `)` and only they are decoded.
    fn fixed_fields(pid: u32, stat: &[u8]) -> Result<Vec<&str>, InspectError> {
        let close = stat
            .iter()
            .rposition(|byte| *byte == b')')
            .ok_or(InspectError::InvalidData)?;
        let reported: u32 = stat
            .iter()
            .position(|byte| *byte == b' ')
            .filter(|&space| stat.get(space + 1) == Some(&b'(') && space < close)
            .and_then(|space| std::str::from_utf8(&stat[..space]).ok())
            .and_then(|reported| reported.parse().ok())
            .ok_or(InspectError::InvalidData)?;
        let rest =
            std::str::from_utf8(&stat[close + 1..]).map_err(|_| InspectError::InvalidData)?;
        if reported != pid {
            return Err(InspectError::InvalidData);
        }
        Ok(rest.split_ascii_whitespace().take(20).collect())
    }

    /// Parse `pid`'s stat line.
    pub(super) fn parse_stat(pid: u32, stat: &[u8]) -> Result<ProcessSnapshot, InspectError> {
        let fields = fixed_fields(pid, stat)?;
        let field = |number: usize| {
            fields
                .get(number - 3)
                .copied()
                .ok_or(InspectError::InvalidData)
        };
        let state = field(3)?;
        let ppid: u32 = field(4)?.parse().map_err(|_| InspectError::InvalidData)?;
        let threads: u64 = field(20)?.parse().map_err(|_| InspectError::InvalidData)?;
        let start: u64 = field(22)?.parse().map_err(|_| InspectError::InvalidData)?;
        // The state is the thread-group leader's own. A leader that called
        // `pthread_exit` reads `Z` while its other threads still run, and it
        // stays counted until the whole group is done, so only a `Z` leader
        // that is the last thread left means the process has exited.
        let exited = matches!(state, "X" | "x") || (state == "Z" && threads <= 1);
        Ok(ProcessSnapshot {
            identity: ProcessIdentity::new(pid, start),
            parent_pid: (ppid != 0).then_some(ppid),
            exited,
        })
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{InspectError, ProcessIdentity, ProcessSnapshot, StandardOutput};

    /// `pbi_status` of a process that has exited and awaits its parent.
    const SZOMB: u32 = 5;

    /// `proc_pidfdinfo`'s flavor for a descriptor's vnode, from
    /// `<sys/proc_info.h>`.
    const PROC_PIDFDVNODEINFO: libc::c_int = 1;
    /// `vnode_info.vi_type` of a character device (`VCHR` in `<sys/vnode.h>`).
    const VCHR: i32 = 4;

    /// `struct proc_fileinfo` from `<sys/proc_info.h>`.
    #[repr(C)]
    struct FileInfo {
        open_flags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guard_flags: u32,
    }

    /// `struct vnode_info` from `<sys/proc_info.h>`.
    #[repr(C)]
    struct VnodeInfo {
        stat: libc::vinfo_stat,
        kind: i32,
        pad: i32,
        fsid: [i32; 2],
    }

    /// `struct vnode_fdinfo` from `<sys/proc_info.h>`: a descriptor and the
    /// vnode it opens. libc declares the call but not the struct.
    #[repr(C)]
    struct VnodeFdInfo {
        file: FileInfo,
        vnode: VnodeInfo,
    }

    // The SDK's sizes, so a mistyped field fails the build, not a read.
    const _: () = assert!(std::mem::size_of::<FileInfo>() == 24);
    const _: () = assert!(std::mem::size_of::<VnodeInfo>() == 152);
    const _: () = assert!(std::mem::size_of::<VnodeFdInfo>() == 176);

    /// What descriptor 1 opens, by its vnode. A pipe or socket has no vnode,
    /// so the call fails for one, as it does for no descriptor or a process
    /// gone; the caller reads the identity again to tell the last apart.
    pub(super) fn standard_output(pid: u32) -> Result<StandardOutput, InspectError> {
        let os_pid = libc::pid_t::try_from(pid).map_err(|_| InspectError::Unavailable)?;
        let size = std::mem::size_of::<VnodeFdInfo>();
        // SAFETY: an all-zero `VnodeFdInfo` is a valid value of this plain C
        // struct of integers.
        let mut info: VnodeFdInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live buffer of exactly `size` bytes for the
        // PROC_PIDFDVNODEINFO flavor, whose layout the asserts above pin; the
        // call writes at most `size` bytes and returns how many it wrote.
        let written = unsafe {
            libc::proc_pidfdinfo(
                os_pid,
                1,
                PROC_PIDFDVNODEINFO,
                std::ptr::from_mut(&mut info).cast(),
                size as libc::c_int,
            )
        };
        if written != size as libc::c_int || info.vnode.kind != VCHR {
            return Ok(StandardOutput::Other);
        }
        Ok(StandardOutput::Device(u64::from(info.vnode.stat.vst_rdev)))
    }

    pub(super) fn process_group(pid: u32) -> Result<u32, InspectError> {
        bsd_info(pid).map(|info| info.pbi_pgid)
    }

    pub(super) fn executable(pid: u32) -> Result<std::path::PathBuf, InspectError> {
        use std::os::unix::ffi::OsStrExt as _;
        let os_pid = libc::pid_t::try_from(pid).map_err(|_| InspectError::Unavailable)?;
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `path` is a live buffer of the length passed; the call
        // writes at most that many bytes and returns how many it wrote.
        let written =
            unsafe { libc::proc_pidpath(os_pid, path.as_mut_ptr().cast(), path.len() as u32) };
        let written = usize::try_from(written).map_err(|_| InspectError::Unavailable)?;
        if written == 0 {
            return Err(InspectError::Unavailable);
        }
        path.truncate(written.min(path.len()));
        Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
    }

    pub(super) fn inspect(pid: u32) -> Result<ProcessSnapshot, InspectError> {
        let info = bsd_info(pid)?;
        let start = info
            .pbi_start_tvsec
            .checked_mul(1_000_000)
            .and_then(|micros| micros.checked_add(info.pbi_start_tvusec))
            .ok_or(InspectError::InvalidData)?;
        Ok(ProcessSnapshot {
            identity: ProcessIdentity::new(pid, start),
            parent_pid: (info.pbi_ppid != 0).then_some(info.pbi_ppid),
            exited: info.pbi_status == SZOMB,
        })
    }

    fn bsd_info(pid: u32) -> Result<libc::proc_bsdinfo, InspectError> {
        let os_pid = libc::pid_t::try_from(pid).map_err(|_| InspectError::Unavailable)?;
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        // SAFETY: an all-zero `proc_bsdinfo` is a valid value of this plain C
        // struct of integers and byte arrays.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live buffer of exactly `size` bytes for the
        // PROC_PIDTBSDINFO flavor; the call writes at most `size` bytes and
        // returns how many it wrote.
        let written = unsafe {
            libc::proc_pidinfo(
                os_pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size as libc::c_int,
            )
        };
        if written <= 0 {
            return Err(InspectError::Unavailable);
        }
        if written != size as libc::c_int || info.pbi_pid != pid {
            return Err(InspectError::InvalidData);
        }
        Ok(info)
    }
}

#[cfg(windows)]
mod imp {
    use super::{InspectError, ProcessIdentity, ProcessSnapshot};
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    /// Closes a handle on every path.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle this guard alone owns.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// `PROCESS_BASIC_INFORMATION`, the `ProcessBasicInformation` (class 0)
    /// layout of `NtQueryInformationProcess`.
    #[repr(C)]
    struct BasicInformation {
        exit_status: i32,
        peb_base_address: *mut c_void,
        affinity_mask: usize,
        base_priority: i32,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
    }

    type QueryInformationProcess =
        unsafe extern "system" fn(HANDLE, i32, *mut c_void, u32, *mut u32) -> i32;

    /// `NtQueryInformationProcess`, which Microsoft documents must be
    /// resolved at run time. `None` when this Windows does not export it.
    fn query_information_process() -> Option<QueryInformationProcess> {
        static QUERY: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
        let address = *QUERY.get_or_init(|| {
            let module: Vec<u16> = "ntdll.dll".encode_utf16().chain([0]).collect();
            // SAFETY: `module` is a NUL-terminated UTF-16 string that outlives
            // the call; ntdll is mapped into every Windows process.
            let ntdll = unsafe { GetModuleHandleW(module.as_ptr()) };
            if ntdll.is_null() {
                return None;
            }
            // SAFETY: the name is a NUL-terminated ASCII string and `ntdll` a
            // live module handle.
            unsafe { GetProcAddress(ntdll, c"NtQueryInformationProcess".as_ptr().cast()) }
                .map(|function| function as usize)
        });
        // SAFETY: the address is `NtQueryInformationProcess`, whose ABI is
        // `QueryInformationProcess`.
        address.map(|address| unsafe {
            std::mem::transmute::<usize, QueryInformationProcess>(address)
        })
    }

    fn open(pid: u32) -> Result<Owned, InspectError> {
        // SAFETY: plain call; a null return means no handle was opened.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(InspectError::Unavailable);
        }
        Ok(Owned(handle))
    }

    pub(super) fn inspect(pid: u32) -> Result<ProcessSnapshot, InspectError> {
        // One handle answers every question, so a pid reused between them
        // cannot mix two processes' answers.
        let process = open(pid)?;
        let (identity, exited) = times(&process, pid)?;
        Ok(ProcessSnapshot {
            identity,
            parent_pid: parent_of(&process, pid)?,
            exited,
        })
    }

    pub(super) fn identity(pid: u32) -> Result<ProcessIdentity, InspectError> {
        let process = open(pid)?;
        times(&process, pid).map(|(identity, _)| identity)
    }

    /// The path and the identity come from one handle, so they belong to
    /// one process.
    pub(super) fn executable(
        expected: ProcessIdentity,
    ) -> Result<std::path::PathBuf, InspectError> {
        use std::os::windows::ffi::OsStringExt as _;
        let process = open(expected.pid())?;
        let (identity, exited) = times(&process, expected.pid())?;
        if identity != expected || exited {
            return Err(InspectError::Unavailable);
        }
        // Windows paths are at most 32,767 UTF-16 units.
        let mut path = vec![0u16; 32_768];
        let mut size = path.len() as u32;
        // SAFETY: the handle is live, `path` holds `size` units, and the call
        // writes at most that many and stores the count it wrote in `size`.
        if unsafe {
            QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut size)
        } == 0
        {
            return Err(InspectError::Unavailable);
        }
        path.truncate(size as usize);
        Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
            &path,
        )))
    }

    /// The identity and exit state from the process's own handle.
    fn times(process: &Owned, pid: u32) -> Result<(ProcessIdentity, bool), InspectError> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle is live and the four FILETIME out-parameters are
        // valid for the call.
        if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return Err(InspectError::Unavailable);
        }
        let mut code = 0u32;
        // SAFETY: the handle is live and `code` is a valid out-parameter.
        if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
            return Err(InspectError::Unavailable);
        }
        let start = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        // STILL_ACTIVE can also be a real exit code; the exit time settles
        // it, since a running process has none.
        let has_exited =
            code != STILL_ACTIVE as u32 || exited.dwLowDateTime != 0 || exited.dwHighDateTime != 0;
        Ok((ProcessIdentity::new(pid, start), has_exited))
    }

    /// The creator's pid the kernel records. Windows keeps it after that
    /// creator exits and its pid is reused, so callers must still check the
    /// parent's start time.
    fn parent_of(process: &Owned, pid: u32) -> Result<Option<u32>, InspectError> {
        let query = query_information_process().ok_or(InspectError::Unsupported)?;
        let size = std::mem::size_of::<BasicInformation>();
        // SAFETY: an all-zero value is valid for this struct of integers and
        // a raw pointer.
        let mut info: BasicInformation = unsafe { std::mem::zeroed() };
        let mut written = 0u32;
        // SAFETY: the handle is live, `info` is a buffer of exactly `size`
        // bytes for class 0, and `written` is a valid out-parameter.
        let status = unsafe {
            query(
                process.0,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size as u32,
                &mut written,
            )
        };
        if status < 0 {
            return Err(InspectError::Unavailable);
        }
        if written as usize != size || info.unique_process_id != pid as usize {
            return Err(InspectError::InvalidData);
        }
        let parent = u32::try_from(info.inherited_from_unique_process_id)
            .map_err(|_| InspectError::InvalidData)?;
        Ok((parent != 0).then_some(parent))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::{InspectError, ProcessSnapshot};

    pub(super) fn inspect(_pid: u32) -> Result<ProcessSnapshot, InspectError> {
        Err(InspectError::Unsupported)
    }

    pub(super) fn executable(_pid: u32) -> Result<std::path::PathBuf, InspectError> {
        Err(InspectError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_with_its_real_parent() {
        let me = current().expect("inspect self");
        assert_eq!(me.identity.pid(), std::process::id());
        assert!(!me.exited);
        assert!(me.identity.start() > 0);
        #[cfg(unix)]
        assert_eq!(me.parent_pid, Some(std::os::unix::process::parent_id()));
        // The same instance reads the same identity twice.
        assert_eq!(current().unwrap().identity, me.identity);
    }

    #[test]
    fn a_child_starts_no_earlier_than_its_parent_and_names_it() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" })
            .args(if cfg!(windows) {
                &["/C", "ping -n 3 127.0.0.1 >NUL"][..]
            } else {
                &["2"][..]
            })
            .spawn()
            .expect("spawn child");
        let observed = inspect(child.id()).expect("inspect child");
        let me = current().unwrap();
        assert_eq!(observed.parent_pid, Some(std::process::id()));
        assert!(observed.identity.start() >= me.identity.start());
        assert!(!observed.exited);
        child.kill().ok();
        // Until it is reaped, a killed child reads as exited (Linux) or as
        // unavailable (macOS), never as a live process with a new identity.
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match inspect(child.id()) {
                    Ok(snapshot) if snapshot.exited => {
                        assert_eq!(snapshot.identity, observed.identity);
                        break;
                    }
                    Err(InspectError::Unavailable) if cfg!(target_os = "macos") => break,
                    Ok(snapshot)
                        if snapshot.identity == observed.identity
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    other => panic!("killed child read as {other:?}"),
                }
            }
        }
        child.wait().ok();
    }

    #[test]
    fn the_executable_is_this_test_binary_and_only_for_its_own_identity() {
        let me = current().unwrap().identity;
        let path = executable(me).unwrap();
        let expected = std::env::current_exe().unwrap();
        assert_eq!(
            std::fs::canonicalize(&path).unwrap(),
            std::fs::canonicalize(&expected).unwrap()
        );
        // The same pid with another start time is another process.
        let reused = ProcessIdentity::new(me.pid(), me.start().wrapping_add(1));
        assert_eq!(executable(reused), Err(InspectError::Unavailable));
        assert_eq!(
            executable(ProcessIdentity::new(0, 0)),
            Err(InspectError::Unavailable)
        );
    }

    /// A new pseudo-terminal, its controller and its terminal end, neither
    /// handed to children that do not ask for it.
    #[cfg(unix)]
    fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        let (mut controller, mut terminal) = (-1, -1);
        // SAFETY: both out-pointers are live; the null name, settings and
        // size ask for defaults.
        let status = unsafe {
            libc::openpty(
                &mut controller,
                &mut terminal,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty returned two new descriptors this test alone owns.
        let ends = unsafe {
            (
                OwnedFd::from_raw_fd(controller),
                OwnedFd::from_raw_fd(terminal),
            )
        };
        for end in [&ends.0, &ends.1] {
            // SAFETY: a live descriptor this test owns.
            unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        ends
    }

    /// A child that waits half a minute unless killed.
    #[cfg(unix)]
    fn waiting(command: &mut std::process::Command) -> std::process::Child {
        command.arg("30").spawn().expect("spawn sleep")
    }

    /// Descriptor 1 is told by the device it opens: the child's own
    /// terminal, another terminal, another character device, or a pipe, and
    /// only while the identity asked about is still the process.
    #[cfg(unix)]
    #[test]
    fn standard_output_names_the_device_a_child_writes_to() {
        use std::process::{Command, Stdio};
        let (_controller, terminal) = pty();
        let (_other_controller, other) = pty();
        let number = |end: &std::os::fd::OwnedFd| {
            device_number(
                &std::fs::File::from(end.try_clone().unwrap())
                    .metadata()
                    .unwrap(),
            )
        };
        let (device, other_device) = (number(&terminal), number(&other));
        assert_ne!(device, other_device);
        let null = device_number(&std::fs::metadata("/dev/null").unwrap());
        let mut children = [
            waiting(Command::new("sleep").stdout(Stdio::from(terminal))),
            waiting(Command::new("sleep").stdout(Stdio::from(other))),
            waiting(Command::new("sleep").stdout(Stdio::null())),
            waiting(Command::new("sleep").stdout(Stdio::piped())),
        ];
        let identities: Vec<_> = children
            .iter()
            .map(|child| inspect(child.id()).unwrap().identity)
            .collect();
        let outputs: Vec<_> = identities.iter().map(|&id| standard_output(id)).collect();
        assert_eq!(
            outputs,
            [
                Ok(StandardOutput::Device(device)),
                Ok(StandardOutput::Device(other_device)),
                Ok(StandardOutput::Device(null)),
                Ok(StandardOutput::Other),
            ]
        );
        let first = identities[0];
        let reused = ProcessIdentity::new(first.pid(), first.start().wrapping_add(1));
        assert_eq!(standard_output(reused), Err(InspectError::Unavailable));
        for child in &mut children {
            child.kill().ok();
            child.wait().ok();
        }
        assert_eq!(standard_output(first), Err(InspectError::Unavailable));
        assert_eq!(
            standard_output(ProcessIdentity::new(0, 0)),
            Err(InspectError::Unavailable)
        );
    }

    /// A child that leads its own group is that group; one that does not is
    /// in its parent's.
    #[cfg(unix)]
    #[test]
    fn process_group_is_the_one_a_child_runs_in() {
        use std::os::unix::process::CommandExt as _;
        let mut leader = waiting(std::process::Command::new("sleep").process_group(0));
        let mut member = waiting(&mut std::process::Command::new("sleep"));
        let group =
            |child: &std::process::Child| process_group(inspect(child.id()).unwrap().identity);
        assert_eq!(group(&leader), Ok(leader.id()));
        // SAFETY: getpgrp has no failure mode.
        let ours = unsafe { libc::getpgrp() };
        assert_eq!(group(&member), Ok(ours as u32));
        let identity = inspect(member.id()).unwrap().identity;
        for child in [&mut leader, &mut member] {
            child.kill().ok();
            child.wait().ok();
        }
        assert_eq!(process_group(identity), Err(InspectError::Unavailable));
    }

    #[test]
    fn identity_agrees_with_a_full_inspection() {
        assert_eq!(
            identity(std::process::id()),
            Ok(current().unwrap().identity)
        );
        assert_eq!(identity(0), Err(InspectError::Unavailable));
    }

    #[test]
    fn no_pid_zero_or_absent_process() {
        assert_eq!(inspect(0), Err(InspectError::Unavailable));
        assert_eq!(inspect(u32::MAX - 1), Err(InspectError::Unavailable));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_parsing_survives_hostile_names_and_rejects_mismatches() {
        let line = |name: &str| {
            format!("42 ({name}) S 7 42 42 0 -1 4194560 100 0 0 0 1 1 0 0 20 0 1 0 98765 100 10")
        };
        for name in ["sh", "sh (mine)", ") S 1 1", "a b ) c"] {
            let parsed = imp::parse_stat(42, line(name).as_bytes()).expect(name);
            assert_eq!(parsed.identity, ProcessIdentity::new(42, 98765), "{name}");
            assert_eq!(parsed.parent_pid, Some(7), "{name}");
            assert!(!parsed.exited, "{name}");
        }
        // Field 20 (threads) is 1 in `line`: a zombie that is the last
        // thread left has exited...
        let zombie = line("x").replace(") S 7", ") Z 7");
        assert!(imp::parse_stat(42, zombie.as_bytes()).unwrap().exited);
        // ...but a leader that exited alone while a worker thread runs has not.
        let leader_gone = zombie.replace(" 20 0 1 0 98765", " 20 0 2 0 98765");
        assert_ne!(leader_gone, zombie);
        assert!(!imp::parse_stat(42, leader_gone.as_bytes()).unwrap().exited);
        let orphan = line("x").replace(") S 7", ") S 0");
        assert_eq!(
            imp::parse_stat(42, orphan.as_bytes()).unwrap().parent_pid,
            None
        );
        assert_eq!(
            imp::parse_stat(43, line("sh").as_bytes()),
            Err(InspectError::InvalidData),
            "the line must describe the pid asked about"
        );
        assert_eq!(
            imp::parse_stat(42, b"42 (sh) S 7"),
            Err(InspectError::InvalidData)
        );
        assert_eq!(imp::parse_stat(42, b"\xff"), Err(InspectError::InvalidData));
        // A name that is not UTF-8 is still a process.
        let mut raw = b"42 (\xff\xfe) ".to_vec();
        raw.extend_from_slice(line("x").split_once(") ").unwrap().1.as_bytes());
        let parsed = imp::parse_stat(42, &raw).expect("raw comm bytes");
        assert_eq!(parsed.identity, ProcessIdentity::new(42, 98765));
        // The numeric fields themselves must still be text.
        let mut bad_tail = b"42 (sh) S 7 ".to_vec();
        bad_tail.push(0xff);
        assert_eq!(
            imp::parse_stat(42, &bad_tail),
            Err(InspectError::InvalidData)
        );
    }
}
