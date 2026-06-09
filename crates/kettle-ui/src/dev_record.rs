//! Cycle 875: developer-only session recorder (Cargo feature `dev-record`,
//! compiled OUT of released / packaged builds).
//!
//! Cycle 924 (agent-first A1): the recorder itself was promoted to
//! `kettle_core::record` so it is shared by the GUI's `--record` (this
//! `dev-record` feature) and `kettle exec --record`. This module is now a thin
//! re-export, so every existing `crate::dev_record::Recorder` /
//! `crate::dev_record::printable_token` call site keeps resolving unchanged.
//! The `dev-record` feature turns on `kettle-core/asciicast`, which is what
//! actually compiles the recorder in.

pub use kettle_core::record::{Recorder, printable_token};
