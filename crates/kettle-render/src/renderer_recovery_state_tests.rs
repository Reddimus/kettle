use super::{RendererRecoveryState, ScreenshotOutputPolicy, ScreenshotRequest};
use std::sync::Arc;

#[test]
fn clone_retains_live_overrides_and_screenshot_completion() {
    let (completion, finished) = std::sync::mpsc::channel();
    let expected_path = std::path::PathBuf::from("recovered-screenshot.png");
    let state = RendererRecoveryState {
        font_family: Arc::from("Live Runtime Font"),
        font_size: 19.5,
        cell_scale_w: 1.25,
        cell_scale_h: 1.5,
        pending_screenshot: Some(ScreenshotRequest {
            out_path: expected_path.clone(),
            output_policy: ScreenshotOutputPolicy::UserSelected,
            crop: Some((1.0, 2.0, 3.0, 4.0)),
            completion: Some(completion),
            cancellation: None,
            recovery_wake: None,
        }),
    };

    let retained = state.clone();
    drop(state);
    assert_eq!(retained.font_family.as_ref(), "Live Runtime Font");
    assert_eq!(retained.font_size, 19.5);
    assert_eq!((retained.cell_scale_w, retained.cell_scale_h), (1.25, 1.5));
    let request = retained.pending_screenshot.expect("queued screenshot");
    assert_eq!(request.out_path, expected_path);
    assert_eq!(request.output_policy, ScreenshotOutputPolicy::UserSelected);
    assert_eq!(request.crop, Some((1.0, 2.0, 3.0, 4.0)));
    request
        .completion
        .expect("completion sender")
        .send(Ok(expected_path.clone()))
        .unwrap();
    assert_eq!(finished.recv().unwrap(), Ok(expected_path));
}

#[test]
fn recovery_tries_same_model_adapters_in_enumeration_order() {
    use super::{AdapterEscalation::*, GpuAdapterKey, recovery_candidate_indices};
    let failed = GpuAdapterKey {
        vendor: 0x10de,
        device: 0x1234,
        backend: wgpu::Backend::Vulkan,
    };
    let keys = [failed, failed];
    for stage in [AlternateBackend, SurfacePreferred, AnyHardware] {
        assert_eq!(
            recovery_candidate_indices(&keys, stage, Some(failed), Some(failed)),
            vec![0, 1],
            "recovery must try both ambiguous same-model adapters before fallback at {stage:?}"
        );
    }
}

#[test]
fn recovery_prefers_another_backend_even_when_failed_devices_build() {
    use super::{
        AdapterEscalation, GpuAdapterKey, recovery_candidate_indices, try_adapter_candidates,
    };
    let failed = GpuAdapterKey {
        vendor: 1,
        device: 2,
        backend: wgpu::Backend::Vulkan,
    };
    let alternate = GpuAdapterKey {
        backend: wgpu::Backend::Gl,
        ..failed
    };
    for keys in [
        vec![failed, alternate],
        vec![failed, alternate, failed, alternate],
    ] {
        let indices = recovery_candidate_indices(
            &keys,
            AdapterEscalation::AlternateBackend,
            Some(failed),
            None,
        );
        let (chosen, ()) = pollster::block_on(try_adapter_candidates(indices.clone(), |_index| {
            // Device creation also succeeds on the backend that fails while drawing.
            std::future::ready(Ok(()))
        }))
        .unwrap();
        assert_eq!(keys[chosen], alternate);
        let expected: Vec<_> = (0..keys.len())
            .filter(|&i| keys[i] == alternate)
            .chain((0..keys.len()).filter(|&i| keys[i] == failed))
            .collect();
        assert_eq!(
            indices, expected,
            "keep enumeration order within each backend tier"
        );
    }
}

#[test]
fn recovery_keeps_unique_model_excluded_and_bounds_device_attempts() {
    use super::{
        AdapterEscalation::*, GpuAdapterKey, MAX_RECOVERY_ADAPTER_ATTEMPTS,
        recovery_candidate_indices, try_adapter_candidates,
    };
    let failed = GpuAdapterKey {
        vendor: 1,
        device: 2,
        backend: wgpu::Backend::Vulkan,
    };
    let other = GpuAdapterKey {
        device: 3,
        ..failed
    };
    assert_eq!(
        recovery_candidate_indices(&[failed, other], AnyHardware, Some(failed), None),
        vec![1]
    );
    assert!(
        recovery_candidate_indices(
            &[failed, other],
            SurfacePreferred,
            Some(failed),
            Some(failed)
        )
        .is_empty()
    );
    let calls = std::cell::RefCell::new(Vec::new());
    let chosen = pollster::block_on(try_adapter_candidates(vec![0, 1, 2], |index| {
        calls.borrow_mut().push(index);
        std::future::ready(if index == 1 {
            Ok(())
        } else {
            Err(anyhow::anyhow!("failed"))
        })
    }))
    .unwrap();
    assert_eq!(chosen.0, 1);
    assert_eq!(*calls.borrow(), vec![0, 1]);
    calls.borrow_mut().clear();
    let result = pollster::block_on(try_adapter_candidates((0..20).collect(), |index| {
        calls.borrow_mut().push(index);
        std::future::ready(Err::<(), _>(anyhow::anyhow!("failed")))
    }));
    assert!(result.is_err());
    assert_eq!(
        *calls.borrow(),
        (0..MAX_RECOVERY_ADAPTER_ATTEMPTS).collect::<Vec<_>>()
    );
}
