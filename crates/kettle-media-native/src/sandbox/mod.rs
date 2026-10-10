//! The worker's self-sandbox: once a job's own files are open, and before
//! any byte of its media or fonts is parsed, the worker confines itself to
//! them. A decoder it starts inherits the confinement.
//!
//! What a confined job may still do is a [`Policy`]: read the files it holds
//! (its source and its fallback fonts), run the trusted external decoder's
//! programs, if it has one, and read the root-owned trees those programs
//! load from. Nothing else: no other file read, no write but `/dev/null`, no
//! network, no other program, no leaving its process group, no changing a
//! file's mode, owner, attributes or times.
//!
//! On Linux, Landlock confines file access (ABI 1 and later, every right
//! the running kernel knows handled, so a right the policy does not grant is
//! refused) and, from ABI 4, TCP; a seccomp filter on every thread refuses
//! what Landlock does not cover. On macOS, a deny-by-default Seatbelt
//! profile, applied with `sandbox_init_with_parameters`, covers the whole
//! process. A system without either, or a confinement that does not apply,
//! is [`SandboxError`]: the job is refused, never run unconfined.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::fs::File;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

/// What a confined job may still do.
#[derive(Debug, Default)]
pub struct Policy<'a> {
    /// Files it reads, already open: its source, its fallback fonts. Each is
    /// granted as the file it is, not by its name, where the platform can.
    pub files: Vec<&'a File>,
    /// Programs it may run, by canonical path: a trusted decoder's ffmpeg
    /// and ffprobe. Without any, it starts no process at all.
    pub programs: Vec<PathBuf>,
    /// Root-owned trees those programs load libraries and data from, read
    /// only, by canonical path. The platform's own system library trees are
    /// added for any program.
    pub trees: Vec<PathBuf>,
}

/// Why a job could not be confined. It then does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxError {
    /// This system has no sandbox Kettle can use: a Linux kernel without
    /// Landlock (or with it turned off), or a macOS without
    /// `sandbox_init_with_parameters`.
    Unavailable,
    /// The sandbox exists but could not be built or applied.
    Failed,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "this system has no sandbox Kettle can use",
            Self::Failed => "the sandbox could not be applied",
        })
    }
}

impl std::error::Error for SandboxError {}

/// The interpreter a script names on its `#!` line, when `program` is one:
/// running the script runs it, so a policy grants it as a program too. A
/// program that is not a script has none.
fn script_interpreter(program: &std::path::Path) -> std::io::Result<Option<PathBuf>> {
    use std::io::Read as _;
    let mut head = [0_u8; 256];
    let read = File::open(program)?.read(&mut head)?;
    let Some(line) = head[..read].strip_prefix(b"#!") else {
        return Ok(None);
    };
    let line = line.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let interpreter = line
        .split(|byte| *byte == b' ' || *byte == b'\t')
        .find(|word| !word.is_empty())
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    let interpreter = std::path::Path::new(std::ffi::OsStr::from_bytes(interpreter));
    if !interpreter.is_absolute() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    Ok(Some(std::fs::canonicalize(interpreter)?))
}

/// Confine the calling thread, the threads it starts and the processes they
/// start (Linux), or the whole process (macOS), to `policy`, for good.
pub fn confine(policy: &Policy<'_>) -> Result<(), SandboxError> {
    #[cfg(target_os = "linux")]
    return linux::confine(policy);
    #[cfg(target_os = "macos")]
    return macos::confine(policy);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = policy;
        Err(SandboxError::Unavailable)
    }
}

/// Confine the calling thread to nothing at all: no file, no network, no
/// program. For a thread started before the job's policy exists, such as
/// the worker's watchdog, which then only waits and ends the process. On
/// macOS the job's policy covers every thread, so this does nothing there.
pub fn confine_thread_to_nothing() -> Result<(), SandboxError> {
    #[cfg(target_os = "linux")]
    return linux::confine_thread_to_nothing();
    #[cfg(not(target_os = "linux"))]
    Ok(())
}
