#![no_main]
//! `kettle_ctl::fuzzing::ctl`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_ctl::fuzzing::ctl(data));
