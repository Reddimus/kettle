//! GPU uploads that skip data the GPU already holds.
//!
//! On Apple GPUs every `Queue::write_buffer` and `write_texture` copies
//! through a staging buffer with a blit, and the driver keeps its blit pool
//! (about 112 MiB, counted in the process's memory) resident for as long as
//! frames keep blitting. A single 16-byte write per frame is enough to hold
//! it. So a frame whose GPU data did not change must write nothing: every
//! buffer upload goes through [`write_buffer_if_changed`], which compares the
//! bytes with an exact CPU copy of what the buffer holds and writes only the
//! part that differs, and every texture upload through
//! [`write_texture_counted`], so the counters see it.

use std::sync::atomic::{AtomicU64, Ordering};

/// An exact CPU copy of the bytes a GPU buffer holds from offset 0.
///
/// Empty means unknown: the next write goes through whole. A pipeline calls
/// [`RetainedBytes::invalidate`] whenever it replaces the buffer.
#[derive(Debug, Default)]
pub(crate) struct RetainedBytes {
    shadow: Vec<u8>,
}

impl RetainedBytes {
    /// Forget the copy: the buffer behind it was replaced.
    pub(crate) fn invalidate(&mut self) {
        self.shadow.clear();
    }

    /// The byte range of `bytes` that must be written, or `None` when the
    /// buffer already holds all of them: equal to the copy, or a prefix of it.
    /// The range starts at the first differing byte, rounded down to wgpu's
    /// copy alignment, and runs to the end of `bytes`.
    fn changed(&self, bytes: &[u8]) -> Option<std::ops::Range<usize>> {
        let common = self
            .shadow
            .iter()
            .zip(bytes)
            .take_while(|(held, new)| held == new)
            .count();
        if common == bytes.len() {
            return None;
        }
        let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
        let start = common - common % align;
        // Our buffers hold whole f32/u32 records, so the length is aligned;
        // if a record ever is not, write everything rather than misalign.
        if !(bytes.len() - start).is_multiple_of(align) {
            return Some(0..bytes.len());
        }
        Some(start..bytes.len())
    }

    /// Record that the buffer now holds `bytes` from offset 0 up to their
    /// length; anything beyond that it held before is unchanged.
    fn store(&mut self, bytes: &[u8]) {
        if self.shadow.len() < bytes.len() {
            self.shadow.resize(bytes.len(), 0);
        }
        self.shadow[..bytes.len()].copy_from_slice(bytes);
    }
}

/// How much one pipeline wrote to the GPU. The renderer sums its pipelines'
/// counts for `ui_geometry`'s read-only `render_uploads` object.
#[derive(Debug, Default)]
pub struct UploadCounters {
    buffer_writes: AtomicU64,
    buffer_bytes: AtomicU64,
    texture_writes: AtomicU64,
    skipped_writes: AtomicU64,
}

/// A copy of the counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadCounts {
    pub buffer_writes: u64,
    pub buffer_bytes: u64,
    pub texture_writes: u64,
    pub skipped_writes: u64,
}

impl std::ops::Add for UploadCounts {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            buffer_writes: self.buffer_writes + other.buffer_writes,
            buffer_bytes: self.buffer_bytes + other.buffer_bytes,
            texture_writes: self.texture_writes + other.texture_writes,
            skipped_writes: self.skipped_writes + other.skipped_writes,
        }
    }
}

/// What a renderer has sent to the GPU since it was built: counts only, never
/// content. `ui_geometry` reports it as the read-only `render_uploads` object,
/// so a test can check that steady frames (blink edges included) write
/// nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderUploads {
    pub frames_presented: u64,
    pub buffer_writes: u64,
    pub buffer_bytes: u64,
    pub texture_writes: u64,
    pub text_prepares: u64,
    pub skipped_writes: u64,
}

impl std::iter::Sum for UploadCounts {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |a, b| a + b)
    }
}

impl UploadCounters {
    pub fn snapshot(&self) -> UploadCounts {
        UploadCounts {
            buffer_writes: self.buffer_writes.load(Ordering::Relaxed),
            buffer_bytes: self.buffer_bytes.load(Ordering::Relaxed),
            texture_writes: self.texture_writes.load(Ordering::Relaxed),
            skipped_writes: self.skipped_writes.load(Ordering::Relaxed),
        }
    }
}

/// The renderer's only `Queue::write_buffer` call: writes the part of `bytes`
/// that `buffer` does not already hold, from offset 0, and nothing when it
/// holds all of it.
pub(crate) fn write_buffer_if_changed(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    retained: &mut RetainedBytes,
    bytes: &[u8],
    counters: &UploadCounters,
) {
    match retained.changed(bytes) {
        None => {
            counters.skipped_writes.fetch_add(1, Ordering::Relaxed);
        }
        Some(range) => {
            queue.write_buffer(buffer, range.start as u64, &bytes[range.clone()]);
            counters.buffer_writes.fetch_add(1, Ordering::Relaxed);
            counters
                .buffer_bytes
                .fetch_add(range.len() as u64, Ordering::Relaxed);
            retained.store(bytes);
        }
    }
}

/// A counted write with no retained copy, for a buffer whose caller already
/// uploads only when its content changed: the grid's glyph instances, behind
/// the renderer's own damage gate. A second CPU copy of that buffer would cost
/// megabytes on a large window and save nothing.
pub(crate) fn write_buffer_counted(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    bytes: &[u8],
    counters: &UploadCounters,
) {
    if bytes.is_empty() {
        return;
    }
    queue.write_buffer(buffer, 0, bytes);
    counters.buffer_writes.fetch_add(1, Ordering::Relaxed);
    counters
        .buffer_bytes
        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
}

/// The renderer's only `Queue::write_texture` call. Texture uploads happen
/// only when content changes (a new glyph, a new image), so they are counted,
/// not compared.
pub(crate) fn write_texture_counted(
    queue: &wgpu::Queue,
    texture: wgpu::TexelCopyTextureInfo<'_>,
    data: &[u8],
    layout: wgpu::TexelCopyBufferLayout,
    size: wgpu::Extent3d,
    counters: &UploadCounters,
) {
    queue.write_texture(texture, data, layout, size);
    counters.texture_writes.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained(bytes: &[u8]) -> RetainedBytes {
        let mut r = RetainedBytes::default();
        r.store(bytes);
        r
    }

    #[test]
    fn unknown_contents_write_everything() {
        assert_eq!(RetainedBytes::default().changed(&[1, 2, 3, 4]), Some(0..4));
    }

    #[test]
    fn equal_bytes_or_a_prefix_write_nothing() {
        let r = retained(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(r.changed(&[1, 2, 3, 4, 5, 6, 7, 8]), None);
        assert_eq!(
            r.changed(&[1, 2, 3, 4]),
            None,
            "the buffer already holds them"
        );
        assert_eq!(r.changed(&[]), None);
    }

    #[test]
    fn a_change_writes_from_the_aligned_first_difference() {
        let r = retained(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(r.changed(&[1, 2, 3, 4, 5, 9, 7, 8]), Some(4..8));
        assert_eq!(
            r.changed(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 9, 9]),
            Some(8..12),
            "growth"
        );
        assert_eq!(r.changed(&[0, 2, 3, 4]), Some(0..4));
    }

    #[test]
    fn a_shorter_write_keeps_the_rest_of_the_copy() {
        let mut r = retained(&[1, 2, 3, 4, 5, 6, 7, 8]);
        r.store(&[9, 9, 9, 9]);
        assert_eq!(r.changed(&[9, 9, 9, 9, 5, 6, 7, 8]), None);
    }

    #[test]
    fn invalidation_forgets_the_copy() {
        let mut r = retained(&[1, 2, 3, 4]);
        r.invalidate();
        assert_eq!(r.changed(&[1, 2, 3, 4]), Some(0..4));
    }

    /// Every GPU upload goes through this module, so none can skip the
    /// changed-bytes check or the counters. New files are covered too.
    #[test]
    fn no_other_production_code_writes_to_the_gpu() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for entry in std::fs::read_dir(&src).expect("src") {
            let path = entry.expect("entry").path();
            if path.extension().is_none_or(|ext| ext != "rs") || path.ends_with("upload.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source");
            let production = kettle_test_support::production_source(&text);
            for needle in [".write_buffer(", ".write_texture("] {
                assert!(
                    !production.contains(needle),
                    "{} writes to the GPU directly ({needle}); use upload.rs",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 10, "the guard read the renderer's sources");
    }

    #[test]
    fn an_unaligned_tail_writes_everything() {
        let r = retained(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(r.changed(&[1, 2, 3, 4, 5, 7]), Some(0..6));
    }
}
