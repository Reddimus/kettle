//! Visual regression test for the right-click context menu.
//!
//! v1.3.0 and v1.3.1 shipped a *blank* context menu — the panel bg
//! quad was drawn AFTER the menu text in the same render pass, so the
//! opaque bg painted right on top of the just-rendered text. The bug
//! survived two releases because nothing in CI exercised the menu's
//! render path: `--screenshot` rendered a no-overlay representative
//! frame, and the unit tests in `kettle-ui::app` only pinned the
//! menu's *behavior* (highlight stepping, anchor clamping, hit-test
//! geometry), not its appearance.
//!
//! This test renders the menu via the new
//! `capture_png_with(.., DebugScene::ContextMenu)` headless path
//! and asserts two invariants:
//!
//! 1. The menu PNG differs from the no-menu PNG by enough pixels that
//!    it's clearly drawing *something* (a blank-menu regression
//!    matches the no-menu baseline pixel-for-pixel inside the panel
//!    rect, because the panel bg color equals the pane bg color).
//! 2. Inside the menu area, enough pixels approximately match
//!    `theme.foreground` to be real label glyphs — not just chrome.
//!
//! Two invariants in one test (rather than two separate tests) so we
//! only spin up one pair of wgpu adapters per `cargo test` run; with
//! the offscreen software-Vulkan adapter, four-devices-concurrent
//! has segfaulted in the past on shared CI runners.

use kettle_config::Config;
use kettle_render::{DebugScene, capture_png_with};
use tempfile::Builder;

#[test]
fn context_menu_renders_visibly_with_text() {
    // Parallels' Windows ARM WDDM adapter faults during a headless wgpu device
    // request. WARP still exercises the complete screenshot pipeline and exact
    // pixels; physical Windows machines and the live renderer retain
    // hardware-first coverage.
    let cfg = Config {
        gpu_force_software: cfg!(all(target_os = "windows", target_arch = "aarch64")),
        ..Config::default()
    };
    // `.png` suffix matters — kettle-render's `capture_png_with`
    // hands the path to `image::save`, which dispatches on extension.
    let default_tmp = Builder::new()
        .suffix(".png")
        .tempfile()
        .expect("temp file for default render");
    let menu_tmp = Builder::new()
        .suffix(".png")
        .tempfile()
        .expect("temp file for menu render");
    capture_png_with(&cfg, 96, 28, default_tmp.path(), DebugScene::Default)
        .expect("default screenshot");
    capture_png_with(&cfg, 96, 28, menu_tmp.path(), DebugScene::ContextMenu)
        .expect("context-menu screenshot");
    let default_px = image::open(default_tmp.path())
        .expect("open default PNG")
        .to_rgba8();
    let menu_px = image::open(menu_tmp.path())
        .expect("open menu PNG")
        .to_rgba8();
    assert_eq!(
        (default_px.width(), default_px.height()),
        (menu_px.width(), menu_px.height()),
        "both renders should produce the same PNG dimensions"
    );

    // --- Invariant 1: the menu render visibly differs from the no-
    // menu baseline.
    //
    // v1.3.0/v1.3.1 regression: the menu rendered a fully-opaque
    // panel bg over its own text in a post-text quad pass. The
    // resulting PNG was byte-identical to the no-menu baseline
    // inside the panel area (because the panel bg color equals the
    // pane bg color — `theme.background` opaque on both).
    let mut diff_count = 0u64;
    for (a, b) in default_px.pixels().zip(menu_px.pixels()) {
        if a != b {
            diff_count += 1;
        }
    }
    // Floor at 1000 pixels — well below the ~5000+ pixels a real
    // menu render adds (panel ≈ 180×190 = 34_200 px; the shadow +
    // border + 6 label rows + 2 separators each contribute hundreds
    // more), but well above the 0 a blank menu would produce.
    assert!(
        diff_count >= 1000,
        "context menu render differed from default by only {diff_count} pixels — \
         likely the v1.3.0/v1.3.1 blank-menu regression (panel paints over its own \
         text). Expected ≥ 1000."
    );

    // --- Invariant 2: the menu text glyphs are actually visible.
    //
    // Scan the lower-left quadrant (where the synthetic menu lives)
    // and count pixels approximately matching the theme foreground
    // that did NOT match before — i.e. they're newly drawn by the
    // menu and roughly the right color to be label text.
    let theme = cfg.theme;
    let fg = (theme.foreground.r, theme.foreground.g, theme.foreground.b);
    let (w, h) = (menu_px.width(), menu_px.height());
    let mut fg_count_in_menu_area = 0u64;
    let x_hi = w / 2;
    let y_lo = (h / 4).min(h);
    let y_hi = (3 * h / 4).min(h);
    for y in y_lo..y_hi {
        for x in 0..x_hi {
            let a = default_px.get_pixel(x, y);
            let b = menu_px.get_pixel(x, y);
            if a == b {
                continue;
            }
            // Generous sRGB tolerance — anti-aliased glyph strokes
            // produce partial-coverage pixels that lean toward but
            // don't exactly match the source color.
            let near = |c, target: u8| (c as i32 - target as i32).abs() < 32;
            if near(b[0], fg.0) && near(b[1], fg.1) && near(b[2], fg.2) {
                fg_count_in_menu_area += 1;
            }
        }
    }
    // Six visible label rows of 4-11 chars each, anti-aliased glyph
    // strokes ≈ 200-400 pixels per row → 1000+ fg-leaning pixels in
    // the menu area is the realistic floor. Pick 200 as a generous
    // lower bound that still rules out a fully-blank menu (0 fg).
    assert!(
        fg_count_in_menu_area >= 200,
        "found only {fg_count_in_menu_area} foreground-leaning pixels in the \
         menu area — likely the blank-menu regression. Expected ≥ 200."
    );
}
