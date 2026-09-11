//! Session recorder wiring for the GUI. A runtime toggle present in every build
//! (config `record = on` / `--record` / `KETTLE_RECORD*`), not a compile-time
//! feature.
//!
//! The recorder engine lives in `kettle_core::record` (compiled in via
//! `kettle-core/asciicast`, which kettle-ui enables unconditionally) so it is
//! shared by the GUI's `--record` and `kettle exec --record`. This module is a
//! thin re-export, so every `crate::dev_record::Recorder` /
//! `crate::dev_record::printable_token` call site resolves unchanged.

pub use kettle_core::record::{RecordStatus, Recorder, printable_token};

use kettle_config::Config;

/// Publish retention budgets to the recorder engine, at startup and on reload.
/// Unset keys resolve to the defaults here so a dropped key un-latches the
/// previous override.
pub fn apply_retention_config(cfg: &Config) {
    use kettle_core::record::{MAX_RECORD_BYTES, MAX_RECORD_DIRECTORY_BYTES, MAX_RECORD_FILES};
    kettle_core::record::configure_limits(
        cfg.record_max_bytes.unwrap_or(MAX_RECORD_BYTES),
        cfg.record_max_files.unwrap_or(MAX_RECORD_FILES),
        cfg.record_max_directory_bytes
            .unwrap_or(MAX_RECORD_DIRECTORY_BYTES),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_core::record::{
        MAX_RECORD_BYTES, MAX_RECORD_DIRECTORY_BYTES, MAX_RECORD_FILES, record_max_bytes,
        record_max_directory_bytes, record_max_files,
    };

    /// The policy is process-wide, so these cases cannot overlap.
    static POLICY: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_dropped_key_un_latches_the_previous_override() {
        let _policy = POLICY.lock().unwrap_or_else(|e| e.into_inner());

        let cfg = Config {
            record_max_bytes: Some(64 * 1024 * 1024),
            record_max_files: Some(20),
            record_max_directory_bytes: Some(1024 * 1024 * 1024),
            ..Config::default()
        };
        apply_retention_config(&cfg);
        assert_eq!(record_max_bytes(), 64 * 1024 * 1024);
        assert_eq!(record_max_files(), 20);
        assert_eq!(record_max_directory_bytes(), 1024 * 1024 * 1024);

        // Reloading a config with the keys removed must restore the defaults,
        // not leave the override latched in the static.
        apply_retention_config(&Config::default());
        assert_eq!(record_max_bytes(), MAX_RECORD_BYTES);
        assert_eq!(record_max_files(), MAX_RECORD_FILES);
        assert_eq!(record_max_directory_bytes(), MAX_RECORD_DIRECTORY_BYTES);
    }

    #[test]
    fn an_unusable_per_cast_budget_falls_back() {
        let _policy = POLICY.lock().unwrap_or_else(|e| e.into_inner());

        let cfg = Config {
            record_max_bytes: Some(64),
            ..Config::default()
        };
        apply_retention_config(&cfg);
        assert_eq!(
            record_max_bytes(),
            MAX_RECORD_BYTES,
            "a budget too small for the header must not reach the recorder"
        );
        apply_retention_config(&Config::default());
    }
}
