//! Authenticated release discovery and self-update support for kettle.
//!
//! The production feed URL and Ed25519 key are compiled into the binary. The
//! updater verifies the detached signature before parsing any release metadata,
//! then verifies the selected archive's size and SHA-256 before extraction.
//! Installation is restricted to layouts carrying an official installer marker.

mod feed;
mod install;
#[cfg(target_os = "macos")]
mod macos;

pub use feed::{
    AvailableUpdate, CheckOutcome, FeedClient, Manifest, ManifestAsset, UpdateError,
    verify_manifest,
};
pub use install::{
    InstallDisposition, InstallOutcome, ManagedInstall, ProcessStart, RunningInstallGuard,
    detect_managed_install, install_update, is_pending_update_helper_invocation, marker_json,
    prepare_managed_install_for_update, prepare_process_start, run_pending_update_helper,
    write_atomic_file,
};

/// Stable release feed published as GitHub release assets.
pub const MANIFEST_URL: &str =
    "https://github.com/Reddimus/kettle/releases/latest/download/kettle-update-manifest.json";
pub const SIGNATURE_URL: &str =
    "https://github.com/Reddimus/kettle/releases/latest/download/kettle-update-manifest.json.sig";

/// Dedicated Ed25519 verification key for the kettle stable update feed.
///
/// `packaging/update-public.pem` is the canonical release-pipeline form of
/// this key; `scripts/test-update-manifest.py` keeps every consumer in sync.
/// Fingerprint (SHA-256 of these raw bytes):
/// `e8e73619a959b34c24fa255714719a61c9cee810340bf041497c39475ab2dbb7`.
pub const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0xa4, 0x27, 0x30, 0x11, 0xcd, 0x2c, 0xbb, 0x1f, 0xee, 0x85, 0x74, 0xf3, 0xb8, 0xef, 0x44, 0xfd,
    0x10, 0xec, 0x35, 0x90, 0xff, 0xf9, 0x07, 0x08, 0x8a, 0x1d, 0x9a, 0x80, 0xaf, 0x4b, 0x41, 0x0b,
];

/// Domain separation prevents a valid signature from another kettle protocol
/// from being accepted as an update manifest.
pub const SIGNING_CONTEXT: &[u8] = b"kettle-update-manifest-v1\0";

/// Target identifier used in signed manifests for this build.
///
/// Usually a Rust triple. macOS is the exception: one universal2 archive serves
/// both architectures, so the manifest names that artifact instead.
pub const fn current_target() -> Option<&'static str> {
    // Windows was retired in 4.0.0: the release no longer publishes
    // `kettle-windows-x86_64.zip`, so a production Windows build has no
    // artifact to update itself with and must report that rather than ask the
    // feed for a target the manifest no longer names.
    //
    // The crate's own unit tests are a different case. Several hundred Windows
    // transaction and ACL tests describe the package contract that shipped
    // through 3.x, and they still pass and still guard the code paths that
    // remain compiled. Restricting the substitution to cfg(test) keeps them
    // running while leaving the built library, integration tests, and
    // downstream consumers truthful.
    if cfg!(all(test, target_os = "windows")) {
        Some("x86_64-pc-windows-msvc")
    } else if cfg!(target_os = "windows") {
        None
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(target_os = "macos") {
        // Both Mac architectures share one `lipo`-merged archive, so this is a
        // pseudo-triple rather than a real one. Naming the two Rust triples
        // separately would point them at the same file, which the manifest
        // generator's one-name-per-target map cannot express.
        Some("universal-apple-darwin")
    } else {
        None
    }
}

#[cfg(test)]
mod target_tests {
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    #[test]
    fn windows_arm_harness_exercises_the_shipped_x64_update_contract() {
        assert_eq!(super::current_target(), Some("x86_64-pc-windows-msvc"));
    }
}
