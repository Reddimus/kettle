use std::io;
use std::process::{Command, Output};
use std::time::Duration;

pub(super) fn output(
    mut command: Command,
    timeout: Duration,
    max_bytes: usize,
) -> io::Result<Output> {
    use std::io::Read as _;
    use std::process::Stdio;
    let deadline = std::time::Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut pipe = child.stdout.take().expect("stdout was piped");
    let result = (|| {
        let mut stdout = Vec::new();
        let mut buffer = [0; 8192];
        let mut status = None;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shell probe timed out",
                ));
            }
            if stdout_ready(&pipe)? {
                let count = pipe.read(&mut buffer)?;
                if count != 0 {
                    if stdout.len().saturating_add(count) > max_bytes {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "shell probe output exceeded its limit",
                        ));
                    }
                    stdout.extend_from_slice(&buffer[..count]);
                    continue;
                }
            }
            if let Some(status) = status {
                return Ok(Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                });
            }
            status = child.try_wait()?;
            if status.is_some() {
                continue;
            }
            std::thread::sleep(
                Duration::from_millis(5)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(windows)]
fn stdout_ready(pipe: &std::process::ChildStdout) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let mut available = 0;
    let result = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Ok(available != 0);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
        Ok(true)
    } else {
        Err(error)
    }
}

#[cfg(all(test, unix))]
fn stdout_ready(pipe: &std::process::ChildStdout) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result != 0)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn shell_probe_timeout_kills_and_reaps_the_child() {
        let temp = kettle_test_support::private_tempdir("kettle-shell-probe-");
        let pid_file = temp.path().join("pid");
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(text) = std::fs::read_to_string(&self.0)
                    && let Ok(pid) = text.trim().parse::<libc::pid_t>()
                {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
        let _cleanup = Cleanup(pid_file.clone());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo $$ > \"$1\"; exec sleep 30", "probe"]);
        command.arg(&pid_file);
        let result = output(command, Duration::from_millis(200), 1024);
        assert!(result.is_err_and(|error| error.kind() == io::ErrorKind::TimedOut));
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "timed-out probe remains alive"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        std::fs::remove_file(pid_file).unwrap();
    }

    #[test]
    fn shell_probe_output_is_capped() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec head -c 8192 /dev/zero"]);
        let result = output(command, Duration::from_secs(2), 1024);
        assert!(result.is_err(), "probe output exceeded the capture limit");
    }

    #[test]
    fn shell_probe_captures_successful_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'probe result'"]);
        let result = output(command, Duration::from_secs(2), 1024).unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"probe result");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn command(script: &str) -> Command {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C", script]);
        command
    }

    #[test]
    fn shell_probe_reads_anonymous_pipe_output() {
        let out = output(command("echo probe result"), Duration::from_secs(2), 1024).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"probe result\r\n");
    }

    #[test]
    fn shell_probe_caps_anonymous_pipe_output() {
        let result = output(
            command("for /L %i in (1,1,1024) do @echo probe result"),
            Duration::from_secs(2),
            1024,
        );
        assert!(result.is_err_and(|error| error.kind() == io::ErrorKind::InvalidData));
    }

    #[test]
    fn shell_probe_times_out_a_running_process() {
        let result = output(
            command("for /L %i in (1,1,2147483647) do @rem"),
            Duration::from_millis(100),
            1024,
        );
        assert!(result.is_err_and(|error| error.kind() == io::ErrorKind::TimedOut));
    }
}
