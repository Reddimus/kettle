//! Kettle's native media code: the parts of video decoding that reach the
//! operating system directly, kept out of the renderer, which stays in safe
//! code and starts no process.
//!
//! [`tools`] finds and trusts an external decoder's binaries: only in fixed
//! places or where Kettle's configuration names, and only when no one but
//! the user or root could have put them there. [`ffmpeg`] is the external
//! decoder: ffmpeg and ffprobe, run as the worker's children with a fixed
//! argument list, exactly sized output and a deadline (or, playing a video
//! for a session, whole frames and a processor-time limit), contained so
//! they start no process of their own. `acl` (macOS) reads extended ACLs, which
//! can let another user write a file its mode says is private. [`sandbox`]
//! is the worker's self-sandbox: Landlock and seccomp on Linux, a Seatbelt
//! profile on macOS.
//!
//! The unsafe code here is libc calls, each with its own SAFETY comment: the
//! user id, a descriptor's path, ACLs, and the containment hook that runs
//! between fork and exec. On Windows, where no worker runs, the crate is
//! empty.

#[cfg(target_os = "macos")]
pub mod acl;
#[cfg(all(target_os = "macos", feature = "avfoundation"))]
pub mod audio_out;
#[cfg(all(target_os = "macos", feature = "avfoundation"))]
pub mod avfoundation;
#[cfg(unix)]
mod contain;
#[cfg(unix)]
pub mod ffmpeg;
#[cfg(all(unix, any(test, feature = "fuzzing")))]
pub mod fuzzing;
#[cfg(all(target_os = "macos", feature = "avfoundation"))]
mod imageio;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod pulse;
#[cfg(unix)]
mod reopen;
#[cfg(unix)]
mod run;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod sandbox;
#[cfg(unix)]
pub mod tools;

#[cfg(unix)]
pub use decoders::Decoders;

#[cfg(unix)]
mod decoders {
    use std::time::Instant;

    use kettle_media::FailureCode;
    use kettle_media::VideoInfo;
    use kettle_media::video::{
        DecodedStills, PlayRequest, Playback, StillsPlan, VideoDecoder, VideoInput, VideoPlayer,
    };

    use crate::ffmpeg::Ffmpeg;

    /// The decoders a worker has, in the order they are tried: Apple's own
    /// for MP4 and QuickTime (macOS, with the `avfoundation` feature), then
    /// the external ffmpeg the parent named. A container or codec the first
    /// cannot read, or a stream it fails on, goes to the next.
    #[derive(Debug, Default)]
    pub struct Decoders {
        #[cfg(all(target_os = "macos", feature = "avfoundation"))]
        native: Option<crate::avfoundation::AvFoundation>,
        ffmpeg: Option<Ffmpeg>,
    }

    impl Decoders {
        /// Every decoder this build has, the external one as the parent
        /// named it (refused when it is not trusted).
        pub fn configured() -> Self {
            Self {
                #[cfg(all(target_os = "macos", feature = "avfoundation"))]
                native: Some(crate::avfoundation::AvFoundation),
                ffmpeg: Ffmpeg::configured().and_then(Result::ok),
            }
        }

        /// Only the external decoder, or none.
        pub fn external(ffmpeg: Option<Ffmpeg>) -> Self {
            Self {
                #[cfg(all(target_os = "macos", feature = "avfoundation"))]
                native: None,
                ffmpeg,
            }
        }

        /// What these decoders need a sandbox to let them do: the programs
        /// they run, and the trees those load from. Apple's decoder runs in
        /// the worker and needs neither.
        pub fn sandbox_needs(&self) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
            self.ffmpeg
                .as_ref()
                .map_or_else(Default::default, |ffmpeg| {
                    let programs = ffmpeg.programs().map(std::path::Path::to_path_buf).to_vec();
                    // Each program's own: ffprobe may come from another
                    // package.
                    let mut trees: Vec<_> = programs
                        .iter()
                        .filter_map(|program| crate::tools::library_tree(program))
                        .collect();
                    trees.sort();
                    trees.dedup();
                    (programs, trees)
                })
        }

        /// The native decoder that needs audio-service access. External
        /// decoders always remain outside that grant.
        pub fn sounding(&self) -> Option<Self> {
            #[cfg(all(target_os = "macos", feature = "avfoundation"))]
            if let Some(native) = self.native {
                return Some(Self {
                    native: Some(native),
                    ffmpeg: None,
                });
            }
            None
        }

        /// Whether there is any decoder to try.
        pub fn is_empty(&self) -> bool {
            #[cfg(all(target_os = "macos", feature = "avfoundation"))]
            if self.native.is_some() {
                return false;
            }
            self.ffmpeg.is_none()
        }
    }

    /// Whether a decoder's failure leaves the next one something to try: it
    /// could not read the container or decode the codec, or failed on the
    /// stream in a way another decoder may not.
    #[cfg(all(target_os = "macos", feature = "avfoundation"))]
    fn next_may_succeed(failure: FailureCode) -> bool {
        matches!(
            failure,
            FailureCode::UnsupportedContainer
                | FailureCode::CodecUnavailable
                | FailureCode::BackendUnavailable
                | FailureCode::RenderParse
        )
    }

    /// Whether a still the native decoder failed on is the external one's to
    /// try: only one the native decoder cannot read at all (AVIF before
    /// macOS 13). A still it could not parse is broken, and stays so rather
    /// than becoming a decoder that is missing.
    #[cfg(all(target_os = "macos", feature = "avfoundation"))]
    fn still_falls_through(failure: FailureCode) -> bool {
        failure == FailureCode::CodecUnavailable
    }

    /// The native decoder's answer, or the external one's when the native
    /// one failed in a way `falls_through` passes on and there is one to ask.
    #[cfg(all(target_os = "macos", feature = "avfoundation"))]
    fn native_then_external<T>(
        native: Result<T, FailureCode>,
        external: Option<impl FnOnce() -> Result<T, FailureCode>>,
        falls_through: fn(FailureCode) -> bool,
    ) -> Result<T, FailureCode> {
        match (native, external) {
            (Err(failure), Some(external)) if falls_through(failure) => external(),
            (answer, _) => answer,
        }
    }

    impl VideoDecoder for Decoders {
        /// A HEIF or AVIF still: Apple's own decoder on macOS, then the
        /// trusted ffmpeg for one it cannot read (AVIF before macOS 13);
        /// elsewhere the trusted ffmpeg, from 7.1.
        fn still_image(
            &self,
            input: kettle_media::video::StillInput<'_>,
            still: kettle_media::video::StillImage,
            max_edge: u32,
            deadline: Instant,
        ) -> Result<kettle_media::video::DecodedStill, FailureCode> {
            #[cfg(all(target_os = "macos", feature = "avfoundation"))]
            if let Some(native) = &self.native {
                return native_then_external(
                    native.still_image(input, still, max_edge, deadline),
                    self.ffmpeg
                        .as_ref()
                        .map(|ffmpeg| move || ffmpeg.still_image(input, still, max_edge, deadline)),
                    still_falls_through,
                );
            }
            self.ffmpeg
                .as_ref()
                .ok_or(FailureCode::BackendUnavailable)?
                .still_image(input, still, max_edge, deadline)
        }

        fn stills(
            &self,
            input: VideoInput<'_>,
            plan: &mut dyn FnMut(&VideoInfo) -> Result<StillsPlan, FailureCode>,
            deadline: Instant,
        ) -> Result<DecodedStills, FailureCode> {
            #[cfg(all(target_os = "macos", feature = "avfoundation"))]
            if let Some(native) = &self.native {
                match native.stills(input, plan, deadline) {
                    Ok(stills) => return Ok(stills),
                    Err(failure) if next_may_succeed(failure) && self.ffmpeg.is_some() => {}
                    Err(failure) => return Err(failure),
                }
            }
            self.ffmpeg
                .as_ref()
                .ok_or(FailureCode::BackendUnavailable)?
                .stills(input, plan, deadline)
        }
    }

    impl VideoPlayer for Decoders {
        fn play(
            &self,
            input: VideoInput<'_>,
            request: PlayRequest,
            deadline: Instant,
        ) -> Result<Playback, FailureCode> {
            #[cfg(all(target_os = "macos", feature = "avfoundation"))]
            if let Some(native) = &self.native {
                match native.play(input, request, deadline) {
                    Ok(playback) => return Ok(playback),
                    Err(failure) if next_may_succeed(failure) && self.ffmpeg.is_some() => {}
                    Err(failure) => return Err(failure),
                }
            }
            self.ffmpeg
                .as_ref()
                .ok_or(FailureCode::BackendUnavailable)?
                .play(input, request, deadline)
        }
    }

    #[cfg(all(test, target_os = "macos", feature = "avfoundation"))]
    mod tests {
        use super::*;

        /// The external decoder is asked only when the native one could not
        /// read the still at all, as ImageIO cannot read an AVIF before
        /// macOS 13; a still it found broken, or refused for itself, stays
        /// so, and the native answer stands when there is no external one.
        /// `still_image` asks by this rule.
        #[test]
        fn the_external_decoder_reads_what_the_native_one_cannot() {
            let external = || Ok::<_, FailureCode>("ffmpeg");
            let still = |native| native_then_external(native, Some(external), still_falls_through);
            assert_eq!(still(Ok("imageio")), Ok("imageio"));
            assert_eq!(still(Err(FailureCode::CodecUnavailable)), Ok("ffmpeg"));
            for kept in [
                FailureCode::RenderParse,
                FailureCode::BackendUnavailable,
                FailureCode::FileTooLarge,
                FailureCode::SandboxUnavailable,
                FailureCode::Changed,
            ] {
                assert_eq!(still(Err(kept)), Err(kept), "{kept:?}");
            }
            let none: Option<fn() -> Result<&'static str, FailureCode>> = None;
            assert_eq!(
                native_then_external(
                    Err(FailureCode::CodecUnavailable),
                    none,
                    still_falls_through
                ),
                Err(FailureCode::CodecUnavailable)
            );
            let code = kettle_test_support::code_only(&kettle_test_support::production_source(
                include_str!("lib.rs"),
            ));
            let asked = code
                .split("fn still_image(")
                .nth(1)
                .and_then(|body| body.split("fn stills(").next())
                .unwrap();
            assert!(asked.contains("native_then_external("));
            assert!(asked.contains("still_falls_through,"));
        }
    }
}

#[cfg(test)]
mod tests {
    use kettle_test_support::{code_only, production_source};

    /// Every process this crate starts goes through `run`, which contains
    /// it: no other module builds a command, and `run` contains each one it
    /// builds before starting it.
    #[test]
    fn every_decoder_runs_contained() {
        for (name, source) in [
            ("lib", include_str!("lib.rs")),
            ("tools", include_str!("tools.rs")),
            ("reopen", include_str!("reopen.rs")),
            ("ffmpeg", include_str!("ffmpeg.rs")),
            ("contain", include_str!("contain.rs")),
            ("acl", include_str!("acl.rs")),
            ("avfoundation", include_str!("avfoundation.rs")),
        ] {
            let code = code_only(&production_source(source));
            for spawning in [
                "Command::new",
                ".spawn(",
                ".output(",
                ".status(",
                "posix_spawn",
                "fork(",
            ] {
                assert!(
                    !code.contains(spawning),
                    "{name} starts a process: {spawning}"
                );
            }
        }
        let run = code_only(&production_source(include_str!("run.rs")));
        assert_eq!(run.matches("Command::new").count(), 1);
        let built = run.find("Command::new").unwrap();
        let contained = run.find("crate::contain::contain(&mut command,").unwrap();
        let spawned = run.find(".spawn()").unwrap();
        assert!(built < contained && contained < spawned);
        assert_eq!(run.matches(".spawn()").count(), 1);
    }
}
