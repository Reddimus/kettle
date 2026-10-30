use std::fs::{File, Metadata};
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const STAGED_PREFIX: &str = ".kettle-update-staged-";
pub(crate) const PREVIOUS_PREFIX: &str = ".kettle-update-previous-";
const MAX_TRANSACTIONS: usize = 8;
const MAX_RECORD_BYTES: u64 = 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    name: String,
    identity: Option<Identity>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

pub(crate) struct Transaction {
    path: PathBuf,
    file: File,
    record: Record,
}

impl Transaction {
    pub(crate) fn create(directory: &Path, name: String) -> io::Result<Self> {
        if !valid_name(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid staging name",
            ));
        }
        let record = Record {
            name,
            identity: None,
        };
        let bytes = serde_json::to_vec(&record)?;
        for slot in 0..MAX_TRANSACTIONS {
            let path = directory.join(format!("{slot}.json"));
            if kettle_state::atomic_create_new(
                &path,
                &bytes,
                kettle_state::AtomicWriteOptions::PRIVATE,
            )? {
                return Ok(Self {
                    file: kettle_state::open_trusted_file_read(&path)?,
                    path,
                    record,
                });
            }
        }
        Err(io::Error::other(
            "too many pending macOS update transactions",
        ))
    }

    pub(crate) fn load_all(directory: &Path) -> Vec<Self> {
        (0..MAX_TRANSACTIONS)
            .filter_map(|slot| Self::load(directory.join(format!("{slot}.json"))).ok())
            .collect()
    }

    fn load(path: PathBuf) -> io::Result<Self> {
        let file = kettle_state::open_trusted_file_read(&path)?;
        let metadata = file.metadata()?;
        // Trusted reads also accept root-owned files; provenance must be ours alone.
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.len() > MAX_RECORD_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid update record metadata",
            ));
        }
        let mut bytes = Vec::new();
        (&file).take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "update record is too large",
            ));
        }
        let record: Record = serde_json::from_slice(&bytes)?;
        if !valid_name(&record.name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid staging name",
            ));
        }
        Ok(Self { path, file, record })
    }

    pub(crate) fn record_identity(&mut self, metadata: &Metadata) -> io::Result<()> {
        self.record.identity = Some(Identity::of(metadata));
        kettle_state::atomic_replace(
            &self.path,
            &serde_json::to_vec(&self.record)?,
            kettle_state::AtomicWriteOptions::PRIVATE,
        )?;
        self.file = kettle_state::open_trusted_file_read(&self.path)?;
        Ok(())
    }

    pub(crate) fn names(&self) -> [String; 2] {
        let id = self.record.name.strip_prefix(STAGED_PREFIX).unwrap();
        [self.record.name.clone(), format!("{PREVIOUS_PREFIX}{id}")]
    }

    pub(crate) fn matches(&self, name: &str, metadata: &Metadata) -> bool {
        // SAFETY: geteuid has no preconditions.
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return false;
        }
        match &self.record.identity {
            Some(identity) => {
                self.names().iter().any(|recorded| recorded == name)
                    && *identity == Identity::of(metadata)
            }
            // Only creation can precede the identity write, never the final rename.
            None => name == self.record.name && metadata.mode() & 0o7777 == 0o700,
        }
    }

    pub(crate) fn has_identity(&self) -> bool {
        self.record.identity.is_some()
    }

    pub(crate) fn remove(self) -> io::Result<()> {
        kettle_state::remove_open_private_file(self.file, &self.path)?;
        File::open(self.path.parent().unwrap())?.sync_all()
    }
}

fn valid_name(name: &str) -> bool {
    name.strip_prefix(STAGED_PREFIX).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_private_bounded_and_track_directory_identity() {
        let temp = kettle_test_support::private_tempdir("kettle-provenance-");
        let records = temp.path().join("records");
        let name = format!("{STAGED_PREFIX}one");
        let path = temp.path().join(&name);
        let mut transaction = Transaction::create(&records, name.clone()).unwrap();
        assert!(!transaction.has_identity());
        assert_eq!(
            std::fs::metadata(&transaction.path).unwrap().mode() & 0o777,
            0o600
        );
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let held = File::open(&path).unwrap();
        assert!(transaction.matches(&name, &held.metadata().unwrap()));
        assert!(!transaction.matches(&format!("{PREVIOUS_PREFIX}one"), &held.metadata().unwrap()));
        transaction
            .record_identity(&held.metadata().unwrap())
            .unwrap();
        assert!(transaction.has_identity());
        drop(transaction);
        let transaction = Transaction::load_all(&records).pop().unwrap();
        assert!(transaction.matches(&name, &held.metadata().unwrap()));
        assert!(transaction.matches(&format!("{PREVIOUS_PREFIX}one"), &held.metadata().unwrap()));
        std::fs::rename(&path, temp.path().join("moved")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!transaction.matches(&name, &std::fs::metadata(&path).unwrap()));
        for slot in 1..MAX_TRANSACTIONS {
            Transaction::create(&records, format!("{STAGED_PREFIX}{slot}")).unwrap();
        }
        assert!(Transaction::create(&records, format!("{STAGED_PREFIX}overflow")).is_err());
        transaction.remove().unwrap();
        Transaction::create(&records, format!("{STAGED_PREFIX}reuse")).unwrap();
    }

    #[test]
    fn corrupt_unsafe_and_oversized_records_are_ignored() {
        let temp = kettle_test_support::private_tempdir("kettle-provenance-invalid-");
        let records = temp.path();
        for bytes in [
            b"broken".to_vec(),
            br#"{"name":".kettle-update-staged-../victim","identity":null}"#.to_vec(),
            vec![b' '; MAX_RECORD_BYTES as usize + 1],
        ] {
            kettle_state::atomic_replace(
                &records.join("0.json"),
                &bytes,
                kettle_state::AtomicWriteOptions::PRIVATE,
            )
            .unwrap();
            assert!(Transaction::load_all(records).is_empty());
        }
        let transaction = Transaction::create(records, format!("{STAGED_PREFIX}valid")).unwrap();
        std::fs::set_permissions(
            &transaction.path,
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        assert!(Transaction::load_all(records).is_empty());
        std::fs::remove_file(&transaction.path).unwrap();
        let target = records.join("outside.json");
        kettle_state::atomic_replace(
            &target,
            br#"{"name":".kettle-update-staged-outside","identity":null}"#,
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, &transaction.path).unwrap();
        assert!(Transaction::load_all(records).is_empty());
    }
}
