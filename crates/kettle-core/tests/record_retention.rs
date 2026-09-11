#![cfg(feature = "asciicast")]
//! End-to-end: a configured budget, not the shipped default, bounds a managed
//! recording directory. Lives here rather than in `record.rs` because
//! `configure_limits` writes process-wide statics that the in-module directory
//! tests read; a separate test binary is a separate process.

use std::io::Write as _;

use kettle_core::record::{
    MAX_RECORD_BYTES, MAX_RECORD_DIRECTORY_BYTES, MAX_RECORD_FILES, Recorder, RecordingTarget,
    configure_limits, record_max_directory_bytes, record_max_files,
};

/// `configure_limits` writes process-wide statics, so these tests cannot
/// overlap even though each is correct alone.
static POLICY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn seed(directory: &std::path::Path, count: usize) {
    for index in 0..count {
        let path = directory.join(format!("kettle-session-{index:03}-1-0.cast"));
        let mut file = kettle_state::create_private_file_new(&path).unwrap();
        file.write_all(&[b'x'; 64]).unwrap();
    }
}

fn casts(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("kettle-session-")
        })
        .count()
}

#[test]
fn a_configured_file_budget_prunes_a_managed_directory() {
    let _policy = POLICY.lock().unwrap_or_else(|e| e.into_inner());
    let temp = kettle_test_support::private_tempdir("kettle-retention-e2e-");
    seed(temp.path(), 8);
    assert_eq!(casts(temp.path()), 8);

    configure_limits(MAX_RECORD_BYTES, 3, MAX_RECORD_DIRECTORY_BYTES);
    assert_eq!(record_max_files(), 3);

    // Starting a managed recording applies retention, then adds its own cast.
    let (recorder, path) = Recorder::start_target(
        &RecordingTarget::Directory(temp.path().to_path_buf()),
        80,
        24,
        false,
    )
    .unwrap();
    drop(recorder);

    assert!(
        path.exists(),
        "the new cast survives its own retention pass"
    );
    assert!(
        casts(temp.path()) <= 3,
        "expected the configured budget of 3, found {}",
        casts(temp.path())
    );
}

#[test]
fn a_zero_budget_falls_back_to_the_shipped_default() {
    let _policy = POLICY.lock().unwrap_or_else(|e| e.into_inner());
    configure_limits(0, 0, 0);
    assert_eq!(record_max_files(), MAX_RECORD_FILES);
    assert_eq!(record_max_directory_bytes(), MAX_RECORD_DIRECTORY_BYTES);
}
