//! Showing a file Kettle does not draw where it lives: its folder open in
//! Finder on macOS, or in the desktop's file manager on Linux, with the file
//! selected. Nothing opens the file itself, and the default association is
//! never used.
//!
//! The file is checked to be the one shown, by its device and inode, just
//! before it is revealed. On macOS, Apple's own `/usr/bin/open -R` reveals
//! it. On Linux, the file's URI goes to `org.freedesktop.FileManager1`'s
//! `ShowItems`, and only to the unique bus connection that owns that name
//! when it is the user's own process running a known file manager that only
//! root may change; when none runs, the bus starts the desktop's own.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::OnceLock;

use kettle_media::PathIdentity;

use super::{OpenFailure, Slot};
use crate::media::{ShelfItem, SourceInput};

/// Where a session bus names the file manager it starts for
/// `org.freedesktop.FileManager1`.
#[cfg(target_os = "linux")]
const FILE_MANAGER_SERVICE: &str =
    "/usr/share/dbus-1/services/org.freedesktop.FileManager1.service";

/// What shows a file in its folder on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Revealer {
    /// macOS: Finder, through Apple's `open -R`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Finder,
    /// Linux: the desktop's file manager, through its bus name.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    FileManager,
}

impl Revealer {
    /// What this platform has, by a quick look once per process; each
    /// reveal checks what it reaches before it hands anything over.
    pub(crate) fn find() -> Option<Self> {
        static FOUND: OnceLock<Option<Revealer>> = OnceLock::new();
        *FOUND.get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                Path::new("/usr/bin/open").is_file().then_some(Self::Finder)
            }
            #[cfg(target_os = "linux")]
            {
                Path::new(FILE_MANAGER_SERVICE)
                    .is_file()
                    .then_some(Self::FileManager)
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                None
            }
        })
    }

    /// The menu row, and the lane button's accessible name, that reveals a
    /// file this way.
    pub(crate) fn label(self, tr: &kettle_i18n::Translator) -> &'static str {
        tr.text(match self {
            Self::Finder => kettle_i18n::Text::MenuShowInFinder,
            Self::FileManager => kettle_i18n::Text::MenuShowInFolder,
        })
    }
}

/// A file shown as itself: where it is, and the file it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RevealTarget {
    pub path: PathBuf,
    pub identity: PathIdentity,
}

impl RevealTarget {
    /// What revealing `item` needs, when it is a file shown as itself.
    pub(crate) fn of(item: &ShelfItem) -> Option<Self> {
        let facts = item.kind.file()?;
        let SourceInput::Path { path, .. } = &item.source.spec.input else {
            return None;
        };
        Some(Self {
            path: super::video::native_path(path)?,
            identity: facts.identity,
        })
    }

    #[cfg(any(target_os = "linux", all(test, unix)))]
    fn path_for_handoff(&self) -> Result<&Path, OpenFailure> {
        if !self.still_shown() {
            return Err(OpenFailure::Changed);
        }
        Ok(&self.path)
    }

    /// Whether the path still names the file shown: the same device and
    /// inode, a regular file. A file written in place since is still it.
    fn still_shown(&self) -> bool {
        super::video::open_read_only(&self.path)
            .and_then(|file| file.metadata())
            .is_ok_and(|metadata| {
                metadata.is_file()
                    && super::video::identity(&metadata).is_some_and(|now| {
                        (now.dev, now.ino) == (self.identity.dev, self.identity.ino)
                    })
            })
    }
}

/// Reveal `target` with `revealer`, on a short thread. `failed` hears why,
/// if it does not show; it runs on that thread.
pub(crate) fn reveal(
    target: RevealTarget,
    revealer: Revealer,
    failed: impl FnOnce(OpenFailure) + Send + 'static,
) {
    let Some(slot) = Slot::take() else {
        failed(OpenFailure::Busy);
        return;
    };
    super::start_open("kettle-ui-reveal", failed, move |failed| {
        let shown = hand_over(&target, revealer);
        drop(slot);
        match shown {
            Ok(Some(child)) => super::watch(child, failed),
            Ok(None) => {}
            Err(why) => failed(why),
        }
    });
}

/// Check the file is still the one shown, then reveal it: what was started,
/// if Kettle started something.
fn hand_over(target: &RevealTarget, revealer: Revealer) -> Result<Option<Child>, OpenFailure> {
    if !target.still_shown() {
        return Err(OpenFailure::Changed);
    }
    match revealer {
        Revealer::Finder => command(&target.path)
            .spawn()
            .map(Some)
            .map_err(|_| OpenFailure::Viewer),
        #[cfg(target_os = "linux")]
        Revealer::FileManager => file_manager::show(target).map(|()| None),
        #[cfg(not(target_os = "linux"))]
        Revealer::FileManager => Err(OpenFailure::Viewer),
    }
}

/// Apple's `open -R`, which selects `file`, an absolute path, in its Finder
/// folder: a fixed program and arguments, no shell and no search path.
fn command(file: &Path) -> std::process::Command {
    debug_assert!(file.is_absolute(), "a leading `/` is never an option");
    let mut command = std::process::Command::new("/usr/bin/open");
    command
        .arg("-R")
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[cfg(target_os = "linux")]
mod file_manager {
    use std::path::Path;
    use std::time::Duration;

    use futures_lite::future;
    use kettle_ctl::process;
    use zbus::connection::Builder;

    use super::super::loupe::{CALL_TIMEOUT, OPEN_TIMEOUT, bounded, bus_query, owner};
    use super::{OpenFailure, RevealTarget};

    const NAME: &str = "org.freedesktop.FileManager1";
    const OBJECT: &str = "/org/freedesktop/FileManager1";

    /// The file managers that may receive a file to show, each only where
    /// root alone may change it.
    pub(super) const FILE_MANAGERS: [&str; 5] = [
        "/usr/bin/nautilus",
        "/usr/bin/nemo",
        "/usr/bin/caja",
        "/usr/bin/dolphin",
        "/usr/bin/thunar",
    ];

    /// Whether `executable`, a process's as the kernel names it, is a file
    /// manager that may receive a file to show.
    pub(super) fn permitted(executable: &Path) -> bool {
        FILE_MANAGERS
            .iter()
            .any(|manager| executable == Path::new(manager))
            && super::super::trusted_program(executable)
    }

    /// Show `file` in its folder through the file manager that owns the
    /// bus name, started by the bus when none runs, once that owner checks.
    pub(super) fn show(target: &RevealTarget) -> Result<(), OpenFailure> {
        let uri = url::Url::from_file_path(&target.path).map_err(|_| OpenFailure::Changed)?;
        if uri.as_str().len() > 16384 {
            return Err(OpenFailure::Changed);
        }
        future::block_on(future::or(
            async {
                let connection = Builder::session()
                    .map_err(|_| OpenFailure::Viewer)?
                    .max_queued(1)
                    .method_timeout(CALL_TIMEOUT)
                    .build()
                    .await
                    .map_err(|_| OpenFailure::Viewer)?;
                let mut name = owner(&connection, NAME)
                    .await
                    .map_err(|_| OpenFailure::Viewer)?;
                if name.is_none() {
                    // The bus starts the file manager the desktop names; a
                    // start slower than one call is waited for below.
                    let _ = connection
                        .call_method(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            Some("org.freedesktop.DBus"),
                            "StartServiceByName",
                            &(NAME, 0_u32),
                        )
                        .await;
                    while name.is_none() {
                        async_io::Timer::after(Duration::from_millis(20)).await;
                        name = owner(&connection, NAME)
                            .await
                            .map_err(|_| OpenFailure::Viewer)?;
                    }
                }
                let name = name.ok_or(OpenFailure::Viewer)?;
                let uid: u32 = bus_query(&connection, "GetConnectionUnixUser", name.as_str())
                    .await
                    .and_then(|reply| reply.body().deserialize())
                    .map_err(|_| OpenFailure::Viewer)?;
                let pid: u32 = bus_query(&connection, "GetConnectionUnixProcessID", name.as_str())
                    .await
                    .and_then(|reply| reply.body().deserialize())
                    .map_err(|_| OpenFailure::Viewer)?;
                let identity = process::identity(pid).map_err(|_| OpenFailure::Viewer)?;
                let executable = process::executable(identity).map_err(|_| OpenFailure::Viewer)?;
                // SAFETY: geteuid has no preconditions.
                if uid != unsafe { libc::geteuid() } || !permitted(&executable) {
                    return Err(OpenFailure::Viewer);
                }
                // The connection named is still that process's.
                if process::identity(pid).ok() != Some(identity) {
                    return Err(OpenFailure::Viewer);
                }
                target.path_for_handoff()?;
                let reply = connection
                    .call_method(
                        Some(name.as_str()),
                        OBJECT,
                        Some(NAME),
                        "ShowItems",
                        &(vec![uri.as_str()], ""),
                    )
                    .await
                    .map_err(|_| OpenFailure::Refused)?;
                bounded(reply)
                    .and_then(|reply| reply.body().deserialize::<()>())
                    .map_err(|_| OpenFailure::Refused)
            },
            async {
                async_io::Timer::after(OPEN_TIMEOUT).await;
                Err(OpenFailure::Viewer)
            },
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A file shown as itself names its path and identity; anything else
    /// names nothing to reveal.
    #[test]
    fn a_file_item_names_what_to_reveal() {
        let dir = kettle_test_support::private_tempdir("kettle-reveal-");
        let path = dir.path().join("report.docx");
        std::fs::write(&path, b"PK").unwrap();
        let identity = PathIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap();
        let facts = crate::media::file_card::FileFacts {
            bytes: 2,
            identity,
            why: crate::media::FileWhy::Unsupported,
        };
        let digest = kettle_media::Digest {
            sha256: [0; 32],
            path_identity: Some(identity),
        };
        let file = ShelfItem::file(
            1,
            None,
            "report.docx".into(),
            crate::media::Provenance::User,
            facts,
            crate::media::ItemSource::sample_file(&path, digest.clone()),
        );
        assert_eq!(
            RevealTarget::of(&file),
            Some(RevealTarget {
                path: path.clone(),
                identity
            })
        );
        assert_eq!(
            super::super::Outside::of(&file),
            Revealer::find().map(super::super::Outside::Reveal)
        );
        let picture = ShelfItem::new(
            2,
            None,
            "report.png".into(),
            crate::media::Provenance::User,
            kettle_media::MediaKind::Raster,
            Vec::new(),
            kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap(),
            crate::media::ItemSource::sample_file(&path, digest),
        );
        assert_eq!(RevealTarget::of(&picture), None);
    }

    /// The file is revealed only while the path names it: written in place
    /// it still does; replaced, gone or a directory it does not.
    #[test]
    fn only_the_file_shown_is_revealed() {
        let dir = kettle_test_support::private_tempdir("kettle-reveal-");
        let path = dir.path().join("report.docx");
        std::fs::write(&path, b"PK").unwrap();
        let identity = PathIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap();
        let target = RevealTarget {
            path: path.clone(),
            identity,
        };
        assert!(target.still_shown());
        std::fs::write(&path, b"PK and more").unwrap();
        assert!(target.still_shown(), "written in place");
        // Made before the original goes, so it cannot take its inode.
        let next = dir.path().join("next.docx");
        std::fs::write(&next, b"PK").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert!(!target.still_shown(), "replaced");
        assert_eq!(
            hand_over(&target, Revealer::Finder).map(|_| ()),
            Err(OpenFailure::Changed)
        );
        std::fs::remove_file(&path).unwrap();
        assert!(!target.still_shown(), "gone");
        let folder = RevealTarget {
            path: dir.path().to_path_buf(),
            identity: PathIdentity::of(&std::fs::metadata(dir.path()).unwrap()).unwrap(),
        };
        assert!(!folder.still_shown(), "a directory");
    }

    #[test]
    fn reveal_rechecks_the_target_after_delayed_startup() {
        let dir = kettle_test_support::private_tempdir("kettle-reveal-startup-");
        let path = dir.path().join("shown.docx");
        let replacement = dir.path().join("replacement.docx");
        std::fs::write(&path, b"shown").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        let target = RevealTarget {
            identity: PathIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap(),
            path: path.clone(),
        };
        assert!(target.still_shown(), "the target matched before startup");
        let (started, startup) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            std::fs::rename(replacement, path).unwrap();
            started.send(()).unwrap();
        });
        startup.recv().unwrap();
        assert_eq!(
            target.path_for_handoff(),
            Err(OpenFailure::Changed),
            "replacement reached the handoff after startup"
        );
        worker.join().unwrap();
    }

    /// Finder's reveal is a fixed program that selects the absolute path.
    #[test]
    fn finder_selects_the_file() {
        let file = Path::new("/Users/someone/report.docx");
        let command = command(file);
        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-R", "/Users/someone/report.docx"]
        );
    }

    /// Only a known file manager, at its own system path, receives a file
    /// to show: never another program that took the bus name.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_known_file_manager_receives_the_file() {
        assert!(!file_manager::permitted(Path::new("/usr/bin/python3")));
        assert!(!file_manager::permitted(Path::new("/tmp/nautilus")));
        assert!(!file_manager::permitted(Path::new(
            "/home/someone/bin/nautilus"
        )));
        for manager in file_manager::FILE_MANAGERS {
            let path = Path::new(manager);
            assert_eq!(
                file_manager::permitted(path),
                super::super::trusted_program(path),
                "{manager}"
            );
        }
    }

    /// Live, by hand on a Mac with a desktop session: Finder opens the
    /// file's folder with it selected. It opens a Finder window, so it is
    /// ignored by default.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "opens a Finder window"]
    fn finder_reveals_a_file_live() {
        let dir = kettle_test_support::private_tempdir("kettle-reveal-live-");
        let path = dir.path().join("report.docx");
        std::fs::write(&path, b"PK").unwrap();
        let identity = PathIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap();
        let target = RevealTarget { path, identity };
        let mut child = hand_over(&target, Revealer::Finder)
            .expect("revealed")
            .expect("open -R");
        assert!(child.wait().expect("open").success());
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
