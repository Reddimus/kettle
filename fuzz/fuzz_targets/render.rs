#![no_main]
//! `kettle_media_render::fuzzing::render`: see its documentation for what it
//! checks. A library panic inside one of the renderer's guarded calls is the
//! guard's to answer with a fixed failure, as it is in the worker, so it
//! unwinds to the guard; any other panic aborts and is a finding.

libfuzzer_sys::fuzz_target!(
    init: {
        let finding = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !kettle_media_render::fuzzing::in_guard() {
                finding(info);
            }
        }));
        kettle_media_render::prepare();
    },
    |data: &[u8]| kettle_media_render::fuzzing::render(data)
);
