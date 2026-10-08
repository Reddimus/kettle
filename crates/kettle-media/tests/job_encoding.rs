//! A worker job is encoded from a borrow: byte-for-byte the frame encoding,
//! with the same refusals, and without copying its source. This test binary
//! installs a counting allocator, so it lives outside the crate, which forbids
//! unsafe code.
use kettle_media::wire::{Direction, Frame, WireError, encode, encode_job};
use kettle_media::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Counts bytes this thread allocates while `allocated_by` measures; other
/// threads and other tests are not counted.
struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}
fn count(bytes: usize) {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATED.try_with(|n| n.set(n.get().saturating_add(bytes)));
    }
}
// SAFETY: every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocated_by(work: impl FnOnce()) -> usize {
    ALLOCATED.with(|n| n.set(0));
    COUNTING.with(|on| on.set(true));
    work();
    COUNTING.with(|on| on.set(false));
    ALLOCATED.with(Cell::get)
}

fn job(kind: JobKind, source: Source) -> Job {
    Job {
        kind,
        source,
        theme: Theme {
            background: [1, 2, 3, 255],
            foreground: [4, 5, 6, 255],
            palette: [[7, 8, 9, 255]; 16],
            accent: [10, 11, 12, 255],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width: 64,
            height: 32,
            scale: 2.0,
            crop: None,
        },
        fallback_fonts: vec![],
    }
}

fn frame_encoding(job: &Job) -> Result<Vec<u8>, WireError> {
    encode(&Frame::Job(job.clone()), Direction::ParentToWorker)
}

#[test]
fn borrowed_job_encoding_matches_the_frame_encoding() {
    let path = NativePath::new(b"/tmp/diagram.svg".to_vec()).unwrap();
    let mut with_fonts = job(
        JobKind::Svg,
        Source::user_pull(path.clone(), GuiActionWitness::from_explicit_gui_action()),
    );
    with_fonts.fallback_fonts = vec![FallbackFont {
        path: NativePath::new(b"/fonts/fallback.ttc".to_vec()).unwrap(),
        face_index: 3,
    }];
    for job in [
        job(JobKind::Raster, Source::Bytes(vec![1, 2, 3])),
        job(JobKind::Svg, Source::Bytes(b"<svg/>".to_vec())),
        job(
            JobKind::Raster,
            Source::Path {
                path,
                authorization: Authorization::ExternalAttested(ExternalAttested { dev: 1, ino: 2 }),
            },
        ),
        with_fonts,
    ] {
        assert_eq!(encode_job(&job), frame_encoding(&job), "{job:?}");
        assert!(encode_job(&job).is_ok());
    }
}

#[test]
fn borrowed_job_encoding_refuses_what_the_frame_encoding_refuses() {
    let mut no_width = job(JobKind::Raster, Source::Bytes(vec![1]));
    no_width.target.width = 0;
    for job in [
        job(JobKind::Svg, Source::Bytes(vec![0xff, 0xfe])),
        job(JobKind::Svg, Source::Bytes(vec![b' '; MAX_SVG_BYTES + 1])),
        no_width,
    ] {
        let refused = frame_encoding(&job).unwrap_err();
        assert_eq!(encode_job(&job).unwrap_err(), refused);
    }
}

/// The worker job's source can be a multi-megabyte image. Encoding it from
/// a borrow allocates the frame once; the frame path, which clones the job
/// first, shows the measurement would see a copy.
#[test]
fn borrowed_job_encoding_does_not_copy_the_source() {
    let len = 2 * 1024 * 1024;
    let job = job(JobKind::Raster, Source::Bytes(vec![7; len]));
    let mut frame = Vec::new();
    let borrowed = allocated_by(|| frame = encode_job(&job).unwrap());
    assert!(frame.len() > len);
    assert!(
        borrowed < len + len / 8,
        "encode_job allocated {borrowed} bytes for a {len}-byte source"
    );
    let cloned = allocated_by(|| drop(frame_encoding(&job).unwrap()));
    assert!(
        cloned >= 2 * len,
        "the cloning path allocated only {cloned} bytes, so a copy would go unseen"
    );
}
