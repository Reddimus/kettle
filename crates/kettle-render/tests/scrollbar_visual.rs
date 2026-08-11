use kettle_config::Config;
use kettle_render::{DebugScene, capture_png_with};
use tempfile::Builder;

#[test]
fn compact_scrollbar_is_visible_contrasting_and_edge_scoped() {
    // Keep the visual fixture usable in Parallels Windows ARM, whose virtual
    // WDDM adapter faults on headless wgpu device creation. Physical Windows
    // machines and the live renderer retain hardware-first coverage; this test
    // asserts the rendered pixels.
    let cfg = Config {
        gpu_force_software: cfg!(all(target_os = "windows", target_arch = "aarch64")),
        ..Config::default()
    };
    let baseline = Builder::new().suffix(".png").tempfile().unwrap();
    let scrollbar = Builder::new().suffix(".png").tempfile().unwrap();
    capture_png_with(&cfg, 96, 28, baseline.path(), DebugScene::Default).unwrap();
    capture_png_with(&cfg, 96, 28, scrollbar.path(), DebugScene::Scrollbar).unwrap();

    let baseline_pixels = image::open(baseline.path()).unwrap().to_rgba8();
    let scrollbar_pixels = image::open(scrollbar.path()).unwrap().to_rgba8();
    assert_eq!(baseline_pixels.dimensions(), scrollbar_pixels.dimensions());
    let (width, height) = scrollbar_pixels.dimensions();
    let edge_start = width.saturating_sub(24);
    let mut edge_changes = 0_u64;
    let mut outside_changes = 0_u64;
    let mut foreground_leaning = 0_u64;
    let foreground = cfg.theme.foreground;
    for y in 0..height {
        for x in 0..width {
            let before = baseline_pixels.get_pixel(x, y);
            let after = scrollbar_pixels.get_pixel(x, y);
            if before == after {
                continue;
            }
            if x >= edge_start {
                edge_changes += 1;
                let near = |value: u8, target: u8| (value as i16 - target as i16).abs() < 72;
                if near(after[0], foreground.r)
                    && near(after[1], foreground.g)
                    && near(after[2], foreground.b)
                {
                    foreground_leaning += 1;
                }
            } else {
                outside_changes += 1;
            }
        }
    }
    assert!(
        edge_changes > 250,
        "scrollbar changed only {edge_changes} edge pixels"
    );
    assert!(
        foreground_leaning > 100,
        "scrollbar lacks a visible theme-foreground thumb"
    );
    assert_eq!(
        outside_changes, 0,
        "scrollbar paint escaped the right-edge overlay zone"
    );

    if let Some(output) = std::env::var_os("KETTLE_SCROLLBAR_SCREENSHOT") {
        std::fs::copy(scrollbar.path(), output).unwrap();
    }
}
