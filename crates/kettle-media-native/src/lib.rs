//! Kettle's native media code: the parts of video decoding that reach the
//! operating system directly, kept out of the renderer, which stays in safe
//! code and starts no process.
//!
//! [`tools`] finds and trusts an external decoder's binaries: only in fixed
//! places or where Kettle's configuration names, and only when no one but
//! the user or root could have put them there. [`ffmpeg`] is the external
//! decoder: ffmpeg and ffprobe, run as the worker's children with a fixed
//! argument list, exactly sized output and a deadline, contained so they
//! start no process of their own. [`acl`] reads macOS extended ACLs, which
//! can let another user write a file its mode says is private.
//!
//! The unsafe code here is libc calls, each with its own SAFETY comment: the
//! user id, a descriptor's path, ACLs, and the containment hook that runs
//! between fork and exec. On Windows, where no worker runs, the crate is
//! empty.

#[cfg(target_os = "macos")]
pub mod acl;
#[cfg(all(target_os = "macos", feature = "avfoundation"))]
pub mod avfoundation;
#[cfg(unix)]
mod contain;
#[cfg(unix)]
pub mod ffmpeg;
#[cfg(unix)]
mod reopen;
#[cfg(unix)]
mod run;
#[cfg(unix)]
pub mod tools;

#[cfg(unix)]
pub use decoders::Decoders;

#[cfg(unix)]
mod decoders {
    use std::time::Instant;

    use kettle_media::FailureCode;
    use kettle_media::VideoInfo;
    use kettle_media::video::{DecodedStills, StillsPlan, VideoDecoder, VideoInput};

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

    impl VideoDecoder for Decoders {
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
        let contained = run.find("crate::contain::contain(&mut command)").unwrap();
        let spawned = run.find(".spawn()").unwrap();
        assert!(built < contained && contained < spawned);
        assert_eq!(run.matches(".spawn()").count(), 1);
    }
}
