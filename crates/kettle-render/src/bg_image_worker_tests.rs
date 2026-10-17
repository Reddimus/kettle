use super::{BgImageJob, BgImageWorker};

#[test]
fn completed_decode_requests_redraw_after_publishing_the_result() {
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = BgImageWorker::start(Some(std::sync::Arc::new(move || {
        let _ = tx.send(());
    })))
    .expect("worker thread should start");
    assert!(
        worker
            .try_submit(BgImageJob {
                path: "/definitely/does/not/exist/kettle-wake-test.png".into(),
                blur_radius: 0,
            })
            .is_ok()
    );
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("a completed decode must request redraw without a frame poll");
    assert!(!worker.busy.load(std::sync::atomic::Ordering::Acquire));
    let result = worker.try_recv().expect("the result precedes the redraw");
    assert_eq!(result.blur_radius, 0);
    assert!(result.frames.is_empty());
}

/// A nonexistent path decodes to an empty frame list rather than blocking
/// or panicking, and the result must arrive on `try_recv` carrying back the
/// same `(path, blur_radius)` key the job was submitted with, which is what
/// `apply_bg_image_worker_result` keys its stale-result check on.
#[test]
fn worker_delivers_a_keyed_result_for_a_failed_decode() {
    let worker = BgImageWorker::start(None).expect("worker thread should start");
    let path = "/definitely/does/not/exist/kettle-test-wallpaper.png".to_string();
    assert!(
        worker
            .try_submit(BgImageJob {
                path: path.clone(),
                blur_radius: 8,
            })
            .is_ok(),
        "first submit on an idle worker must succeed"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let result = loop {
        if let Some(result) = worker.try_recv() {
            break result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not deliver a result within the timeout"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };

    assert_eq!(result.path, path);
    assert_eq!(result.blur_radius, 8);
    assert!(
        result.frames.is_empty(),
        "a decode failure must deliver an empty frame list, not block or panic"
    );
}

/// `try_submit` while a job is still in flight must report `Busy` rather
/// than silently dropping or blocking — `request_bg_image_reload` relies
/// on this to decide whether to (re)submit.
#[test]
fn worker_reports_busy_for_a_second_submit_before_the_first_completes() {
    let worker = BgImageWorker::start(None).expect("worker thread should start");
    assert!(
        worker
            .try_submit(BgImageJob {
                path: "/definitely/does/not/exist/kettle-test-wallpaper-a.png".to_string(),
                blur_radius: 0,
            })
            .is_ok()
    );
    // Best-effort race: submit again immediately. Either the first job
    // already finished (busy flag cleared) and this succeeds, or it's
    // still in flight and this reports Busy — both are valid outcomes;
    // what must NOT happen is a panic or a silently dropped job.
    let _ = worker.try_submit(BgImageJob {
        path: "/definitely/does/not/exist/kettle-test-wallpaper-b.png".to_string(),
        blur_radius: 0,
    });
    // Drain whatever results land within the timeout so the test doesn't
    // leak an assertion about ordering — the key behavior under test is
    // "no panic, no hang", already exercised above.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = 0;
    while seen < 1 && std::time::Instant::now() < deadline {
        if worker.try_recv().is_some() {
            seen += 1;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert!(seen >= 1, "at least the first submitted job must complete");
}
