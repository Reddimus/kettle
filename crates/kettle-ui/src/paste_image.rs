//! Clipboard bitmap → temp PNG, so a screenshot can be pasted into a pane.
//!
//! Copying a *file* in the OS file manager populates `CF_HDROP` /
//! `text/uri-list`, which `paste_clipboard` already turns into a shell-quoted
//! path. Capturing a *screenshot* (Win+Shift+S, Snipping Tool, macOS
//! Cmd+Shift+4, GNOME Screenshot) does something different: it puts a raw
//! bitmap on the clipboard with no file and no text behind it. There was
//! nothing for the file-paste branch to path-ify, so the paste did nothing at
//! all.
//!
//! This module closes that gap by materializing the bitmap as a PNG and handing
//! back its path, which then flows through the exact same
//! [`crate::mux::format_paths_for_paste`] pipeline as a copied file — shell-aware
//! quoting and WSL `C:\` → `/mnt/c` translation included. Handing a CLI agent a
//! path also sidesteps the terminal-side clipboard-bitmap decoding that agents
//! do inconsistently across platforms.
//!
//! **Privacy.** A pasted screenshot is arbitrary user content — it can contain
//! anything that was on screen. Files use owner-only permissions inside a
//! per-process scratch directory, are bounded in count and total bytes so a
//! paste loop cannot fill the disk, and are deleted when kettle exits. Windows
//! uses per-user Local App Data because the process temp directory can grant
//! sandbox principals delete-child access; other platforms use the OS temp
//! directory. A startup sweep removes directories left behind by a previous
//! crash, so captured screen content does not accumulate indefinitely.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Directory-name prefix inside the platform scratch root. Also the sweep key,
/// so it must stay stable across versions or old directories become
/// unreclaimable.
const DIR_PREFIX: &str = "kettle-paste-";

/// Per-session ceilings. A paste is a deliberate user action, so these are
/// generous — they exist to bound a stuck key or a hostile automation loop, not
/// to ration normal use.
const MAX_FILES: usize = 64;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Reject absurd dimensions before allocating. 16384² RGBA is ~1 GiB, already
/// far past any real screenshot; beyond this a malformed clipboard descriptor is
/// likelier than a genuine capture.
const MAX_DIMENSION: usize = 16_384;

/// Directories from an earlier run are removed once they are older than this.
/// Only relevant when a previous process died without running its cleanup.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Owner of this process's pasted-image scratch directory.
///
/// The directory is created lazily on the first successful paste, so a session
/// that never pastes an image touches the filesystem not at all.
pub(crate) struct PastedImages {
    dir: PathBuf,
    seq: usize,
    files: usize,
    bytes: u64,
}

impl PastedImages {
    pub(crate) fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self {
            dir: scratch_root().join(format!("{DIR_PREFIX}{}-{nonce}", std::process::id())),
            seq: 0,
            files: 0,
            bytes: 0,
        }
    }

    /// Encode `width`×`height` RGBA8 `rgba` as a PNG and return its path.
    ///
    /// Split from the clipboard call so the encode/bounds policy is testable
    /// without a live clipboard or a display server.
    pub(crate) fn save_rgba(
        &mut self,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> io::Result<PathBuf> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard image has a zero dimension",
            ));
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("clipboard image {width}x{height} exceeds the {MAX_DIMENSION}px limit"),
            ));
        }
        // A short read here would make the encoder index out of bounds, so the
        // length is checked against the declared geometry rather than trusted.
        let expected = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "clipboard image size overflows")
            })?;
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "clipboard image is {} bytes, expected {expected} for {width}x{height} RGBA",
                    rgba.len()
                ),
            ));
        }
        if self.files >= MAX_FILES || self.bytes >= MAX_TOTAL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!(
                    "pasted-image budget reached ({} files, {} bytes)",
                    self.files, self.bytes
                ),
            ));
        }

        self.seq += 1;
        let path = self.dir.join(format!("{:04}.png", self.seq));
        let file = create_private_file(&path)?;
        let mut writer = io::BufWriter::new(file);
        // `image` is already a kettle-ui dependency (the window icon decodes
        // through it), and the `png` feature it is built with covers encoding.
        use image::ImageEncoder as _;
        image::codecs::png::PngEncoder::new(&mut writer)
            .write_image(
                rgba,
                width as u32,
                height as u32,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| io::Error::other(format!("PNG encode failed: {e}")))?;
        {
            use io::Write as _;
            writer.flush()?;
        }

        let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.files += 1;
        self.bytes = self.bytes.saturating_add(written);
        Ok(path)
    }

    /// Remove this process's directory. Best effort: a failure here must never
    /// take down a shutdown path, so it is logged and swallowed.
    pub(crate) fn cleanup(&mut self) {
        if self.files == 0 && !self.dir.exists() {
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.dir)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::debug!(
                "pasted-image cleanup failed for {}: {error}",
                self.dir.display()
            );
        }
        self.files = 0;
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn with_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            seq: 0,
            files: 0,
            bytes: 0,
        }
    }
}

/// Remove pasted-image directories left by a process that died before its own
/// cleanup ran. Age-gated rather than PID-gated: PIDs are reused, and probing
/// liveness would race a sibling instance that is mid-paste.
pub(crate) fn sweep_stale() {
    let tmp = scratch_root();
    let Ok(entries) = std::fs::read_dir(&tmp) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(DIR_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn scratch_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("kettle").join("paste");
        }
    }
    std::env::temp_dir()
}

fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    kettle_state::create_private_file_new(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = scratch_root().join(format!("kettle-paste-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saves_rgba_as_a_real_png() {
        let dir = scratch("save");
        let mut images = PastedImages::with_dir(dir.clone());
        // 2x2 opaque red.
        let rgba = [255u8, 0, 0, 255].repeat(4);
        let path = images.save_rgba(2, 2, &rgba).expect("save");
        assert!(path.exists(), "the PNG must actually be written");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "must be a real PNG, not raw bytes"
        );
        // Round-trip through the decoder to prove the geometry survived.
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        // A second paste gets its own file rather than clobbering the first.
        let second = images.save_rgba(2, 2, &rgba).expect("save 2");
        assert_ne!(path, second);
        images.cleanup();
        assert!(!dir.exists(), "cleanup removes the whole directory");
    }

    #[cfg(windows)]
    #[test]
    fn default_windows_scratch_directory_uses_local_app_data() {
        let images = PastedImages::new();
        assert_eq!(
            images.dir.parent(),
            Some(scratch_root().as_path()),
            "Windows scratch images must not use a temp directory shared with sandbox principals"
        );
    }

    #[test]
    fn rejects_malformed_geometry_instead_of_panicking() {
        let dir = scratch("malformed");
        let mut images = PastedImages::with_dir(dir.clone());
        // Byte count disagrees with the declared size — the encoder would index
        // out of bounds if this were trusted.
        assert!(images.save_rgba(4, 4, &[0u8; 8]).is_err());
        assert!(images.save_rgba(0, 4, &[]).is_err(), "zero dimension");
        assert!(images.save_rgba(4, 0, &[]).is_err(), "zero dimension");
        assert!(
            images.save_rgba(MAX_DIMENSION + 1, 1, &[0u8; 4]).is_err(),
            "absurd dimensions are refused before allocating"
        );
        // A rejected paste must not create anything on disk.
        assert!(!dir.exists());
        images.cleanup();
    }

    #[test]
    fn enforces_the_per_session_file_budget() {
        let dir = scratch("budget");
        let mut images = PastedImages::with_dir(dir.clone());
        let rgba = vec![0u8, 0, 0, 255];
        for i in 0..MAX_FILES {
            images.save_rgba(1, 1, &rgba).unwrap_or_else(|e| {
                panic!("save {i} within budget failed: {e}");
            });
        }
        assert!(
            images.save_rgba(1, 1, &rgba).is_err(),
            "a paste loop must not be able to fill the disk"
        );
        images.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn sweep_leaves_this_process_directory_alone() {
        // A fresh live directory is well below the age threshold and must
        // survive a sweep.
        let mut images = PastedImages::new();
        let own = images.dir.clone();
        images.save_rgba(1, 1, &[1u8, 2, 3, 4]).expect("save");
        sweep_stale();
        assert!(
            own.exists(),
            "sweep must skip the running process's own dir"
        );
        images.cleanup();
    }
}
