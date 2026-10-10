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
