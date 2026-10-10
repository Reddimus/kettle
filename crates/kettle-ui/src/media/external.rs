//! Opening a shelf item in another app, which only the user does, from a
//! card's or the viewer's menu, and never with the default association.
//! Kettle writes the pixels it shows as a PNG copy into its private
//! viewer-copy store ([`crate::paste_image::OPENED`]), so the viewer never
//! parses the bytes a program sent; marks the copy as downloaded (macOS
//! quarantine); checks that the viewer is the one this platform permits and
//! that the copy is still the one Kettle wrote; and only then hands it over.
//! A video goes to the permitted video player instead, as a checked private
//! copy of its file ([`video`]), never as its poster. The work runs on a
//! short thread, never the window thread.

mod video;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use kettle_core::ImageData;

use crate::media::ShelfItem;
use crate::paste_image::PastedImages;

pub(crate) use video::{Player, VideoCopies, VideoSource, open as open_video};

/// Opens being prepared at once; one more is refused rather than queued.
const MAX_OPENS: usize = 2;

static OPENS: AtomicUsize = AtomicUsize::new(0);

/// Preview, on the sealed system volume.
const PREVIEW_APP: &str = "/System/Applications/Preview.app";
/// Eye of GNOME, in a fresh instance of its own, so the program Kettle
/// checked is the one that opens the file, not a running one on the bus.
const EOG: &str = "/usr/bin/eog";

/// The image viewer this platform permits. Each is found only on its own
/// platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Viewer {
    /// macOS: Preview, under Apple's own signature.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Preview,
    /// Linux: Eye of GNOME, a root-owned program nobody else can change.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    EyeOfGnome,
}

impl Viewer {
    /// The permitted viewer this platform has, by a quick look once per
    /// process; each open checks it fully before it hands anything over.
    pub(crate) fn find() -> Option<Self> {
        static FOUND: OnceLock<Option<Viewer>> = OnceLock::new();
        *FOUND.get_or_init(Self::look)
    }

    fn look() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Path::new(PREVIEW_APP).is_dir().then_some(Self::Preview)
        }
        #[cfg(target_os = "linux")]
        {
            trusted_program(Path::new(EOG)).then_some(Self::EyeOfGnome)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// The menu row that opens an item in this viewer.
    pub(crate) fn label(self, tr: &kettle_i18n::Translator) -> &'static str {
        tr.text(match self {
            Self::Preview => kettle_i18n::Text::MenuOpenInPreview,
            Self::EyeOfGnome => kettle_i18n::Text::MenuOpenInImageViewer,
        })
    }

    /// Whether the viewer is still the one permitted: Preview signed by
    /// Apple as itself, or Eye of GNOME owned by root and writable by no one
    /// else.
    fn verified(self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::Preview => kettle_ctl::signing::verify_static(
                Path::new(PREVIEW_APP),
                kettle_ctl::signing::Requirement::PREVIEW,
            )
            .is_ok(),
            #[cfg(target_os = "linux")]
            Self::EyeOfGnome => trusted_program(Path::new(EOG)),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// The command that opens `file`, an absolute path, in this viewer: a
    /// fixed program and arguments, no shell and no search path.
    fn command(self, file: &Path) -> std::process::Command {
        debug_assert!(file.is_absolute(), "a leading `/` is never an option");
        let (program, args): (&str, &[&str]) = match self {
            Self::Preview => ("/usr/bin/open", &["-a", PREVIEW_APP]),
            Self::EyeOfGnome => (EOG, &["--new-instance"]),
        };
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .arg(file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }
}

/// A program file owned by root that only root may change, in a directory
/// the same holds for.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn trusted_program(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let owned_by_root = |path: &Path, file: bool| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            (if file { meta.is_file() } else { meta.is_dir() })
                && meta.uid() == 0
                && meta.mode() & 0o022 == 0
                && (!file || meta.mode() & 0o111 != 0)
        })
    };
    owned_by_root(path, true) && path.parent().is_some_and(|dir| owned_by_root(dir, false))
}

/// What opens a shelf item outside Kettle on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outside {
    /// The image viewer, given a PNG of the pixels Kettle shows.
    Image(Viewer),
    /// The video player, given a private copy of the video's file.
    Video(Player),
}

impl Outside {
    /// What opens `item` outside Kettle, if this platform has it: the video
    /// player for a video shown from its file, the image viewer for
    /// anything else, an animation included.
    pub(crate) fn of(item: &ShelfItem) -> Option<Self> {
        if item.video.is_some_and(|video| video.container.is_some()) {
            Player::find().map(Self::Video)
        } else {
            Viewer::find().map(Self::Image)
        }
    }

    /// The menu row, and the lane button's accessible name, that opens an
    /// item there.
    pub(crate) fn label(self, tr: &kettle_i18n::Translator) -> &'static str {
        match self {
            Self::Image(viewer) => viewer.label(tr),
            Self::Video(player) => player.label(tr),
        }
    }
}

/// Why an open did not happen, for the notice the user sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenFailure {
    /// Opens are already being prepared, or every copy is too new to drop.
    Busy,
    /// The copy could not be made, or was changed before the viewer got it.
    Copy,
    /// The viewer or player is gone, or is not the one permitted.
    Viewer,
    /// The player does not play this video's container or codec.
    Unplayable,
    /// The video's file is not the one shown any more.
    Changed,
    /// The video is past what Kettle copies, or its copy would leave the
    /// disk too full.
    Room,
    /// The viewer or player started but could not open the copy.
    Refused,
}

impl OpenFailure {
    /// The notice's title and body, for an image or a video.
    pub(crate) fn notice(self, video: bool) -> (kettle_i18n::Text, kettle_i18n::Text) {
        use kettle_i18n::Text;
        if !video {
            return (
                Text::NotifyTitleOpenImage,
                match self {
                    Self::Busy => Text::NotifyBodyOpenImageBusy,
                    Self::Viewer => Text::NotifyBodyOpenImageViewer,
                    Self::Refused => Text::NotifyBodyOpenImageRefused,
                    // An image is the pixels Kettle shows: nothing else
                    // applies to it.
                    Self::Copy | Self::Unplayable | Self::Changed | Self::Room => {
                        Text::NotifyBodyOpenImageCopy
                    }
                },
            );
        }
        (
            Text::NotifyTitleOpenVideo,
            match self {
                Self::Busy => Text::NotifyBodyOpenVideoBusy,
                Self::Copy => Text::NotifyBodyOpenVideoCopy,
                Self::Viewer => Text::NotifyBodyOpenVideoPlayer,
                Self::Unplayable => Text::NotifyBodyOpenVideoUnplayable,
                Self::Changed => Text::NotifyBodyOpenVideoChanged,
                Self::Room => Text::NotifyBodyOpenVideoRoom,
                Self::Refused => Text::NotifyBodyOpenVideoRefused,
            },
        )
    }
}

/// Wait for a viewer or player `child`, so it leaves no zombie, and tell
/// `failed` when it could not open what it was given: it exited with an
/// error, or could not be waited for.
fn watch(mut child: std::process::Child, failed: impl FnOnce(OpenFailure)) {
    if !child.wait().is_ok_and(|status| status.success()) {
        failed(OpenFailure::Refused);
    }
}

/// One of the [`MAX_OPENS`] slots, given back when dropped.
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        if OPENS.fetch_add(1, Ordering::AcqRel) >= MAX_OPENS {
            OPENS.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OPENS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Open a PNG copy of `image`, kept in `copies`, in `viewer`, on a short
/// thread. `failed` hears why, if it does not open; it runs on that thread.
pub(crate) fn open(
    copies: Arc<Mutex<PastedImages>>,
    image: ImageData,
    viewer: Viewer,
    failed: impl FnOnce(OpenFailure) + Send + 'static,
) {
    let Some(slot) = Slot::take() else {
        failed(OpenFailure::Busy);
        return;
    };
    // A thread that cannot start drops the slot with its closure.
    let _ = std::thread::Builder::new()
        .name("kettle-ui-open".into())
        .spawn(move || match hand_over(&copies, &image, viewer) {
            Ok(child) => {
                // The slot bounds the work of opening, not how long the
                // viewer stays up: Eye of GNOME runs until the user closes
                // it.
                drop(slot);
                watch(child, failed);
            }
            Err(why) => {
                drop(slot);
                failed(why);
            }
        });
}

/// Check the viewer, write and mark the copy, check it is still Kettle's,
/// and start the viewer on it.
fn hand_over(
    copies: &Mutex<PastedImages>,
    image: &ImageData,
    viewer: Viewer,
) -> Result<std::process::Child, OpenFailure> {
    if !viewer.verified() {
        return Err(OpenFailure::Viewer);
    }
    let width = usize::try_from(image.width).map_err(|_| OpenFailure::Copy)?;
    let height = usize::try_from(image.height).map_err(|_| OpenFailure::Copy)?;
    let mut copies = copies.lock().unwrap_or_else(PoisonError::into_inner);
    let path = copies
        .save_rgba(width, height, &image.rgba[..], false)
        .map_err(|error| {
            log::debug!("open: could not copy the image: {error}");
            // A store full of copies too new to drop asks for a moment.
            if error.kind() == std::io::ErrorKind::QuotaExceeded {
                OpenFailure::Busy
            } else {
                OpenFailure::Copy
            }
        })?;
    copies
        .mark_downloaded(&path)
        .map_err(|_| OpenFailure::Copy)?;
    if !copies.path_still_matches(&path) {
        return Err(OpenFailure::Copy);
    }
    // Started while the store is held, so the copy cannot be dropped for a
    // newer one before the viewer is on its way to it.
    viewer
        .command(&path)
        .spawn()
        .map_err(|_| OpenFailure::Viewer)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The copy the viewer gets is a 0600 PNG of exactly what Kettle shows,
    /// marked downloaded on macOS, and an open past the slots is refused at
    /// once.
    #[test]
    fn the_viewer_gets_a_marked_private_png() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!(
            "kettle-open-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let copies = Mutex::new(PastedImages::of_kind_in(
            crate::paste_image::OPENED,
            dir.clone(),
        ));
        let image = ImageData::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let path = {
            let mut store = copies.lock().unwrap();
            let path = store.save_rgba(2, 1, &image.rgba[..], false).unwrap();
            store.mark_downloaded(&path).unwrap();
            assert!(store.path_still_matches(&path));
            path
        };
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.as_raw().as_slice(), &image.rgba[..]);
        copies.lock().unwrap().cleanup();
        assert!(!dir.exists());

        let held: Vec<_> = std::iter::from_fn(Slot::take).take(MAX_OPENS + 1).collect();
        assert_eq!(held.len(), MAX_OPENS, "no slot past the bound");
        let (tx, rx) = std::sync::mpsc::channel();
        open(
            Arc::new(Mutex::new(PastedImages::of_kind_in(
                crate::paste_image::OPENED,
                dir.clone(),
            ))),
            image,
            Viewer::Preview,
            move |why| tx.send(why).unwrap(),
        );
        assert_eq!(rx.try_recv(), Ok(OpenFailure::Busy), "refused at once");
        drop(held);
        assert!(Slot::take().is_some(), "slots come back");
        assert!(!dir.exists(), "a refused open writes nothing");
    }

    /// The viewer gets the copy only after it is checked, the copy is
    /// written and marked, and the copy is checked to be still Kettle's, all
    /// in that order and under the store's lock.
    #[test]
    fn the_copy_reaches_the_viewer_only_after_every_check() {
        let source = include_str!("external.rs")
            .split("#[cfg(all(test, unix))]")
            .next()
            .expect("production part")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let body = source
            .split("fn hand_over(")
            .nth(1)
            .expect("hand_over")
            .split("\n}")
            .next()
            .expect("body");
        let order = [
            "if !viewer.verified() { return Err(OpenFailure::Viewer); }",
            "let mut copies = copies.lock().unwrap_or_else(PoisonError::into_inner);",
            ".save_rgba(width, height, &image.rgba[..], false)",
            ".mark_downloaded(&path)",
            "if !copies.path_still_matches(&path) { return Err(OpenFailure::Copy); }",
            ".command(&path) .spawn()",
        ]
        .map(|step| body.find(step).unwrap_or_else(|| panic!("{step}")));
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    }

    /// A store full of copies too new to drop asks for a moment rather than
    /// saying the copy failed.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_full_store_of_new_copies_is_busy() {
        let dir = std::env::temp_dir().join(format!("kettle-open-busy-{}", std::process::id()));
        let copies = Mutex::new(PastedImages::of_kind_in(
            crate::paste_image::OPENED,
            dir.clone(),
        ));
        let image = ImageData::new(1, 1, vec![0, 0, 0, 255]).unwrap();
        for _ in 0..32 {
            copies
                .lock()
                .unwrap()
                .save_rgba(1, 1, &image.rgba[..], false)
                .expect("save");
        }
        assert_eq!(
            hand_over(&copies, &image, Viewer::Preview).map(|_| ()),
            Err(OpenFailure::Busy)
        );
        copies.lock().unwrap().cleanup();
        assert!(!dir.exists());
    }

    /// Live, by hand on a Mac with a desktop session: Preview passes its
    /// check and opens a marked copy, and the copy goes with the store. It
    /// opens a Preview window, so it is ignored by default.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "opens a Preview window"]
    fn preview_opens_a_marked_copy_live() {
        let dir = std::env::temp_dir().join(format!("kettle-open-live-{}", std::process::id()));
        let copies = Mutex::new(PastedImages::of_kind_in(
            crate::paste_image::OPENED,
            dir.clone(),
        ));
        let pixels: Vec<u8> = (0..64 * 64)
            .flat_map(|index| [(index % 64 * 4) as u8, (index / 64 * 4) as u8, 160, 255])
            .collect();
        let image = ImageData::new(64, 64, pixels).unwrap();
        let mut child = hand_over(&copies, &image, Viewer::Preview).expect("handed over");
        assert!(child.wait().expect("open").success());
        // Preview reads the file once it is up.
        std::thread::sleep(std::time::Duration::from_secs(3));
        copies.lock().unwrap().cleanup();
        assert!(!dir.exists());
    }

    /// A viewer or player that exits with an error is heard as refusing the
    /// copy; one that exits cleanly is not.
    #[test]
    fn a_child_that_fails_is_heard() {
        let heard = |program: &str| {
            let child = std::process::Command::new(program).spawn().unwrap();
            let mut why = None;
            watch(child, |failure| why = Some(failure));
            why
        };
        assert_eq!(heard("/usr/bin/false"), Some(OpenFailure::Refused));
        assert_eq!(heard("/usr/bin/true"), None);
    }

    /// Each way a video open fails has a notice of its own, and an image's
    /// says only what can go wrong with an image.
    #[test]
    fn each_failure_says_why() {
        use kettle_i18n::Text;
        let all = [
            OpenFailure::Busy,
            OpenFailure::Copy,
            OpenFailure::Viewer,
            OpenFailure::Unplayable,
            OpenFailure::Changed,
            OpenFailure::Room,
            OpenFailure::Refused,
        ];
        let video = all.map(|why| why.notice(true));
        assert!(
            video
                .iter()
                .all(|(title, _)| *title == Text::NotifyTitleOpenVideo)
        );
        for (index, (_, body)) in video.iter().enumerate() {
            assert!(
                video[index + 1..].iter().all(|(_, other)| other != body),
                "{:?} shares its notice",
                all[index]
            );
        }
        let image = all.map(|why| why.notice(false));
        assert!(
            image
                .iter()
                .all(|(title, _)| *title == Text::NotifyTitleOpenImage)
        );
        assert_eq!(
            image.map(|(_, body)| body),
            [
                Text::NotifyBodyOpenImageBusy,
                Text::NotifyBodyOpenImageCopy,
                Text::NotifyBodyOpenImageViewer,
                Text::NotifyBodyOpenImageCopy,
                Text::NotifyBodyOpenImageCopy,
                Text::NotifyBodyOpenImageCopy,
                Text::NotifyBodyOpenImageRefused,
            ]
        );
    }

    /// The viewer command is a fixed program with the absolute file last, and
    /// a program is trusted only when root owns it and its directory and
    /// nobody else may write either.
    #[test]
    fn the_viewer_is_fixed_and_checked() {
        use std::os::unix::fs::PermissionsExt as _;
        let file = Path::new("/tmp/kettle-open-1-0.png");
        let shape = |command: std::process::Command| {
            (
                command.get_program().to_os_string(),
                command
                    .get_args()
                    .map(|arg| arg.to_os_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            shape(Viewer::Preview.command(file)),
            (
                "/usr/bin/open".into(),
                vec!["-a".into(), PREVIEW_APP.into(), file.as_os_str().into()]
            )
        );
        assert_eq!(
            shape(Viewer::EyeOfGnome.command(file)),
            (
                EOG.into(),
                vec!["--new-instance".into(), file.as_os_str().into()]
            )
        );
        // A system program is trusted: a regular file on every Unix, where
        // `/bin/sh` may be a link.
        assert!(trusted_program(Path::new("/usr/bin/env")));
        let dir = kettle_test_support::private_tempdir("kettle-open-trust-");
        let mine = dir.path().join("viewer");
        std::fs::write(&mine, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: `geteuid` has no preconditions. As root, the program is
        // root's own, which is what trust means here.
        if unsafe { libc::geteuid() } != 0 {
            assert!(
                !trusted_program(&mine),
                "a user's own program is not trusted"
            );
        }
    }
}
