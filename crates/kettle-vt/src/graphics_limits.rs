//! Shared resource budgets for terminal graphics protocols and rendering.
//!
//! A PTY is an untrusted byte stream. These limits are therefore enforced at
//! allocation boundaries, not merely after a decoder has produced an image.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

const MIB: usize = 1024 * 1024;

/// Hard graphics-resource limits used by the VT decoder and GPU renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsLimits {
    pub sequence_bytes: usize,
    pub transmission_bytes: usize,
    pub in_flight_slots: usize,
    pub in_flight_bytes: usize,
    pub image_bytes: usize,
    pub retained_bytes: usize,
    pub process_cpu_bytes: usize,
    pub process_gpu_bytes: usize,
    pub animation_frames: usize,
    pub animation_bytes: usize,
    pub placements: usize,
    /// Sixel columns one image may paint, across every band and colour pass.
    pub sixel_column_writes: usize,
}

impl Default for GraphicsLimits {
    fn default() -> Self {
        Self {
            sequence_bytes: 16 * MIB,
            transmission_bytes: 96 * MIB,
            in_flight_slots: 8,
            in_flight_bytes: 128 * MIB,
            image_bytes: 64 * MIB,
            retained_bytes: 256 * MIB,
            process_cpu_bytes: 512 * MIB,
            process_gpu_bytes: 512 * MIB,
            animation_frames: 128,
            animation_bytes: 128 * MIB,
            placements: 256,
            // A full 8192x8192 canvas is 11.2M columns, so this allows about
            // six passes over a maximum-size image. See `sixel::decode`.
            sixel_column_writes: 64 * 1024 * 1024,
        }
    }
}

impl GraphicsLimits {
    /// Reject internally inconsistent limits before they reach allocation code.
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.sequence_bytes == 0
            || self.transmission_bytes == 0
            || self.in_flight_slots == 0
            || self.in_flight_bytes == 0
            || self.image_bytes == 0
            || self.retained_bytes == 0
            || self.process_cpu_bytes == 0
            || self.process_gpu_bytes == 0
            || self.animation_frames == 0
            || self.animation_bytes == 0
            || self.placements == 0
            || self.sixel_column_writes == 0
        {
            return Err("graphics limits must be non-zero");
        }
        if self.in_flight_bytes < self.transmission_bytes {
            return Err("in-flight byte limit must hold one transmission");
        }
        if self.retained_bytes < self.image_bytes {
            return Err("retained byte limit must hold one image");
        }
        if self.process_cpu_bytes < self.retained_bytes
            || self.process_gpu_bytes < self.retained_bytes
        {
            return Err("process limits must be at least one retained scope");
        }
        if self.animation_bytes < self.image_bytes {
            return Err("animation byte limit must hold one image");
        }
        Ok(self)
    }
}

#[derive(Default)]
struct Counters {
    cpu: AtomicUsize,
    gpu: AtomicUsize,
    image_order: AtomicU64,
}

static PROCESS_COUNTERS: OnceLock<Arc<Counters>> = OnceLock::new();

#[derive(Clone, Copy)]
enum Resource {
    Cpu,
    Gpu,
}

fn counter(counters: &Counters, resource: Resource) -> &AtomicUsize {
    match resource {
        Resource::Cpu => &counters.cpu,
        Resource::Gpu => &counters.gpu,
    }
}

fn try_add(counter: &AtomicUsize, bytes: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(bytes) else {
            return false;
        };
        if next > limit {
            return false;
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

fn subtract(counter: &AtomicUsize, bytes: usize) {
    let previous = counter.fetch_sub(bytes, Ordering::AcqRel);
    debug_assert!(previous >= bytes, "graphics reservation counter underflow");
}

/// A process-wide account plus one terminal/window retained-resource scope.
#[derive(Clone)]
pub struct GraphicsBudget {
    limits: GraphicsLimits,
    process: Arc<Counters>,
    scope: Arc<Counters>,
}

impl Default for GraphicsBudget {
    fn default() -> Self {
        let limits = GraphicsLimits::default();
        Self {
            limits,
            process: PROCESS_COUNTERS
                .get_or_init(|| Arc::new(Counters::default()))
                .clone(),
            scope: Arc::new(Counters::default()),
        }
    }
}

impl GraphicsBudget {
    pub(crate) fn next_image_order(&self) -> Option<u64> {
        let counter = &self.scope.image_order;
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let next = current.checked_add(1)?;
            match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Some(next),
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn retained_cpu_bytes(&self) -> usize {
        self.scope.cpu.load(Ordering::Acquire)
    }

    pub fn limits(&self) -> GraphicsLimits {
        self.limits
    }

    /// Start an independent resource domain. Clone this account for every
    /// preview layer and window that must share its aggregate limits.
    /// Terminal graphics keep their default account.
    pub fn independent(limits: GraphicsLimits) -> Result<Self, &'static str> {
        Ok(Self {
            limits: limits.validate()?,
            process: Arc::new(Counters::default()),
            scope: Arc::new(Counters::default()),
        })
    }

    /// Process-wide card/shelf preview account. Every window and GPU rebuild
    /// shares this domain; terminal pixels, glyphs and presentation use theirs.
    pub fn previews() -> Self {
        static PREVIEWS: OnceLock<GraphicsBudget> = OnceLock::new();
        PREVIEWS
            .get_or_init(|| Self {
                limits: GraphicsLimits {
                    retained_bytes: 128 * MIB,
                    process_cpu_bytes: 128 * MIB,
                    process_gpu_bytes: 128 * MIB,
                    ..GraphicsLimits::default()
                },
                process: Arc::new(Counters::default()),
                scope: Arc::new(Counters::default()),
            })
            .clone()
    }

    /// An isolated account for deterministic, small-budget unit tests.
    #[cfg(test)]
    pub(crate) fn isolated(limits: GraphicsLimits) -> Result<Self, &'static str> {
        Ok(Self {
            limits: limits.validate()?,
            process: Arc::new(Counters::default()),
            scope: Arc::new(Counters::default()),
        })
    }

    /// A temporary decode scope charged to the same process account. Capability
    /// probes release it after decoding; uploads transfer their pixel reservation
    /// into the terminal scope only after admission.
    pub(crate) fn query_scope(&self) -> Self {
        Self {
            limits: self.limits,
            process: self.process.clone(),
            scope: Arc::new(Counters::default()),
        }
    }

    /// Reserve retained CPU image memory in both this terminal and the process.
    pub(crate) fn reserve_image_cpu(&self, bytes: usize) -> Option<GraphicsReservation> {
        if bytes == 0 || bytes > self.limits.image_bytes {
            return None;
        }
        self.reserve(Resource::Cpu, bytes, true)
    }

    /// Reserve retained CPU storage that is not an image, such as a
    /// preview's source bytes or text, in this scope and the process: held
    /// to the retained and process limits, not the per-image cap.
    pub fn reserve_retained_cpu(&self, bytes: usize) -> Option<GraphicsReservation> {
        if bytes == 0 {
            return None;
        }
        self.reserve(Resource::Cpu, bytes, true)
    }

    /// Reserve short-lived decode/escape storage in the process account only.
    pub fn reserve_transient_cpu(&self, bytes: usize) -> Option<GraphicsReservation> {
        if bytes == 0 {
            return None;
        }
        self.reserve(Resource::Cpu, bytes, false)
    }

    /// Reserve short-lived renderer storage in the process GPU account only.
    ///
    /// A live screenshot target has its own 256 MiB readback limit and can
    /// exceed the 64 MiB cap on one retained terminal image (a 6K window is
    /// about 78 MiB). It is not retained window state, so it counts only
    /// against the process limit, and only for the GPU submission and readback.
    /// [`Self::reserve_gpu`] still holds hostile images to the per-image cap.
    pub fn reserve_transient_gpu(&self, bytes: usize) -> Option<GraphicsReservation> {
        if bytes == 0 {
            return None;
        }
        self.reserve(Resource::Gpu, bytes, false)
    }

    /// Reserve a retained GPU texture/buffer in this window and process.
    pub fn reserve_gpu(&self, bytes: usize) -> Option<GraphicsReservation> {
        if bytes == 0 || bytes > self.limits.image_bytes {
            return None;
        }
        self.reserve(Resource::Gpu, bytes, true)
    }

    fn reserve(
        &self,
        resource: Resource,
        bytes: usize,
        retained: bool,
    ) -> Option<GraphicsReservation> {
        let process_limit = match resource {
            Resource::Cpu => self.limits.process_cpu_bytes,
            Resource::Gpu => self.limits.process_gpu_bytes,
        };
        let process_counter = counter(&self.process, resource);
        if !try_add(process_counter, bytes, process_limit) {
            return None;
        }
        if retained {
            let scope_counter = counter(&self.scope, resource);
            if !try_add(scope_counter, bytes, self.limits.retained_bytes) {
                subtract(process_counter, bytes);
                return None;
            }
        }
        Some(GraphicsReservation {
            budget: self.clone(),
            resource,
            bytes,
            retained,
        })
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> (usize, usize, usize, usize) {
        (
            self.process.cpu.load(Ordering::Acquire),
            self.scope.cpu.load(Ordering::Acquire),
            self.process.gpu.load(Ordering::Acquire),
            self.scope.gpu.load(Ordering::Acquire),
        )
    }
}

/// RAII storage reservation. Dropping a decoder, image, or cached texture
/// immediately returns its bytes to both applicable accounts.
pub struct GraphicsReservation {
    budget: GraphicsBudget,
    resource: Resource,
    bytes: usize,
    retained: bool,
}

impl GraphicsReservation {
    // The process already accounts for staged pixels. Transfer only their
    // retained scope, after reclamation, without charging or copying them twice.
    pub(crate) fn try_move_to_scope(&mut self, budget: &GraphicsBudget) -> bool {
        if !self.retained
            || !Arc::ptr_eq(&self.budget.process, &budget.process)
            || self.budget.limits != budget.limits
        {
            return false;
        }
        if Arc::ptr_eq(&self.budget.scope, &budget.scope) {
            return true;
        }
        if !try_add(
            counter(&budget.scope, self.resource),
            self.bytes,
            budget.limits.retained_bytes,
        ) {
            return false;
        }
        subtract(counter(&self.budget.scope, self.resource), self.bytes);
        self.budget = budget.clone();
        true
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn budget(&self) -> &GraphicsBudget {
        &self.budget
    }

    /// Return unused capacity after a worst-case decoder reservation.
    pub(crate) fn shrink_to(&mut self, bytes: usize) -> bool {
        if bytes == 0 || bytes > self.bytes {
            return false;
        }
        let released = self.bytes - bytes;
        if released != 0 {
            subtract(counter(&self.budget.process, self.resource), released);
            if self.retained {
                subtract(counter(&self.budget.scope, self.resource), released);
            }
            self.bytes = bytes;
        }
        true
    }

    /// Grow an existing allocation's charge before its backing buffer grows.
    pub(crate) fn try_grow_to(&mut self, bytes: usize) -> bool {
        if bytes <= self.bytes {
            return self.shrink_to(bytes);
        }
        let additional = bytes - self.bytes;
        let Some(mut extra) = self
            .budget
            .reserve(self.resource, additional, self.retained)
        else {
            return false;
        };
        self.bytes = bytes;
        // The counters charged by `extra` now belong to this reservation.
        extra.bytes = 0;
        true
    }
}

impl Drop for GraphicsReservation {
    fn drop(&mut self) {
        subtract(counter(&self.budget.process, self.resource), self.bytes);
        if self.retained {
            subtract(counter(&self.budget.scope, self.resource), self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_namespaces_share_a_nonwrapping_image_creation_order() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        assert_eq!(budget.next_image_order(), Some(1));
        assert_eq!(budget.clone().next_image_order(), Some(2));
        budget
            .scope
            .image_order
            .store(u64::MAX - 1, Ordering::Relaxed);
        assert_eq!(budget.next_image_order(), Some(u64::MAX));
        assert_eq!(budget.next_image_order(), None);
        assert_eq!(budget.scope.image_order.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn concurrent_screen_namespaces_allocate_distinct_image_creation_orders() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..128)
                        .map(|_| budget.next_image_order().unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut orders: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        orders.sort_unstable();
        assert_eq!(orders, (1..=1024).collect::<Vec<_>>());
    }

    fn tiny_limits() -> GraphicsLimits {
        GraphicsLimits {
            sequence_bytes: 8,
            transmission_bytes: 16,
            in_flight_slots: 2,
            in_flight_bytes: 32,
            image_bytes: 16,
            retained_bytes: 32,
            process_cpu_bytes: 64,
            process_gpu_bytes: 64,
            animation_frames: 2,
            animation_bytes: 16,
            placements: 2,
            sixel_column_writes: 4,
        }
    }

    #[test]
    fn defaults_match_the_security_envelope() {
        let l = GraphicsLimits::default().validate().unwrap();
        assert_eq!(l.sequence_bytes, 16 * MIB);
        assert_eq!(l.transmission_bytes, 96 * MIB);
        assert_eq!((l.in_flight_slots, l.in_flight_bytes), (8, 128 * MIB));
        assert_eq!((l.image_bytes, l.retained_bytes), (64 * MIB, 256 * MIB));
        assert_eq!(
            (l.process_cpu_bytes, l.process_gpu_bytes),
            (512 * MIB, 512 * MIB)
        );
        assert_eq!((l.animation_frames, l.animation_bytes), (128, 128 * MIB));
        assert_eq!(l.placements, 256);
    }

    #[test]
    fn retained_reservations_are_atomic_and_raii_released() {
        let b = GraphicsBudget::isolated(tiny_limits()).unwrap();
        let a = b.reserve_image_cpu(16).unwrap();
        let c = b.reserve_image_cpu(16).unwrap();
        assert!(b.reserve_image_cpu(1).is_none(), "scope is exactly full");
        assert_eq!(b.usage(), (32, 32, 0, 0));
        drop(a);
        assert_eq!(b.usage(), (16, 16, 0, 0));
        drop(c);
        assert_eq!(b.usage(), (0, 0, 0, 0));
    }

    /// Retained storage that is not an image is held to the scope and the
    /// process, but not to the per-image cap, and goes back when dropped.
    #[test]
    fn retained_cpu_storage_bypasses_only_the_per_image_limit() {
        let b = GraphicsBudget::isolated(tiny_limits()).unwrap();
        assert!(b.reserve_retained_cpu(0).is_none(), "nothing to hold");
        let text = b
            .reserve_retained_cpu(24)
            .expect("a source may exceed one image's cap");
        assert_eq!(b.usage(), (24, 24, 0, 0));
        assert!(
            b.reserve_retained_cpu(9).is_none(),
            "the retained scope holds 32"
        );
        assert_eq!(b.usage(), (24, 24, 0, 0), "a refusal holds nothing");
        drop(text);
        assert_eq!(b.usage(), (0, 0, 0, 0));
    }

    #[test]
    fn transient_storage_counts_process_but_not_retained_scope() {
        let b = GraphicsBudget::isolated(tiny_limits()).unwrap();
        let _r = b.reserve_transient_cpu(48).unwrap();
        assert_eq!(b.usage(), (48, 0, 0, 0));
        assert!(b.reserve_transient_cpu(17).is_none());
    }

    #[test]
    fn transient_gpu_storage_bypasses_only_the_per_image_limit() {
        let b = GraphicsBudget::isolated(tiny_limits()).unwrap();
        assert_eq!(b.limits().image_bytes, 16);
        let texture = b
            .reserve_transient_gpu(32)
            .expect("a transient frame may exceed one retained image");
        let staging = b
            .reserve_transient_gpu(32)
            .expect("its separately allocated staging buffer is charged too");
        assert_eq!((texture.bytes(), staging.bytes()), (32, 32));
        assert_eq!(b.usage(), (0, 0, 64, 0));
        assert!(
            b.reserve_transient_gpu(1).is_none(),
            "the combined allocations reach the process-wide GPU limit"
        );
        drop((texture, staging));
        assert_eq!(b.usage(), (0, 0, 0, 0));
    }

    #[test]
    fn failed_scope_reservation_rolls_back_process_counter() {
        let b = GraphicsBudget::isolated(tiny_limits()).unwrap();
        let _a = b.reserve_gpu(16).unwrap();
        let _c = b.reserve_gpu(16).unwrap();
        assert!(b.reserve_gpu(1).is_none());
        assert_eq!(b.usage(), (0, 0, 32, 32));
    }

    #[test]
    fn independent_preview_accounts_do_not_consume_each_others_cpu_space() {
        let first = GraphicsBudget::independent(tiny_limits()).unwrap();
        let second = GraphicsBudget::independent(tiny_limits()).unwrap();
        let first_pixels = first.reserve_transient_cpu(64).unwrap();
        let second_pixels = second.reserve_transient_cpu(64).unwrap();
        assert!(first.clone().reserve_transient_cpu(1).is_none());
        assert!(second.clone().reserve_transient_cpu(1).is_none());
        drop(first_pixels);
        assert!(first.reserve_transient_cpu(64).is_some());
        assert!(second.reserve_transient_cpu(1).is_none());
        drop(second_pixels);
        assert!(second.reserve_transient_cpu(64).is_some());
    }

    #[test]
    fn preview_windows_share_the_same_gpu_account_until_resources_drop() {
        let preview = GraphicsBudget::independent(tiny_limits()).unwrap();
        let window_a = preview.clone();
        let window_b = preview.clone();
        let independent = GraphicsBudget::independent(tiny_limits()).unwrap();
        let texture = window_a.reserve_transient_gpu(64).unwrap();
        drop(window_a);
        assert!(window_b.reserve_transient_gpu(1).is_none());
        let other_texture = independent.reserve_transient_gpu(64).unwrap();
        drop(texture);
        assert!(window_b.reserve_transient_gpu(64).is_some());
        assert!(independent.reserve_transient_gpu(1).is_none());
        drop(other_texture);
    }

    #[test]
    fn preview_pixels_charge_once_and_release_after_the_last_snapshot() {
        let account = GraphicsBudget::independent(tiny_limits()).unwrap();
        let image = crate::ImageData::new_with_budget(2, 2, vec![7; 16], &account).unwrap();
        let snapshot = image.clone();
        assert_eq!(account.usage(), (16, 16, 0, 0));
        drop(image);
        assert_eq!(account.usage(), (16, 16, 0, 0));
        drop(snapshot);
        assert_eq!(account.usage(), (0, 0, 0, 0));
    }
    #[test]
    fn a_full_account_hands_the_pixels_back_for_a_retry() {
        let account = GraphicsBudget::independent(tiny_limits()).unwrap();
        // Two 2x2 images fill the account's 32 retained bytes.
        let held = crate::ImageData::new_with_budget(2, 2, vec![1; 16], &account).unwrap();
        let _also = crate::ImageData::new_with_budget(2, 2, vec![2; 16], &account).unwrap();
        let pixels = vec![9; 16];
        let pointer = pixels.as_ptr();
        let Err(crate::ImageRefusal::NoRoom(back)) =
            crate::ImageData::try_with_budget(2, 2, pixels, &account)
        else {
            panic!("a full account has no room");
        };
        assert_eq!(back.as_ptr(), pointer);
        drop(held);
        let image = crate::ImageData::try_with_budget(2, 2, back, &account).unwrap();
        assert_eq!(image.rgba.as_ptr(), pointer);
        assert_eq!(
            crate::ImageData::try_with_budget(2, 2, vec![0; 15], &account).err(),
            Some(crate::ImageRefusal::Invalid)
        );
    }

    #[test]
    fn preview_domain_survives_new_window_and_gpu_context_handles() {
        let first = GraphicsBudget::previews();
        let second = GraphicsBudget::previews();
        let terminal = GraphicsBudget::default();
        assert!(Arc::ptr_eq(&first.process, &second.process));
        assert!(Arc::ptr_eq(&first.scope, &second.scope));
        assert!(!Arc::ptr_eq(&first.process, &terminal.process));
        assert_eq!(first.limits().validate(), Ok(first.limits()));
        assert_eq!(first.limits().process_cpu_bytes, 128 * MIB);
        assert_eq!(first.limits().process_gpu_bytes, 128 * MIB);
    }
}
