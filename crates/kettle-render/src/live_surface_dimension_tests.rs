use super::{live_device_limits, live_surface_dimensions};

// wgpu's DEFAULT requested limit is 8192; a capable adapter reports more.
// Requesting the adapter's real limit is what lets a large window keep its
// true size.
#[test]
fn live_surface_keeps_real_window_dimensions_when_the_device_allows_them() {
    assert_eq!(
        live_surface_dimensions(12_000, 9_000, 16_384),
        (12_000, 9_000)
    );
    assert_eq!(live_surface_dimensions(0, 0, 16_384), (1, 1));
}

// The regression this guards: dropping the clamp made `Surface::configure`
// reject an oversized window outright, leaving a surface that paints
// nothing. Clipping to the visible top-left region is the better failure.
#[test]
fn live_surface_clips_to_a_device_that_cannot_present_the_window() {
    assert_eq!(
        live_surface_dimensions(10_000, 9_000, 8_192),
        (8_192, 8_192)
    );
    assert_eq!(live_surface_dimensions(4_000, 9_000, 8_192), (4_000, 8_192));
    // A degenerate limit must still produce a configurable surface.
    assert_eq!(live_surface_dimensions(4_000, 4_000, 0), (1, 1));
}

#[test]
fn live_device_requests_the_adapters_full_texture_dimension_limit() {
    let adapter = wgpu::Limits {
        max_texture_dimension_2d: 16_384,
        ..Default::default()
    };
    let requested = live_device_limits(adapter.clone());
    assert_eq!(requested.max_texture_dimension_2d, 16_384);
    assert!(requested.check_limits(&adapter));
}

/// Parallels' virtual GLES adapter on Windows 11 ARM and Ubuntu ARM
/// advertises a valid graphics device with no compute dispatch capacity.
/// Kettle has no compute pipelines, so that must lower the device request
/// rather than making an otherwise usable terminal fail at startup.
#[test]
fn live_device_clamps_unused_compute_limits_to_the_adapter() {
    let adapter = wgpu::Limits {
        max_texture_dimension_2d: 16_384,
        max_compute_workgroup_storage_size: 0,
        max_compute_invocations_per_workgroup: 0,
        max_compute_workgroup_size_x: 0,
        max_compute_workgroup_size_y: 0,
        max_compute_workgroup_size_z: 0,
        max_compute_workgroups_per_dimension: 0,
        ..Default::default()
    };
    let requested = live_device_limits(adapter.clone());

    assert_eq!(requested.max_texture_dimension_2d, 16_384);
    assert_eq!(requested.max_compute_workgroups_per_dimension, 0);
    assert!(
        requested.check_limits(&adapter),
        "every requested limit must fit the virtual adapter"
    );
}
