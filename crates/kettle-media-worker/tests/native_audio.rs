//! Opt-in native Linux playback through the real client and two production workers.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kettle_media::client::{
    FileIdentity, SpawnedWorker, UnavailableCause, WorkerClient, WorkerExit, WorkerPlatform,
    WorkerProcess, worker_command,
};
use kettle_media::session::{PlaySession, SessionStatus};
use kettle_media::{
    AudioRequest, AudioStatus, BuildId, GuiActionWitness, NativePath, PlayOpen, PlayState, Source,
};

struct Group {
    child: Child,
    reaped: bool,
}
impl Group {
    fn signal(&self) {
        if !self.reaped {
            // SAFETY: this test spawned and has not reaped this process-group leader.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
    }
}
impl WorkerProcess for Group {
    fn try_wait(&mut self) -> std::io::Result<Option<WorkerExit>> {
        use std::os::unix::process::ExitStatusExt as _;
        if self.reaped {
            return Ok(None);
        }
        // SAFETY: zeroed siginfo_t is writable and waitid leaves the child unreaped.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        self.signal();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(Some(status.code().map_or_else(
            || WorkerExit::Signal(status.signal().unwrap()),
            WorkerExit::Code,
        )))
    }
    fn kill(&mut self) {
        self.signal();
    }
    fn footprint(&mut self) -> std::io::Result<u64> {
        let mut total = 0u64;
        // SAFETY: sysconf has no pointer arguments.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some((_, rest)) = stat.rsplit_once(") ") else {
                continue;
            };
            let fields: Vec<_> = rest.split_whitespace().collect();
            if fields.get(2).and_then(|v| v.parse::<u32>().ok()) == Some(self.child.id()) {
                let rss: u64 = fields[21].parse().map_err(std::io::Error::other)?;
                total = total.saturating_add(rss.saturating_mul(page));
            }
        }
        Ok(total)
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if !self.reaped {
            self.signal();
            let _ = self.child.wait();
        }
    }
}
struct Platform {
    unavailable_broker: bool,
    attempts: std::sync::atomic::AtomicUsize,
    path: PathBuf,
    children: Arc<Mutex<Vec<u32>>>,
    logs: PathBuf,
}
impl WorkerPlatform for Platform {
    fn worker_path(&self) -> Result<&Path, UnavailableCause> {
        Ok(&self.path)
    }
    fn inspect(&self, path: &Path) -> Result<FileIdentity, UnavailableCause> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path).map_err(|_| UnavailableCause::WorkerMissing)?;
        Ok(FileIdentity {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime_seconds: m.mtime(),
            mtime_nanos: m.mtime_nsec(),
            ctime_seconds: m.ctime(),
            ctime_nanos: m.ctime_nsec(),
        })
    }
    fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
        Ok(())
    }
    fn guard_pipe_writes(&self) -> std::io::Result<()> {
        Ok(())
    }
    fn spawn(&self, path: &Path) -> std::io::Result<SpawnedWorker> {
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.unavailable_broker && attempt == 1 {
            return Err(std::io::Error::other(
                "fixture: PCM service worker unavailable",
            ));
        }
        let mut children = self.children.lock().unwrap();
        let log = std::fs::File::create(self.logs.join(format!("worker-{}.log", children.len())))?;
        let mut child = worker_command(path)
            .env(kettle_media::video::DECODER_ENV, "/usr/bin/ffmpeg")
            .stderr(log)
            .spawn()?;
        children.push(child.id());
        eprintln!("owned native worker PID/PGID {}", child.id());
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Ok(SpawnedWorker {
            process: Box::new(Group {
                child,
                reaped: false,
            }),
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
        })
    }
}

fn wait(session: &PlaySession, condition: impl Fn(SessionStatus) -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        session.keep();
        let status = session.status();
        assert!(
            !matches!(status, SessionStatus::Failed(_)),
            "native playback failed: {status:?}"
        );
        if condition(status) {
            return;
        }
        assert!(
            Instant::now() < until,
            "native playback did not reach state: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn next(session: &PlaySession, after: u64) -> u64 {
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        session.keep();
        assert_eq!(session.audio(), Some(AudioStatus::Ready));
        assert!(!matches!(session.status(), SessionStatus::Failed(_)));
        if let Some(frame) = session.take_frame()
            && frame.pts_ms > after
        {
            return frame.pts_ms;
        }
        assert!(Instant::now() < until, "no video after {after}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires native PulseAudio service, trusted FFmpeg, and a production worker"]
fn linux_client_decodes_audio_pauses_seeks_and_closes_both_workers() {
    let path = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_WORKER").expect("explicit production worker"),
    );
    let clip = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_CLIP").expect("explicit ticks.mp4 fixture"),
    );
    let logs = kettle_test_support::private_tempdir("kettle-native-audio-");
    let children = Arc::new(Mutex::new(Vec::new()));
    let client = WorkerClient::new(
        BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH")).unwrap(),
        Box::new(Platform {
            unavailable_broker: false,
            attempts: std::sync::atomic::AtomicUsize::new(0),
            path,
            children: Arc::clone(&children),
            logs: logs.path().to_path_buf(),
        }),
    );
    let request = PlayOpen {
        source: Source::user_pull(
            NativePath::from_path(&clip).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        ),
        max_width: 64,
        max_height: 36,
        start_ms: 0,
        max_fps_milli: 30_000,
        audio: AudioRequest::Muted,
    };
    let session = client.play(&request, Box::new(|| {})).unwrap();
    wait(&session, |s| {
        matches!(
            s,
            SessionStatus::Open {
                state: PlayState::Paused,
                ..
            }
        )
    });
    assert_eq!(session.audio(), Some(AudioStatus::Ready));
    session.play();
    let before = next(&session, 100);
    session.pause();
    wait(&session, |s| {
        matches!(
            s,
            SessionStatus::Open {
                state: PlayState::Paused,
                ..
            }
        )
    });
    let _ = session.take_frame();
    let until = Instant::now() + Duration::from_millis(350);
    while Instant::now() < until {
        session.keep();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(session.take_frame().is_none(), "paused video advanced");
    session.set_muted(false);
    session.play();
    let _ = next(&session, before);
    let other = client
        .play(
            &PlayOpen {
                start_ms: 1533,
                max_width: 640,
                max_height: 360,
                ..request.clone()
            },
            Box::new(|| {}),
        )
        .unwrap();
    // Opening-time commands must also reach the broker created later.
    let playing_at = Instant::now();
    other.set_muted(false);
    other.play();
    wait(&other, |s| {
        matches!(
            s,
            SessionStatus::Open {
                state: PlayState::Playing,
                ..
            }
        )
    });
    assert_eq!(other.audio(), Some(AudioStatus::Ready));
    assert!(next(&other, 1500) > 1500);
    drop(session);
    // Closing one decoder/broker pair must not stop the other.
    let until = Instant::now() + Duration::from_secs(8);
    let mut last = None;
    loop {
        other.keep();
        assert_eq!(other.audio(), Some(AudioStatus::Ready));
        if let Some(frame) = other.take_frame() {
            last = Some(frame.pts_ms);
        }
        match other.status() {
            SessionStatus::Open {
                state: PlayState::Ended,
                ..
            } => break,
            SessionStatus::Failed(code) => panic!("surviving playback failed: {code:?}"),
            _ => {}
        }
        assert!(
            Instant::now() < until,
            "surviving audio session did not end"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(last, Some(3933));
    assert!(
        playing_at.elapsed() >= Duration::from_secs(2),
        "presentation clock ran ahead"
    );
    drop(other);
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        let live: Vec<_> = children
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
            .collect();
        if live.is_empty() {
            break;
        }
        assert!(
            Instant::now() < until,
            "workers outlived both sessions: {live:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        children.lock().unwrap().len(),
        4,
        "each session needs its own decoder and PCM broker"
    );
    for entry in std::fs::read_dir(logs.path()).unwrap() {
        let entry = entry.unwrap();
        assert!(
            std::fs::read(entry.path()).unwrap().is_empty(),
            "worker stderr: {}",
            entry.path().display()
        );
    }
}

#[test]
#[ignore = "requires trusted FFmpeg and a production worker; deliberately refuses the PCM worker"]
fn linux_client_keeps_video_when_the_pcm_broker_cannot_start() {
    let path = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_WORKER").expect("explicit production worker"),
    );
    let clip = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_CLIP").expect("explicit ticks.mp4 fixture"),
    );
    let logs = kettle_test_support::private_tempdir("kettle-native-no-audio-");
    let children = Arc::new(Mutex::new(Vec::new()));
    let client = WorkerClient::new(
        BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH")).unwrap(),
        Box::new(Platform {
            unavailable_broker: true,
            attempts: std::sync::atomic::AtomicUsize::new(0),
            path,
            children: Arc::clone(&children),
            logs: logs.path().to_path_buf(),
        }),
    );
    let request = PlayOpen {
        source: Source::user_pull(
            NativePath::from_path(&clip).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        ),
        max_width: 64,
        max_height: 36,
        start_ms: 0,
        max_fps_milli: 30_000,
        audio: AudioRequest::On,
    };
    let session = client.play(&request, Box::new(|| {})).unwrap();
    wait(&session, |s| {
        matches!(
            s,
            SessionStatus::Open {
                state: PlayState::Paused,
                ..
            }
        )
    });
    assert_eq!(session.audio(), Some(AudioStatus::Unavailable));
    session.play();
    let until = Instant::now() + Duration::from_secs(8);
    let mut last = None;
    loop {
        session.keep();
        if let Some(frame) = session.take_frame() {
            last = Some(frame.pts_ms);
        }
        assert_eq!(session.audio(), Some(AudioStatus::Unavailable));
        match session.status() {
            SessionStatus::Open {
                state: PlayState::Ended,
                ..
            } => break,
            SessionStatus::Failed(code) => panic!("video fallback failed: {code:?}"),
            _ => {}
        }
        assert!(Instant::now() < until, "silent fallback did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(last, Some(3900));
    drop(session);
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let live = children
            .lock()
            .unwrap()
            .iter()
            .any(|pid| Path::new(&format!("/proc/{pid}")).exists());
        if !live {
            break;
        }
        assert!(
            Instant::now() < until,
            "decoder outlived its silent fallback"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(children.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires native PulseAudio service, trusted FFmpeg, and a production worker"]
fn linux_client_keeps_the_last_picture_while_longer_audio_finishes() {
    let path = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_WORKER").expect("explicit production worker"),
    );
    let clip = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_LONG_CLIP").expect("explicit long-audio.mp4 fixture"),
    );
    let logs = kettle_test_support::private_tempdir("kettle-native-audio-tail-");
    let children = Arc::new(Mutex::new(Vec::new()));
    let client = WorkerClient::new(
        BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH")).unwrap(),
        Box::new(Platform {
            unavailable_broker: false,
            attempts: std::sync::atomic::AtomicUsize::new(0),
            path,
            children: Arc::clone(&children),
            logs: logs.path().to_path_buf(),
        }),
    );
    let request = PlayOpen {
        source: Source::user_pull(
            NativePath::from_path(&clip).unwrap(),
            GuiActionWitness::from_explicit_gui_action(),
        ),
        max_width: 64,
        max_height: 36,
        start_ms: 3500,
        max_fps_milli: 30_000,
        audio: AudioRequest::Muted,
    };
    let session = client.play(&request, Box::new(|| {})).unwrap();
    wait(&session, |s| {
        matches!(
            s,
            SessionStatus::Open {
                state: PlayState::Paused,
                ..
            }
        )
    });
    assert_eq!(session.audio(), Some(AudioStatus::Ready));
    session.play();
    let started = Instant::now();
    let mut last = None;
    loop {
        session.keep();
        assert_eq!(session.audio(), Some(AudioStatus::Ready));
        if let Some(frame) = session.take_frame() {
            last = Some(frame.pts_ms);
        }
        match session.status() {
            SessionStatus::Open {
                state: PlayState::Ended,
                ..
            } => break,
            SessionStatus::Failed(code) => {
                panic!("valid audio tail failed: {code:?}, last={last:?}")
            }
            _ => {}
        }
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "audio tail did not end"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(last, Some(3900));
    assert!(
        started.elapsed() >= Duration::from_secs(6),
        "audio tail was cut short"
    );
    drop(session);
    let until = Instant::now() + Duration::from_secs(3);
    while children
        .lock()
        .unwrap()
        .iter()
        .any(|pid| Path::new(&format!("/proc/{pid}")).exists())
    {
        assert!(
            Instant::now() < until,
            "audio-tail workers outlived their session"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(children.lock().unwrap().len(), 2);
}

#[test]
#[ignore = "requires native PulseAudio service, trusted FFmpeg, and a production worker"]
fn linux_client_seeks_pcm_between_frame_boundaries() {
    let path = PathBuf::from(
        std::env::var_os("KETTLE_TEST_PCM_WORKER").expect("explicit production worker"),
    );
    let logs = kettle_test_support::private_tempdir("kettle-native-pcm-seeks-");
    let clip = logs.path().join("seek.mov");
    let generated = std::process::Command::new("/usr/bin/ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=640x360:r=30:d=4",
            "-f",
            "lavfi",
            "-i",
            "sine=f=1000:d=4:r=48000",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-g",
            "30",
            "-bf",
            "0",
            "-c:a",
            "pcm_s16le",
            "-shortest",
        ])
        .arg(&clip)
        .output()
        .unwrap();
    assert!(generated.status.success(), "fixture: {generated:?}");
    let children = Arc::new(Mutex::new(Vec::new()));
    let client = WorkerClient::new(
        BuildId::from_embedded(env!("CARGO_PKG_VERSION"), env!("KETTLE_SOURCE_HASH")).unwrap(),
        Box::new(Platform {
            unavailable_broker: false,
            attempts: std::sync::atomic::AtomicUsize::new(0),
            path,
            children: Arc::clone(&children),
            logs: logs.path().to_path_buf(),
        }),
    );
    for start_ms in [2100, 2133, 2166, 2233, 2366] {
        eprintln!("native PCM seek {start_ms}");
        let session = client
            .play(
                &PlayOpen {
                    source: Source::user_pull(
                        NativePath::from_path(&clip).unwrap(),
                        GuiActionWitness::from_explicit_gui_action(),
                    ),
                    max_width: 332,
                    max_height: 187,
                    start_ms,
                    max_fps_milli: 30_000,
                    audio: AudioRequest::Muted,
                },
                Box::new(|| {}),
            )
            .unwrap();
        session.play();
        wait(&session, |s| {
            matches!(
                s,
                SessionStatus::Open {
                    state: PlayState::Playing,
                    ..
                }
            )
        });
        next(&session, start_ms + 150);
        drop(session);
        let until = Instant::now() + Duration::from_secs(8);
        while children
            .lock()
            .unwrap()
            .iter()
            .any(|pid| Path::new(&format!("/proc/{pid}")).exists())
        {
            assert!(Instant::now() < until, "seek workers did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
