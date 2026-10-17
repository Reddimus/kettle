use super::{
    MAX_SCREENSHOT_PERSISTENCE_JOBS, ScreenshotCancellation, ScreenshotOutputPolicy,
    ScreenshotPersistencePermit, ScreenshotPersistencePool, create_screenshot_file,
    crop_screenshot, persist_screenshot_file, persist_screenshot_file_if,
    persist_screenshot_file_with_flush, persist_screenshot_file_with_steps, production_source,
    screenshot_publication_error_message, screenshot_target_bytes,
    wait_for_screenshot_submission_with,
};

static SHARED_PERSISTENCE_POOL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serialize the tests that use the process-wide persistence pool: a test
/// that counts its permits must not overlap another test's capture in
/// flight. A capture holds this until its completion arrives, and the
/// worker releases the permit before sending that, so the pool is idle
/// whenever the guard is free.
pub(crate) fn shared_persistence_pool_guard() -> std::sync::MutexGuard<'static, ()> {
    SHARED_PERSISTENCE_POOL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_tempdir() -> kettle_test_support::PrivateTempDir {
    kettle_test_support::private_tempdir("kettle-render-test-")
}

fn pixels(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|pixel| [pixel as u8, 0, 0, 255])
        .collect()
}

/// The cards a press can hit are the ones on screen: a frame that never
/// reached it (a timed-out or occluded acquire) keeps the last presented
/// frame's, or a click could open a card the user cannot see, or miss one
/// that is still there.
#[test]
fn cards_reach_the_screen_only_with_a_presented_frame() {
    let src = production_source();
    let body = src
        .split("pub fn render_frame_with_status_and_pre_present")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("render_frame_with_status_and_pre_present body");
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let commit = "self.commit_painted_cards();";
    let present = normalized
        .find("self.gpu.queue.present(frame);")
        .expect("presentation");
    let headless = normalized
        .find("// Headless (tests): the offscreen capture is the whole frame.")
        .expect("headless branch");
    let headless_end = headless
        + normalized[headless..]
            .find("return Ok(FrameOutcome::Occluded);")
            .expect("headless return");
    let commits: Vec<usize> = normalized.match_indices(commit).map(|(at, _)| at).collect();
    assert_eq!(commits.len(), 2, "{commits:?}");
    assert!(
        headless < commits[0] && commits[0] < headless_end,
        "a headless capture stands in for a presented frame"
    );
    assert!(
        present < commits[1],
        "only a presented frame shows its cards"
    );
    for field in ["painted_cards", "painted_shown"] {
        assert_eq!(src.matches(&format!("self.{field} =")).count(), 0);
        assert_eq!(src.matches(&format!("{field}.extend(")).count(), 0);
        assert_eq!(src.matches(&format!("{field}.push(")).count(), 0);
        assert_eq!(
            src.matches(&format!("self.{field}.clone_from(")).count(),
            1,
            "only commit_painted_cards puts {field} on screen"
        );
    }
}

/// Metal returns `SurfaceError::Occluded` before it vends a drawable. The
/// capture therefore has to render and copy Kettle's own target before
/// swapchain acquisition, and every no-drawable outcome still has to submit
/// that work. Moving any of those operations restores the control timeout
/// even though visible screenshots continue to pass.
#[test]
fn live_capture_is_encoded_before_swapchain_acquisition() {
    let src = production_source();
    let body = src
        .split("pub fn render_frame_with_status_and_pre_present")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("render_frame_with_status_and_pre_present body");
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let target = normalized
        .find("self.create_screenshot_target(target_size)")
        .expect("live capture must allocate an independent offscreen target");
    let render = normalized
        .find("self.encode_scene_pass( &capture.view")
        .expect("live capture must render the scene into its target");
    let copy = normalized
        .find("self.prepare_texture_screenshot(")
        .expect("live capture must copy the rendered target");
    let acquire = normalized
        .find("self .surface .as_ref() .map(wgpu::Surface::get_current_texture)")
        .expect("live rendering must acquire the swapchain");
    let presentation = normalized
        .find(".presentation .ensure_target(")
        .expect("translucent visible frames need a presentation target");
    assert!(
        target < render && render < copy && copy < acquire && acquire < presentation,
        "capture ordering must be target creation < scene render < readback copy < acquire < presentation allocation"
    );
    let acquire_tail = &normalized[acquire..];
    assert_eq!(
        acquire_tail
            .matches("self.submit_offscreen_screenshot(encoder, prepared_screenshot)")
            .count(),
        7,
        "a headless renderer, Occluded, Timeout, Outdated, Lost, Validation, and presentation-allocation failure must all submit the offscreen capture"
    );
    assert!(
        src.contains(
            "usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC"
        ),
        "only Kettle's transient capture texture, not the compositor surface, needs COPY_SRC"
    );
    let creator = src
        .split("fn create_screenshot_target")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("screenshot target creator");
    let staging_reservation = creator
        .find("reserve_transient_gpu(staging_bytes)")
        .expect("staging memory must be reserved");
    let texture_creation = creator
        .find("device.create_texture")
        .expect("capture texture must be created");
    assert!(
        staging_reservation < texture_creation,
        "all screenshot allocations must be reserved before a texture can enter an encoder"
    );
}

#[test]
fn live_capture_keeps_the_advertised_6k_and_256_mib_bounds() {
    let six_k = screenshot_target_bytes(6016, 3384).expect("6K target must fit");
    assert!(
        six_k > 64 * 1024 * 1024,
        "this discriminates the old image cap"
    );
    assert_eq!(screenshot_target_bytes(8192, 8192), Some(256 * 1024 * 1024));
    assert_eq!(screenshot_target_bytes(8193, 8192), None);
    assert_eq!(screenshot_target_bytes(0, 1), None);

    let src = production_source();
    let creator = src
        .split("fn create_screenshot_target")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("screenshot target creator");
    assert!(
        creator.contains("reserve_transient_gpu(bytes)"),
        "large captures must use the process-only transient budget, not the 64 MiB image cap"
    );
    assert!(
        src.contains("_capture_texture: wgpu::Texture")
            && src.contains("_capture_gpu: kettle_core::GraphicsReservation")
            && src.contains("_staging_gpu: kettle_core::GraphicsReservation"),
        "the copy source, staging buffer, and both accounting reservations must travel with the worker job"
    );
}

#[test]
fn control_timeout_cancellation_is_shared_with_the_renderer() {
    let cancellation = ScreenshotCancellation::default();
    let renderer_side = cancellation.clone();
    assert!(!renderer_side.is_cancelled());
    assert!(cancellation.cancel());
    assert!(renderer_side.is_cancelled());
    assert!(!renderer_side.commit(), "cancelled work cannot publish");

    let committed = ScreenshotCancellation::default();
    assert!(committed.commit());
    assert!(
        !committed.cancel(),
        "a caller must await the real result once publication committed"
    );
}

#[test]
fn cancellation_during_encoding_discards_instead_of_publishing() {
    use std::io::Write as _;

    let dir = test_tempdir();
    let path = dir.path().join("cancelled-during-encode.png");
    let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected).unwrap();
    assert!(
        !path.exists(),
        "the requested leaf must stay absent while encoding is cancellable"
    );
    let cancellation = ScreenshotCancellation::default();
    let worker_state = cancellation.clone();
    let (encoding_tx, encoding_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);

    let worker = std::thread::spawn(move || {
        persist_screenshot_file_if(
            file,
            |writer| {
                writer.write_all(b"encoded bytes").unwrap();
                encoding_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(())
            },
            || worker_state.commit(),
        )
    });
    encoding_rx.recv().unwrap();
    assert!(
        !path.exists(),
        "a blocked encoder must not expose a changing destination leaf"
    );
    assert!(
        cancellation.cancel(),
        "timeout must win while the encoder is still active"
    );
    resume_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "screenshot request was cancelled"
    );
    assert!(!path.exists(), "cancelled output must never become visible");
}

#[test]
fn cancellation_can_win_during_the_staged_inode_sync() {
    use std::io::Write as _;

    let dir = test_tempdir();
    let path = dir.path().join("cancelled-during-sync.png");
    let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected).unwrap();
    let cancellation = ScreenshotCancellation::default();
    let worker_state = cancellation.clone();
    let (syncing_tx, syncing_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);

    let worker = std::thread::spawn(move || {
        persist_screenshot_file_with_steps(
            file,
            |writer| {
                writer.write_all(b"encoded bytes").unwrap();
                Ok(())
            },
            std::io::Write::flush,
            |_| {
                syncing_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(())
            },
            || worker_state.commit(),
        )
    });
    syncing_rx.recv().unwrap();
    assert!(
        cancellation.cancel(),
        "a slow durability flush must remain on the reversible side of publication"
    );
    resume_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "screenshot request was cancelled"
    );
    assert!(!path.exists(), "cancelled output must never become visible");
}

#[test]
fn a_gpu_wait_timeout_keeps_accounting_live_until_retirement() {
    struct Reservation(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Reservation {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    let usage = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let reservation = Reservation(usage.clone());
    let mut polls = 0;
    wait_for_screenshot_submission_with(
        || {
            polls += 1;
            assert_eq!(
                usage.load(std::sync::atomic::Ordering::Acquire),
                1,
                "timeout must not retire the in-flight reservation"
            );
            if polls == 1 {
                Err(wgpu::PollError::Timeout)
            } else {
                Ok(())
            }
        },
        || false,
        || {},
        2,
        || panic!("one timeout followed by success must not reset the device"),
    )
    .unwrap();
    assert_eq!(usage.load(std::sync::atomic::Ordering::Acquire), 1);
    drop(reservation);
    assert_eq!(usage.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn repeated_gpu_wait_timeouts_reset_instead_of_stranding_admission() {
    let resets = std::cell::Cell::new(0);
    let error = wait_for_screenshot_submission_with(
        || Err(wgpu::PollError::Timeout),
        || false,
        || {},
        2,
        || resets.set(resets.get() + 1),
    )
    .unwrap_err();
    assert_eq!(resets.get(), 1);
    assert!(error.contains("resetting the GPU device"));

    let src = production_source();
    let wait = src
        .split("wait_for_screenshot_submission_with(")
        .nth(2)
        .and_then(|rest| rest.split("map_rx").next())
        .expect("live screenshot GPU wait call");
    let destroy = wait.find("device.destroy()").expect("device reset");
    let wake = wait
        .rfind("wake.wake()")
        .expect("event-loop recovery wake after reset");
    assert!(
        destroy < wake,
        "a wedged device must wake the event loop after it is destroyed"
    );
}

#[test]
fn device_loss_wakes_recovery_even_when_poll_reports_success() {
    let lost = std::cell::Cell::new(false);
    let wakes = std::cell::Cell::new(0_u32);
    let error = wait_for_screenshot_submission_with(
        || {
            lost.set(true);
            Ok(())
        },
        || lost.get(),
        || wakes.set(wakes.get() + 1),
        2,
        || panic!("a real device loss is not a submission timeout"),
    )
    .unwrap_err();
    assert!(error.contains("device was lost"));
    assert_eq!(wakes.get(), 1, "device loss must wake an occluded loop");
}

#[test]
fn capture_admission_clears_before_persistence_can_block() {
    let src = production_source();
    let worker = src
        .split("impl ScreenshotWorker")
        .nth(1)
        .and_then(|rest| rest.split("enum ScreenshotSubmitError").next())
        .expect("ScreenshotWorker implementation");
    let clear = worker
        .find("worker_busy.store(false")
        .expect("worker busy flag must be cleared");
    let persist = worker
        .find("persistence.try_submit(job)")
        .expect("captured pixels must move to the bounded persistence pool");
    assert!(
        clear < persist,
        "filesystem persistence must not retain GPU capture admission"
    );
}

#[test]
fn persistence_admission_is_process_wide_across_renderer_generations() {
    let _pool = shared_persistence_pool_guard();
    let first_generation = ScreenshotPersistencePool::shared().unwrap();
    let replacement_generation = ScreenshotPersistencePool::shared().unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &first_generation.outstanding,
        &replacement_generation.outstanding
    ));

    let outstanding = &first_generation.outstanding;
    let first = ScreenshotPersistencePermit::try_acquire(outstanding).unwrap();
    let second =
        ScreenshotPersistencePermit::try_acquire(&replacement_generation.outstanding).unwrap();
    assert!(ScreenshotPersistencePermit::try_acquire(outstanding).is_none());
    assert_eq!(
        outstanding.load(std::sync::atomic::Ordering::Acquire),
        MAX_SCREENSHOT_PERSISTENCE_JOBS
    );
    drop(first);
    let replacement = ScreenshotPersistencePermit::try_acquire(outstanding)
        .expect("a finished persistence job must reopen exactly one slot");
    drop((second, replacement));
    assert_eq!(outstanding.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn publication_errors_distinguish_kettles_commit_from_path_existence() {
    assert_eq!(
        screenshot_publication_error_message(
            false,
            std::io::ErrorKind::AlreadyExists,
            "racing writer won",
        ),
        "Kettle did not publish this screenshot because the destination already exists: racing writer won"
    );
    assert_eq!(
        screenshot_publication_error_message(
            false,
            std::io::ErrorKind::PermissionDenied,
            "parent refused publication",
        ),
        "Kettle did not publish this screenshot: parent refused publication"
    );
    assert_eq!(
        screenshot_publication_error_message(
            true,
            std::io::ErrorKind::Other,
            "directory sync failed",
        ),
        "PNG publication completed, but cleanup or durability verification failed; Kettle may have published the destination: directory sync failed"
    );
}

#[test]
fn no_crop_preserves_surface_pixels() {
    let input = pixels(2, 2);
    let (width, height, output) = crop_screenshot(2, 2, input.clone(), None).unwrap();
    assert_eq!((width, height), (2, 2));
    assert_eq!(output, input);
}

#[test]
fn crop_extracts_requested_rows_and_columns() {
    let (width, height, output) =
        crop_screenshot(3, 2, pixels(3, 2), Some((1.0, 0.0, 2.0, 2.0))).unwrap();
    assert_eq!((width, height), (2, 2));
    assert_eq!(
        output
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>(),
        [1, 2, 4, 5]
    );
}

#[test]
fn crop_rejects_rectangles_outside_the_surface() {
    let error = crop_screenshot(2, 2, pixels(2, 2), Some((5.0, 5.0, 1.0, 1.0))).unwrap_err();
    assert_eq!(error, "screenshot crop is outside the surface");
}

#[test]
fn crop_rejects_short_source_buffers() {
    let error = crop_screenshot(2, 2, vec![0; 4], Some((0.0, 0.0, 2.0, 2.0))).unwrap_err();
    assert_eq!(error, "screenshot source buffer is shorter than expected");
}

// A screenshot can capture the same class of transient secrets a `.cast`
// recording can (kettle-core/src/record.rs chmods those to 0o600), so the
// PNG file must land with the same owner-only permissions regardless of
// the process umask.
#[cfg(unix)]
#[test]
fn private_screenshot_file_is_created_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = test_tempdir();
    let path = dir.path().join("shot.png");
    let file = create_screenshot_file(&path, ScreenshotOutputPolicy::PrivateState).expect("open");
    let mode = file.metadata().expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "screenshot file must be owner-read/write only");
}

/// Anything already at the path (a leftover file or, in the threat model, a
/// symlink planted into the check-then-use window to redirect the write at
/// a sensitive file) must make creation fail with `AlreadyExists` rather
/// than being followed or truncated. The no-replace publication is the
/// atomic guard; the ctl-side `validate_screenshot_path` pre-check is only a
/// fast-fail.
#[test]
fn private_screenshot_file_refuses_a_pre_existing_path() {
    let dir = test_tempdir();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"stale").expect("seed file");
    let err = create_screenshot_file(&path, ScreenshotOutputPolicy::PrivateState)
        .expect_err("must refuse an existing path");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&path).expect("original file untouched"),
        b"stale",
        "an existing file at the path must not be truncated or overwritten"
    );
}

#[test]
fn failed_screenshot_encode_removes_the_exact_created_leaf() {
    use std::io::Write as _;

    let dir = test_tempdir();
    for (name, policy) in [
        ("private.png", ScreenshotOutputPolicy::PrivateState),
        ("selected.png", ScreenshotOutputPolicy::UserSelected),
    ] {
        let path = dir.path().join(name);
        let file = create_screenshot_file(&path, policy).expect("create screenshot leaf");
        let error = persist_screenshot_file(file, |writer| {
            writer.write_all(b"partial PNG bytes").unwrap();
            Err("injected encoder failure".to_string())
        })
        .expect_err("the injected encoder failure must propagate");
        assert_eq!(error, "injected encoder failure");
        assert!(
            !path.exists(),
            "an encode failure must not strand a path that blocks retry: {}",
            path.display()
        );
    }
}

#[test]
fn failed_screenshot_flush_removes_an_on_disk_partial_leaf() {
    use std::io::Write as _;

    let dir = test_tempdir();
    let path = dir.path().join("flush-failure.png");
    let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected)
        .expect("create screenshot leaf");
    let error = persist_screenshot_file_with_flush(
        file,
        |writer| {
            writer
                .write_all(&vec![0x5a; 32 * 1024])
                .map_err(|error| error.to_string())
        },
        |_writer| Err(std::io::Error::other("injected flush failure")),
    )
    .expect_err("the injected flush failure must propagate");
    assert!(error.contains("injected flush failure"));
    assert!(
        !path.exists(),
        "a flush failure after underlying writes must remove the partial leaf"
    );
}

#[test]
fn selected_screenshot_never_replaces_a_racing_leaf() {
    use std::io::Write as _;

    let dir = test_tempdir();
    let path = dir.path().join("selected.png");
    let file = create_screenshot_file(&path, ScreenshotOutputPolicy::UserSelected)
        .expect("create screenshot leaf");
    std::fs::write(&path, b"replacement").expect("install replacement leaf");

    let error = persist_screenshot_file(file, |writer| {
        writer
            .write_all(b"complete png")
            .map_err(|error| error.to_string())
    })
    .expect_err("no-replace publication must lose to the racing writer");
    assert!(
        error.starts_with(
            "Kettle did not publish this screenshot because the destination already exists:"
        ),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
}

/// The same no-replace guarantee, exercised against a symlink: a planted
/// symlink at the output path must not be followed to overwrite its target.
#[cfg(unix)]
#[test]
fn private_screenshot_file_refuses_to_follow_a_symlink() {
    let dir = test_tempdir();
    let sensitive = dir.path().join("sensitive");
    std::fs::write(&sensitive, b"secret").expect("seed target");
    let link = dir.path().join("shot.png");
    std::os::unix::fs::symlink(&sensitive, &link).expect("plant symlink");
    let err = create_screenshot_file(&link, ScreenshotOutputPolicy::UserSelected)
        .expect_err("must refuse a symlink");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&sensitive).expect("symlink target untouched"),
        b"secret",
        "the symlink target must not be truncated through the followed link"
    );
}

#[cfg(unix)]
#[test]
fn user_selected_screenshot_accepts_a_public_existing_parent() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = test_tempdir();
    let exports = dir.path().join("exports");
    std::fs::create_dir(&exports).unwrap();
    std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o775)).unwrap();

    let private_path = exports.join("private.png");
    let error = create_screenshot_file(&private_path, ScreenshotOutputPolicy::PrivateState)
        .expect_err("the private-state policy must retain ancestor checks");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

    let selected_path = exports.join("selected.png");
    let file = create_screenshot_file(&selected_path, ScreenshotOutputPolicy::UserSelected)
        .expect("an explicit path may live under an ordinary user directory");
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
}
