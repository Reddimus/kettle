//! The inert worker capsule carried through the 4.9 Linux updater.

use serde::{Deserialize, Serialize};

/// These names are package data, never worker lookup locations.
pub const CAPSULE_BYTES: &str = "shell-integration/kettle-media-worker.bin";
pub const CAPSULE_METADATA: &str = "shell-integration/kettle-media-worker.json";

/// Package identity required when promoting a capsule to the binary directory.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCapsule {
    pub schema: u32,
    pub target: String,
    pub version: String,
    pub source_hash: String,
    pub size: u64,
    pub sha256: String,
}

impl WorkerCapsule {
    /// Identity embedded in the same source build as the running terminal.
    pub fn matches_running_build(&self, target: &str, version: &str) -> bool {
        self.schema == 1
            && self.target == target
            && self.version == version
            && self.source_hash == env!("KETTLE_SOURCE_HASH")
    }

    /// Describe worker bytes for the release packager.
    pub fn for_package(target: String, bytes: &[u8]) -> Self {
        use sha2::{Digest as _, Sha256};
        Self {
            schema: 1,
            target,
            version: env!("CARGO_PKG_VERSION").into(),
            source_hash: env!("KETTLE_SOURCE_HASH").into(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_records_exact_package_bytes_and_build() {
        let capsule = WorkerCapsule::for_package("aarch64-unknown-linux-gnu".into(), b"abc");
        assert_eq!(capsule.size, 3);
        assert_eq!(
            capsule.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(
            capsule.matches_running_build("aarch64-unknown-linux-gnu", env!("CARGO_PKG_VERSION"))
        );
        assert!(
            !capsule.matches_running_build("x86_64-unknown-linux-gnu", env!("CARGO_PKG_VERSION"))
        );
        assert!(!capsule.matches_running_build("aarch64-unknown-linux-gnu", "0.0.0"));
    }
}
