//! Reduced motion holds an animated background on its frame. Through the
//! real frame path on the CPU backend, so every host runs it.

use std::time::{Duration, Instant};

use crate::Renderer;
use crate::backend::RendererBackend;
use crate::backend::layers::StarfieldLayer;
use crate::background::BgImageAnim;
use crate::gpu_tests::gpu_test_config;
use crate::headless_tests::{capture, focused};
use crate::starfield::CpuStarfield;
use kettle_config::{BackgroundAnimation, BackgroundType, Config};

const SIZE: (u32, u32) = (64, 48);

fn cpu_renderer(cfg: &Config) -> Renderer {
    Renderer::headless_for_tests_on(RendererBackend::Cpu, cfg, SIZE.0, SIZE.1, 1.0, false)
        .expect("a CPU renderer builds")
        .expect("a CPU renderer needs no adapter")
}

fn stars(field: &CpuStarfield) -> Vec<[f32; 4]> {
    field
        .uniforms()
        .stars
        .iter()
        .map(|star| star.geom)
        .collect()
}

fn twenty_seconds_ago() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(20))
        .expect("the clock reaches back 20 s")
}

/// Held, the starfield shows the frame of the moment motion stopped, asks
/// for no wakes or frame edges, and does not drift however long it is held.
/// Allowed again, it plays on from that frame rather than jumping to where
/// the time would have taken it.
#[test]
fn reduced_motion_holds_the_starfield_on_its_frame() {
    let cfg = Config {
        background_type: BackgroundType::Starfield,
        background_animation: BackgroundAnimation::Always,
        background_opacity: 1.0,
        ..gpu_test_config()
    };
    let mut renderer = cpu_renderer(&cfg);
    assert!(renderer.background_is_animating(&cfg, true));
    assert!(renderer.bg_anim_interval_ms(&cfg, true).is_some());
    let started = twenty_seconds_ago();
    renderer.starfield_started = started;
    renderer.set_background_motion(false, started + Duration::from_secs(12));
    assert!(!renderer.background_is_animating(&cfg, true));
    assert_eq!(renderer.bg_anim_interval_ms(&cfg, true), None);
    assert_eq!(renderer.bg_current_frame_index(&cfg, true), None);
    capture(&mut renderer, &cfg, &[], &focused(false));
    let mut held = CpuStarfield::new();
    held.upload([SIZE.0 as f32, SIZE.1 as f32], 12.0);
    let StarfieldLayer::Cpu(drawn) = &renderer.starfield else {
        panic!("a CPU renderer draws a CPU starfield");
    };
    assert_eq!(stars(drawn), stars(&held), "the frame motion stopped on");
    // Holding again changes nothing; releasing resumes from 12 s.
    renderer.set_background_motion(false, started + Duration::from_secs(15));
    renderer.set_background_motion(true, started + Duration::from_secs(19));
    assert!(renderer.background_is_animating(&cfg, true));
    assert_eq!(renderer.starfield_started, started + Duration::from_secs(7));
}

/// An animated image holds the same way: a repaint while held shows the
/// frame of the moment motion stopped, not the one the time has reached,
/// and wakes nothing.
#[test]
fn reduced_motion_holds_an_animated_image_on_its_frame() {
    let cfg = Config {
        background_type: BackgroundType::Image,
        background_image: "held.gif".into(),
        background_blur: false,
        background_animation: BackgroundAnimation::Always,
        background_opacity: 1.0,
        ..gpu_test_config()
    };
    let mut renderer = cpu_renderer(&cfg);
    let started = twenty_seconds_ago();
    let frames: Vec<_> = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
        .into_iter()
        .map(|rgba| kettle_core::ImageData::new(1, 1, rgba.to_vec()).expect("a 1x1 frame"))
        .collect();
    renderer.bg_image_cache = Some(BgImageAnim {
        path: "held.gif".into(),
        blur: 0,
        frames,
        opaque_frames: vec![true; 3],
        gaps: vec![100; 3],
        started,
    });
    assert!(renderer.background_is_animating(&cfg, true));
    // 1050 ms into a 300 ms loop is the green frame; the time now (20 s
    // and a little) is on the blue one.
    renderer.set_background_motion(false, started + Duration::from_millis(1050));
    assert_eq!(renderer.bg_anim_interval_ms(&cfg, true), None);
    assert_eq!(renderer.bg_current_frame_index(&cfg, true), None);
    let frame = capture(&mut renderer, &cfg, &[], &focused(false));
    let [r, g, b, _] = frame.get_pixel(SIZE.0 / 2, SIZE.1 / 2).0;
    assert!(g > r && g > b, "the held green frame, not ({r}, {g}, {b})");
    renderer.set_background_motion(true, started + Duration::from_secs(30));
    let cache = renderer.bg_image_cache.as_ref().expect("still cached");
    assert_eq!(cache.started, started + Duration::from_millis(28_950));
}
