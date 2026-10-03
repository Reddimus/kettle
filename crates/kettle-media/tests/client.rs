//! The client against real worker processes: the feature-gated stub worker,
//! copied under a name that picks how it misbehaves.
#![cfg(all(feature = "test-worker", unix))]
mod common;

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use kettle_media::client::{
    FileIdentity, SpawnedWorker, UnavailableCause, WorkerClient, WorkerExit, WorkerPlatform,
    WorkerProcess, worker_command,
};
use kettle_media::{FailureCode, Source, content_digest};

const READY: Duration = Duration::from_millis(500);
const RENDER: Duration = Duration::from_millis(500);

/// The stub worker, copied as `media-test-worker[-mode]` in its own
/// directory, which the directory keeps alive.
struct Stub {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl Stub {
    fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let name = if mode.is_empty() {
            "media-test-worker".to_owned()
        } else {
            format!("media-test-worker-{mode}")
        };
        let path = directory.path().join(name);
        std::fs::copy(env!("CARGO_BIN_EXE_media-test-worker"), &path).unwrap();
        Self {
            _directory: directory,
            path,
        }
    }
}

/// The worker as a plain child: the stub starts nothing of its own, so
/// killing it is killing its group.
struct Plain(Child);

impl WorkerProcess for Plain {
    fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
        use std::os::unix::process::ExitStatusExt as _;
        Ok(self.0.try_wait()?.map(|status| match status.code() {
            Some(code) => WorkerExit::Code(code),
            None => WorkerExit::Signal(status.signal().unwrap_or(0)),
        }))
    }
    fn kill(&mut self) {
        let _ = self.0.kill();
    }
    fn footprint(&mut self) -> std::io::Result<u64> {
        // Measured for real by the kettle platform's own tests.
        Ok(0)
    }
}

impl WorkerPlatform for Stub {
    fn worker_path(&self) -> Result<&Path, UnavailableCause> {
        Ok(&self.path)
    }
    fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
        Ok(FileIdentity {
            dev: 0,
            ino: 0,
            size: 0,
            mtime_seconds: 0,
            mtime_nanos: 0,
            ctime_seconds: 0,
            ctime_nanos: 0,
        })
    }
    fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
        Ok(())
    }
    fn guard_pipe_writes(&self) -> std::io::Result<()> {
        // Test binaries keep Rust's ignored SIGPIPE.
        Ok(())
    }
    fn spawn(&self, path: &Path) -> std::io::Result<SpawnedWorker> {
        let mut child = worker_command(path).spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Ok(SpawnedWorker {
            process: Box::new(Plain(child)),
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
        })
    }
}

fn client(mode: &str) -> WorkerClient {
    WorkerClient::with_test_budgets(
        common::build(),
        Box::new(Stub::new(mode)),
        READY,
        RENDER,
        u64::MAX,
    )
}

#[test]
fn a_real_worker_renders_a_job() {
    let mut expected = common::rendered();
    expected.digest = content_digest(&[7], None).unwrap();
    assert_eq!(client("").render(&common::job()), Ok(expected));
}

#[test]
fn a_worker_that_never_answers_is_tried_twice() {
    let started = Instant::now();
    assert_eq!(
        client("stall").render(&common::job()),
        Err(FailureCode::RenderTimeout)
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= READY * 2, "{elapsed:?}");
    assert!(elapsed < READY * 2 + Duration::from_secs(3), "{elapsed:?}");
}

#[test]
fn blocked_stdin_is_deadline_bounded() {
    // A job far larger than a pipe holds, to a worker that never reads it:
    // the write blocks, and the deadline still ends the attempt.
    let mut job = common::job();
    job.source = Source::Bytes(vec![7; 4 * 1024 * 1024]);
    let started = Instant::now();
    assert_eq!(
        client("no-read").render(&job),
        Err(FailureCode::RenderTimeout)
    );
    assert!(started.elapsed() < READY + RENDER + Duration::from_secs(3));
}

#[test]
fn complete_reply_before_crash_is_discarded() {
    assert_eq!(
        client("crash-after-reply").render(&common::job()),
        Err(FailureCode::RenderResource)
    );
}

#[test]
fn the_worker_s_own_watchdog_exit_is_a_timeout() {
    assert_eq!(
        client("exit-4").render(&common::job()),
        Err(FailureCode::RenderTimeout)
    );
}
