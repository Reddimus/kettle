//! The entry point for fuzzing what the updater reads from the network and
//! from an archive: the signed feed manifest and the paths inside a release
//! archive. [`update`] takes any bytes and must return without panicking.
//! The fuzz target (feature `fuzzing`) and the in-process sweep below call
//! the same function.
//!
//! Inputs may include authentic release metadata. Signed with a test key, a
//! manifest the updater accepts has unique, safely named assets of a bounded
//! size with a lowercase SHA-256, and flipping any bit of it voids the signature.

use std::path::{Component, Path};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};

use crate::feed::{MAX_ARTIFACT_BYTES, validate_manifest_freshness};
use crate::install::validate_archive_path;
use crate::{SIGNING_CONTEXT, UPDATE_PUBLIC_KEY, verify_manifest};

const TEST_SECRET: [u8; 32] = [7; 32];

/// Read `data` past its first byte as a feed (manifest, then a NUL and its
/// signature) or as an archive path, as that byte picks.
pub fn update(data: &[u8]) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    if pick % 2 == 0 {
        feed(bytes, usize::from(pick / 2));
    } else if let Ok(path) = std::str::from_utf8(bytes) {
        archive_path(path);
    }
}

fn sign(manifest: &[u8]) -> Vec<u8> {
    let mut signed = SIGNING_CONTEXT.to_vec();
    signed.extend_from_slice(manifest);
    let signature = SigningKey::from_bytes(&TEST_SECRET).sign(&signed);
    base64::engine::general_purpose::STANDARD
        .encode(signature.to_bytes())
        .into_bytes()
}

fn feed(bytes: &[u8], flip: usize) {
    let (manifest, signature) = match bytes.iter().position(|&byte| byte == 0) {
        Some(at) => (&bytes[..at], &bytes[at + 1..]),
        None => (bytes, &[][..]),
    };
    let _ = verify_manifest(manifest, signature, &UPDATE_PUBLIC_KEY);
    let test_key = SigningKey::from_bytes(&TEST_SECRET)
        .verifying_key()
        .to_bytes();
    let Ok(accepted) = verify_manifest(manifest, &sign(manifest), &test_key) else {
        return;
    };
    let mut targets = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for asset in &accepted.assets {
        assert!(targets.insert(&asset.target) && names.insert(&asset.name));
        assert!((1..=MAX_ARTIFACT_BYTES).contains(&asset.size));
        assert!(asset.name.starts_with("kettle-") && !asset.name.contains(['/', '\\']));
        assert!(!asset.name.contains(".."));
        assert!(
            asset.sha256.len() == 64
                && asset
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
    }
    assert!(!accepted.assets.is_empty());
    for now in [
        UNIX_EPOCH,
        SystemTime::now(),
        UNIX_EPOCH + Duration::from_secs(u64::from(u32::MAX) * 64),
    ] {
        let _ = validate_manifest_freshness(&accepted, now);
    }
    // The signature covers every bit.
    let mut flipped = manifest.to_vec();
    let at = flip % flipped.len();
    flipped[at] ^= 1 << (flip % 8);
    assert!(
        verify_manifest(&flipped, &sign(manifest), &test_key).is_err(),
        "a manifest verified with a bit flipped at {at}"
    );
}

/// An archive path the updater accepts has only plain relative components.
fn archive_path(text: &str) {
    let path = Path::new(text);
    if validate_archive_path(path).is_err() {
        return;
    }
    assert!(!path.is_absolute() && !text.is_empty());
    for component in path.components() {
        let Component::Normal(name) = component else {
            panic!("{text:?} has a {component:?} component");
        };
        let name = name.to_str().expect("an accepted name is UTF-8");
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        );
        assert!(name != "." && name != ".." && !name.ends_with('.'));
    }
}

#[cfg(test)]
mod tests {
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    #[test]
    fn authentic_release_metadata_does_not_panic() {
        let mut input = vec![0];
        input.extend_from_slice(include_bytes!("../tests/fixtures/manifest-v1.json"));
        input.push(0);
        input.extend_from_slice(include_bytes!("../tests/fixtures/manifest-v1.json.sig"));
        update(&input);
    }

    #[test]
    fn mutated_feeds_and_archive_paths_stay_within_what_the_updater_accepts() {
        let manifest = br#"{"schema":1,"product":"kettle","channel":"stable","version":"9.1.0","tag":"v9.1.0","published_at":"2026-07-11T00:00:00Z","assets":[{"target":"x86_64-unknown-linux-gnu","name":"kettle-linux-x86_64.tar.gz","size":4,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"target":"universal-apple-darwin","name":"kettle-macos-universal.zip","size":268435456,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}]}"#;
        let mut signed = vec![0];
        signed.extend_from_slice(manifest);
        signed.push(0);
        signed.extend_from_slice(&sign(manifest));
        let mut unsigned = vec![2];
        unsigned.extend_from_slice(manifest);
        let inputs: Vec<Vec<u8>> = vec![
            signed,
            unsigned,
            b"\x01kettle/bin/kettle".to_vec(),
            b"\x01share/applications/kettle.desktop".to_vec(),
            b"\x01kettle.app/Contents/MacOS/kettle".to_vec(),
            b"\x01../etc/passwd".to_vec(),
            b"\x01CON.txt".to_vec(),
        ];
        let seeds: Vec<&[u8]> = inputs.iter().map(Vec::as_slice).collect();
        save_seeds("update", &seeds);
        for case in cases(&seeds, 600, 0x5eed_0401) {
            update(&case);
        }
    }

    /// The checks are live: a signed manifest is accepted under the test
    /// key, so the flipped-bit check runs on it.
    #[test]
    fn a_signed_manifest_reaches_the_checks() {
        let manifest = br#"{"schema":1,"product":"kettle","channel":"stable","version":"9.1.0","tag":"v9.1.0","published_at":"2026-07-11T00:00:00Z","assets":[{"target":"t","name":"kettle-a","size":1,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#;
        let test_key = SigningKey::from_bytes(&TEST_SECRET)
            .verifying_key()
            .to_bytes();
        assert!(verify_manifest(manifest, &sign(manifest), &test_key).is_ok());
        feed(manifest, 3);
    }
}
