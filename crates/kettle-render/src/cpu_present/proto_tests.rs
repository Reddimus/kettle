//! The presenters against real protocol peers: the in-process compositor in
//! `test_wl` for Wayland, and an X server named by
//! `KETTLE_TEST_X11_DISPLAY` for X11 (skipped when unset).

use std::sync::Arc;

use super::PresentOutcome;
use super::damage::Rect;
use super::shm::ShmPresenter;
use super::test_wl::{Config, Server, TestClient};

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
