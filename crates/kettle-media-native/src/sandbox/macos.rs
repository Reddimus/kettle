//! macOS: a deny-by-default Seatbelt profile over the whole process,
//! applied with `sandbox_init_with_parameters`. Apple's own services apply
//! profiles this way, but the SDK no longer declares the call, so it is
//! looked up when needed; a system without it is `Unavailable`.
//!
//! The profile's text is fixed fragments only. Every path in it is a
//! parameter, so no file name can change what the profile says. Beyond the
//! job's own grants it allows what AVFoundation needs to decode, as measured
//! on macOS 27: reading the system's frameworks, libraries and shared cache
//! (`/System/Library`, never all of `/System`, whose `Volumes/Data` holds
//! the users' homes),
//! file metadata (names and sizes, never contents), sysctl reads, Apple's
//! video decoder service and the IOSurface client its frames come back
//! through, and `/dev/null`; and what dyld needs to start a program the job
//! may run: opening `/`, mapping system code, and registering signatures. It refuses `setsid` and `setpgid`, so a decoder
//! cannot leave the worker's process group, which the worker's children
//! inherit with the rest of the profile.

use std::ffi::{CStr, CString, c_char, c_int};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use super::{Policy, SandboxError};

/// What every confined job may do.
const BASE: &str = r#"(version 1)
(deny default)
(allow file-read*
  (subpath "/System/Library")
  (subpath "/usr/lib")
  (subpath "/Library/Apple")
  (subpath "/private/var/db/dyld")
  (subpath "/System/Volumes/Preboot/Cryptexes"))
(allow file-read-metadata)
(allow file-read* (literal "/"))
(allow file-read* file-map-executable
  (subpath "/System/Cryptexes")
  (subpath "/System/Volumes/Preboot/Cryptexes"))
(allow file-map-executable
  (subpath "/System/Library")
  (subpath "/usr/lib")
  (subpath "/Library/Apple"))
(allow system-fcntl (fcntl-command F_ADDFILESIGS_RETURN F_CHECK_LV F_GETPATH))
(allow sysctl-read)
(allow iokit-open-user-client (iokit-user-client-class "IOSurfaceRootUserClient"))
(allow mach-lookup (xpc-service-name "com.apple.coremedia.videodecoder"))
(allow file-read-data file-write-data (literal "/dev/null"))
(allow file-read* (subpath "/dev/fd"))
(allow signal (target same-sandbox))
(deny syscall-unix (syscall-number SYS_setsid SYS_setpgid))
(deny syscall-unix (syscall-number
  SYS_shmget SYS_shmat SYS_shmctl SYS_shmdt SYS_shmsys
  SYS_semget SYS_semop SYS_semctl SYS_semsys
  SYS_msgget SYS_msgsnd SYS_msgrcv SYS_msgctl SYS_msgsys
  SYS_msgsnd_nocancel SYS_msgrcv_nocancel))
"#;

type InitWithParameters =
    unsafe extern "C" fn(*const c_char, u64, *const *const c_char, *mut *mut c_char) -> c_int;
type FreeError = unsafe extern "C" fn(*mut c_char);

pub(super) fn confine(policy: &Policy<'_>) -> Result<(), SandboxError> {
    let init = lookup::<InitWithParameters>(c"sandbox_init_with_parameters")
        .ok_or(SandboxError::Unavailable)?;
    let (profile, parameters) = profile(policy)?;
    let profile = CString::new(profile).map_err(|_| SandboxError::Failed)?;
    let parameters = parameters
        .into_iter()
        .flat_map(|(name, value)| [name.into_bytes(), value])
        .map(CString::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SandboxError::Failed)?;
    let mut pointers: Vec<*const c_char> = parameters.iter().map(|value| value.as_ptr()).collect();
    pointers.push(std::ptr::null());
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: the profile and every parameter are NUL-terminated and outlive
    // the call; the list ends in NULL; `error` receives an allocation the
    // call makes only on failure.
    let status = unsafe { init(profile.as_ptr(), 0, pointers.as_ptr(), &mut error) };
    if !error.is_null()
        && let Some(free) = lookup::<FreeError>(c"sandbox_free_error")
    {
        // SAFETY: `error` came from the call above and is freed once.
        unsafe { free(error) };
    }
    if status == 0 {
        Ok(())
    } else {
        Err(SandboxError::Failed)
    }
}

/// A profile's parameters: each name and the path it stands for.
type Parameters = Vec<(String, Vec<u8>)>;

/// The profile for `policy`, and the parameters its paths come in.
fn profile(policy: &Policy<'_>) -> Result<(String, Parameters), SandboxError> {
    let mut profile = String::from(BASE);
    let mut parameters = Vec::new();
    // The frameworks read the worker's own program and the directory it is
    // in, which hold nothing of the user's.
    if let Ok(me) = std::env::current_exe().and_then(std::fs::canonicalize) {
        profile.push_str(
            "(allow file-read* (literal (param \"SELF\")) (literal (param \"SELF_DIR\")))\n",
        );
        parameters.push(("SELF_DIR".into(), path_bytes(me.parent().unwrap_or(&me))));
        parameters.push(("SELF".into(), path_bytes(&me)));
    }
    for (index, file) in policy.files.iter().enumerate() {
        let path = descriptor_path(file.as_raw_fd())?;
        let (name, fd) = (format!("FILE{index}"), format!("FD{index}"));
        profile.push_str(&format!(
            "(allow file-read* (literal (param \"{name}\")) (literal (param \"{fd}\")))\n"
        ));
        parameters.push((name, path));
        parameters.push((fd, format!("/dev/fd/{}", file.as_raw_fd()).into_bytes()));
    }
    for (index, program) in policy.programs.iter().enumerate() {
        let name = format!("PROGRAM{index}");
        profile.push_str(&format!(
            "(allow process-exec file-read* file-map-executable (literal (param \"{name}\")))\n"
        ));
        parameters.push((name, path_bytes(program)));
        if let Some(interpreter) =
            super::script_interpreter(program).map_err(|_| SandboxError::Failed)?
        {
            let name = format!("INTERPRETER{index}");
            profile.push_str(&format!(
                "(allow process-exec-interpreter file-read* file-map-executable (literal (param \"{name}\")))\n"
            ));
            parameters.push((name, path_bytes(&interpreter)));
        }
    }
    if !policy.programs.is_empty() {
        profile.push_str("(allow process-fork)\n");
    }
    for (index, tree) in policy.trees.iter().enumerate() {
        let name = format!("TREE{index}");
        profile.push_str(&format!(
            "(allow file-read* file-map-executable (subpath (param \"{name}\")))\n"
        ));
        parameters.push((name, path_bytes(tree)));
    }
    Ok((profile, parameters))
}

fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

/// The path the open descriptor `fd` names now.
fn descriptor_path(fd: c_int) -> Result<Vec<u8>, SandboxError> {
    let mut path = vec![0 as c_char; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL included, into the
    // buffer, which is that long.
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) } == -1 {
        return Err(SandboxError::Failed);
    }
    // SAFETY: the call wrote a NUL-terminated path into the buffer.
    let path = unsafe { CStr::from_ptr(path.as_ptr()) };
    Ok(path.to_bytes().to_vec())
}

/// The function `name` in the loaded libraries, if there is one.
fn lookup<F: Copy>(name: &CStr) -> Option<F> {
    // SAFETY: dlsym with RTLD_DEFAULT only searches the loaded images.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    debug_assert_eq!(
        std::mem::size_of::<F>(),
        std::mem::size_of::<*mut libc::c_void>()
    );
    // SAFETY: `F` is the function pointer type the symbol has.
    Some(unsafe { std::mem::transmute_copy::<*mut libc::c_void, F>(&symbol) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile is the fixed base and fixed fragments: every path is a
    /// parameter, whatever characters it has.
    #[test]
    fn paths_reach_the_profile_only_as_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = dir.path().join("a\")) (allow default) ((\"");
        std::fs::write(&hostile, b"x").unwrap();
        let file = std::fs::File::open(&hostile).unwrap();
        let policy = Policy {
            files: vec![&file],
            programs: vec![hostile.clone()],
            trees: vec![dir.path().to_owned()],
        };
        let (profile, parameters) = profile(&policy).unwrap();
        assert!(profile.starts_with(BASE));
        assert!(!profile.contains("allow default"), "{profile}");
        assert!(parameters.iter().any(|(name, value)| name == "PROGRAM0"
            && value.ends_with(b"(allow default) ((\"")));
        assert_eq!(
            parameters
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["SELF_DIR", "SELF", "FILE0", "FD0", "PROGRAM0", "TREE0"]
        );
    }
}
