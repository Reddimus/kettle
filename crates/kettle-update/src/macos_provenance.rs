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
    #[serde(default)]
    bundle_identities: Option<[Identity; 2]>,
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
            bundle_identities: None,
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
        self.persist()
    }

    pub(crate) fn record_exchange(&mut self, staged: &Metadata, live: &Metadata) -> io::Result<()> {
        self.record.bundle_identities = Some([Identity::of(staged), Identity::of(live)]);
        self.persist()
    }

    pub(crate) fn bundle_matches(&self, metadata: Option<&Metadata>) -> bool {
        self.record
            .bundle_identities
            .as_ref()
            .is_none_or(|identities| {
                metadata.is_some_and(|metadata| identities.contains(&Identity::of(metadata)))
            })
    }

    fn persist(&mut self) -> io::Result<()> {
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
    name.strip_prefix(STAGED_PREFIX).is_some_and(valid_id)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether `name` is a displaced bundle's directory as an update names it.
pub(crate) fn is_previous_name(name: &str) -> bool {
    name.strip_prefix(PREVIOUS_PREFIX).is_some_and(valid_id)
}

/// The most leftovers one notice record describes.
pub(crate) const MAX_REPORTED_LEFTOVERS: usize = 256;
const MAX_REPORT_BYTES: u64 = 128 * 1024;

/// A leftover the sweep has already named to the user, by name and identity.
///
/// Identity matters as much as the name: a folder deleted and recreated under
/// the same name is a different folder, and earns its own notice.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(crate) struct Leftover {
    pub(crate) name: String,
    device: u64,
    inode: u64,
}

impl Leftover {
    pub(crate) fn of(name: &str, metadata: &Metadata) -> Self {
        Self {
            name: name.to_string(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// What the last notice named, which of those the proof refused, and which
/// kettle version made the checks.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportedLeftovers {
    pub(crate) checked_by: String,
    pub(crate) leftovers: Vec<Leftover>,
    /// Only a proof that ran and said no; one that could not run is retried.
    #[serde(default)]
    pub(crate) refused: Vec<Leftover>,
    /// Bundles a quarantine could not put back, by where they now are.
    #[serde(default)]
    pub(crate) stranded: Vec<String>,
}

/// The last notice's record, or an empty one when the record is missing,
/// foreign, oversized or malformed. A bad record only costs a repeated notice
/// and a repeated check; it never authorizes a removal.
pub(crate) fn load_reported(path: &Path) -> ReportedLeftovers {
    let read = || -> io::Result<ReportedLeftovers> {
        let file = kettle_state::open_trusted_file_read(path)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.len() > MAX_REPORT_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid leftover notice record metadata",
            ));
        }
        let mut bytes = Vec::new();
        (&file).take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
        let reported: ReportedLeftovers = serde_json::from_slice(&bytes)?;
        if bytes.len() as u64 > MAX_REPORT_BYTES
            || reported.checked_by.len() > 64
            || reported.leftovers.len() > MAX_REPORTED_LEFTOVERS
            || reported.refused.len() > MAX_REPORTED_LEFTOVERS
            || reported.stranded.len() > MAX_REPORTED_LEFTOVERS
            || reported.stranded.iter().any(|path| path.len() > 4096)
            || !reported
                .leftovers
                .iter()
                .chain(&reported.refused)
                .all(|leftover| {
                    // NAME_MAX: any entry the directory listing could have produced.
                    leftover.name.len() <= 255
                        && (leftover.name.starts_with(STAGED_PREFIX)
                            || leftover.name.starts_with(PREVIOUS_PREFIX))
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid leftover notice record",
            ));
        }
        Ok(reported)
    };
    read().unwrap_or_default()
}

/// Record the leftovers a notice has named and the version that checked them,
/// or forget them all when none remain.
pub(crate) fn store_reported(
    path: &Path,
    checked_by: &str,
    leftovers: &[Leftover],
    refused: &[Leftover],
    stranded: &[String],
) -> io::Result<()> {
    if leftovers.is_empty() && stranded.is_empty() {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let reported = ReportedLeftovers {
        checked_by: checked_by.to_string(),
        leftovers: leftovers[..leftovers.len().min(MAX_REPORTED_LEFTOVERS)].to_vec(),
        refused: refused[..refused.len().min(MAX_REPORTED_LEFTOVERS)].to_vec(),
        stranded: stranded[..stranded.len().min(MAX_REPORTED_LEFTOVERS)].to_vec(),
    };
    kettle_state::atomic_replace(
        path,
        &serde_json::to_vec(&reported)?,
        kettle_state::AtomicWriteOptions::PRIVATE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_records_bind_cleanup_to_either_expected_bundle() {
        let temp = kettle_test_support::private_tempdir("kettle-exchange-record-");
        let records = temp.path().join("records");
        let mut transaction = Transaction::create(&records, format!("{STAGED_PREFIX}one")).unwrap();
        let metadata = ["staged", "live", "unexpected"].map(|name| {
            let path = temp.path().join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::metadata(path).unwrap()
        });
        transaction
            .record_exchange(&metadata[0], &metadata[1])
            .unwrap();
        drop(transaction);
        let transaction = Transaction::load_all(&records).pop().unwrap();
        assert!(transaction.bundle_matches(Some(&metadata[0])));
        assert!(transaction.bundle_matches(Some(&metadata[1])));
        assert!(!transaction.bundle_matches(Some(&metadata[2])));
        assert!(!transaction.bundle_matches(None));
    }

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
    fn previous_names_carry_a_bounded_plain_identifier() {
        assert!(is_previous_name(
            ".kettle-update-previous-39483-1793354175937283000"
        ));
        for name in [
            ".kettle-update-previous-",
            ".kettle-update-previous-../victim",
            ".kettle-update-previous-a b",
            ".kettle-update-staged-1-1",
            "kettle.app",
        ] {
            assert!(!is_previous_name(name), "{name}");
        }
        assert!(!is_previous_name(&format!(
            "{PREVIOUS_PREFIX}{}",
            "1".repeat(129)
        )));
    }

    #[test]
    fn leftover_notice_records_are_private_bounded_and_identity_bound() {
        let temp = kettle_test_support::private_tempdir("kettle-leftover-record-");
        let path = temp.path().join("update.leftovers");
        assert_eq!(load_reported(&path), ReportedLeftovers::default());
        let name = format!("{PREVIOUS_PREFIX}1-1");
        let directory = temp.path().join(&name);
        std::fs::create_dir(&directory).unwrap();
        let leftover = Leftover::of(&name, &std::fs::metadata(&directory).unwrap());
        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        assert_ne!(
            Leftover::of(&name, &std::fs::metadata(&elsewhere).unwrap()),
            leftover,
            "the same name on another directory is another leftover"
        );

        store_reported(&path, "6.0.0", std::slice::from_ref(&leftover), &[], &[]).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let loaded = load_reported(&path);
        assert_eq!(loaded.checked_by, "6.0.0");
        assert_eq!(loaded.leftovers, std::slice::from_ref(&leftover));

        let many = (0..MAX_REPORTED_LEFTOVERS + 4)
            .map(|index| Leftover {
                name: format!("{STAGED_PREFIX}{index}"),
                device: 1,
                inode: index as u64,
            })
            .collect::<Vec<_>>();
        store_reported(&path, "6.0.0", &many, &many, &[]).unwrap();
        let loaded = load_reported(&path);
        assert_eq!(loaded.leftovers, many[..MAX_REPORTED_LEFTOVERS]);
        assert_eq!(loaded.refused, many[..MAX_REPORTED_LEFTOVERS]);

        // A record someone else could have written, or one that names anything
        // but a leftover, is no record at all: the cost is one more notice.
        store_reported(&path, "6.0.0", std::slice::from_ref(&leftover), &[], &[]).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        assert_eq!(load_reported(&path), ReportedLeftovers::default());
        for bytes in [
            b"broken".to_vec(),
            br#"{"checked_by":"6.0.0","leftovers":[{"name":"Documents","device":1,"inode":2}]}"#
                .to_vec(),
            br#"{"checked_by":"6.0.0","leftovers":[],"extra":0}"#.to_vec(),
            br#"[{"name":".kettle-update-staged-1","device":1,"inode":2}]"#.to_vec(),
            vec![b' '; MAX_REPORT_BYTES as usize + 1],
        ] {
            kettle_state::atomic_replace(&path, &bytes, kettle_state::AtomicWriteOptions::PRIVATE)
                .unwrap();
            assert_eq!(load_reported(&path), ReportedLeftovers::default());
        }

        store_reported(&path, "6.0.0", &[], &[], &[]).unwrap();
        assert!(!path.exists(), "an empty set leaves no record");
        store_reported(&path, "6.0.0", &[], &[], &[]).unwrap();
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
