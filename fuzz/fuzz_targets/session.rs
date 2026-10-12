#![no_main]
//! `kettle_media::fuzzing::session`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_media::fuzzing::session(data));
