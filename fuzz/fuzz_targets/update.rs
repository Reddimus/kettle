#![no_main]
//! `kettle_update::fuzzing::update`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_update::fuzzing::update(data));
