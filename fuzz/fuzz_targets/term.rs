#![no_main]
//! `kettle_core::fuzzing::term`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_core::fuzzing::term(data));
