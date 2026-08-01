use crate::{Child, ChildKiller, ExitStatus};
use anyhow::Context as _;
use std::io::{Error as IoError, Result as IoResult};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::ERROR_ACCESS_DENIED;
use winapi::um::minwinbase::STILL_ACTIVE;
use winapi::um::processthreadsapi::*;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{INFINITE, WAIT_FAILED};

pub mod conpty;
mod procthreadattr;
mod psuedocon;

use filedescriptor::OwnedHandle;

#[derive(Debug)]
pub struct WinChild {
    proc: Mutex<OwnedHandle>,
}

/// Terminate a process, mapping the Win32 convention correctly.
///
/// `TerminateProcess` returns NONZERO on success and zero on failure. Reading
/// it backwards reported every successful kill as an error and, worse, every
/// real failure as success — so a process that could not be terminated looked
/// terminated, and a caller could wait on it forever.
fn terminate(handle: RawHandle) -> IoResult<()> {
    if unsafe { TerminateProcess(handle as _, 1) } != 0 {
        Ok(())
    } else {
        Err(IoError::last_os_error())
    }
}

/// Duplicate the process handle, surfacing exhaustion as an error.
///
/// `try_clone().unwrap()` aborted the process when handles ran out, turning a
/// recoverable resource condition into a crash of the terminal itself.
fn clone_handle(handle: &OwnedHandle) -> IoResult<OwnedHandle> {
    handle
        .try_clone()
        .map_err(|err| IoError::other(format!("duplicate process handle: {err}")))
}

impl WinChild {
    fn is_complete(&mut self) -> IoResult<Option<ExitStatus>> {
        let mut status: DWORD = 0;
        let proc = clone_handle(&self.proc.lock().unwrap())?;
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res == 0 {
            // A failed query is not "still running". Reporting Ok(None) here
            // made an unreadable process indistinguishable from a live one, so
            // callers waited on something they could no longer observe.
            return Err(IoError::last_os_error());
        }
        if status == STILL_ACTIVE {
            Ok(None)
        } else {
            Ok(Some(ExitStatus::with_exit_code(status)))
        }
    }

    fn do_kill(&mut self) -> IoResult<()> {
        let proc = clone_handle(&self.proc.lock().unwrap())?;
        terminate(proc.as_raw_handle())
    }
}

impl ChildKiller for WinChild {
    fn kill(&mut self) -> IoResult<()> {
        // Report the outcome. Swallowing it meant a caller that failed to
        // terminate a child had no way to know, and no reason to escalate.
        match self.do_kill() {
            Ok(()) => Ok(()),
            // The child finishing on its own is the outcome kill wanted.
            Err(err) if already_exited(&err) => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        // The trait cannot report failure here. Retain the original handle
        // rather than aborting the whole terminal on handle exhaustion; a
        // killer built from it still terminates the same process.
        let proc = self
            .proc
            .lock()
            .ok()
            .and_then(|proc| clone_handle(&proc).ok());
        Box::new(WinChildKiller { proc })
    }
}

/// Whether a termination failure just means the process already exited.
///
/// `TerminateProcess` reports `ERROR_ACCESS_DENIED` for a handle whose process
/// has already terminated, which is success from the caller's point of view.
fn already_exited(err: &IoError) -> bool {
    err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
}

#[derive(Debug)]
pub struct WinChildKiller {
    /// `None` when the handle could not be duplicated. Kept fallible rather
    /// than aborting: losing the ability to kill one child is recoverable,
    /// crashing the terminal is not.
    proc: Option<OwnedHandle>,
}

impl ChildKiller for WinChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        let Some(proc) = self.proc.as_ref() else {
            return Err(IoError::other("no process handle available to terminate"));
        };
        match terminate(proc.as_raw_handle()) {
            Ok(()) => Ok(()),
            Err(err) if already_exited(&err) => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self
            .proc
            .as_ref()
            .and_then(|proc| clone_handle(proc).ok());
        Box::new(WinChildKiller { proc })
    }
}

impl Child for WinChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.is_complete()
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        if let Ok(Some(status)) = self.try_wait() {
            return Ok(status);
        }
        let proc = clone_handle(&self.proc.lock().unwrap())?;
        // WAIT_FAILED means the wait never happened. Ignoring it sent the
        // caller straight to GetExitCodeProcess on a handle it could not wait
        // on, reporting whatever that returned as the child's real status.
        if unsafe { WaitForSingleObject(proc.as_raw_handle() as _, INFINITE) } == WAIT_FAILED {
            return Err(IoError::last_os_error());
        }
        let mut status: DWORD = 0;
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res != 0 {
            Ok(ExitStatus::with_exit_code(status))
        } else {
            Err(IoError::last_os_error())
        }
    }

    fn process_id(&self) -> Option<u32> {
        let res = unsafe { GetProcessId(self.proc.lock().unwrap().as_raw_handle() as _) };
        if res == 0 {
            None
        } else {
            Some(res)
        }
    }

    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        let proc = self.proc.lock().unwrap();
        Some(proc.as_raw_handle())
    }
}

impl std::future::Future for WinChild {
    type Output = anyhow::Result<ExitStatus>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<anyhow::Result<ExitStatus>> {
        match self.is_complete() {
            Ok(Some(status)) => Poll::Ready(Ok(status)),
            Err(err) => Poll::Ready(Err(err).context("Failed to retrieve process exit status")),
            Ok(None) => {
                struct PassRawHandleToWaiterThread(pub RawHandle);
                unsafe impl Send for PassRawHandleToWaiterThread {}

                let proc = self.proc.lock().unwrap().try_clone()?;
                let handle = PassRawHandleToWaiterThread(proc.as_raw_handle());

                let waker = cx.waker().clone();
                std::thread::spawn(move || {
                    unsafe {
                        WaitForSingleObject(handle.0 as _, INFINITE);
                    }
                    waker.wake();
                });
                Poll::Pending
            }
        }
    }
}
