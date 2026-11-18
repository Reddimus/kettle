//! The startup shortcuts in GPU init: no indirect-call validation, pipelines
//! compiled on worker threads, and the first Vulkan instance created early.

use super::gpu::{
    BackendPlatform, EarlyGpuInstance, OwnedGpuDisplayHandle, early_instance_for,
    early_vulkan_instance_applies_for, gpu_instance_for_backends, instance_descriptor,
};
use super::gpu_tests::{gpu_test_config, gpu_test_guard};
use super::{Config, Renderer};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle};

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
        &auto
    ));
    assert!(early_vulkan_instance_applies_for(
        BackendPlatform::Other,
        &backend(GpuBackend::Vulkan)
    ));
    assert!(
        early_vulkan_instance_applies_for(
            BackendPlatform::Other,
            &Config {
                gpu_force_software: true,
                ..Config::default()
            }
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
            !early_vulkan_instance_applies_for(BackendPlatform::Other, &cfg),
            "{label} does not start with a Vulkan-only instance"
        );
    }
    for platform in [BackendPlatform::Windows, BackendPlatform::MacOs] {
        for cfg in [Config::default(), backend(GpuBackend::Vulkan)] {
            assert!(
                !early_vulkan_instance_applies_for(platform, &cfg),
                "{platform:?} never creates an early instance"
            );
        }
    }
}

/// Only the first Vulkan attempt takes the early instance. An attempt on
/// another backend leaves it for Vulkan, and a later Vulkan attempt, such
/// as the software pass after the hardware pass, creates its own.
#[test]
fn only_the_first_vulkan_attempt_takes_the_early_instance() {
    let mut early = EarlyGpuInstance::spawn(|| gpu_instance_for_backends(wgpu::Backends::empty()));
    assert!(early.is_some(), "the instance thread started");
    assert!(early_instance_for(wgpu::Backend::Gl, &mut early).is_none());
    assert!(early.is_some(), "a GL attempt leaves it");
    let taken = early_instance_for(wgpu::Backend::Vulkan, &mut early)
        .expect("the first Vulkan attempt takes it");
    assert!(taken.take().is_some(), "and gets the instance");
    assert!(early.is_none());
    assert!(
        early_instance_for(wgpu::Backend::Vulkan, &mut early).is_none(),
        "a later Vulkan attempt creates its own"
    );
}

/// A failed early instance is no instance: the attempt creates its own.
#[test]
fn a_panicked_early_instance_thread_gives_no_instance() {
    let early = EarlyGpuInstance::spawn(|| panic!("ICD failed")).expect("thread started");
    assert!(early.take().is_none());
}

/// Every renderer compiles the five pipelines that depend only on the
/// device and the format on their own threads, while it builds the text
/// renderers and the shared quad and image pipelines itself.
#[test]
fn the_renderer_compiles_its_independent_pipelines_on_worker_threads() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    super::pipeline_build::tests::take_spawned();
    let Some(_renderer) = Renderer::headless_for_tests(&cfg, 320, 120).expect("renderer") else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let mut spawned = super::pipeline_build::tests::take_spawned();
    spawned.sort_unstable();
    assert_eq!(
        spawned,
        [
            "kettle-glyphs",
            "kettle-outline",
            "kettle-present",
            "kettle-rounded",
            "kettle-stars",
        ]
    );
}
