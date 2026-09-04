use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::win::procthreadattr::ProcThreadAttributeList;
use anyhow::{bail, ensure, Error};
use filedescriptor::{FileDescriptor, OwnedHandle};
use lazy_static::lazy_static;
use shared_library::shared_library;
use std::ffi::OsString;
use std::io::{Error as IoError, Result as IoResult};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::{mem, ptr};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::handleapi::*;
use winapi::um::jobapi2::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
};
use winapi::um::processthreadsapi::*;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, WAIT_OBJECT_0,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::{
    JobObjectExtendedLimitInformation, HANDLE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

// Match the Windows SDK name used by the dynamically loaded ABI.
#[allow(clippy::upper_case_acronyms)]
pub type HPCON = HANDLE;

pub const PSUEDOCONSOLE_INHERIT_CURSOR: DWORD = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: DWORD = 0x8;

shared_library!(ConPtyFuncs,
    pub fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        flags: DWORD,
        hpc: *mut HPCON
    ) -> HRESULT,
    pub fn ResizePseudoConsole(hpc: HPCON, size: COORD) -> HRESULT,
    pub fn ClosePseudoConsole(hpc: HPCON),
);

fn load_conpty() -> ConPtyFuncs {
    // If the kernel doesn't export these functions then their system is
    // too old and we cannot run.
    ConPtyFuncs::open(Path::new("kernel32.dll")).expect(
        "this system does not support conpty.  Windows 10 October 2018 or newer is required",
    )
}

lazy_static! {
    static ref CONPTY: ConPtyFuncs = load_conpty();
}

const SPAWN_ROLLBACK_WAIT_MS: DWORD = 5_000;

/// End a child that cannot safely be returned from `spawn_command` and prove
/// the process handle reached the signalled state before its suspended thread
/// handle is dropped. Reporting only the original setup error can otherwise
/// hide a live, uncontained process after assignment failure.
fn terminate_process_and_wait(proc: &OwnedHandle) -> IoResult<()> {
    if unsafe { TerminateProcess(proc.as_raw_handle() as _, 1) } == 0 {
        return Err(IoError::last_os_error());
    }
    if unsafe { WaitForSingleObject(proc.as_raw_handle() as _, SPAWN_ROLLBACK_WAIT_MS) }
        != WAIT_OBJECT_0
    {
        return Err(IoError::other(
            "terminated process did not exit within rollback deadline",
        ));
    }
    Ok(())
}

fn terminate_job_and_wait(job: &OwnedHandle, proc: &OwnedHandle) -> IoResult<()> {
    if unsafe { TerminateJobObject(job.as_raw_handle() as _, 1) } == 0 {
        return Err(IoError::last_os_error());
    }
    if unsafe { WaitForSingleObject(proc.as_raw_handle() as _, SPAWN_ROLLBACK_WAIT_MS) }
        != WAIT_OBJECT_0
    {
        return Err(IoError::other(
            "terminated Job Object child did not exit within rollback deadline",
        ));
    }
    Ok(())
}

pub struct PsuedoCon {
    con: HPCON,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        unsafe { (CONPTY.ClosePseudoConsole)(self.con) };
    }
}

impl PsuedoCon {
    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (CONPTY.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSUEDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create psuedo console: HRESULT {}",
            result
        );
        Ok(Self { con })
    }

    pub fn resize(&self, size: COORD) -> Result<(), Error> {
        let result = unsafe { (CONPTY.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    pub fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        // Explicitly set the stdio handles as invalid handles otherwise
        // we can end up with a weird state where the spawned process can
        // inherit the explicitly redirected output handles from its parent.
        // For example, when daemonizing wezterm-mux-server, the stdio handles
        // are redirected to a log file and the spawned process would end up
        // writing its output there instead of to the pty we just created.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut attrs = ProcThreadAttributeList::with_capacity(1)?;
        attrs.set_pty(self.con)?;
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();
        let contain_process_tree = cmd.process_tree_containment();
        let job = if contain_process_tree {
            let raw = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
            if raw.is_null() {
                bail!("CreateJobObjectW failed: {}", IoError::last_os_error());
            }
            let job = unsafe { OwnedHandle::from_raw_handle(raw as _) };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    &mut limits as *mut _ as *mut _,
                    mem::size_of_val(&limits) as DWORD,
                )
            };
            if configured == 0 {
                bail!(
                    "SetInformationJobObject failed: {}",
                    IoError::last_os_error()
                );
            }
            Some(Arc::new(job))
        } else {
            None
        };

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_slice().as_mut_ptr(),
                cmdline.as_mut_slice().as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | if contain_process_tree {
                        CREATE_SUSPENDED
                    } else {
                        0
                    },
                cmd.environment_block().as_mut_slice().as_mut_ptr() as *mut _,
                cwd.as_ref()
                    .map(|c| c.as_slice().as_ptr())
                    .unwrap_or(ptr::null()),
                &mut si.StartupInfo,
                &mut pi,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            bail!("{}", msg);
        }

        // Make sure we close out the thread handle so we don't leak it;
        // we do this simply by making it owned
        let main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };

        if let Some(job) = &job {
            // The primary thread is still suspended, so the command cannot
            // create a descendant before the containment boundary exists.
            if unsafe {
                AssignProcessToJobObject(job.as_raw_handle() as _, proc.as_raw_handle() as _)
            } == 0
            {
                let error = IoError::last_os_error();
                terminate_process_and_wait(&proc).map_err(|rollback| {
                    anyhow::anyhow!(
                        "AssignProcessToJobObject failed: {error}; rollback failed: {rollback}"
                    )
                })?;
                bail!("AssignProcessToJobObject failed: {}", error);
            }
            if unsafe { ResumeThread(main_thread.as_raw_handle() as _) } == u32::MAX {
                let error = IoError::last_os_error();
                terminate_job_and_wait(job, &proc).map_err(|rollback| {
                    anyhow::anyhow!("ResumeThread failed: {error}; rollback failed: {rollback}")
                })?;
                bail!("ResumeThread failed: {}", error);
            }
        }

        Ok(WinChild {
            proc: Mutex::new(proc),
            job,
            waiter: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::Duration;

    use crate::{native_pty_system, CommandBuilder, PtySize};

    const DESCENDANT_FIXTURE: &str = "win::psuedocon::tests::contained_descendant_helper";
    const LATE_DESCENDANT_ENV: &str = "KETTLE_PORTABLE_PTY_LATE_DESCENDANT";

    fn invoked_as_fixture() -> bool {
        let mut exact = false;
        let mut named = false;
        for arg in std::env::args().skip(1) {
            exact |= arg == "--exact";
            named |= arg == DESCENDANT_FIXTURE;
        }
        exact && named
    }

    /// Re-execute this small test binary rather than cold-starting PowerShell.
    /// Its first test action creates the descendant, minimizing the window an
    /// attach-after-spawn implementation could exploit before containment.
    #[test]
    #[allow(clippy::zombie_processes)] // the parent-exit branch is the behavior under test
    fn contained_descendant_helper() {
        if !invoked_as_fixture() {
            return;
        }
        match std::env::var(LATE_DESCENDANT_ENV).as_deref() {
            Ok("parent") => {
                let mut descendant = std::process::Command::new(
                    std::env::current_exe().expect("resolve late descendant helper"),
                );
                descendant.args([
                    "--exact",
                    DESCENDANT_FIXTURE,
                    "--nocapture",
                    "--test-threads=1",
                ]);
                descendant.env(LATE_DESCENDANT_ENV, "child");
                let _descendant = descendant.spawn().expect("spawn late descendant");
                return;
            }
            Ok("child") => {
                std::thread::sleep(Duration::from_secs(2));
                println!("JOB_LATE_DESCENDANT_OUTPUT");
                return;
            }
            _ => {}
        }
        let mut descendant = std::process::Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn immediate descendant");
        println!("JOB_DESCENDANT {}", descendant.id());
        std::io::stdout().flush().expect("announce descendant");
        let _ = descendant.wait();
    }

    #[test]
    fn conpty_loader_uses_only_kernel32() {
        let source = include_str!("psuedocon.rs");
        let open_call = ["ConPtyFuncs", "::open("].concat();
        assert_eq!(
            source.matches(&open_call).count(),
            1,
            "ConPTY must have exactly one library load path"
        );
        let unsafe_sideload_name = ["conpty", ".dll"].concat();
        assert!(
            !source.contains(&unsafe_sideload_name),
            "ConPTY must never probe a working-directory or PATH DLL"
        );
    }

    #[test]
    fn contained_spawn_is_suspended_until_job_assignment() {
        let source = include_str!("psuedocon.rs");
        let body = source
            .split("pub fn spawn_command")
            .nth(1)
            .expect("spawn_command body");
        let create = body.find("CREATE_SUSPENDED").expect("suspended spawn flag");
        let assign = body
            .find("AssignProcessToJobObject(")
            .expect("job assignment");
        let resume = body.find("ResumeThread(").expect("primary-thread resume");
        assert!(create < assign && assign < resume);
        assert!(
            body[assign..resume].contains("bail!("),
            "failed assignment must abort spawn before the child can resume"
        );
    }

    #[test]
    fn contained_child_kill_reaches_its_immediate_descendant() {
        use winapi::shared::minwindef::FALSE;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::OpenProcess;
        use winapi::um::synchapi::WaitForSingleObject;
        use winapi::um::winbase::WAIT_OBJECT_0;
        use winapi::um::winnt::{PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE};

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open ConPTY");
        if invoked_as_fixture() {
            return;
        }
        let helper = std::env::current_exe().expect("resolve test binary");
        let mut command = CommandBuilder::new(helper);
        command.args([
            "--exact",
            DESCENDANT_FIXTURE,
            "--nocapture",
            "--test-threads=1",
        ]);
        command.set_process_tree_containment(true);
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("spawn contained fixture");
        drop(pair.slave);

        let mut writer = pair.master.take_writer().expect("take ConPTY input");
        let mut reader = pair.master.try_clone_reader().expect("clone ConPTY output");
        let (output_tx, output_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 1024];
            while bytes.len() < 64 * 1024 {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        bytes.extend_from_slice(&chunk[..read]);
                        if bytes.windows(4).any(|window| window == b"\x1b[6n") {
                            // ConPTY's startup DSR must be answered before
                            // PowerShell finishes initializing and runs the
                            // fixture command.
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                        }
                        if String::from_utf8_lossy(&bytes).contains("JOB_DESCENDANT ") {
                            break;
                        }
                    }
                }
            }
            let _ = output_tx.send(bytes);
        });
        let output = output_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fixture announces its descendant");
        let output = String::from_utf8_lossy(&output);
        let pid = output
            .split("JOB_DESCENDANT ")
            .nth(1)
            .map(|tail| {
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|digits| digits.parse::<u32>().ok())
            .unwrap_or_else(|| panic!("missing descendant pid in {:?}", output));

        let process =
            unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
        if process.is_null() {
            let error = std::io::Error::last_os_error();
            panic!(
                "cannot retain immediate descendant {} before Job termination: {}",
                pid, error
            );
        }
        child.kill().expect("terminate the Job Object");
        let exited = unsafe { WaitForSingleObject(process, 1000) } == WAIT_OBJECT_0;
        unsafe { CloseHandle(process) };
        assert!(
            exited,
            "immediate descendant {} survived Job Object kill",
            pid
        );
    }

    #[test]
    fn job_accounting_retains_a_late_same_console_descendant() {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open ConPTY");
        if invoked_as_fixture() {
            return;
        }
        let helper = std::env::current_exe().expect("resolve test binary");
        let mut command = CommandBuilder::new(helper);
        command.args([
            "--exact",
            DESCENDANT_FIXTURE,
            "--nocapture",
            "--test-threads=1",
        ]);
        command.env(LATE_DESCENDANT_ENV, "parent");
        command.set_process_tree_containment(true);
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("spawn late-descendant fixture");
        drop(pair.slave);

        let mut writer = pair.master.take_writer().expect("take ConPTY input");
        let mut reader = pair.master.try_clone_reader().expect("clone ConPTY output");
        let (output_tx, output_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 1024];
            while bytes.len() < 64 * 1024 {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        bytes.extend_from_slice(&chunk[..read]);
                        if bytes.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                        }
                        if String::from_utf8_lossy(&bytes).contains("JOB_LATE_DESCENDANT_OUTPUT") {
                            break;
                        }
                    }
                }
            }
            let _ = output_tx.send(bytes);
        });

        let direct_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("poll direct fixture").is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < direct_deadline,
                "direct fixture did not exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            child
                .process_tree_active_processes()
                .expect("query contained process count")
                .is_some_and(|active| active > 0),
            "the direct child exited while its contained descendant was still active"
        );

        let output = output_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("late descendant writes through inherited ConPTY");
        assert!(
            String::from_utf8_lossy(&output).contains("JOB_LATE_DESCENDANT_OUTPUT"),
            "late descendant output was lost: {:?}",
            String::from_utf8_lossy(&output)
        );
    }
}
