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
//!
//! A frame whose instances did change, such as each line of output, would
//! still blit. Where the CPU and GPU share memory (Metal or Vulkan on an
//! integrated or software adapter, see [`mapped_upload_features`]), the
//! per-frame instances go through a [`MappedRing`] instead: the CPU copies
//! them into a mapped buffer that the draw then reads in place, with no
//! staging copy and no blit.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use kettle_core::{GraphicsBudget, GraphicsReservation};

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

    /// The bytes the buffer holds from offset 0, as far as they are known.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.shadow
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

    /// Record that a different buffer now holds exactly `bytes`. Whatever lay
    /// past them in the previous buffer is not in this one.
    fn replace(&mut self, bytes: &[u8]) {
        self.shadow.clear();
        self.shadow.extend_from_slice(bytes);
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
    mapped_writes: AtomicU64,
    mapped_bytes: AtomicU64,
}

/// A copy of the counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadCounts {
    pub buffer_writes: u64,
    pub buffer_bytes: u64,
    pub texture_writes: u64,
    pub skipped_writes: u64,
    pub mapped_writes: u64,
    pub mapped_bytes: u64,
}

impl std::ops::Add for UploadCounts {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            buffer_writes: self.buffer_writes + other.buffer_writes,
            buffer_bytes: self.buffer_bytes + other.buffer_bytes,
            texture_writes: self.texture_writes + other.texture_writes,
            skipped_writes: self.skipped_writes + other.skipped_writes,
            mapped_writes: self.mapped_writes + other.mapped_writes,
            mapped_bytes: self.mapped_bytes + other.mapped_bytes,
        }
    }
}

/// What a renderer has sent to the GPU since it was built: counts only, never
/// content. `ui_geometry` reports it as the read-only `render_uploads` object,
/// so a test can check that steady frames (blink edges included) write
/// nothing, and that a window printing on shared memory writes its instances
/// through mapped buffers rather than the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderUploads {
    pub frames_presented: u64,
    /// Queue writes (`write_buffer`): each one is a staging copy and a blit.
    pub buffer_writes: u64,
    pub buffer_bytes: u64,
    pub texture_writes: u64,
    pub text_prepares: u64,
    /// Main and menu prepares, excluding the cursor-only renderer.
    pub chrome_prepares: u64,
    pub skipped_writes: u64,
    /// Whether this renderer's device writes per-frame instances through
    /// mapped buffers (see `mapped_upload_features`).
    pub mapped_uploads: bool,
    /// Instance data copied into a mapped buffer: no staging copy, no blit.
    pub mapped_writes: u64,
    pub mapped_bytes: u64,
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
            mapped_writes: self.mapped_writes.load(Ordering::Relaxed),
            mapped_bytes: self.mapped_bytes.load(Ordering::Relaxed),
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

/// The device features the renderer asks `adapter` for: only
/// `MAPPABLE_PRIMARY_BUFFERS`, and only where a buffer the CPU maps is memory
/// the GPU reads in place, which is Metal or Vulkan on an integrated or
/// software adapter. On a discrete GPU a mapped vertex buffer lives in system
/// memory and every draw reads it across the bus (wgpu warns that this is a
/// performance trap); GL cannot map one at all. Those keep the queue path.
/// Windows keeps it too, whatever the adapter: the mapped path is untested
/// there, and DX12, its default backend, cannot take it anyway.
pub(crate) fn mapped_upload_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let info = adapter.get_info();
    mapped_upload_features_for(
        cfg!(windows),
        info.backend,
        info.device_type,
        adapter.features(),
    )
}

fn mapped_upload_features_for(
    windows: bool,
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    supported: wgpu::Features,
) -> wgpu::Features {
    if windows {
        return wgpu::Features::empty();
    }
    let shared_memory = matches!(
        device_type,
        wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::Cpu
    );
    let mappable_backend = matches!(backend, wgpu::Backend::Metal | wgpu::Backend::Vulkan);
    if shared_memory
        && mappable_backend
        && supported.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS)
    {
        wgpu::Features::MAPPABLE_PRIMARY_BUFFERS
    } else {
        wgpu::Features::empty()
    }
}

/// Whether `device` writes per-frame instances through a [`MappedRing`]: only
/// a device requested with [`mapped_upload_features`] has the feature.
pub(crate) fn mapped_uploads(device: &wgpu::Device) -> bool {
    device
        .features()
        .contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS)
}

/// The most buffers one ring holds. This is a memory cap, not a guarantee
/// that a spare is ready. Metal may have three drawables outstanding at
/// upload time. Completion callbacks decide reuse; backpressure uses the queue.
pub(crate) const MAPPED_RING_SLOTS: usize = 3;

/// The smallest buffer a ring maps, so a prompt's handful of instances does
/// not reallocate as it grows by a few.
const MAPPED_RING_MIN_BYTES: u64 = 4096;

const MAP_PENDING: u8 = 0;
const MAP_READY: u8 = 1;
const MAP_FAILED: u8 = 2;

/// A spare buffer as the ring's plan sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlotInfo {
    /// Mapped: the CPU may fill it.
    ready: bool,
    size: u64,
    /// When it stopped being drawn from; lower is older.
    retired_at: u64,
}

/// What [`MappedRing::write`] does with new data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RingStep {
    /// Fill this spare: it is mapped and large enough.
    Fill(usize),
    /// Map a new buffer, first dropping this spare to stay within the ring.
    Create { evict: Option<usize> },
    /// Reuse a fitting current buffer when a spare is unavailable:
    /// write into it through the queue, as the unmapped path does. That is a
    /// blit, and counted as a buffer write.
    Queue,
}

/// The ring's choice for `needed` bytes, from its spares and the size of the
/// buffer the draws read now (`current`).
fn plan(spare: &[SlotInfo], current: Option<u64>, needed: u64) -> RingStep {
    if let Some(index) = spare.iter().position(|s| s.ready && s.size >= needed) {
        return RingStep::Fill(index);
    }
    // A mapped buffer too small for the data is replaced, not kept beside
    // the new one.
    if let Some(index) = spare.iter().position(|s| s.ready) {
        return RingStep::Create { evict: Some(index) };
    }
    if spare.len() + usize::from(current.is_some()) < MAPPED_RING_SLOTS {
        return RingStep::Create { evict: None };
    }
    if current.is_some_and(|size| size >= needed) {
        return RingStep::Queue;
    }
    // The data outgrew the current buffer while every other one is still in
    // use: replace the one retired longest ago.
    let oldest = spare
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| s.retired_at)
        .map(|(index, _)| index);
    RingStep::Create { evict: oldest }
}

/// The size of a new ring buffer for `needed` bytes: a power of two, at least
/// [`MAPPED_RING_MIN_BYTES`], or `None` if that overflows.
fn ring_bytes(needed: u64) -> Option<u64> {
    needed
        .max(MAPPED_RING_MIN_BYTES)
        .checked_next_power_of_two()
}

struct MappedSlot {
    buffer: wgpu::Buffer,
    /// `MAP_PENDING`, `MAP_READY` or `MAP_FAILED`. The map callback sets it
    /// on whichever thread polls the device, the screenshot worker included.
    map: Arc<AtomicU8>,
    retired_at: u64,
    /// The buffer's charge against the window's GPU budget, if it has one.
    _charge: Option<GraphicsReservation>,
}

impl MappedSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            ready: self.map.load(Ordering::Acquire) == MAP_READY,
            size: self.buffer.size(),
            retired_at: self.retired_at,
        }
    }
}

/// Per-frame instances written through mapped buffers, on a device with
/// [`mapped_upload_features`].
///
/// The draws read the current buffer, which is unmapped. New data goes into
/// a spare that is mapped, which then becomes current; the buffer it replaces
/// is mapped again, and wgpu completes that map only once every frame that
/// drew from it has finished on the GPU. So the CPU never writes a buffer a
/// frame in flight still reads. Unchanged data writes nothing, as on the
/// queue path, and the draws keep reading the current buffer.
pub(crate) struct MappedRing {
    label: &'static str,
    usage: wgpu::BufferUsages,
    budget: Option<GraphicsBudget>,
    current: Option<MappedSlot>,
    spare: Vec<MappedSlot>,
    retired: u64,
    warned_map_failure: bool,
}

impl MappedRing {
    /// A ring of `usage` buffers (with `MAP_WRITE` and `COPY_DST` added),
    /// each charged to `budget` when there is one.
    pub(crate) fn new(
        label: &'static str,
        usage: wgpu::BufferUsages,
        budget: Option<GraphicsBudget>,
    ) -> Self {
        Self {
            label,
            usage,
            budget,
            current: None,
            spare: Vec::with_capacity(MAPPED_RING_SLOTS),
            retired: 0,
            warned_map_failure: false,
        }
    }

    /// The buffer the draws read, once anything has been written.
    pub(crate) fn current(&self) -> Option<&wgpu::Buffer> {
        self.current.as_ref().map(|slot| &slot.buffer)
    }

    /// Make the current buffer hold `bytes` from offset 0. With `retained`,
    /// data the current buffer already holds writes nothing; without it, the
    /// caller uploads only data that changed. Returns false when no buffer
    /// could take the data (the GPU budget refused it), and the caller then
    /// draws nothing.
    pub(crate) fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        retained: Option<&mut RetainedBytes>,
        bytes: &[u8],
        counters: &UploadCounters,
    ) -> bool {
        if bytes.is_empty() {
            if retained.is_some() {
                counters.skipped_writes.fetch_add(1, Ordering::Relaxed);
            }
            return true;
        }
        if self.current.is_some()
            && retained
                .as_deref()
                .is_some_and(|held| held.changed(bytes).is_none())
        {
            counters.skipped_writes.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        // A buffer whose map failed (the device was lost) will never map.
        self.spare
            .retain(|slot| slot.map.load(Ordering::Acquire) != MAP_FAILED);
        let needed = bytes.len() as u64;
        let mut infos = [SlotInfo::default(); MAPPED_RING_SLOTS];
        let known = self.spare.len().min(MAPPED_RING_SLOTS);
        for (info, slot) in infos.iter_mut().zip(&self.spare) {
            *info = slot.info();
        }
        let current_size = self.current.as_ref().map(|slot| slot.buffer.size());
        let slot = match plan(&infos[..known], current_size, needed) {
            RingStep::Queue => return self.write_current(queue, retained, bytes, counters),
            RingStep::Fill(index) => self.spare.swap_remove(index),
            RingStep::Create { evict } => {
                if let Some(index) = evict {
                    // wgpu keeps the buffer alive until frames still reading
                    // it finish; a pending map is abandoned.
                    drop(self.spare.swap_remove(index));
                }
                match ring_bytes(needed).and_then(|size| self.create(device, size)) {
                    Some(slot) => slot,
                    None => {
                        // Refusing a spare must not blank a frame whose data
                        // still fits the unmapped current buffer.
                        if self.write_current(queue, retained, bytes, counters) {
                            return true;
                        }
                        log::warn!(
                            "{}: no mapped buffer for {needed} bytes within the GPU budget",
                            self.label
                        );
                        return false;
                    }
                }
            }
        };
        // The slot is mapped whole (at creation, or by `retire`), and wgpu
        // zeroed any part never written, so only the data's own bytes go in.
        match slot.buffer.get_mapped_range_mut(..needed) {
            Ok(mut view) => view.copy_from_slice(bytes),
            Err(error) => {
                if !self.warned_map_failure {
                    log::warn!("{}: mapped write failed: {error:?}", self.label);
                    self.warned_map_failure = true;
                }
                return false;
            }
        }
        slot.buffer.unmap();
        counters.mapped_writes.fetch_add(1, Ordering::Relaxed);
        counters.mapped_bytes.fetch_add(needed, Ordering::Relaxed);
        if let Some(held) = retained {
            held.replace(bytes);
        }
        if let Some(previous) = self.current.replace(slot) {
            self.retire(previous);
        }
        true
    }

    /// Queue fallback preserves the current buffer and its retained tail.
    /// It cannot handle the first upload or growth beyond that buffer.
    fn write_current(
        &self,
        queue: &wgpu::Queue,
        retained: Option<&mut RetainedBytes>,
        bytes: &[u8],
        counters: &UploadCounters,
    ) -> bool {
        let Some(current) = &self.current else {
            return false;
        };
        if current.buffer.size() < bytes.len() as u64 {
            return false;
        }
        match retained {
            Some(held) => write_buffer_if_changed(queue, &current.buffer, held, bytes, counters),
            None => write_buffer_counted(queue, &current.buffer, bytes, counters),
        }
        true
    }

    /// A new buffer of `size` bytes, mapped at creation. wgpu maps a
    /// `MAP_WRITE` buffer directly rather than through a staging copy.
    fn create(&self, device: &wgpu::Device, size: u64) -> Option<MappedSlot> {
        let charge = match &self.budget {
            Some(budget) => Some(budget.reserve_gpu(usize::try_from(size).ok()?)?),
            None => None,
        };
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size,
            usage: self.usage | wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        Some(MappedSlot {
            buffer,
            map: Arc::new(AtomicU8::new(MAP_READY)),
            retired_at: 0,
            _charge: charge,
        })
    }

    /// Hand a buffer the draws no longer read back to the CPU. wgpu maps it
    /// once the frames that drew from it have finished, and the callback
    /// marks it ready at the next poll or submit.
    fn retire(&mut self, mut slot: MappedSlot) {
        slot.map.store(MAP_PENDING, Ordering::Release);
        let map = Arc::clone(&slot.map);
        slot.buffer
            .map_async(wgpu::MapMode::Write, .., move |result| {
                let state = if result.is_ok() {
                    MAP_READY
                } else {
                    MAP_FAILED
                };
                map.store(state, Ordering::Release);
            });
        self.retired += 1;
        slot.retired_at = self.retired;
        self.spare.push(slot);
    }
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

    /// A ring's new buffer holds only what was just written, so data that
    /// matched the old buffer's tail must still be written.
    #[test]
    fn a_new_buffer_forgets_the_old_tail() {
        let mut r = retained(&[1, 2, 3, 4, 5, 6, 7, 8]);
        r.replace(&[1, 2, 3, 4]);
        assert_eq!(r.changed(&[1, 2, 3, 4, 5, 6, 7, 8]), Some(4..8));
        assert_eq!(r.changed(&[1, 2, 3, 4]), None);
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
        let root = std::fs::read_to_string(src.join("lib.rs")).expect("lib.rs");
        let production_root = kettle_test_support::production_source(&root);
        let mut checked = 0;
        for entry in std::fs::read_dir(&src).expect("src") {
            let path = entry.expect("entry").path();
            if path.extension().is_none_or(|ext| ext != "rs") || path.ends_with("upload.rs") {
                continue;
            }
            // Out-of-line test modules have no item-level cfg in their file.
            // Skip only declarations that the real production parser removed.
            let stem = path
                .file_stem()
                .expect("source name")
                .to_str()
                .expect("UTF-8");
            let declaration = format!("mod {stem};");
            if root.lines().any(|line| line.trim() == declaration)
                && !production_root
                    .lines()
                    .any(|line| line.trim() == declaration)
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source");
            let production = kettle_test_support::production_source(&text);
            for needle in [
                ".write_buffer(",
                ".write_texture(",
                "MAP_WRITE",
                "MAPPABLE_PRIMARY_BUFFERS",
            ] {
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

    /// The live device asks for mapped uploads through the one function
    /// that keeps discrete GPUs and GL off them.
    #[test]
    fn the_live_device_requests_features_only_through_the_upload_check() {
        let text = include_str!("lib.rs");
        let production = kettle_test_support::production_source(text);
        assert_eq!(
            production.matches("required_features:").count(),
            1,
            "one live device request names features"
        );
        assert!(
            production.contains("required_features: upload::mapped_upload_features(&adapter),")
        );
    }

    #[test]
    fn an_unaligned_tail_writes_everything() {
        let r = retained(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(r.changed(&[1, 2, 3, 4, 5, 7]), Some(0..6));
    }

    #[test]
    fn mapped_uploads_only_where_the_gpu_reads_cpu_memory_in_place() {
        use wgpu::{Backend, DeviceType, Features};
        let yes = Features::MAPPABLE_PRIMARY_BUFFERS;
        let no = Features::empty();
        for (windows, backend, device, supported, expected) in [
            (false, Backend::Metal, DeviceType::IntegratedGpu, yes, yes),
            (false, Backend::Vulkan, DeviceType::IntegratedGpu, yes, yes),
            (false, Backend::Vulkan, DeviceType::Cpu, yes, yes),
            // An Intel Mac's discrete GPU, and any other: system memory
            // read across the bus on every draw.
            (false, Backend::Metal, DeviceType::DiscreteGpu, yes, no),
            (false, Backend::Vulkan, DeviceType::DiscreteGpu, yes, no),
            (false, Backend::Vulkan, DeviceType::VirtualGpu, yes, no),
            (false, Backend::Vulkan, DeviceType::Other, yes, no),
            (false, Backend::Dx12, DeviceType::IntegratedGpu, yes, no),
            (false, Backend::Gl, DeviceType::IntegratedGpu, yes, no),
            (false, Backend::Gl, DeviceType::Cpu, no, no),
            (false, Backend::Metal, DeviceType::IntegratedGpu, no, no),
            // Windows never maps, even an integrated or software Vulkan adapter.
            (true, Backend::Vulkan, DeviceType::IntegratedGpu, yes, no),
            (true, Backend::Vulkan, DeviceType::Cpu, yes, no),
            (true, Backend::Dx12, DeviceType::IntegratedGpu, yes, no),
        ] {
            assert_eq!(
                mapped_upload_features_for(windows, backend, device, supported),
                expected,
                "windows={windows} {backend:?} {device:?} supporting {supported:?}"
            );
        }
    }

    fn slot(ready: bool, size: u64, retired_at: u64) -> SlotInfo {
        SlotInfo {
            ready,
            size,
            retired_at,
        }
    }

    #[test]
    fn a_mapped_spare_that_fits_is_filled() {
        let spare = [slot(false, 4096, 1), slot(true, 4096, 2)];
        assert_eq!(plan(&spare, Some(4096), 64), RingStep::Fill(1));
    }

    #[test]
    fn a_mapped_spare_too_small_is_replaced() {
        let spare = [slot(true, 4096, 1)];
        assert_eq!(
            plan(&spare, Some(4096), 8192),
            RingStep::Create { evict: Some(0) }
        );
    }

    #[test]
    fn with_nothing_mapped_the_ring_grows_to_three_buffers() {
        assert_eq!(plan(&[], None, 64), RingStep::Create { evict: None });
        assert_eq!(plan(&[], Some(4096), 64), RingStep::Create { evict: None });
        let one_in_flight = [slot(false, 4096, 1)];
        assert_eq!(
            plan(&one_in_flight, Some(4096), 64),
            RingStep::Create { evict: None }
        );
    }

    /// With the current buffer and two in flight, the data goes through the
    /// queue into the current one rather than into a fourth buffer.
    #[test]
    fn with_every_buffer_in_use_the_data_goes_through_the_queue() {
        let in_flight = [slot(false, 4096, 1), slot(false, 4096, 2)];
        assert_eq!(plan(&in_flight, Some(4096), 64), RingStep::Queue);
    }

    #[test]
    fn growth_with_every_buffer_in_use_replaces_the_oldest() {
        let in_flight = [slot(false, 4096, 7), slot(false, 4096, 3)];
        assert_eq!(
            plan(&in_flight, Some(4096), 8192),
            RingStep::Create { evict: Some(1) }
        );
    }

    #[test]
    fn ring_buffers_are_powers_of_two_of_at_least_4_kib() {
        assert_eq!(ring_bytes(1), Some(4096));
        assert_eq!(ring_bytes(4096), Some(4096));
        assert_eq!(ring_bytes(4097), Some(8192));
        assert_eq!(ring_bytes(100 * 64), Some(8192));
        assert_eq!(ring_bytes(u64::MAX), None);
    }
}
