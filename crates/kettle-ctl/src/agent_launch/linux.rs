use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use crate::process::{self, ProcessIdentity};

const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

/// The file held at launch. Pathnames alone are never executable identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableStamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    owner: u32,
    mode: u32,
}

impl ExecutableStamp {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            owner: metadata.uid(),
            mode: metadata.mode(),
        }
    }
}

/// A read-held native executable. Its contents are inspected only at open;
/// process checks compare fixed-size kernel metadata, never hash a binary.
#[derive(Debug)]
pub struct HeldExecutable {
    file: File,
    path: PathBuf,
    stamp: ExecutableStamp,
}

impl HeldExecutable {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().as_bytes().len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid executable path",
            ));
        }
        let path = std::fs::canonicalize(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        Self::from_file(file, path)
    }

    fn from_file(file: File, path: PathBuf) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !acceptable(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported executable",
            ));
        }
        let stamp = ExecutableStamp::of(&metadata);
        let mut header = [0; 64];
        file.read_exact_at(&mut header, 0)?;
        if !native_header(&header) || ExecutableStamp::of(&file.metadata()?) != stamp {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "changed or unsupported executable",
            ));
        }
        Ok(Self { file, path, stamp })
    }

    /// Duplicate only a descriptor the checked connecting process already
    /// holds. The launcher's copy closes on exec; this read-held copy remains.
    pub fn from_peer(
        peer: ProcessIdentity,
        descriptor: u32,
        expected: ExecutableStamp,
    ) -> io::Result<Self> {
        let live =
            || process::inspect(peer.pid()).is_ok_and(|now| now.identity == peer && !now.exited);
        if !(3..1_048_576).contains(&descriptor) || !live() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid launch descriptor",
            ));
        }
        // This is the checked peer's kernel descriptor, intentionally followed;
        // no pathname from the registration request is opened by the server.
        let path = PathBuf::from(format!("/proc/{}/fd/{descriptor}", peer.pid()));
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let held = Self::from_file(file, path)?;
        if held.stamp != expected || !live() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "changed launch descriptor",
            ));
        }
        Ok(held)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stamp(&self) -> ExecutableStamp {
        self.stamp
    }

    pub fn unchanged(&self) -> bool {
        self.file
            .metadata()
            .is_ok_and(|metadata| ExecutableStamp::of(&metadata) == self.stamp)
    }

    /// Still that process, running precisely the held file, without an edit
    /// between launch and this check. A zombie cannot print another hook.
    pub fn matches(&self, identity: ProcessIdentity) -> bool {
        let live = || {
            process::inspect(identity.pid())
                .is_ok_and(|now| now.identity == identity && !now.exited)
        };
        if !self.unchanged() || !live() {
            return false;
        }
        std::fs::metadata(format!("/proc/{}/exe", identity.pid()))
            .is_ok_and(|metadata| ExecutableStamp::of(&metadata) == self.stamp)
            && live()
    }
}

impl AsFd for HeldExecutable {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

fn acceptable(metadata: &Metadata) -> bool {
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    metadata.is_file()
        && (64..=MAX_EXECUTABLE_BYTES).contains(&metadata.len())
        && (metadata.uid() == user || metadata.uid() == 0)
        && metadata.mode() & 0o6022 == 0
        && metadata.mode() & 0o111 != 0
}

fn native_header(header: &[u8; 64]) -> bool {
    let machine = if cfg!(target_arch = "x86_64") {
        62
    } else if cfg!(target_arch = "aarch64") {
        183
    } else {
        return false;
    };
    &header[..7] == b"\x7fELF\x02\x01\x01"
        && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
        && u16::from_le_bytes([header[18], header[19]]) == machine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    fn elf() -> [u8; 64] {
        let mut header = [0; 64];
        header[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        header[16] = 3;
        header[18] = if cfg!(target_arch = "x86_64") {
            62
        } else {
            183
        };
        header
    }

    fn write(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_held_executable_matches_only_its_exact_running_process() {
        let me = process::current().unwrap().identity;
        let held = HeldExecutable::open(&std::env::current_exe().unwrap())
            .expect("hold native executable");
        assert!(held.matches(me));
        assert!(!held.matches(ProcessIdentity::new_for_tests(me.pid(), me.start() + 1)));
        assert!(!held.matches(ProcessIdentity::new_for_tests(0, me.start())));
    }

    #[test]
    fn descriptor_transfer_is_bound_to_the_checked_peer_and_exact_file() {
        use std::os::fd::AsRawFd as _;
        let me = process::current().unwrap().identity;
        let held = HeldExecutable::open(&std::env::current_exe().unwrap()).unwrap();
        let descriptor = u32::try_from(held.as_fd().as_raw_fd()).unwrap();
        let transferred = HeldExecutable::from_peer(me, descriptor, held.stamp())
            .expect("transfer held descriptor");
        assert!(transferred.matches(me));
        let reused = ProcessIdentity::new_for_tests(me.pid(), me.start() + 1);
        assert!(HeldExecutable::from_peer(reused, descriptor, held.stamp()).is_err());
        let mut other = held.stamp();
        other.inode ^= 1;
        assert!(HeldExecutable::from_peer(me, descriptor, other).is_err());
        assert!(HeldExecutable::from_peer(me, 0, held.stamp()).is_err());
    }

    #[test]
    fn edits_invalidate_a_held_executable() {
        let root = kettle_test_support::private_tempdir("kettle-agent-edit");
        let path = root.path().join("agent");
        write(&path, &elf(), 0o755);
        let held = HeldExecutable::open(&path).expect("hold ELF fixture");
        assert!(held.unchanged());
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all(b"changed").unwrap();
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        assert!(
            !held.unchanged(),
            "an in-place edit kept its launch identity"
        );
    }

    #[test]
    fn scripts_writable_files_and_oversized_files_are_refused() {
        let root = kettle_test_support::private_tempdir("kettle-agent-input");
        let path = root.path().join("agent");
        write(&path, b"#!/bin/sh\nexit 0\n", 0o755);
        assert!(HeldExecutable::open(&path).is_err(), "script");
        write(&path, &elf(), 0o775);
        assert!(HeldExecutable::open(&path).is_err(), "group writable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o4755)).unwrap();
        assert!(HeldExecutable::open(&path).is_err(), "set-id");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_EXECUTABLE_BYTES + 1)
            .unwrap();
        assert!(HeldExecutable::open(&path).is_err(), "oversized");
        assert_eq!(
            HeldExecutable::open(Path::new("relative-agent"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
