#![forbid(unsafe_code)]
//! The media worker's renderer: one job in, straight RGBA or a fixed failure
//! out, in safe code. It runs only inside `kettle-media-worker`, under that
//! process's resource limits and the client's deadlines and memory limit;
//! nothing here trusts those to be the only bound, so every size is checked
//! before the work it would cost.
//!
//! Raster jobs are rendered. Every other kind answers `UnsupportedMedia` until
//! its renderer lands, and on Windows, where no worker runs, every job answers
//! `UnsupportedPlatform`.

use kettle_media::{FailureCode, Job, Rendered};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod container;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod raster;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod source;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod svg;

/// Build what every job shares (the bundled font database) ahead of any job,
/// so a worker does it before it reports Ready rather than inside a job's
/// deadline.
pub fn prepare() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    svg::prepare();
}

/// Render one job.
pub fn render(job: &Job) -> Result<Rendered, FailureCode> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        job.target.validate().map_err(|_| FailureCode::BadParams)?;
        match job.kind {
            kettle_media::JobKind::Raster => raster::render(job),
            kettle_media::JobKind::Svg => svg::render(job),
            _ => Err(FailureCode::UnsupportedMedia),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = job;
        Err(FailureCode::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use kettle_test_support::{code_only, production_source};

    /// The renderer reads its source and writes nothing: no file creation,
    /// write, rename, removal or permission change anywhere in its code.
    #[test]
    fn media_loader_never_writes() {
        for (name, source) in [
            ("lib", include_str!("lib.rs")),
            ("source", include_str!("source.rs")),
            ("container", include_str!("container.rs")),
            ("svg", include_str!("svg/mod.rs")),
            ("svg/css", include_str!("svg/css.rs")),
            ("svg/fonts", include_str!("svg/fonts.rs")),
            ("svg/layers", include_str!("svg/layers.rs")),
            ("svg/sanitize", include_str!("svg/sanitize.rs")),
            ("svg/structure", include_str!("svg/structure.rs")),
            ("raster", include_str!("raster.rs")),
        ] {
            let code = code_only(&production_source(source));
            for forbidden in [
                "File::create",
                "fs::write",
                "fs::remove",
                "fs::rename",
                "fs::create_dir",
                "fs::copy",
                "set_permissions",
                ".write(true)",
                ".append(true)",
                ".create(true)",
                ".truncate(true)",
                "OpenOptions::new().write",
                "std::process",
                "std::net",
                "std::env",
            ] {
                assert!(!code.contains(forbidden), "{name} uses {forbidden}");
            }
        }
    }
}
