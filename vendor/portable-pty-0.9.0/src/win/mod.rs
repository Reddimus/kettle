use crate::{Child, ChildKiller, ExitStatus};
use anyhow::Context as _;
use std::io::{Error as IoError, Result as IoResult};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::{ERROR_ACCESS_DENIED, WAIT_TIMEOUT};
use winapi::um::jobapi2::QueryInformationJobObject;
use winapi::um::processthreadsapi::*;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{INFINITE, WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::{
    JobObjectBasicAccountingInformation, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
};

pub mod conpty;
mod procthreadattr;
mod psuedocon;

use filedescriptor::OwnedHandle;

#[derive(Debug)]
pub struct WinChild {
    proc: Mutex<OwnedHandle>,
    /// Present when the caller requested pre-resume process-tree containment.
    /// Closing or terminating this Job Object reaches every inherited child.
    job: Option<Arc<OwnedHandle>>,
    /// Present once a waiter thread exists for this child, so repeated polls
    /// update its waker instead of spawning another thread.
    waiter: Option<Arc<WaiterShared>>,
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
        let proc = clone_handle(&self.proc.lock().unwrap())?;
        // `STILL_ACTIVE` is also a legal process exit code (259). The process
        // handle's signalled state is the authoritative liveness test; treating
        // the numeric status as liveness makes a child that deliberately exits
        // 259 look alive forever.
        match unsafe { WaitForSingleObject(proc.as_raw_handle() as _, 0) } {
            WAIT_TIMEOUT => return Ok(None),
            WAIT_OBJECT_0 => {}
            WAIT_FAILED => return Err(IoError::last_os_error()),
            other => {
                return Err(IoError::other(format!(
                    "unexpected process wait result {other:#x}"
                )));
            }
        }
        let mut status: DWORD = 0;
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res == 0 {
            // A failed query is not "still running". Reporting Ok(None) here
            // made an unreadable process indistinguishable from a live one, so
            // callers waited on something they could no longer observe.
            return Err(IoError::last_os_error());
        }
        Ok(Some(ExitStatus::with_exit_code(status)))
    }

    fn do_kill(&mut self) -> IoResult<()> {
        if let Some(job) = &self.job {
            if unsafe { winapi::um::jobapi2::TerminateJobObject(job.as_raw_handle() as _, 1) } != 0
            {
                return Ok(());
            }
            // ERROR_ACCESS_DENIED is normalized only for TerminateProcess on a
            // process that already exited. A live Job Object, even an empty
            // one, accepts TerminateJobObject; every failure here is real.
            return Err(IoError::last_os_error());
        }
        let proc = clone_handle(&self.proc.lock().unwrap())?;
        match terminate(proc.as_raw_handle()) {
            Ok(()) => Ok(()),
            Err(err) if already_exited(&err) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

impl ChildKiller for WinChild {
    fn kill(&mut self) -> IoResult<()> {
        // Report the outcome. Swallowing it meant a caller that failed to
        // terminate a child had no way to know, and no reason to escalate.
        self.do_kill()
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
        // Arc cloning is infallible. A contained killer must never silently
        // degrade to direct-process termination because DuplicateHandle was
        // exhausted for the Job Object.
        let job = self.job.clone();
        Box::new(WinChildKiller { proc, job })
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
    job: Option<Arc<OwnedHandle>>,
}

impl ChildKiller for WinChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        if let Some(job) = self.job.as_ref() {
            if unsafe { winapi::um::jobapi2::TerminateJobObject(job.as_raw_handle() as _, 1) } != 0
            {
                return Ok(());
            }
            return Err(IoError::last_os_error());
        }
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
        let proc = self.proc.as_ref().and_then(|proc| clone_handle(proc).ok());
        let job = self.job.clone();
        Box::new(WinChildKiller { proc, job })
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

    fn process_tree_active_processes(&self) -> IoResult<Option<u32>> {
        let Some(job) = self.job.as_ref() else {
            return Ok(None);
        };
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let queried = unsafe {
            QueryInformationJobObject(
                job.as_raw_handle() as _,
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut _,
                std::mem::size_of_val(&accounting) as DWORD,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            Err(IoError::last_os_error())
        } else {
            Ok(Some(accounting.ActiveProcesses))
        }
    }
}

/// State shared with the single waiter thread for one child.
///
/// The waiter OWNS its handle for the whole wait, and the waker is replaced in
/// place across polls, so neither of the previous hazards can recur.
#[derive(Debug, Default)]
struct WaiterShared {
    waker: Mutex<Option<std::task::Waker>>,
}

impl WaiterShared {
    fn store(&self, waker: &std::task::Waker) {
        if let Ok(mut slot) = self.waker.lock() {
            // Replace rather than accumulate: an executor may poll with a
            // different waker each time, and only the latest one is valid.
            *slot = Some(waker.clone());
        }
    }

    fn wake(&self) {
        let waker = self.waker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl std::future::Future for WinChild {
    type Output = anyhow::Result<ExitStatus>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<anyhow::Result<ExitStatus>> {
        match self.is_complete() {
            Ok(Some(status)) => Poll::Ready(Ok(status)),
            Err(err) => Poll::Ready(Err(err).context("Failed to retrieve process exit status")),
            Ok(None) => {
                // Two defects lived here. The waiter received only the RAW
                // value of a cloned `OwnedHandle` whose owner was dropped as
                // soon as `poll` returned, so the handle was closed while a
                // wait was pending — explicitly undefined by Win32, and able to
                // observe a reused handle once the number was recycled. And a
                // thread was spawned on EVERY pending poll, so an executor that
                // polls repeatedly grew the thread population without bound.
                //
                // Now: one waiter per child, owning its handle until the wait
                // finishes, with the waker swapped in place on later polls.
                if let Some(shared) = self.waiter.as_ref() {
                    shared.store(cx.waker());
                    return Poll::Pending;
                }

                let proc = clone_handle(&self.proc.lock().unwrap())?;
                let shared = Arc::new(WaiterShared::default());
                shared.store(cx.waker());
                let waiter = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("portable-pty-child-waiter".into())
                    .spawn(move || {
                        // `proc` is MOVED here and stays alive for the whole
                        // wait; it is dropped only after the wait returns.
                        unsafe {
                            WaitForSingleObject(proc.as_raw_handle() as _, INFINITE);
                        }
                        waiter.wake();
                    })?;
                self.waiter = Some(shared);
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod waiter_tests {
    use super::{WaiterShared, WinChild};
    use crate::Child as _;
    use filedescriptor::OwnedHandle;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    /// `STILL_ACTIVE` is the numeric value 259, and Windows permits a process
    /// to exit with it. Liveness must come from the process handle, not from
    /// interpreting this otherwise valid status as a sentinel forever.
    #[test]
    fn exit_code_259_is_reported_after_the_process_handle_signals() {
        let child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit", "259"])
            .spawn()
            .expect("spawn exit-259 child");
        let proc = OwnedHandle::dup(&child).expect("retain process handle");
        drop(child);
        let mut child = WinChild {
            proc: std::sync::Mutex::new(proc),
            job: None,
            waiter: None,
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll exit-259 child") {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "exit code 259 was mistaken for a permanently live process"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(status.exit_code(), 259);
    }

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// An executor may poll with a different waker each time, and only the
    /// latest is valid. Storing must REPLACE, so a stale waker is never the one
    /// woken — and so repeated polls cannot accumulate state per poll, which is
    /// what previously grew a thread population without bound.
    #[test]
    fn storing_a_waker_replaces_the_previous_one() {
        let shared = WaiterShared::default();

        let first = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let second = Arc::new(CountingWaker(AtomicUsize::new(0)));
        shared.store(&Waker::from(Arc::clone(&first)));
        shared.store(&Waker::from(Arc::clone(&second)));

        shared.wake();

        assert_eq!(
            first.0.load(Ordering::Acquire),
            0,
            "a superseded waker must not be woken"
        );
        assert_eq!(
            second.0.load(Ordering::Acquire),
            1,
            "the most recent waker must be the one woken"
        );
    }

    /// The waiter fires once. A second wake with nothing stored must be a
    /// no-op rather than re-waking a consumed waker.
    #[test]
    fn waking_twice_does_not_rewake_a_consumed_waker() {
        let shared = WaiterShared::default();
        let waker = Arc::new(CountingWaker(AtomicUsize::new(0)));
        shared.store(&Waker::from(Arc::clone(&waker)));

        shared.wake();
        shared.wake();

        assert_eq!(
            waker.0.load(Ordering::Acquire),
            1,
            "the waker must be consumed by the first wake"
        );
    }
}
