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
