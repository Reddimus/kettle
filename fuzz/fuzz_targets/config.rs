#![no_main]
//! `kettle_config::fuzzing::config`: see its documentation for what it checks.

libfuzzer_sys::fuzz_target!(|data: &[u8]| kettle_config::fuzzing::config(data));
