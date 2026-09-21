//! Pixel-level check of the visual bell's brightness.
//!
//! `bell-flash-intensity` is a step of CIE L* rather than an alpha because the
//! quad pipeline blends in linear light on an sRGB attachment, where one alpha
//! is a very different visible change on a dark theme than on a light one.
//! This renders the `DebugScene::BellFlash` fixture, which paints the peak
//! wash through the same `bell_flash_alpha` helper and quad pipeline the live
//! frame uses, and measures the lightness of background pixels against the
//! plain `DebugScene::Default` render. The shipped default must move a dark
//! theme up and a light theme down by the same number of L* units, within the
//! rounding of 8-bit sRGB.

use kettle_config::{Config, Theme};
use kettle_render::{DebugScene, capture_png_with, lightness, relative_luminance};
use tempfile::Builder;

fn mean_lightness(png: &std::path::Path, x0: u32, x1: u32, y0: u32, y1: u32) -> (f64, f64, f64) {
    let pixels = image::open(png).unwrap().to_rgba8();
    let mut sum = 0.0;
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    let mut n = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = pixels.get_pixel(x, y);
            let l = lightness(relative_luminance(kettle_config::Rgb::new(
                p[0], p[1], p[2],
            )));
            sum += l;
            min = min.min(l);
            max = max.max(l);
            n += 1.0;
        }
    }
    (sum / n, min, max)
}

fn flash_delta(theme: Theme) -> (f64, f64) {
    // Parallels' Windows ARM WDDM adapter faults during a headless wgpu device
    // request; WARP still renders exact pixels there.
    let cfg = Config {
        gpu_force_software: cfg!(all(target_os = "windows", target_arch = "aarch64")),
        theme,
        ..Config::default()
    };
    let baseline = Builder::new().suffix(".png").tempfile().unwrap();
    let flashed = Builder::new().suffix(".png").tempfile().unwrap();
    capture_png_with(&cfg, 96, 28, baseline.path(), DebugScene::Default).unwrap();
    capture_png_with(&cfg, 96, 28, flashed.path(), DebugScene::BellFlash).unwrap();
    let (w, h) = image::open(baseline.path())
        .unwrap()
        .to_rgba8()
        .dimensions();
    assert_eq!(
        image::open(flashed.path()).unwrap().to_rgba8().dimensions(),
        (w, h)
    );
    // A background-only patch: the bottom-right corner of the body, clear of
    // the sample text at the top-left and of any bottom chrome edge.
    let (x0, x1, y0, y1) = (w - 48, w - 16, h - 40, h - 16);
    let (before, before_min, before_max) = mean_lightness(baseline.path(), x0, x1, y0, y1);
    let (after, after_min, after_max) = mean_lightness(flashed.path(), x0, x1, y0, y1);
    assert!(
        before_max - before_min < 0.5 && after_max - after_min < 0.5,
        "the sampled patch must be flat background before ({before_min}..{before_max}) \
         and after ({after_min}..{after_max})"
    );
    let bg = lightness(relative_luminance(cfg.theme.background));
    assert!(
        (before - bg).abs() < 0.6,
        "the baseline patch must be the theme background: {before} vs {bg}"
    );
    (after - before, f64::from(cfg.bell_flash_intensity) * 100.0)
}

#[test]
fn bell_flash_moves_dark_and_light_themes_by_the_configured_lightness_step() {
    let (dark_delta, want) = flash_delta(Theme::default());
    assert!(
        (dark_delta - want).abs() < 0.6,
        "dark theme: the flash lifted the background by {dark_delta:+.2} L*, want {want:+.2}"
    );
    let light = Theme::by_name("iTerm2 Solarized Light");
    assert!(
        !light.is_dark(),
        "the light fixture must be a bundled light theme"
    );
    let (light_delta, want) = flash_delta(light);
    assert!(
        (light_delta + want).abs() < 0.6,
        "light theme: the flash moved the background by {light_delta:+.2} L*, want {:+.2}",
        -want
    );
}
