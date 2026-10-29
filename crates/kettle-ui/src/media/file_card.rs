//! A file Kettle does not draw, shown as itself.
//!
//! When a file a user or a program asked to show is a kind Kettle does not
//! draw, one it cannot decode on this system, or one past what it draws,
//! the shelf gets an item for the file rather than an error: its name, its
//! size and why nothing was drawn. Kettle reads nothing of it but its
//! metadata, from a descriptor it opens itself, and only while that is the
//! file the request named. Its card shows one picture Kettle draws itself,
//! a page, shared by every such card; its lane shows no picture, only why.

use std::sync::OnceLock;

use kettle_core::ImageData;
use kettle_media::{FailureCode, JobKind, PathIdentity};

/// Why a file was not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileWhy {
    /// Kettle draws no file of its kind.
    Unsupported,
    /// Kettle has no decoder on this system for its codec or its container,
    /// or none at all for what it is.
    NoDecoder,
    /// Larger than Kettle draws.
    TooLarge,
}

impl FileWhy {
    /// The reason a render that failed with `code` shows its file instead,
    /// if it does: never for a file that is broken, changed, or refused.
    pub(crate) fn of(code: FailureCode) -> Option<Self> {
        match code {
            FailureCode::UnsupportedMedia => Some(Self::Unsupported),
            FailureCode::CodecUnavailable
            | FailureCode::UnsupportedContainer
            | FailureCode::BackendUnavailable => Some(Self::NoDecoder),
            FailureCode::FileTooLarge => Some(Self::TooLarge),
            _ => None,
        }
    }

    /// The reason a `kind` job that failed with `code` shows its file
    /// instead. Only a job that classified the file (`Auto`) can say Kettle
    /// does not draw it: one that asked for a kind (a PDF's page, a Markdown
    /// gallery's, Mermaid) and failed says the file is not that kind, which
    /// stays the failure it was.
    pub(crate) fn of_job(kind: &JobKind, code: FailureCode) -> Option<Self> {
        matches!(kind, JobKind::Auto)
            .then(|| Self::of(code))
            .flatten()
    }

    /// The word a reply carries for it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported_media",
            Self::NoDecoder => "codec_unavailable",
            Self::TooLarge => "too_large",
        }
    }
}

/// What Kettle knows of a file it shows as itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileFacts {
    /// Its length in bytes.
    pub bytes: u64,
    /// The file it was: device, inode, size and modification time.
    pub identity: PathIdentity,
    pub why: FileWhy,
}

pub(crate) struct FactsQuery {
    pub window: u64,
    pub pane: u64,
    pub item: u64,
    pub generation: u64,
    pub source: kettle_media::Source,
    pub why: FileWhy,
}

pub(crate) struct FactsAnswer {
    pub query: FactsQuery,
    pub identity: Option<PathIdentity>,
}

pub(crate) struct FactsLane {
    work: crossbeam_channel::Sender<FactsQuery>,
    done: crossbeam_channel::Receiver<FactsAnswer>,
}

impl FactsLane {
    pub(crate) fn start(
        source: impl Fn(&FactsQuery) -> Option<PathIdentity> + Send + 'static,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Self> {
        let (work, jobs) = crossbeam_channel::bounded::<FactsQuery>(32);
        let (finished, done) = crossbeam_channel::bounded(32);
        std::thread::Builder::new()
            .name("kettle-file-facts".into())
            .spawn(move || {
                for query in jobs {
                    let identity = source(&query);
                    if finished.send(FactsAnswer { query, identity }).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .ok()?;
        Some(Self { work, done })
    }

    pub(crate) fn request(&self, query: FactsQuery) {
        let _ = self.work.try_send(query);
    }

    pub(crate) fn try_recv(&self) -> Option<FactsAnswer> {
        self.done.try_recv().ok()
    }
}

/// The picture a file's card shows: a page with its corner turned down,
/// drawn once and shared by every file card. `None` while the preview
/// account has no room for it, so no card is printed then.
pub(crate) fn poster() -> Option<ImageData> {
    static POSTER: OnceLock<ImageData> = OnceLock::new();
    shared_poster(&POSTER, &kettle_core::GraphicsBudget::previews())
}

/// The poster `cache` holds, or one drawn into it from `budget`. Only a
/// poster is kept: an account with no room now is asked again next time, as
/// it may have room once other previews are let go.
fn shared_poster(
    cache: &OnceLock<ImageData>,
    budget: &kettle_core::GraphicsBudget,
) -> Option<ImageData> {
    if let Some(poster) = cache.get() {
        return Some(poster.clone());
    }
    let (width, height) = POSTER_SIZE;
    let poster = ImageData::try_with_budget(width, height, draw_page(), budget).ok()?;
    Some(cache.get_or_init(|| poster).clone())
}

/// The poster's size: a page's shape, large enough to stay sharp on a card.
const POSTER_SIZE: (u32, u32) = (192, 240);

/// Straight RGBA of a page, edges smoothed by 4x4 samples a pixel; clear
/// outside it.
fn draw_page() -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let (width, height) = POSTER_SIZE;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (mut sum, mut covered) = ([0_u32; 3], 0_u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let point = (
                        x as f32 + (sx as f32 + 0.5) / SAMPLES as f32,
                        y as f32 + (sy as f32 + 0.5) / SAMPLES as f32,
                    );
                    if let Some(color) = page_color(point) {
                        for (total, channel) in sum.iter_mut().zip(color) {
                            *total += u32::from(channel);
                        }
                        covered += 1;
                    }
                }
            }
            match covered {
                0 => rgba.extend([0; 4]),
                _ => {
                    rgba.extend(sum.map(|total| (total / covered) as u8));
                    rgba.push((covered * 255 / (SAMPLES * SAMPLES)) as u8);
                }
            }
        }
    }
    rgba
}

/// The color of the page at `(x, y)`, or `None` off it.
fn page_color((x, y): (f32, f32)) -> Option<[u8; 3]> {
    const LEFT: f32 = 24.0;
    const RIGHT: f32 = 168.0;
    const TOP: f32 = 12.0;
    const BOTTOM: f32 = 228.0;
    const FOLD: f32 = 44.0;
    const EDGE: f32 = 4.0;
    const PAPER: [u8; 3] = [244, 244, 244];
    const BORDER: [u8; 3] = [112, 112, 112];
    const TURNED: [u8; 3] = [206, 206, 206];
    const TEXT: [u8; 3] = [186, 186, 186];
    // How far inside the page's outline `(x, y)` is, its turned corner cut
    // off along the line from (RIGHT - FOLD, TOP) to (RIGHT, TOP + FOLD).
    let corner = ((y - TOP) - (x - (RIGHT - FOLD))) / std::f32::consts::SQRT_2;
    let inside = (x - LEFT)
        .min(RIGHT - x)
        .min(y - TOP)
        .min(BOTTOM - y)
        .min(corner);
    if inside < 0.0 {
        return None;
    }
    if inside < EDGE {
        return Some(BORDER);
    }
    // The turned corner: the triangle under the cut, with its own edges.
    let (fold_x, fold_y) = (x - (RIGHT - FOLD), (TOP + FOLD) - y);
    if fold_x >= 0.0 && fold_y >= 0.0 {
        return Some(if fold_x < EDGE || fold_y < EDGE {
            BORDER
        } else {
            TURNED
        });
    }
    // Lines of text, the last shorter.
    let line = ((y - 84.0) / 26.0).floor();
    let into = (y - 84.0) - line * 26.0;
    if (0.0..5.0).contains(&line) && into < 9.0 {
        let end = if line == 4.0 { 112.0 } else { 144.0 };
        if (48.0..end).contains(&x) {
            return Some(TEXT);
        }
    }
    Some(PAPER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_file_facts_leave_the_ui_free_to_show_a_placeholder() {
        let (entered, started) = std::sync::mpsc::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let (returned, received) = std::sync::mpsc::channel();
        let ui = std::thread::spawn(move || {
            let lane = FactsLane::start(
                move |_| {
                    entered.send(()).unwrap();
                    let _ = waiting.recv();
                    None
                },
                std::sync::Arc::new(|| {}),
            )
            .unwrap();
            lane.request(FactsQuery {
                window: 1,
                pane: 2,
                item: 3,
                generation: 4,
                source: kettle_media::Source::Bytes(Vec::new()),
                why: FileWhy::Unsupported,
            });
            returned.send(lane).ok();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let responsive = received
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_ok();
        release.send(()).unwrap();
        ui.join().unwrap();
        assert!(
            responsive,
            "file facts must not hold the UI while the source is checking"
        );
        assert_eq!(
            crate::media::card_caption(
                None,
                crate::media::ItemKind::CheckingFile(FileWhy::Unsupported),
                (0, 0),
                None,
                None,
                80
            ),
            "file checking"
        );
    }

    /// A poster an account had no room for is drawn again once it has room,
    /// and kept from then on: no room once is not no card for good.
    #[test]
    fn a_poster_with_no_room_is_tried_again() {
        let (width, height) = POSTER_SIZE;
        let bytes = width as usize * height as usize * 4;
        let account = kettle_core::GraphicsBudget::independent(kettle_core::GraphicsLimits {
            image_bytes: bytes,
            retained_bytes: bytes,
            process_cpu_bytes: bytes,
            process_gpu_bytes: bytes,
            ..kettle_core::GraphicsLimits::default()
        })
        .unwrap();
        // Another picture fills the account.
        let other = ImageData::try_with_budget(width, height, vec![0; bytes], &account).unwrap();
        let cache = OnceLock::new();
        assert!(shared_poster(&cache, &account).is_none(), "no room");
        drop(other);
        let poster = shared_poster(&cache, &account).expect("room now");
        assert_eq!((poster.width, poster.height), POSTER_SIZE);
        // The poster now fills the account itself, and is kept.
        assert!(shared_poster(&cache, &account).is_some(), "kept once drawn");
    }

    /// Only a job that classified the file shows it: a PDF's page, a
    /// gallery's page or Mermaid asked of a file that is not one stays the
    /// failure it was, as `kettle show --page 2 chart.png` must.
    #[test]
    fn only_a_job_that_classified_the_file_shows_it() {
        assert_eq!(
            FileWhy::of_job(&JobKind::Auto, FailureCode::UnsupportedMedia),
            Some(FileWhy::Unsupported)
        );
        assert_eq!(
            FileWhy::of_job(&JobKind::Auto, FailureCode::FileTooLarge),
            Some(FileWhy::TooLarge)
        );
        for kind in [
            JobKind::PdfPage { index: 1 },
            JobKind::MarkdownDiagrams { index: 0 },
            JobKind::Mermaid,
        ] {
            for code in [FailureCode::UnsupportedMedia, FailureCode::FileTooLarge] {
                assert_eq!(FileWhy::of_job(&kind, code), None, "{kind:?} {code:?}");
            }
        }
    }

    /// Only a failure that says Kettle does not draw the file, cannot here,
    /// or will not at its size shows the file; a broken, changed or refused
    /// one stays an error.
    #[test]
    fn only_undrawable_files_are_shown_as_files() {
        assert_eq!(
            FileWhy::of(FailureCode::UnsupportedMedia),
            Some(FileWhy::Unsupported)
        );
        assert_eq!(
            FileWhy::of(FailureCode::CodecUnavailable),
            Some(FileWhy::NoDecoder)
        );
        assert_eq!(
            FileWhy::of(FailureCode::UnsupportedContainer),
            Some(FileWhy::NoDecoder),
            "an audio file in a video container, with no decoder to look inside"
        );
        assert_eq!(
            FileWhy::of(FailureCode::BackendUnavailable),
            Some(FileWhy::NoDecoder),
            "no decoder at all on this system"
        );
        assert_eq!(
            FileWhy::of(FailureCode::FileTooLarge),
            Some(FileWhy::TooLarge)
        );
        for code in FailureCode::ALL {
            if !matches!(
                code,
                FailureCode::UnsupportedMedia
                    | FailureCode::CodecUnavailable
                    | FailureCode::UnsupportedContainer
                    | FailureCode::BackendUnavailable
                    | FailureCode::FileTooLarge
            ) {
                assert_eq!(FileWhy::of(code), None, "{code:?}");
            }
        }
    }

    /// The poster is a page on a clear ground: clear in its corners and
    /// where its corner is turned down, opaque paper inside, and shared.
    #[test]
    fn the_poster_is_a_page() {
        let rgba = draw_page();
        let (width, height) = POSTER_SIZE;
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        let at = |x: u32, y: u32| -> [u8; 4] {
            let index = ((y * width + x) * 4) as usize;
            rgba[index..index + 4].try_into().unwrap()
        };
        assert_eq!(at(0, 0)[3], 0, "clear outside");
        assert_eq!(at(width - 1, height - 1)[3], 0, "clear outside");
        assert_eq!(at(width - 26, 13)[3], 0, "the turned corner is cut away");
        assert_eq!(at(40, 40), [244, 244, 244, 255], "paper");
        assert_eq!(at(26, 120), [112, 112, 112, 255], "its edge");
        assert_eq!(at(60, 88), [186, 186, 186, 255], "a line of text");
        let edge = at(132, 20)[3];
        assert!(edge > 0 && edge < 255, "smoothed edge: {edge}");
        let poster = poster().expect("room for the poster");
        let again = poster.clone();
        assert_eq!((poster.width, poster.height), POSTER_SIZE);
        assert!(std::sync::Arc::ptr_eq(&poster.rgba, &again.rgba));
        assert!(std::sync::Arc::ptr_eq(
            &poster.rgba,
            &self::poster().unwrap().rgba
        ));
    }
}
