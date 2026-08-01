//! Working with pseudo-terminals

use crate::{Child, CommandBuilder, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::{bail, Error};
use filedescriptor::FileDescriptor;
use libc::{self, winsize};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::{io, mem, ptr};

pub use std::os::unix::io::RawFd;

#[derive(Default)]
pub struct UnixPtySystem {}

fn openpty(size: PtySize) -> anyhow::Result<(UnixMasterPty, UnixSlavePty)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;

    let mut size = winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    };

    let result = unsafe {
        // BSDish systems may require mut pointers to some args
        #[allow(clippy::unnecessary_mut_passed)]
        libc::openpty(
            &mut master,
            &mut slave,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };

    if result != 0 {
        bail!("failed to openpty: {:?}", io::Error::last_os_error());
    }

    let tty_name = tty_name(slave);

    let master = UnixMasterPty {
        fd: PtyFd(unsafe { FileDescriptor::from_raw_fd(master) }),
        took_writer: RefCell::new(false),
        tty_name,
    };
    let slave = UnixSlavePty {
        fd: PtyFd(unsafe { FileDescriptor::from_raw_fd(slave) }),
    };

    // Ensure that these descriptors will get closed when we execute
    // the child process.  This is done after constructing the Pty
    // instances so that we ensure that the Ptys get drop()'d if
    // the cloexec() functions fail (unlikely!).
    cloexec(master.fd.as_raw_fd())?;
    cloexec(slave.fd.as_raw_fd())?;

    Ok((master, slave))
}

impl PtySystem for UnixPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let (master, slave) = openpty(size)?;
        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct PtyFd(pub FileDescriptor);
impl std::ops::Deref for PtyFd {
    type Target = FileDescriptor;
    fn deref(&self) -> &FileDescriptor {
        &self.0
    }
}
impl std::ops::DerefMut for PtyFd {
    fn deref_mut(&mut self) -> &mut FileDescriptor {
        &mut self.0
    }
}

impl Read for PtyFd {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        match self.0.read(buf) {
            Err(ref e) if e.raw_os_error() == Some(libc::EIO) => {
                // EIO indicates that the slave pty has been closed.
                // Treat this as EOF so that std::io::Read::read_to_string
                // and similar functions gracefully terminate when they
                // encounter this condition
                Ok(0)
            }
            x => x,
        }
    }
}

fn tty_name(fd: RawFd) -> Option<PathBuf> {
    let mut buf = vec![0 as std::ffi::c_char; 128];

    loop {
        let res = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr(), buf.len()) };

        if res == libc::ERANGE {
            if buf.len() > 64 * 1024 {
                // on macOS, if the buf is "too big", ttyname_r can
                // return ERANGE, even though that is supposed to
                // indicate buf is "too small".
                return None;
            }
            buf.resize(buf.len() * 2, 0 as std::ffi::c_char);
            continue;
        }

        return if res == 0 {
            let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
            let osstr = OsStr::from_bytes(cstr.to_bytes());
            Some(PathBuf::from(osstr))
        } else {
            None
        };
    }
}

const FIRST_INHERITED_FD: libc::c_int = 3;
const MAX_FD_SWEEP: libc::rlim_t = 1_048_576;

fn descriptor_sweep_limit() -> libc::c_int {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let soft_limit = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } == 0 {
        limits.rlim_cur
    } else {
        MAX_FD_SWEEP
    };

    // Linux's default per-process fd ceiling is a practical fallback bound;
    // larger or unlimited values must not make child setup effectively hang.
    soft_limit.min(MAX_FD_SWEEP) as libc::c_int
}

/// Prevent leaked descriptors from surviving into the executed program.
///
/// Cocoa can leak descriptors on macOS, and gnome/mutter shell extensions can
/// do the same on Linux. Marking them close-on-exec preserves Rust's private
/// exec-error channel until it has reported a failed exec to the parent.
fn mark_random_fds_cloexec(max_fd: libc::c_int) {
    #[cfg(target_os = "linux")]
    {
        // A raw syscall avoids imposing a newer glibc symbol requirement on
        // binaries that still run on older Linux distributions.
        if unsafe {
            libc::syscall(
                libc::SYS_close_range,
                FIRST_INHERITED_FD as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0
        {
            return;
        }
        // Older kernels and sandboxes that reject close_range still need the
        // portable sweep below or inherited descriptors would escape.
    }

    let mut fd = FIRST_INHERITED_FD;
    while fd < max_fd {
        // Invalid descriptors are expected in this sparse range; ignoring
        // them keeps cleanup best-effort without constructing child errors.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        fd += 1;
    }
}

impl PtyFd {
    fn resize(&self, size: PtySize) -> Result<(), Error> {
        let ws_size = winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };

        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws_size as *const _,
            )
        } != 0
        {
            bail!(
                "failed to ioctl(TIOCSWINSZ): {:?}",
                io::Error::last_os_error()
            );
        }

        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let mut size: winsize = unsafe { mem::zeroed() };
        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCGWINSZ as _,
                &mut size as *mut _,
            )
        } != 0
        {
            bail!(
                "failed to ioctl(TIOCGWINSZ): {:?}",
                io::Error::last_os_error()
            );
        }
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    fn spawn_command(&self, builder: CommandBuilder) -> anyhow::Result<std::process::Child> {
        let configured_umask = builder.umask;

        let mut cmd = builder.as_command()?;
        let controlling_tty = builder.get_controlling_tty();
        // Resolve the bound in the parent because POSIX does not require
        // getrlimit to be async-signal-safe on every supported Unix.
        let max_fd = descriptor_sweep_limit();

        unsafe {
            cmd.stdin(self.as_stdio()?)
                .stdout(self.as_stdio()?)
                .stderr(self.as_stdio()?)
                .pre_exec(move || {
                    // Clean up a few things before we exec the program
                    // Clear out any potentially problematic signal
                    // dispositions that we might have inherited
                    for signo in &[
                        libc::SIGCHLD,
                        libc::SIGHUP,
                        libc::SIGINT,
                        libc::SIGQUIT,
                        libc::SIGTERM,
                        libc::SIGALRM,
                    ] {
                        libc::signal(*signo, libc::SIG_DFL);
                    }

                    let empty_set: libc::sigset_t = std::mem::zeroed();
                    libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());

                    // Establish ourselves as a session leader.
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }

                    // Clippy wants us to explicitly cast TIOCSCTTY using
                    // type::from(), but the size and potentially signedness
                    // are system dependent, which is why we're using `as _`.
                    // Suppress this lint for this section of code.
                    #[allow(clippy::cast_lossless)]
                    if controlling_tty {
                        // Set the pty as the controlling terminal.
                        // Failure to do this means that delivery of
                        // SIGWINCH won't happen when we resize the
                        // terminal, among other undesirable effects.
                        if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    mark_random_fds_cloexec(max_fd);

                    if let Some(mask) = configured_umask {
                        libc::umask(mask);
                    }

                    Ok(())
                })
        };

        let mut child = cmd.spawn()?;

        // Ensure that we close out the slave fds that Child retains;
        // they are not what we need (we need the master side to reference
        // them) and won't work in the usual way anyway.
        // In practice these are None, but it seems best to be move them
        // out in case the behavior of Command changes in the future.
        child.stdin.take();
        child.stdout.take();
        child.stderr.take();

        Ok(child)
    }
}

/// Represents the master end of a pty.
/// The file descriptor will be closed when the Pty is dropped.
struct UnixMasterPty {
    fd: PtyFd,
    took_writer: RefCell<bool>,
    tty_name: Option<PathBuf>,
}

/// Represents the slave end of a pty.
/// The file descriptor will be closed when the Pty is dropped.
struct UnixSlavePty {
    fd: PtyFd,
}

/// Helper function to set the close-on-exec flag for a raw descriptor
fn cloexec(fd: RawFd) -> Result<(), Error> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        bail!(
            "fcntl to read flags failed: {:?}",
            io::Error::last_os_error()
        );
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        bail!(
            "fcntl to set CLOEXEC failed: {:?}",
            io::Error::last_os_error()
        );
    }
    Ok(())
}

impl SlavePty for UnixSlavePty {
    fn spawn_command(
        &self,
        builder: CommandBuilder,
    ) -> Result<Box<dyn Child + Send + Sync>, Error> {
        Ok(Box::new(self.fd.spawn_command(builder)?))
    }
}

impl MasterPty for UnixMasterPty {
    fn resize(&self, size: PtySize) -> Result<(), Error> {
        self.fd.resize(size)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        self.fd.get_size()
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, Error> {
        let fd = PtyFd(self.fd.try_clone()?);
        Ok(Box::new(fd))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, Error> {
        if *self.took_writer.borrow() {
            anyhow::bail!("cannot take writer more than once");
        }
        *self.took_writer.borrow_mut() = true;
        let fd = PtyFd(self.fd.try_clone()?);
        Ok(Box::new(UnixMasterWriter { fd }))
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.fd.0.as_raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        self.tty_name.clone()
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        match unsafe { libc::tcgetpgrp(self.fd.0.as_raw_fd()) } {
            pid if pid > 0 => Some(pid),
            _ => None,
        }
    }

    fn get_termios(&self) -> Option<nix::sys::termios::Termios> {
        nix::sys::termios::tcgetattr(self.fd.0.as_fd()).ok()
    }
}

/// Owns the duplicate master file descriptor used for PTY input.
/// Dropping the writer closes only this descriptor and never synthesizes input.
struct UnixMasterWriter {
    fd: PtyFd,
}

impl Write for UnixMasterWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.fd.write(buf)
    }
    fn flush(&mut self) -> Result<(), io::Error> {
        self.fd.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    const INHERITED_FD_ENV: &str = "PORTABLE_PTY_TEST_INHERITED_FD";

    #[test]
    #[ignore = "run as a subprocess by inherited_descriptor_is_closed_on_exec"]
    fn inherited_descriptor_probe() {
        let fd = std::env::var(INHERITED_FD_ENV)
            .expect("inherited descriptor number")
            .parse::<libc::c_int>()
            .expect("numeric inherited descriptor");

        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "descriptor {} survived exec",
            fd
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn inherited_descriptor_is_closed_on_exec() -> anyhow::Result<()> {
        let source = File::open("/dev/null")?;
        // A moderately high number avoids a false failure if process startup
        // reuses a recently closed descriptor before the probe can inspect it.
        let leaked_fd = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD, 64) };
        assert!(
            leaked_fd >= 64,
            "duplicate test descriptor: {}",
            io::Error::last_os_error()
        );
        let leaked = unsafe { FileDescriptor::from_raw_fd(leaked_fd) };
        let descriptor_flags = unsafe { libc::fcntl(leaked_fd, libc::F_GETFD) };
        assert!(descriptor_flags >= 0, "read descriptor flags");
        assert_eq!(descriptor_flags & libc::FD_CLOEXEC, 0);

        let (_master, slave) = openpty(PtySize::default())?;
        let mut command = CommandBuilder::new(std::env::current_exe()?);
        command.args([
            "--ignored",
            "inherited_descriptor_probe",
            "--test-threads=1",
        ]);
        command.env(INHERITED_FD_ENV, leaked_fd.to_string());
        let mut child = slave.spawn_command(command)?;
        drop(slave);

        let status = child.wait()?;
        drop(leaked);
        assert!(status.success(), "descriptor probe failed: {}", status);
        Ok(())
    }

    #[test]
    fn exec_failure_for_nonexistent_interpreter_is_reported() -> anyhow::Result<()> {
        const MISSING_INTERPRETER: &str = "/portable-pty-test-interpreter-does-not-exist";
        assert!(!std::path::Path::new(MISSING_INTERPRETER).exists());

        let script = std::env::temp_dir().join(format!(
            "portable-pty-missing-interpreter-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&script);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&script)?;
        writeln!(file, "#!{MISSING_INTERPRETER}")?;
        drop(file);
        let mut permissions = std::fs::metadata(&script)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions)?;

        let (_master, slave) = openpty(PtySize::default())?;
        // CommandBuilder rejects an absent program before fork, so a missing
        // shebang interpreter is needed to make exec itself return ENOENT.
        let result = slave.spawn_command(CommandBuilder::new(&script));
        let _ = std::fs::remove_file(&script);

        let error = match result {
            Ok(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("spawn reported success after exec failed");
            }
            Err(error) => error,
        };
        assert_eq!(
            error
                .downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error),
            Some(libc::ENOENT),
            "unexpected spawn error: {:#}",
            error
        );
        Ok(())
    }

    #[test]
    fn pre_exec_descriptor_sweep_has_no_allocating_source_shapes() {
        let source = include_str!("unix.rs");
        let hook = source
            .split_once(".pre_exec(move || {")
            .expect("pre_exec hook")
            .1
            .split_once("                    Ok(())")
            .expect("end of pre_exec hook")
            .0;
        let sweep_name = if source.contains("fn mark_random_fds_cloexec") {
            "fn mark_random_fds_cloexec"
        } else {
            "fn close_random_fds"
        };
        let sweep = source
            .split_once(sweep_name)
            .expect("descriptor sweep helper")
            .1
            .split_once("\n}\n\nimpl PtyFd")
            .expect("end of descriptor sweep helper")
            .0;

        // Holding the allocator across fork is not a deterministic test setup,
        // so guard the hook and its fd sweep against known allocating APIs.
        for forbidden in [
            "std::fs", "read_dir", "Vec", "String", "OsString", "vec!", "format!", ".collect",
            "Box",
        ] {
            assert!(!hook.contains(forbidden), "pre_exec uses {}", forbidden);
            assert!(!sweep.contains(forbidden), "fd sweep uses {}", forbidden);
        }
    }

    fn read_until(
        reader: &mut dyn Read,
        expected: &[u8],
        timeout: Duration,
    ) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut buf = [0; 256];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {}
                Ok(len) => {
                    output.extend_from_slice(&buf[..len]);
                    if output
                        .windows(expected.len())
                        .any(|window| window == expected)
                    {
                        return Ok(output);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }

            if Instant::now() >= deadline {
                return Ok(output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn dropping_writer_does_not_submit_partial_line() -> anyhow::Result<()> {
        let (master, slave) = openpty(PtySize::default())?;
        let master_fd = master.fd.as_raw_fd();
        let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL) };
        assert!(
            flags >= 0,
            "read master status flags: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            unsafe { libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0,
            "make test PTY nonblocking: {}",
            io::Error::last_os_error()
        );

        let mut reader = master.try_clone_reader()?;
        let marker =
            std::env::temp_dir().join(format!("portable-pty-drop-writer-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "printf 'READY\\n'; IFS= read -r line; eval \"$line\"; printf 'EXECUTED:\\n'",
        ]);
        command.env("PTY_DROP_MARKER", marker.as_os_str());
        let mut child = slave.spawn_command(command)?;
        drop(slave);

        let ready = read_until(&mut *reader, b"READY", Duration::from_secs(2))?;
        assert!(
            ready
                .windows(b"READY".len())
                .any(|window| window == b"READY"),
            "test child did not become ready; output={:?}",
            String::from_utf8_lossy(&ready)
        );

        let partial_line = b"touch \"$PTY_DROP_MARKER\"";
        let mut writer = master.take_writer()?;
        writer.write_all(partial_line)?;
        drop(writer);

        let after_drop = read_until(&mut *reader, b"EXECUTED:", Duration::from_millis(300))?;
        let executed = after_drop
            .windows(b"EXECUTED:".len())
            .any(|window| window == b"EXECUTED:");
        let marker_created = marker.exists();

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&marker);

        assert!(
            !executed && !marker_created,
            "dropping the writer executed the partial line; marker_created={marker_created}, output={:?}",
            String::from_utf8_lossy(&after_drop)
        );
        Ok(())
    }
}
