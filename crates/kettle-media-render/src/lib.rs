#![forbid(unsafe_code)]
//! The media worker's renderer: one job in, straight RGBA or a fixed failure
//! out, in safe code. It runs only inside `kettle-media-worker`, under that
//! process's resource limits and the client's deadlines and memory limit;
//! nothing here trusts those to be the only bound, so every size is checked
//! before the work it would cost.
//!
//! Raster, SVG, Mermaid and Markdown diagram gallery jobs are rendered, and
//! Auto jobs, which are classified by their bytes as one of those. A stills
//! job's animation (GIF, APNG, WebP) is decoded and laid out here; a video's
//! frames come from a decoder the worker holds, which this lays out
//! ([`stills`]). VideoProbe answers `UnsupportedMedia`, and on Windows,
//! where no worker runs, every job answers `UnsupportedPlatform`.

use kettle_media::{FailureCode, Job, MediaKind, Rendered};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod animation;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod auto;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod container;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod markdown;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod mermaid;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod raster;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod source;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod stills;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod svg;

/// Run `work`, answering a panic inside it with `failure`. The decoders and
/// renderers this calls may panic on input that admission did not foresee;
/// where the build unwinds (tests, and the worker's own `media-worker`
/// profile) that is a fixed failure, and where it does not, the worker
/// process is the boundary instead. Only library calls are guarded, so a
/// panic in this crate's own checks still shows up in its tests.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn guarded<T>(
    failure: FailureCode,
    work: impl FnOnce() -> Result<T, FailureCode>,
) -> Result<T, FailureCode> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or(Err(failure))
}

/// Build what every job shares (the bundled font database) ahead of any job,
/// so a worker does it before it reports Ready rather than inside a job's
/// deadline.
pub fn prepare() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    svg::prepare();
}

/// Render one job.
pub fn render(job: &Job) -> Result<Rendered, FailureCode> {
    render_with_kind(job, |_| {}).map(|(_, rendered)| rendered)
}

/// Render one job and say what it turned out to be. `on_kind` hears the kind
/// once it is known and before it is decoded or rendered: at once for an
/// explicit kind, and after classification for an Auto job, so the worker
/// can narrow its deadline to that kind's.
pub fn render_with_kind(
    job: &Job,
    on_kind: impl FnMut(MediaKind),
) -> Result<(MediaKind, Rendered), FailureCode> {
    render_with_decoder(job, on_kind, None)
}

/// Render one job as [`render_with_kind`] does, with `decoder` for a video
/// container's stills; without one they are `BackendUnavailable`. The
/// renderer starts no process: the worker supplies the decoder.
pub fn render_with_decoder(
    job: &Job,
    mut on_kind: impl FnMut(MediaKind),
    decoder: Option<&dyn kettle_media::video::VideoDecoder>,
) -> Result<(MediaKind, Rendered), FailureCode> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use kettle_media::JobKind;
        job.target.validate().map_err(|_| FailureCode::BadParams)?;
        match job.kind {
            JobKind::Auto => auto::render(job, &mut on_kind),
            JobKind::Raster => {
                on_kind(MediaKind::Raster);
                raster::render(job).map(|rendered| (MediaKind::Raster, rendered))
            }
            JobKind::Svg => {
                on_kind(MediaKind::Svg);
                svg::render(job).map(|rendered| (MediaKind::Svg, rendered))
            }
            JobKind::Mermaid => {
                on_kind(MediaKind::Mermaid);
                mermaid::render(job).map(|rendered| (MediaKind::Mermaid, rendered))
            }
            JobKind::MarkdownDiagrams { index } => {
                on_kind(MediaKind::Markdown);
                markdown::render(job, index).map(|rendered| (MediaKind::Markdown, rendered))
            }
            JobKind::VideoStills(stills) => stills::render(job, &stills, &mut on_kind, decoder)
                .map(|rendered| (MediaKind::Video, rendered)),
            JobKind::VideoProbe => Err(FailureCode::UnsupportedMedia),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (job, &mut on_kind, decoder);
        Err(FailureCode::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use kettle_test_support::{code_only, production_source};

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_guarded_panic_is_its_failure() {
        use kettle_media::FailureCode;
        assert_eq!(
            super::guarded::<()>(FailureCode::RenderParse, || panic!("a decoder's bug")),
            Err(FailureCode::RenderParse)
        );
        assert_eq!(
            super::guarded(FailureCode::RenderParse, || Err::<(), _>(
                FailureCode::TooLarge
            )),
            Err(FailureCode::TooLarge)
        );
        assert_eq!(super::guarded(FailureCode::RenderParse, || Ok(7)), Ok(7));
    }

    /// The renderer reads its source and writes nothing: no file creation,
    /// write, rename, removal or permission change anywhere in its code.
    #[test]
    fn media_loader_never_writes() {
        for (name, source) in [
            ("lib", include_str!("lib.rs")),
            ("auto", include_str!("auto.rs")),
            ("source", include_str!("source.rs")),
            ("container", include_str!("container.rs")),
            ("markdown", include_str!("markdown.rs")),
            ("animation", include_str!("animation.rs")),
            ("stills", include_str!("stills.rs")),
            ("mermaid", include_str!("mermaid.rs")),
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
