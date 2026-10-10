//! Linux: Landlock for files and TCP, seccomp for the rest.
//!
//! The Landlock ruleset handles every file right the running kernel's ABI
//! knows (1 to 3 add `REFER` and `TRUNCATE`; 5 adds device ioctls) and,
//! from ABI 4, TCP bind and connect; from ABI 6 it scopes signals and
//! abstract Unix sockets to the domain. A right it handles and no rule
//! grants is refused. Rules attach to inodes, through descriptors: a held
//! file is granted as the file it is, so a name moved onto another file
//! grants nothing, and reopening it through `/proc/self/fd` keeps working.
//!
//! The seccomp filter, installed on every thread at once, refuses what
//! Landlock does not cover: creating any socket, truncating, changing a
//! file's mode, owner, extended attributes or times, `O_TRUNC` opens,
//! namespaces and mounts, tracing and reading other processes, BPF, perf,
//! io_uring, keys, leaving the process group, the terminal ioctls that
//! inject input, and, without a program to run, starting any process.
//! `clone3` and `openat2` answer `ENOSYS`, so C libraries fall back to the
//! calls whose arguments the filter can read. A call made under another
//! architecture's numbering ends the process.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use libc::sock_filter;

use super::{Policy, SandboxError};

// Landlock, from <linux/landlock.h>.
const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: u32 = 1;

const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
/// Every right of ABI 1, from executing to making symlinks.
const FS_ABI1: u64 = (1 << 13) - 1;
const FS_REFER: u64 = 1 << 13;
const FS_TRUNCATE: u64 = 1 << 14;
const FS_IOCTL_DEV: u64 = 1 << 15;

const NET_BIND_TCP: u64 = 1 << 0;
const NET_CONNECT_TCP: u64 = 1 << 1;

const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// The system library trees any dynamically linked program loads from, and
/// the loader's cache. Those that exist are granted read only.
const SYSTEM_TREES: [&str; 6] = [
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/usr/local/lib",
    "/etc/ld.so.cache",
];

pub(super) fn confine(policy: &Policy<'_>) -> Result<(), SandboxError> {
    let abi = abi()?;
    let ruleset = Ruleset::new(abi)?;
    for file in &policy.files {
        ruleset.allow_fd(file.as_raw_fd(), FS_READ_FILE)?;
    }
    // What runs a program is granted as one: a script's interpreter, and an
    // ELF program's loader.
    let mut programs = Vec::new();
    for program in &policy.programs {
        let script = super::script_interpreter(program).map_err(|_| SandboxError::Failed)?;
        for program in std::iter::once(program.clone()).chain(script) {
            if let Some(interpreter) = interpreter(&program).map_err(|_| SandboxError::Failed)? {
                programs
                    .push(std::fs::canonicalize(&interpreter).map_err(|_| SandboxError::Failed)?);
            }
            programs.push(program);
        }
    }
    for program in &programs {
        ruleset.allow_path(program, FS_READ_FILE | FS_EXECUTE)?;
    }
    if !programs.is_empty() {
        let system = SYSTEM_TREES.iter().map(PathBuf::from);
        for tree in system.filter(|tree| tree.exists()) {
            ruleset.allow_path(&tree, read_rights(&tree))?;
        }
    }
    for tree in &policy.trees {
        ruleset.allow_path(tree, read_rights(tree))?;
    }
    ruleset.allow_path(Path::new("/dev/null"), FS_READ_FILE | FS_WRITE_FILE)?;
    no_new_privileges()?;
    ruleset.restrict()?;
    install_filter(&filter(
        !policy.programs.is_empty(),
        std::process::id(),
        abi >= 6,
    ))
}

pub(super) fn confine_thread_to_nothing() -> Result<(), SandboxError> {
    let ruleset = Ruleset::new(abi()?)?;
    no_new_privileges()?;
    ruleset.restrict()
}

/// The Landlock ABI the kernel speaks: `Unavailable` when it has none, has
/// it turned off, or a filter above Kettle hides it.
fn abi() -> Result<u32, SandboxError> {
    // SAFETY: the version query takes no attribute and creates nothing.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0_usize,
            CREATE_RULESET_VERSION,
        )
    };
    u32::try_from(abi)
        .ok()
        .filter(|abi| *abi >= 1)
        .ok_or(SandboxError::Unavailable)
}

/// The rights a read-only grant on `path` gives: a directory's files and
/// listings, or a file's bytes.
fn read_rights(path: &Path) -> u64 {
    if path.is_dir() {
        FS_READ_FILE | FS_READ_DIR
    } else {
        FS_READ_FILE
    }
}

/// A Landlock ruleset being built.
struct Ruleset {
    fd: OwnedFd,
    /// The file rights it handles: a rule may grant only these.
    handled: u64,
}

impl Ruleset {
    /// A ruleset handling every right `abi` knows.
    fn new(abi: u32) -> Result<Self, SandboxError> {
        let mut handled = FS_ABI1;
        if abi >= 2 {
            handled |= FS_REFER;
        }
        if abi >= 3 {
            handled |= FS_TRUNCATE;
        }
        if abi >= 5 {
            handled |= FS_IOCTL_DEV;
        }
        let attr = RulesetAttr {
            handled_access_fs: handled,
            handled_access_net: if abi >= 4 {
                NET_BIND_TCP | NET_CONNECT_TCP
            } else {
                0
            },
            scoped: if abi >= 6 {
                SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL
            } else {
                0
            },
        };
        // An older kernel reads only the fields it knows, and refuses a
        // larger structure whose extra fields are not zero.
        let size = match abi {
            1..=3 => 8,
            4 | 5 => 16,
            _ => std::mem::size_of::<RulesetAttr>(),
        };
        // SAFETY: `attr` outlives the call and is at least `size` bytes.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const RulesetAttr,
                size,
                0_u32,
            )
        };
        let fd = i32::try_from(fd)
            .ok()
            .filter(|fd| *fd >= 0)
            .ok_or(SandboxError::Failed)?;
        // SAFETY: the call returned a new descriptor this ruleset owns.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            handled,
        })
    }

    /// Grant `rights` beneath the open descriptor `fd`: a file's own, or a
    /// directory's and everything in it.
    fn allow_fd(&self, fd: i32, rights: u64) -> Result<(), SandboxError> {
        let rule = PathBeneathAttr {
            allowed_access: rights & self.handled,
            parent_fd: fd,
        };
        // SAFETY: `rule` outlives the call, and both descriptors are open.
        let status = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.fd.as_raw_fd(),
                RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0_u32,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(SandboxError::Failed)
        }
    }

    /// Grant `rights` beneath `path`, opened only to name it.
    fn allow_path(&self, path: &Path, rights: u64) -> Result<(), SandboxError> {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| SandboxError::Failed)?;
        // SAFETY: the name is NUL-terminated; an O_PATH open reads nothing.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(SandboxError::Failed);
        }
        // SAFETY: `open` returned a new descriptor owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        self.allow_fd(fd.as_raw_fd(), rights)
    }

    /// Confine the calling thread, and what it starts, to the rules.
    fn restrict(self) -> Result<(), SandboxError> {
        // SAFETY: the ruleset descriptor is open; no flags.
        let status =
            unsafe { libc::syscall(libc::SYS_landlock_restrict_self, self.fd.as_raw_fd(), 0_u32) };
        if status == 0 {
            Ok(())
        } else {
            Err(SandboxError::Failed)
        }
    }
}

/// Give up gaining privileges through exec, which an unprivileged process
/// must before Landlock or seccomp.
fn no_new_privileges() -> Result<(), SandboxError> {
    let (one, zero): (libc::c_ulong, libc::c_ulong) = (1, 0);
    // SAFETY: PR_SET_NO_NEW_PRIVS reads only its integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero) } == 0 {
        Ok(())
    } else {
        Err(SandboxError::Failed)
    }
}

/// The ELF interpreter `program` names, if it is dynamically linked: a
/// 64-bit little-endian ELF file, as both supported architectures run.
fn interpreter(program: &Path) -> io::Result<Option<PathBuf>> {
    const PT_INTERP: u32 = 3;
    const PHDR_SIZE: usize = 56;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a 64-bit ELF program");
    let file = File::open(program)?;
    let mut header = [0_u8; 64];
    let mut read = 0;
    while read < header.len() {
        match file.read_at(&mut header[read..], read as u64)? {
            0 => break,
            more => read += more,
        }
    }
    // A script's own interpreter is found from its `#!` line instead, however
    // short the script.
    if header[..read].starts_with(b"#!") {
        return Ok(None);
    }
    if read < header.len() || &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return Err(invalid());
    }
    let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let phoff = u64::from_le_bytes(header[32..40].try_into().expect("eight bytes"));
    if usize::from(u16_at(54)) != PHDR_SIZE || u16_at(56) > 128 {
        return Err(invalid());
    }
    for index in 0..u64::from(u16_at(56)) {
        let mut phdr = [0_u8; PHDR_SIZE];
        file.read_exact_at(&mut phdr, phoff + index * PHDR_SIZE as u64)?;
        if u32::from_le_bytes(phdr[..4].try_into().expect("four bytes")) != PT_INTERP {
            continue;
        }
        let offset = u64::from_le_bytes(phdr[8..16].try_into().expect("eight bytes"));
        let size = u64::from_le_bytes(phdr[32..40].try_into().expect("eight bytes"));
        if size == 0 || size > 4096 {
            return Err(invalid());
        }
        let mut name = vec![0_u8; size as usize];
        file.read_exact_at(&mut name, offset)?;
        let name = name.strip_suffix(&[0]).unwrap_or(&name);
        if name.is_empty() || name.contains(&0) {
            return Err(invalid());
        }
        return Ok(Some(PathBuf::from(std::ffi::OsStr::from_bytes(name))));
    }
    Ok(None)
}

#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000_003e; // AUDIT_ARCH_X86_64
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc000_00b7; // AUDIT_ARCH_AARCH64
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Offsets into `struct seccomp_data`; both architectures are little-endian,
/// so an argument's low half comes first.
const NR: u32 = 0;
const ARCH_OFFSET: u32 = 4;
const fn arg_low(index: u32) -> u32 {
    16 + 8 * index
}

const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
const EPERM: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
const ENOSYS: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
const ENOTTY: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOTTY as u32;
const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;

/// Calls numbered alike on every architecture since Linux 5.1.
mod unified {
    pub(super) const PIDFD_SEND_SIGNAL: i64 = 424;
    pub(super) const IO_URING_SETUP: i64 = 425;
    pub(super) const IO_URING_ENTER: i64 = 426;
    pub(super) const IO_URING_REGISTER: i64 = 427;
    pub(super) const OPEN_TREE: i64 = 428;
    pub(super) const MOVE_MOUNT: i64 = 429;
    pub(super) const FSOPEN: i64 = 430;
    pub(super) const FSCONFIG: i64 = 431;
    pub(super) const FSMOUNT: i64 = 432;
    pub(super) const FSPICK: i64 = 433;
    pub(super) const CLONE3: i64 = 435;
    pub(super) const OPENAT2: i64 = 437;
    pub(super) const PIDFD_GETFD: i64 = 438;
    pub(super) const MOUNT_SETATTR: i64 = 442;
    pub(super) const FCHMODAT2: i64 = 452;
    pub(super) const SETXATTRAT: i64 = 463;
    pub(super) const REMOVEXATTRAT: i64 = 466;
    pub(super) const OPEN_TREE_ATTR: i64 = 467;
}

/// The calls refused outright, with each one's answer. Process creation is
/// refused only when the job runs no program.
fn refused(runs_programs: bool) -> Vec<(i64, u32)> {
    let mut refused = vec![
        (libc::SYS_socket, EPERM),
        (libc::SYS_setsid, EPERM),
        (libc::SYS_setpgid, EPERM),
        (libc::SYS_truncate, EPERM),
        (libc::SYS_ftruncate, EPERM),
        (libc::SYS_fchmod, EPERM),
        (libc::SYS_fchmodat, EPERM),
        (unified::FCHMODAT2, EPERM),
        (libc::SYS_fchown, EPERM),
        (libc::SYS_fchownat, EPERM),
        (libc::SYS_setxattr, EPERM),
        (libc::SYS_lsetxattr, EPERM),
        (libc::SYS_fsetxattr, EPERM),
        (libc::SYS_removexattr, EPERM),
        (libc::SYS_lremovexattr, EPERM),
        (libc::SYS_fremovexattr, EPERM),
        (unified::SETXATTRAT, EPERM),
        (unified::REMOVEXATTRAT, EPERM),
        (libc::SYS_utimensat, EPERM),
        (libc::SYS_unshare, EPERM),
        (libc::SYS_setns, EPERM),
        (libc::SYS_mount, EPERM),
        (libc::SYS_umount2, EPERM),
        (libc::SYS_pivot_root, EPERM),
        (libc::SYS_chroot, EPERM),
        (unified::OPEN_TREE, EPERM),
        (unified::OPEN_TREE_ATTR, EPERM),
        (unified::MOVE_MOUNT, EPERM),
        (unified::FSOPEN, EPERM),
        (unified::FSCONFIG, EPERM),
        (unified::FSMOUNT, EPERM),
        (unified::FSPICK, EPERM),
        (unified::MOUNT_SETATTR, EPERM),
        (libc::SYS_ptrace, EPERM),
        (libc::SYS_process_vm_readv, EPERM),
        (libc::SYS_process_vm_writev, EPERM),
        (unified::PIDFD_GETFD, EPERM),
        (libc::SYS_bpf, EPERM),
        (libc::SYS_perf_event_open, EPERM),
        (libc::SYS_userfaultfd, EPERM),
        (libc::SYS_keyctl, EPERM),
        (libc::SYS_add_key, EPERM),
        (libc::SYS_request_key, EPERM),
        (unified::IO_URING_SETUP, EPERM),
        (unified::IO_URING_ENTER, EPERM),
        (unified::IO_URING_REGISTER, EPERM),
        (unified::CLONE3, ENOSYS),
        (unified::OPENAT2, ENOSYS),
        // No socket reaching anything: a pair has its own check.
        (libc::SYS_connect, EPERM),
        (libc::SYS_bind, EPERM),
        (libc::SYS_listen, EPERM),
        (libc::SYS_accept, EPERM),
        (libc::SYS_accept4, EPERM),
        // System V IPC, which no file right reaches.
        (libc::SYS_shmget, EPERM),
        (libc::SYS_shmat, EPERM),
        (libc::SYS_shmctl, EPERM),
        (libc::SYS_shmdt, EPERM),
        (libc::SYS_msgget, EPERM),
        (libc::SYS_msgsnd, EPERM),
        (libc::SYS_msgrcv, EPERM),
        (libc::SYS_msgctl, EPERM),
        (libc::SYS_semget, EPERM),
        (libc::SYS_semop, EPERM),
        (libc::SYS_semtimedop, EPERM),
        (libc::SYS_semctl, EPERM),
        // A signal through a process descriptor, which nothing here needs.
        (unified::PIDFD_SEND_SIGNAL, EPERM),
    ];
    #[cfg(target_arch = "x86_64")]
    refused.extend([
        (libc::SYS_chmod, EPERM),
        (libc::SYS_chown, EPERM),
        (libc::SYS_lchown, EPERM),
        (libc::SYS_utime, EPERM),
        (libc::SYS_utimes, EPERM),
        (libc::SYS_futimesat, EPERM),
        (libc::SYS_creat, EPERM),
    ]);
    if !runs_programs {
        refused.extend([(libc::SYS_execve, EPERM), (libc::SYS_execveat, EPERM)]);
        #[cfg(target_arch = "x86_64")]
        refused.extend([(libc::SYS_fork, EPERM), (libc::SYS_vfork, EPERM)]);
    }
    refused
}

/// A jump's target, resolved when the program is laid out.
#[derive(Clone, Copy)]
enum To {
    Next,
    Label(&'static str),
}

enum Op {
    Load(u32),
    /// AND the accumulator with a constant.
    And(u32),
    Ret(u32),
    Jump {
        code: u32,
        k: u32,
        yes: To,
        no: To,
    },
    Label(&'static str),
}

/// Lay `ops` out as a BPF program, each jump's targets turned into offsets.
/// Jumps go forward only and never past 255 instructions, or the program is
/// refused.
fn assemble(ops: &[Op]) -> Option<Vec<sock_filter>> {
    let mut labels = std::collections::HashMap::new();
    let mut index = 0_usize;
    for op in ops {
        match op {
            Op::Label(name) => {
                labels.insert(*name, index);
            }
            _ => index += 1,
        }
    }
    let mut program = Vec::with_capacity(index);
    for op in ops {
        let here = program.len();
        let offset = |to: To| -> Option<u8> {
            match to {
                To::Next => Some(0),
                To::Label(name) => u8::try_from(labels.get(name)?.checked_sub(here + 1)?).ok(),
            }
        };
        let (code, jt, jf, k) = match op {
            Op::Label(_) => continue,
            Op::Load(offset) => (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, *offset),
            Op::And(mask) => (libc::BPF_ALU | libc::BPF_AND | libc::BPF_K, 0, 0, *mask),
            Op::Ret(action) => (libc::BPF_RET | libc::BPF_K, 0, 0, *action),
            Op::Jump { code, k, yes, no } => (
                libc::BPF_JMP | code | libc::BPF_K,
                offset(*yes)?,
                offset(*no)?,
                *k,
            ),
        };
        program.push(sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        });
    }
    Some(program)
}

fn jeq(k: u32, yes: To, no: To) -> Op {
    Op::Jump {
        code: libc::BPF_JEQ,
        k,
        yes,
        no,
    }
}

fn jset(k: u32, yes: To, no: To) -> Op {
    Op::Jump {
        code: libc::BPF_JSET,
        k,
        yes,
        no,
    }
}

/// The worker's filter, for the process `me`, where Landlock does or does not
/// already `scope` signals to the sandbox. Its shape: check the
/// architecture, load the call number, refuse the x32 range on x86_64, send
/// the calls whose arguments matter to their checks, answer each refused
/// call, allow the rest.
fn filter(runs_programs: bool, me: u32, scoped: bool) -> Vec<sock_filter> {
    const NAMESPACES: u32 = (libc::CLONE_NEWNS
        | libc::CLONE_NEWUSER
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP) as u32;
    let mut ops = vec![
        Op::Load(ARCH_OFFSET),
        jeq(ARCH, To::Next, To::Label("die")),
        Op::Load(NR),
    ];
    #[cfg(target_arch = "x86_64")]
    ops.push(Op::Jump {
        code: libc::BPF_JGE,
        k: X32_SYSCALL_BIT,
        yes: To::Label("eperm"),
        no: To::Next,
    });
    ops.push(jeq(libc::SYS_clone as u32, To::Label("clone"), To::Next));
    ops.push(jeq(libc::SYS_openat as u32, To::Label("openat"), To::Next));
    #[cfg(target_arch = "x86_64")]
    ops.push(jeq(libc::SYS_open as u32, To::Label("open"), To::Next));
    ops.push(jeq(libc::SYS_ioctl as u32, To::Label("ioctl"), To::Next));
    ops.push(jeq(
        libc::SYS_socketpair as u32,
        To::Label("pair"),
        To::Next,
    ));
    // Without Landlock's scoping, a signal stays inside the worker's
    // process group: any other process id, or a thread id, could be
    // Kettle's own. A decoder, which inherits this, cannot send a signal
    // directed at itself or one of its threads, only one to the whole
    // group; and the worker cannot kill a decoder, which goes with the
    // worker's process group.
    if !scoped {
        ops.push(jeq(libc::SYS_kill as u32, To::Label("kill"), To::Next));
        for targeted in [
            libc::SYS_tgkill,
            libc::SYS_rt_sigqueueinfo,
            libc::SYS_rt_tgsigqueueinfo,
        ] {
            ops.push(jeq(targeted as u32, To::Label("own"), To::Next));
        }
        ops.push(jeq(libc::SYS_tkill as u32, To::Label("eperm"), To::Next));
    }
    for (number, answer) in refused(runs_programs) {
        ops.push(jeq(
            number as u32,
            To::Label(answer_label(answer)),
            To::Next,
        ));
    }
    ops.push(Op::Ret(ALLOW));
    // A thread shares everything; a process may be started only to run a
    // program; neither may make a namespace.
    ops.push(Op::Label("clone"));
    ops.push(Op::Load(arg_low(0)));
    ops.push(jset(NAMESPACES, To::Label("eperm"), To::Next));
    ops.push(jset(
        libc::CLONE_THREAD as u32,
        To::Label("allow"),
        To::Next,
    ));
    ops.push(Op::Ret(if runs_programs { ALLOW } else { EPERM }));
    // Opening to truncate is refused, whatever Landlock's ABI.
    ops.push(Op::Label("openat"));
    ops.push(Op::Load(arg_low(2)));
    ops.push(jset(
        libc::O_TRUNC as u32,
        To::Label("eperm"),
        To::Label("allow"),
    ));
    #[cfg(target_arch = "x86_64")]
    {
        ops.push(Op::Label("open"));
        ops.push(Op::Load(arg_low(1)));
        ops.push(jset(
            libc::O_TRUNC as u32,
            To::Label("eperm"),
            To::Label("allow"),
        ));
    }
    // Only the ioctls that ask about a descriptor or set its own flags; any
    // other, such as one changing a file's attributes or pushing input into
    // a terminal, is not supported here.
    ops.push(Op::Label("ioctl"));
    ops.push(Op::Load(arg_low(1)));
    for request in [
        libc::TCGETS,
        libc::TIOCGWINSZ,
        libc::FIONREAD,
        libc::FIONBIO,
        libc::FIOCLEX,
        libc::FIONCLEX,
    ] {
        ops.push(jeq(request as u32, To::Label("allow"), To::Next));
    }
    ops.push(Op::Ret(ENOTTY));
    // `kill`: only the caller's own process group, the worker and its
    // decoders, the caller included.
    ops.push(Op::Label("kill"));
    ops.push(Op::Load(arg_low(0)));
    ops.push(jeq(0, To::Label("allow"), To::Label("eperm")));
    // A thread- or queue-directed signal: only to the worker itself.
    ops.push(Op::Label("own"));
    ops.push(Op::Load(arg_low(0)));
    ops.push(jeq(me, To::Label("allow"), To::Label("eperm")));
    // A connected pair of Unix sockets, as the standard library's spawn
    // makes to hear about a failed exec: a stream or sequenced-packet pair
    // can reach nothing but itself. A datagram pair could send to a named
    // socket, and is refused.
    ops.push(Op::Label("pair"));
    ops.push(Op::Load(arg_low(0)));
    ops.push(jeq(libc::AF_UNIX as u32, To::Next, To::Label("eperm")));
    ops.push(Op::Load(arg_low(1)));
    ops.push(Op::And(0xff));
    ops.push(jeq(libc::SOCK_STREAM as u32, To::Label("allow"), To::Next));
    ops.push(jeq(
        libc::SOCK_SEQPACKET as u32,
        To::Label("allow"),
        To::Label("eperm"),
    ));
    ops.push(Op::Label("allow"));
    ops.push(Op::Ret(ALLOW));
    ops.push(Op::Label("enosys"));
    ops.push(Op::Ret(ENOSYS));
    ops.push(Op::Label("eperm"));
    ops.push(Op::Ret(EPERM));
    ops.push(Op::Label("die"));
    ops.push(Op::Ret(KILL));
    assemble(&ops).expect("the worker filter's jumps fit")
}

fn answer_label(answer: u32) -> &'static str {
    if answer == ENOSYS { "enosys" } else { "eperm" }
}

/// Install `filter` on every thread of the process at once.
fn install_filter(filter: &[sock_filter]) -> Result<(), SandboxError> {
    let program = libc::sock_fprog {
        len: u16::try_from(filter.len()).map_err(|_| SandboxError::Failed)?,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: `program` points at `filter`, which outlives the call; the
    // kernel copies it before returning. A thread it could not synchronize
    // makes the call fail without installing anything.
    let status = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &program as *const libc::sock_fprog,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(SandboxError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every jump of the filter lands inside it, on a later instruction, and
    /// it ends in a return, with programs and without.
    #[test]
    fn the_filter_is_well_formed() {
        for (runs_programs, scoped) in [(false, false), (true, false), (false, true)] {
            let program = filter(runs_programs, std::process::id(), scoped);
            for (index, op) in program.iter().enumerate() {
                if u32::from(op.code) & 0x07 == libc::BPF_JMP {
                    for offset in [op.jt, op.jf] {
                        assert!(index + 1 + usize::from(offset) < program.len());
                    }
                }
            }
            let last = program.last().unwrap();
            assert_eq!(u32::from(last.code), libc::BPF_RET | libc::BPF_K);
        }
    }

    /// What `program` answers for a call `nr` with `args`, as the kernel
    /// runs it: loads of the number, architecture and argument words, the
    /// jumps it uses, and returns.
    fn run(program: &[sock_filter], arch: u32, nr: u32, args: [u64; 6]) -> u32 {
        let word = |offset: u32| -> u32 {
            match offset {
                NR => nr,
                ARCH_OFFSET => arch,
                offset if (16..64).contains(&offset) => {
                    let arg = args[((offset - 16) / 8) as usize];
                    if (offset - 16) % 8 == 0 {
                        arg as u32
                    } else {
                        (arg >> 32) as u32
                    }
                }
                other => panic!("load from {other}"),
            }
        };
        let (mut pc, mut accumulator) = (0_usize, 0_u32);
        loop {
            let op = program[pc];
            let code = u32::from(op.code);
            if code == libc::BPF_LD | libc::BPF_W | libc::BPF_ABS {
                accumulator = word(op.k);
            } else if code == libc::BPF_ALU | libc::BPF_AND | libc::BPF_K {
                accumulator &= op.k;
            } else if code == libc::BPF_RET | libc::BPF_K {
                return op.k;
            } else {
                // Only constant-operand jumps of the four tests it uses.
                let test = code & !(libc::BPF_JMP | libc::BPF_K);
                assert_eq!(code & 0x07, libc::BPF_JMP, "opcode {code:#x}");
                assert_eq!(code & 0x08, libc::BPF_K, "operand of {code:#x}");
                let taken = match test {
                    test if test == libc::BPF_JEQ => accumulator == op.k,
                    test if test == libc::BPF_JGT => accumulator > op.k,
                    test if test == libc::BPF_JGE => accumulator >= op.k,
                    test if test == libc::BPF_JSET => accumulator & op.k != 0,
                    other => panic!("jump test {other:#x}"),
                };
                pc += usize::from(if taken { op.jt } else { op.jf });
            }
            pc += 1;
        }
    }

    /// The filter answers each call it judges as the sandbox promises.
    #[test]
    fn the_filter_answers_as_promised() {
        let me = 4000_u32;
        let quiet = filter(false, me, false);
        let runs = filter(true, me, false);
        let scoped = filter(false, me, true);
        let call =
            |program: &[sock_filter], nr: i64, args: [u64; 6]| run(program, ARCH, nr as u32, args);
        let none = [0_u64; 6];
        let first = |value: u64| [value, 0, 0, 0, 0, 0];
        let second = |value: u64| [0, value, 0, 0, 0, 0];
        for (name, nr) in [
            ("socket", libc::SYS_socket),
            ("connect", libc::SYS_connect),
            ("shmget", libc::SYS_shmget),
            ("shmat", libc::SYS_shmat),
            ("semop", libc::SYS_semop),
            ("msgsnd", libc::SYS_msgsnd),
            ("setsid", libc::SYS_setsid),
            ("fchmod", libc::SYS_fchmod),
            ("ptrace", libc::SYS_ptrace),
            ("pidfd_send_signal", unified::PIDFD_SEND_SIGNAL),
        ] {
            assert_eq!(call(&quiet, nr, none), EPERM, "{name}");
        }
        assert_eq!(call(&quiet, unified::CLONE3, none), ENOSYS);
        // A connected Unix pair only: never a datagram pair, nor another
        // family.
        let pair = |family: i32, kind: i32| [family as u64, kind as u64, 0, 0, 0, 0];
        let cloexec = libc::SOCK_CLOEXEC;
        for kind in [libc::SOCK_SEQPACKET | cloexec, libc::SOCK_STREAM] {
            assert_eq!(
                call(&quiet, libc::SYS_socketpair, pair(libc::AF_UNIX, kind)),
                ALLOW
            );
        }
        assert_eq!(
            call(
                &quiet,
                libc::SYS_socketpair,
                pair(libc::AF_UNIX, libc::SOCK_DGRAM | cloexec)
            ),
            EPERM
        );
        assert_eq!(
            call(
                &quiet,
                libc::SYS_socketpair,
                pair(libc::AF_INET, libc::SOCK_STREAM)
            ),
            EPERM
        );
        assert_eq!(call(&quiet, libc::SYS_read, none), ALLOW);
        // ioctl: descriptor queries and own flags only.
        for request in [libc::TCGETS, libc::FIONREAD, libc::FIOCLEX] {
            assert_eq!(call(&quiet, libc::SYS_ioctl, second(request)), ALLOW);
        }
        const FS_IOC_SETFLAGS: u64 = 0x4008_6602;
        for request in [FS_IOC_SETFLAGS, libc::TIOCSTI, libc::TIOCLINUX] {
            assert_eq!(
                call(&quiet, libc::SYS_ioctl, second(request)),
                ENOTTY,
                "{request:#x}"
            );
        }
        // Signals, where Landlock cannot scope them: only the worker's own
        // group by `kill`, only the worker itself by `tgkill` and queued
        // signals, never by thread id. Where it can, the filter leaves
        // signals to it.
        assert_eq!(call(&quiet, libc::SYS_kill, first(0)), ALLOW);
        for other in [me, me + 7, me - 1, 2, u64::MAX as u32] {
            assert_eq!(
                call(&quiet, libc::SYS_kill, first(u64::from(other))),
                EPERM,
                "{other}"
            );
        }
        assert_eq!(
            call(&quiet, libc::SYS_kill, first(u64::MAX)),
            EPERM,
            "-1, everything"
        );
        for targeted in [
            libc::SYS_tgkill,
            libc::SYS_rt_sigqueueinfo,
            libc::SYS_rt_tgsigqueueinfo,
        ] {
            assert_eq!(call(&quiet, targeted, first(u64::from(me))), ALLOW);
            assert_eq!(call(&quiet, targeted, first(u64::from(me) + 7)), EPERM);
        }
        assert_eq!(call(&quiet, libc::SYS_tkill, first(u64::from(me))), EPERM);
        assert_eq!(
            call(&scoped, libc::SYS_kill, first(u64::from(me) + 7)),
            ALLOW
        );
        assert_eq!(
            call(&scoped, libc::SYS_tkill, first(u64::from(me) + 1)),
            ALLOW
        );
        // Processes only to run a program, threads always, namespaces never.
        let thread = (libc::CLONE_VM | libc::CLONE_THREAD) as u64;
        assert_eq!(call(&quiet, libc::SYS_clone, first(thread)), ALLOW);
        assert_eq!(
            call(&quiet, libc::SYS_clone, first(libc::SIGCHLD as u64)),
            EPERM
        );
        assert_eq!(
            call(&runs, libc::SYS_clone, first(libc::SIGCHLD as u64)),
            ALLOW
        );
        assert_eq!(
            call(&runs, libc::SYS_clone, first(libc::CLONE_NEWUSER as u64)),
            EPERM
        );
        assert_eq!(call(&quiet, libc::SYS_execve, none), EPERM);
        assert_eq!(call(&runs, libc::SYS_execve, none), ALLOW);
        // No truncating open.
        let flags = |value: i32| [0, 0, value as u64, 0, 0, 0];
        assert_eq!(call(&quiet, libc::SYS_openat, flags(libc::O_RDONLY)), ALLOW);
        assert_eq!(
            call(
                &quiet,
                libc::SYS_openat,
                flags(libc::O_WRONLY | libc::O_TRUNC)
            ),
            EPERM
        );
        // Another architecture's numbering ends the process.
        assert_eq!(run(&quiet, ARCH ^ 1, libc::SYS_read as u32, none), KILL);
    }

    /// The interpreter of a dynamically linked system program is found, and
    /// a file that is not an ELF program is refused.
    #[test]
    fn a_program_names_its_interpreter() {
        let cat = std::fs::canonicalize("/bin/cat").unwrap();
        let interpreter = interpreter(&cat).unwrap().expect("dynamically linked");
        assert!(interpreter.is_absolute(), "{interpreter:?}");
        assert!(std::fs::canonicalize(&interpreter).unwrap().is_file());
        let text = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            text.path(),
            b"plain text, not a program, padded past sixty-four bytes long",
        )
        .unwrap();
        assert!(super::interpreter(text.path()).is_err());
        // A script shorter than an ELF header is a script all the same.
        std::fs::write(text.path(), b"#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(super::interpreter(text.path()).unwrap(), None);
        assert_eq!(
            super::super::script_interpreter(text.path()).unwrap(),
            Some(std::fs::canonicalize("/bin/sh").unwrap())
        );
    }
}
