//! The presenters against real protocol peers: the in-process compositor in
//! `test_wl` for Wayland, and an X server named by
//! `KETTLE_TEST_X11_DISPLAY` for X11 (skipped when unset).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::damage::Rect;
use super::shm::ShmPresenter;
use super::test_wl::{Config, Server, TestClient};
use super::{FRAME_CALLBACK_TIMEOUT, PresentOutcome};

const W: u32 = 64;
const H: u32 = 32;

fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect::new(x, y, w, h)
}

/// The frame the window should show: every present updates it, and every
/// committed buffer must equal it.
struct Scene {
    truth: Vec<u32>,
}

impl Scene {
    fn new() -> Self {
        Self {
            truth: vec![0; (W * H) as usize],
        }
    }

    fn paint(&mut self, rect: Rect, value: u32) {
        fill(&mut self.truth, rect, value);
    }

    /// Change `rect` to `value` and present it on `p`, repainting only what
    /// the presenter asks for.
    fn present(&mut self, p: &mut ShmPresenter, rect: Rect, value: u32) -> PresentOutcome {
        self.paint(rect, value);
        let mut frame = p.buffer_mut().expect("buffer");
        for rr in frame.repaint_region(&[rect]) {
            copy(frame.pixels_mut(), &self.truth, rr);
        }
        frame.present(&[rect], || {}).expect("present")
    }
}

fn fill(buf: &mut [u32], rect: Rect, value: u32) {
    for y in rect.y..rect.y + rect.height {
        let row = (y * W) as usize;
        buf[row + rect.x as usize..row + (rect.x + rect.width) as usize].fill(value);
    }
}

fn copy(dst: &mut [u32], src: &[u32], rect: Rect) {
    for y in rect.y..rect.y + rect.height {
        let a = (y * W + rect.x) as usize;
        let b = a + rect.width as usize;
        dst[a..b].copy_from_slice(&src[a..b]);
    }
}

fn covers(damage: &[[i32; 4]], rect: Rect) -> bool {
    // Every pixel of `rect` lies in some damage rectangle.
    (rect.y..rect.y + rect.height).all(|y| {
        (rect.x..rect.x + rect.width).all(|x| {
            let (x, y) = (i64::from(x), i64::from(y));
            damage.iter().any(|d| {
                let [dx, dy, dw, dh] = d.map(i64::from);
                x >= dx && y >= dy && x < dx + dw && y < dy + dh
            })
        })
    })
}

/// A compositor, a client playing winit, and a presenter on its surface.
fn fixture(cfg: Config) -> Option<(Server, TestClient, ShmPresenter)> {
    let (server, mut streams) = Server::start(cfg, 1);
    let mut client = TestClient::connect(streams.remove(0))?;
    let mut p = ShmPresenter::new(client.window(), Arc::new(client.display())).expect("presenter");
    p.resize(W, H, 1.0);
    client.roundtrip();
    Some((server, client, p))
}

#[test]
fn an_older_compositor_still_gets_a_presenter() {
    let Some((server, mut client, mut p)) = fixture(Config::default()) else {
        return;
    };
    let mut scene = Scene::new();
    assert_eq!(
        scene.present(&mut p, r(0, 0, W, H), 7),
        PresentOutcome::Presented
    );
    client.roundtrip();
    drop(p);
    client.roundtrip();
    let log = server.log();
    assert_eq!(log.shm_versions, vec![1]);
    assert_eq!(log.commits.last().expect("commit").pixels, scene.truth);
}

// --- an abandoned draw into a deferred frame's buffer ---

#[test]
fn an_abandoned_draw_over_a_deferred_frame_never_reaches_the_screen() {
    let Some((server, mut client, mut p)) = fixture(Config::default()) else {
        return;
    };
    let mut scene = Scene::new();
    assert_eq!(
        scene.present(&mut p, r(0, 0, W, H), 1),
        PresentOutcome::Presented
    );
    client.roundtrip();
    // The frame callback is held, so B is deferred in its buffer.
    assert_eq!(
        scene.present(&mut p, r(0, 0, 10, 10), 2),
        PresentOutcome::Deferred
    );
    // C is drawn into B's buffer and dropped half-way.
    {
        let mut frame = p.buffer_mut().expect("buffer");
        frame.pixels_mut()[..(W * 4) as usize].fill(0xdead_beef);
    }
    server.fire_frames(&mut client);
    assert_eq!(
        p.flush(|| {}).expect("flush"),
        PresentOutcome::Unchanged,
        "the abandoned buffer was committed"
    );
    assert!(!p.has_deferred());
    // D repaints in full and carries B's changes.
    assert_eq!(
        scene.present(&mut p, r(20, 20, 5, 5), 3),
        PresentOutcome::Presented
    );
    client.roundtrip();
    let log = server.log();
    for (i, c) in log.commits.iter().enumerate() {
        assert!(
            !c.pixels.contains(&0xdead_beef),
            "commit {i} shows the abandoned draw"
        );
    }
    assert_eq!(log.commits.len(), 2);
    let last = log.commits.last().expect("commit");
    assert_eq!(last.pixels, scene.truth, "the last commit is the scene");
    assert!(
        covers(&last.damage, r(0, 0, 10, 10)),
        "B's damage was not carried: {:?}",
        last.damage
    );
}

// --- the pre-commit hook unwinds ---

#[test]
fn a_hook_that_unwinds_leaves_nothing_staged_on_the_surface() {
    let cfg = Config {
        release_on_commit: false,
        ..Config::default()
    };
    let Some((server, mut client, mut p)) = fixture(cfg) else {
        return;
    };
    let mut scene = Scene::new();
    scene.present(&mut p, r(0, 0, W, H), 1);
    server.fire_frames(&mut client);
    server.release_all(&mut client);
    scene.paint(r(0, 0, 4, 4), 2);
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let mut frame = p.buffer_mut().expect("buffer");
        for rr in frame.repaint_region(&[r(0, 0, 4, 4)]) {
            copy(frame.pixels_mut(), &scene.truth, rr);
        }
        let _ = frame.present(&[r(0, 0, 4, 4)], || panic!("hook failed"));
    }));
    assert!(unwound.is_err(), "the hook did not run");
    // winit commits the surface for its own reasons (CSD, opaque region).
    client.surface.commit();
    client.roundtrip();
    {
        let log = server.log();
        let last = log.commits.last().expect("commit");
        assert!(
            !last.attached,
            "the surface owner's commit carried the presenter's buffer"
        );
        assert_eq!(log.held, 0, "the compositor holds a buffer");
    }
    // The frame stayed deferred and goes out with the next flush.
    assert!(p.has_deferred());
    assert_eq!(p.flush(|| {}).expect("flush"), PresentOutcome::Presented);
    client.roundtrip();
    assert_eq!(
        server.log().commits.last().expect("commit").pixels,
        scene.truth
    );
}

// --- a hidden surface whose callbacks never come ---

#[test]
fn a_hidden_surface_keeps_at_most_one_frame_callback() {
    let Some((server, mut client, mut p)) = fixture(Config::default()) else {
        return;
    };
    p.frame_timeout = Duration::from_millis(30);
    let mut scene = Scene::new();
    scene.present(&mut p, r(0, 0, W, H), 1);
    client.roundtrip();
    assert_eq!(
        scene.present(&mut p, r(0, 0, 1, 1), 2),
        PresentOutcome::Deferred
    );
    for i in 0..3 {
        std::thread::sleep(Duration::from_millis(40));
        let out = scene.present(&mut p, r(i, 0, 1, 1), 3 + i);
        assert_eq!(out, PresentOutcome::Presented, "timed-out frame {i}");
        client.roundtrip();
    }
    let stats = p.stats();
    assert_eq!(stats.commits, 4);
    assert_eq!(stats.timed_out_commits, 3);
    assert!(stats.frame_callback_pending);
    {
        let log = server.log();
        assert_eq!(log.commits.len(), 4);
        assert_eq!(log.commits.last().expect("commit").pixels, scene.truth);
        assert_eq!(
            log.callbacks_outstanding(),
            1,
            "frame callbacks piled up on a hidden surface"
        );
    }
    // Shown again: the withheld callback comes, and pacing resumes on it.
    server.fire_frames(&mut client);
    assert_eq!(
        scene.present(&mut p, r(5, 5, 1, 1), 9),
        PresentOutcome::Presented
    );
    assert_eq!(
        scene.present(&mut p, r(6, 5, 1, 1), 10),
        PresentOutcome::Deferred
    );
    client.roundtrip();
    assert_eq!(server.log().callbacks_outstanding(), 1);
}

// --- the last frame of a burst ---

#[test]
fn a_deferred_frame_has_a_deadline_until_it_is_flushed() {
    let Some((server, mut client, mut p)) = fixture(Config::default()) else {
        return;
    };
    let mut scene = Scene::new();
    assert_eq!(p.pending_deadline(), None);
    let before = Instant::now();
    scene.present(&mut p, r(0, 0, W, H), 1);
    assert_eq!(p.pending_deadline(), None, "nothing is deferred");
    client.roundtrip();
    assert_eq!(
        scene.present(&mut p, r(1, 1, 2, 2), 2),
        PresentOutcome::Deferred
    );
    let deadline = p.pending_deadline().expect("a deferred frame needs a wake");
    assert!(deadline > Instant::now(), "the deadline must not spin");
    assert!(deadline <= before + FRAME_CALLBACK_TIMEOUT + Duration::from_millis(500));
    // The callback arrives: the frame goes out on the next flush.
    server.fire_frames(&mut client);
    assert_eq!(p.flush(|| {}).expect("flush"), PresentOutcome::Presented);
    assert_eq!(p.pending_deadline(), None);
    client.roundtrip();
    assert_eq!(
        server.log().commits.last().expect("commit").pixels,
        scene.truth
    );
}

#[test]
fn a_frame_deferred_past_its_timeout_is_due_at_once() {
    let Some((_server, mut client, mut p)) = fixture(Config::default()) else {
        return;
    };
    p.frame_timeout = Duration::from_millis(30);
    let mut scene = Scene::new();
    scene.present(&mut p, r(0, 0, W, H), 1);
    client.roundtrip();
    scene.present(&mut p, r(1, 1, 2, 2), 2);
    std::thread::sleep(Duration::from_millis(40));
    let deadline = p.pending_deadline().expect("deferred");
    assert!(deadline <= Instant::now());
    assert_eq!(p.flush(|| {}).expect("flush"), PresentOutcome::Presented);
}

// --- occlusion and releases that come late ---

#[test]
fn a_buffer_released_late_while_occluded_is_freed() {
    let cfg = Config {
        release_on_commit: false,
        ..Config::default()
    };
    let Some((server, mut client, mut p)) = fixture(cfg) else {
        return;
    };
    let mut scene = Scene::new();
    scene.present(&mut p, r(0, 0, W, H), 1);
    client.roundtrip();
    p.set_occluded(true);
    assert_eq!(p.stats().buffers, 1, "the compositor still holds it");
    let deadline = p
        .idle_release_deadline()
        .expect("an occluded window must wake to free the held buffer");
    assert!(deadline > Instant::now(), "a past deadline would spin");
    // The deadline stays put, so a caller that recomputes it on waking
    // sees it come due.
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(p.idle_release_deadline(), Some(deadline));
    server.release_all(&mut client);
    // What the caller does at that deadline.
    p.release_idle_buffers();
    assert_eq!(p.stats().buffers, 0, "the late release freed nothing");
    assert_eq!(p.idle_release_deadline(), None);
}

#[test]
fn a_late_release_while_occluded_goes_on_dispatch() {
    let cfg = Config {
        release_on_commit: false,
        ..Config::default()
    };
    let Some((server, mut client, mut p)) = fixture(cfg) else {
        return;
    };
    let mut scene = Scene::new();
    scene.present(&mut p, r(0, 0, W, H), 1);
    client.roundtrip();
    p.set_occluded(true);
    server.release_all(&mut client);
    // Not the idle path: an unrelated call that dispatches.
    let _ = p.frame_pending();
    assert_eq!(p.stats().buffers, 0);
}

// --- a window and a display from different connections ---

#[test]
fn a_window_from_another_connection_is_refused() {
    let (_server, mut streams) = Server::start(Config::default(), 2);
    let Some(a) = TestClient::connect(streams.remove(0)) else {
        return;
    };
    let b = TestClient::connect(streams.remove(0)).expect("second client");
    let err = ShmPresenter::new(a.window(), Arc::new(b.display()))
        .err()
        .expect("mismatched connections accepted");
    assert!(
        err.to_string().contains("different Wayland connections"),
        "{err}"
    );
}

#[test]
fn a_window_handle_that_names_the_wrong_display_is_refused() {
    let (_server, mut streams) = Server::start(Config::default(), 2);
    let Some(a) = TestClient::connect(streams.remove(0)) else {
        return;
    };
    let b = TestClient::connect(streams.remove(0)).expect("second client");
    // The handle claims b's display, which is also the one passed: only
    // the surface proxy itself tells the truth.
    let result = ShmPresenter::new(a.window_claiming(&b), Arc::new(b.display()));
    if super::shm::can_ask_proxy_display() {
        let err = result
            .err()
            .expect("a surface of another connection accepted");
        assert!(err.to_string().contains("surface is on another"), "{err}");
    } else {
        eprintln!("skipping: libwayland-client older than 1.23");
    }
}

// --- globals bound per presenter ---

#[test]
fn recreating_the_presenter_releases_its_globals() {
    let cfg = Config {
        shm_version: 2,
        fixes: true,
        ..Config::default()
    };
    let (server, mut streams) = Server::start(cfg, 1);
    let Some(mut client) = TestClient::connect(streams.remove(0)) else {
        return;
    };
    for value in 0..3 {
        let mut p =
            ShmPresenter::new(client.window(), Arc::new(client.display())).expect("presenter");
        p.resize(W, H, 1.0);
        let mut scene = Scene::new();
        scene.present(&mut p, r(0, 0, W, H), value);
        drop(p);
    }
    client.roundtrip();
    let log = server.log();
    assert_eq!(log.commits.len(), 3);
    assert_eq!(log.shm_versions, vec![2, 2, 2], "wl_shm v2 has release");
    assert_eq!(log.shm_releases, 3, "a wl_shm was left bound");
    assert_eq!(log.registries_destroyed, 3, "a registry was left");
    assert_eq!(log.fixes_destroyed, 3, "a wl_fixes was left");
}
