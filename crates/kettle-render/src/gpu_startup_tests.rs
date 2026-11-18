//! The startup shortcuts in GPU init: no indirect-call validation, pipelines
//! compiled on worker threads, and the first Vulkan instance created early.

use super::gpu::{
    BackendPlatform, EarlyGpuInstance, EarlyValue, OwnedGpuDisplayHandle,
    early_vulkan_instance_applies_for, gpu_instance_for_backends, instance_descriptor,
    instance_for_attempt, pipelines_compile_on_threads,
};
use super::gpu_tests::{gpu_test_config, gpu_test_guard};
use super::{Config, Renderer};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle};
use std::sync::mpsc;
use std::time::Duration;

/// The backends of a wgpu built with Vulkan, as on Linux and FreeBSD.
const WITH_VULKAN: wgpu::Backends = wgpu::Backends::VULKAN.union(wgpu::Backends::GL);

/// A display handle that is never read: building a descriptor only stores it.
struct UnreadDisplay;

impl HasDisplayHandle for UnreadDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

/// Every instance Kettle creates, with or without a display handle, leaves
/// out wgpu's indirect-call validation, whose two compute pipelines each
/// device would otherwise build at creation, and keeps wgpu's other flags.
#[test]
fn instances_leave_out_indirect_call_validation() {
    let display = OwnedGpuDisplayHandle::new(UnreadDisplay);
    for (label, descriptor) in [
        ("headless", instance_descriptor(wgpu::Backends::all(), None)),
        (
            "live",
            instance_descriptor(wgpu::Backends::VULKAN, Some(&display)),
        ),
    ] {
        assert!(
            !descriptor
                .flags
                .contains(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL),
            "{label}: indirect-call validation is off"
        );
        assert_eq!(
            descriptor.flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL,
            wgpu::InstanceFlags::default() | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL,
            "{label}: wgpu's other default flags are kept"
        );
    }
}

/// The early instance is started only where window 1's first GPU attempt
/// is a Vulkan instance of its own, so no other launch loads the Vulkan
/// driver before it would have.
#[test]
fn the_vulkan_instance_starts_early_only_where_vulkan_is_tried_first() {
    use kettle_config::{GpuBackend, GpuPowerPreference};
    let auto = Config::default();
    let backend = |gpu_backend| Config {
        gpu_backend,
        ..Config::default()
    };
    assert!(early_vulkan_instance_applies_for(
        BackendPlatform::Other,
        &auto,
        WITH_VULKAN
    ));
    assert!(early_vulkan_instance_applies_for(
        BackendPlatform::Other,
        &backend(GpuBackend::Vulkan),
        WITH_VULKAN
    ));
    assert!(
        early_vulkan_instance_applies_for(
            BackendPlatform::Other,
            &Config {
                gpu_force_software: true,
                ..Config::default()
            },
            WITH_VULKAN
        ),
        "the software pass also starts with a Vulkan instance"
    );
    for (label, cfg) in [
        ("gpu-backend = gl", backend(GpuBackend::Gl)),
        (
            "a GPU name pin",
            Config {
                gpu_name: "NVIDIA GeForce RTX 2080".into(),
                ..Config::default()
            },
        ),
        (
            "a vendor and device pin",
            Config {
                gpu_vendor_id: 0x10de,
                gpu_device_id: 0x1e87,
                ..Config::default()
            },
        ),
        (
            "gpu-power-preference = high",
            Config {
                gpu_power_preference: GpuPowerPreference::High,
                ..Config::default()
            },
        ),
        (
            "gpu-power-preference = low",
            Config {
                gpu_power_preference: GpuPowerPreference::Low,
                ..Config::default()
            },
        ),
    ] {
        assert!(
            !early_vulkan_instance_applies_for(BackendPlatform::Other, &cfg, WITH_VULKAN),
            "{label} does not start with a Vulkan-only instance"
        );
    }
    assert!(
        !early_vulkan_instance_applies_for(BackendPlatform::Other, &auto, wgpu::Backends::GL),
        "a wgpu without Vulkan, as on NetBSD and OpenBSD, has no instance to start"
    );
    for platform in [BackendPlatform::Windows, BackendPlatform::MacOs] {
        for cfg in [Config::default(), backend(GpuBackend::Vulkan)] {
            assert!(
                !early_vulkan_instance_applies_for(platform, &cfg, WITH_VULKAN),
                "{platform:?} never creates an early instance"
            );
        }
    }
}

/// An instance with no backends: creating one is instant.
fn empty_instance() -> wgpu::Instance {
    gpu_instance_for_backends(wgpu::Backends::empty())
}

/// Only the first Vulkan attempt takes the early instance. An attempt on
/// another backend leaves it for Vulkan and creates its own, as does a later
/// Vulkan attempt, such as the software pass after the hardware pass, and an
/// attempt whose early instance thread failed.
#[test]
fn only_the_first_vulkan_attempt_takes_the_early_instance() {
    let created = std::cell::Cell::new(0);
    let create = || {
        created.set(created.get() + 1);
        empty_instance()
    };
    let mut early = EarlyGpuInstance::spawn(empty_instance);
    assert!(early.is_some(), "the instance thread started");
    instance_for_attempt(wgpu::Backend::Gl, &mut early, create);
    assert_eq!(created.get(), 1, "a GL attempt creates its own");
    assert!(early.is_some(), "and leaves the early one for Vulkan");
    instance_for_attempt(wgpu::Backend::Vulkan, &mut early, create);
    assert_eq!(created.get(), 1, "the first Vulkan attempt takes it");
    assert!(early.is_none());
    instance_for_attempt(wgpu::Backend::Vulkan, &mut early, create);
    assert_eq!(created.get(), 2, "a later Vulkan attempt creates its own");

    let mut failed = EarlyGpuInstance::spawn(|| panic!("ICD failed"));
    assert!(failed.is_some(), "the instance thread started");
    instance_for_attempt(wgpu::Backend::Vulkan, &mut failed, create);
    assert_eq!(
        created.get(),
        3,
        "a panicked thread's attempt creates its own"
    );
}

/// Every direct-probe attempt in the window resolver gets its instance
/// from `instance_for_attempt`, so the early instance reaches the first
/// Vulkan attempt and no other.
#[test]
fn the_window_resolver_takes_each_attempts_instance_from_the_early_one() {
    let src = super::production_source();
    let resolver = src
        .split_once("async fn resolve_window_adapter<W>(")
        .expect("resolve_window_adapter")
        .1;
    let direct = resolver
        .split_once("for &software_pass in direct_probe_passes(cfg.gpu_force_software) {")
        .expect("the direct probe")
        .1
        .split_once("drop(early_instance);")
        .expect("the end of the direct probe")
        .0;
    assert_eq!(
        direct
            .matches("instance_for_attempt(backend, &mut early_instance, || {")
            .count(),
        1
    );
    assert_eq!(direct.matches("gpu_instance_for_live_display(").count(), 1);
}

/// Records, when dropped, the thread that dropped it, after an optional
/// wait that stands in for a slow `vkDestroyInstance`.
struct Destroyed {
    on: mpsc::Sender<std::thread::ThreadId>,
    hold: Option<mpsc::Receiver<()>>,
}

impl Drop for Destroyed {
    fn drop(&mut self) {
        if let Some(hold) = &self.hold {
            let _ = hold.recv();
        }
        let _ = self.on.send(std::thread::current().id());
    }
}

/// Drop `early` on a helper thread; whether that returned within `limit`.
fn dropped_within(early: EarlyValue<Destroyed>, limit: Duration) -> bool {
    let (returned, dropped) = mpsc::channel();
    std::thread::spawn(move || {
        drop(early);
        let _ = returned.send(());
    });
    dropped.recv_timeout(limit).is_ok()
}

/// An early value nobody took is destroyed by the thread that made it, and
/// its owner waits for that, but only for its bound: neither a maker nor a
/// destructor that never finishes can hold up the startup failure that
/// dropped it. The early instance uses the same type.
#[test]
fn an_unused_early_value_is_destroyed_on_its_thread_within_a_bound() {
    let bound = Duration::from_millis(100);
    let (on, destroyed) = mpsc::channel();
    let quick = EarlyValue::spawn(
        "kettle-test-early",
        move || {
            std::thread::sleep(Duration::from_millis(50));
            Destroyed { on, hold: None }
        },
        Duration::from_secs(2),
    )
    .expect("thread started");
    drop(quick);
    let by = destroyed
        .try_recv()
        .expect("a working driver is waited for, and its instance destroyed");
    assert_ne!(
        by,
        std::thread::current().id(),
        "on the thread that made it"
    );

    let (unblock, blocked) = mpsc::channel::<()>();
    let (on, _destroyed) = mpsc::channel();
    let slow_maker = EarlyValue::spawn(
        "kettle-test-early",
        move || {
            let _ = blocked.recv();
            Destroyed { on, hold: None }
        },
        bound,
    )
    .expect("thread started");
    assert!(
        dropped_within(slow_maker, Duration::from_secs(1)),
        "a driver hung in vkCreateInstance does not hang the drop"
    );
    let _ = unblock.send(());

    let (unblock, hold) = mpsc::channel::<()>();
    let (on, _destroyed) = mpsc::channel();
    let slow_destructor = EarlyValue::spawn(
        "kettle-test-early",
        move || Destroyed {
            on,
            hold: Some(hold),
        },
        bound,
    )
    .expect("thread started");
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        dropped_within(slow_destructor, Duration::from_secs(1)),
        "a driver hung in vkDestroyInstance does not hang the drop"
    );
    let _ = unblock.send(());
}

/// Stands in for a driver's thread-local teardown that blocks when its
/// thread exits.
struct BlockAtExit(mpsc::Receiver<()>);

impl Drop for BlockAtExit {
    fn drop(&mut self) {
        let _ = self.0.recv();
    }
}

thread_local! {
    static AT_EXIT: std::cell::RefCell<Option<BlockAtExit>> =
        const { std::cell::RefCell::new(None) };
}

/// The owner of an unused early value waits for it to be destroyed, not
/// for the thread's own exit, which can block in thread-local teardown.
#[test]
fn an_unused_early_value_does_not_wait_for_its_threads_teardown() {
    let (unblock, blocked) = mpsc::channel::<()>();
    let (on, destroyed) = mpsc::channel();
    let early = EarlyValue::spawn(
        "kettle-test-early",
        move || {
            AT_EXIT.with_borrow_mut(|slot| *slot = Some(BlockAtExit(blocked)));
            Destroyed { on, hold: None }
        },
        Duration::from_secs(2),
    )
    .expect("thread started");
    assert!(
        dropped_within(early, Duration::from_secs(1)),
        "a thread-local teardown that blocks does not hang the drop"
    );
    assert!(destroyed.try_recv().is_ok(), "the value was destroyed");
    let _ = unblock.send(());
}

/// A taken value comes from the thread that made it.
#[test]
fn a_taken_early_value_is_the_one_its_thread_made() {
    let early =
        EarlyValue::spawn("kettle-test-early", || 6 * 7, Duration::from_secs(2)).expect("thread");
    assert_eq!(early.take(), Some(42));
}

/// GL runs every device call under one context lock, so its renderer builds
/// the pipelines itself; on every other backend, a renderer that made its
/// device uses worker threads.
#[test]
fn gl_builds_its_pipelines_on_the_calling_thread() {
    assert!(!pipelines_compile_on_threads(wgpu::Backend::Gl));
    for backend in [
        wgpu::Backend::Vulkan,
        wgpu::Backend::Metal,
        wgpu::Backend::Dx12,
    ] {
        assert!(pipelines_compile_on_threads(backend), "{backend:?}");
    }
}

/// A renderer that made its device compiles, outside GL, the five pipelines
/// that depend only on the device and the format on their own threads, while
/// it builds the text renderers and the shared quad and image pipelines
/// itself.
#[test]
fn the_renderer_compiles_its_independent_pipelines_on_worker_threads() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    super::pipeline_build::tests::take_spawned();
    let Some(renderer) = Renderer::headless_for_tests(&cfg, 320, 120).expect("renderer") else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let mut spawned = super::pipeline_build::tests::take_spawned();
    spawned.sort_unstable();
    let backend = renderer.gpu().adapter.get_info().backend;
    let expected: &[&str] = if backend == wgpu::Backend::Gl {
        &[]
    } else {
        &[
            "kettle-glyphs",
            "kettle-outline",
            "kettle-present",
            "kettle-rounded",
            "kettle-stars",
        ]
    };
    assert_eq!(spawned, expected, "{backend:?}");
}

/// The worker threads charge nothing to the graphics budget: the glyph
/// pipeline only compiles there, and its atlases and buffers are charged
/// after the image layers, where the whole pipeline used to be built, so
/// the budget is charged in the order it was before.
#[test]
fn the_graphics_budget_is_charged_in_the_order_it_was_before() {
    let src = super::production_source();
    let body = src
        .split_once("fn with_gpu(")
        .expect("with_gpu")
        .1
        .split_once("\n    }\n")
        .expect("end of with_gpu")
        .0;
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("with_gpu lacks {needle}"))
    };
    let glyph_start = at("let glyph_build = on_threads.then(|| {");
    let glyph_end = glyph_start + body[glyph_start..].find("});").expect("end of the build");
    let glyph_closure = &body[glyph_start..glyph_end];
    assert!(glyph_closure.contains("GlyphPipeline::compile(&device, format)"));
    assert!(!glyph_closure.contains("budget"));
    let background_layer = at("let bg_imgs = shared");
    let glyph_join = at("let glyph_pipeline = match glyph_build {");
    assert!(background_layer < glyph_join);
    assert_eq!(body.matches("GlyphPipeline::with_shaders(").count(), 1);
    // On GL and on a caller's device, the pipeline is built as it was.
    assert_eq!(
        body.matches(
            "None => GlyphPipeline::new_with_budget(&device, format, graphics_budget.clone()),"
        )
        .count(),
        1
    );
}

/// Only a renderer that created its device compiles on worker threads. A
/// window sharing another's device, whose caller may hold error scopes for
/// its own thread, builds everything there, as before.
#[test]
fn only_a_renderer_with_its_own_device_compiles_on_worker_threads() {
    let src = super::production_source();
    let body = |start: &str| {
        src.split_once(start)
            .unwrap_or_else(|| panic!("{start}"))
            .1
            .split_once("\n    }\n")
            .expect("end of the function")
            .0
            .to_owned()
    };
    assert!(body("pub fn new_with_gpu<W>(").contains("PipelineThreads::CallerThread,"));
    assert!(body("async fn new_with_escalation_inner<W>(").contains("PipelineThreads::Allowed,"));
    let with_gpu = body("fn with_gpu(");
    assert!(with_gpu.contains(
        "let on_threads = threads == PipelineThreads::Allowed\n            && pipelines_compile_on_threads(gpu.adapter.get_info().backend);"
    ));
    assert_eq!(
        with_gpu
            .matches("PipelineBuild::start(on_threads, ")
            .count(),
        4
    );
    assert_eq!(with_gpu.matches("PipelineBuild::spawn(").count(), 1);
}
