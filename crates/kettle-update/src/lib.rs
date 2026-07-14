//! Authenticated release discovery and self-update support for kettle.
//!
//! The production feed URL and Ed25519 key are compiled into the binary. The
//! updater verifies the detached signature before parsing any release metadata,
//! then verifies the selected archive's size and SHA-256 before extraction.
//! Installation is restricted to layouts carrying an official installer marker.

mod feed;
mod install;

pub use feed::{
    AvailableUpdate, CheckOutcome, FeedClient, Manifest, ManifestAsset, UpdateError,
    verify_manifest,
};
pub use install::{
    InstallDisposition, InstallOutcome, ManagedInstall, ProcessStart, RunningInstallGuard,
    detect_managed_install, install_update, is_pending_update_helper_invocation, marker_json,
    prepare_process_start, run_pending_update_helper, write_atomic_file,
};

/// Stable release feed published as GitHub release assets.
pub const MANIFEST_URL: &str =
    "https://github.com/Reddimus/kettle/releases/latest/download/kettle-update-manifest.json";
pub const SIGNATURE_URL: &str =
    "https://github.com/Reddimus/kettle/releases/latest/download/kettle-update-manifest.json.sig";

/// Dedicated Ed25519 verification key for the kettle stable update feed.
///
/// Fingerprint (SHA-256 of these raw bytes):
/// `e8e73619a959b34c24fa255714719a61c9cee810340bf041497c39475ab2dbb7`.
pub const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0xa4, 0x27, 0x30, 0x11, 0xcd, 0x2c, 0xbb, 0x1f, 0xee, 0x85, 0x74, 0xf3, 0xb8, 0xef, 0x44, 0xfd,
    0x10, 0xec, 0x35, 0x90, 0xff, 0xf9, 0x07, 0x08, 0x8a, 0x1d, 0x9a, 0x80, 0xaf, 0x4b, 0x41, 0x0b,
];

/// Domain separation prevents a valid signature from another kettle protocol
/// from being accepted as an update manifest.
pub const SIGNING_CONTEXT: &[u8] = b"kettle-update-manifest-v1\0";

/// Rust target identifier used in signed manifests for this build.
pub const fn current_target() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else {
        None
    }
}
