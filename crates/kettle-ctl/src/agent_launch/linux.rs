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
    metadata.is_file()
        && (64..=MAX_EXECUTABLE_BYTES).contains(&metadata.len())
        && owned_and_writable_by_owner_alone(metadata)
        && metadata.mode() & 0o111 != 0
}

/// Whether a file belongs to this user or root, has no set-id bits, and
/// only its owner can write it. Group write counts as the owner's alone when
/// the group is the owner's private group: Debian and Ubuntu give each user
/// one and default to umask 002, so `npm install -g` and other user installs
/// leave files group-writable. Debian's OpenSSH accepts the same.
pub fn owned_and_writable_by_owner_alone(metadata: &Metadata) -> bool {
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    let mode = metadata.mode();
    (metadata.uid() == user || metadata.uid() == 0)
        && mode & 0o6002 == 0
        && (mode & 0o020 == 0 || private_group_of(metadata.uid(), metadata.gid()))
}

/// The owner and group facts the private-group rule reads.
#[derive(Debug, PartialEq, Eq)]
struct GroupFacts<'a> {
    owner_name: &'a [u8],
    owner_primary_gid: u32,
    gid: u32,
    group_name: &'a [u8],
    group_has_members: bool,
}

/// A user private group: the owner's primary group, named after the owner,
/// listing no supplementary members.
fn is_private_group(facts: &GroupFacts) -> bool {
    facts.owner_primary_gid == facts.gid
        && facts.group_name == facts.owner_name
        && !facts.group_has_members
}

/// [`is_private_group`] for `owner` and `gid` as the system's user and group
/// databases name them. An entry that cannot be read counts as shared.
fn private_group_of(owner: u32, gid: u32) -> bool {
    const BUFFER: usize = 16 * 1024;
    let mut user_buffer = vec![0 as libc::c_char; BUFFER];
    let mut group_buffer = vec![0 as libc::c_char; BUFFER];
    // SAFETY: zeroed POD records the reentrant lookups fill in.
    let mut user: libc::passwd = unsafe { std::mem::zeroed() };
    let mut group: libc::group = unsafe { std::mem::zeroed() };
    let mut user_found = std::ptr::null_mut();
    let mut group_found = std::ptr::null_mut();
    // SAFETY: each buffer outlives the records that point into it, and its
    // length is passed alongside it.
    let looked_up = unsafe {
        libc::getpwuid_r(
            owner,
            &mut user,
            user_buffer.as_mut_ptr(),
            BUFFER,
            &mut user_found,
        ) == 0
            && !user_found.is_null()
            && libc::getgrgid_r(
                gid,
                &mut group,
                group_buffer.as_mut_ptr(),
                BUFFER,
                &mut group_found,
            ) == 0
            && !group_found.is_null()
    };
    if !looked_up || user.pw_name.is_null() || group.gr_name.is_null() {
        return false;
    }
    // SAFETY: the lookups succeeded, so the names are NUL-terminated strings
    // in the buffers, and `gr_mem` is a NULL-terminated array or null.
    let (owner_name, group_name, group_has_members) = unsafe {
        (
            std::ffi::CStr::from_ptr(user.pw_name).to_bytes(),
            std::ffi::CStr::from_ptr(group.gr_name).to_bytes(),
            !group.gr_mem.is_null() && !(*group.gr_mem).is_null(),
        )
    };
    is_private_group(&GroupFacts {
        owner_name,
        owner_primary_gid: user.pw_gid,
        gid,
        group_name,
        group_has_members,
    })
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

    /// Group write is the owner's alone only in the owner's private group:
    /// its primary group, named after it, listing no members.
    #[test]
    fn only_a_user_private_group_counts_as_the_owners() {
        let private = GroupFacts {
            owner_name: b"kevin",
            owner_primary_gid: 1000,
            gid: 1000,
            group_name: b"kevin",
            group_has_members: false,
        };
        assert!(is_private_group(&private));
        for shared in [
            GroupFacts {
                gid: 100,
                ..private
            },
            GroupFacts {
                group_name: b"staff",
                ..private
            },
            GroupFacts {
                group_has_members: true,
                ..private
            },
            GroupFacts {
                owner_primary_gid: 100,
                ..private
            },
        ] {
            assert!(!is_private_group(&shared), "{shared:?}");
        }
    }

    #[test]
    fn scripts_writable_files_and_oversized_files_are_refused() {
        let root = kettle_test_support::private_tempdir("kettle-agent-input");
        let path = root.path().join("agent");
        write(&path, b"#!/bin/sh\nexit 0\n", 0o755);
        assert!(HeldExecutable::open(&path).is_err(), "script");
        write(&path, &elf(), 0o757);
        assert!(HeldExecutable::open(&path).is_err(), "other writable");
        write(&path, &elf(), 0o775);
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(
            HeldExecutable::open(&path).is_ok(),
            private_group_of(metadata.uid(), metadata.gid()),
            "group writable only through the owner's private group"
        );
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
