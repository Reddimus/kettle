#![no_main]
//! `kettle_vt::fuzzing::vt`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_vt::fuzzing::vt(data));
