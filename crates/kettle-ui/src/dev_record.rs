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
