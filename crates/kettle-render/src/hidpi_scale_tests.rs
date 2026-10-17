use super::{measure_cell, metrics_for, pane_metrics};
use glyphon::{Buffer as TextBuffer, FontSystem};

/// The pane text buffer must advance lines by the grid's `cell_h`
/// (which includes the `cfg.cell_height` multiplier) so the cursor and
/// selection/vi quads — which step by `cell_h` per row — stay locked to the
/// text. Laying out at the unscaled `metrics.line_height` drifts a fraction
/// of a row per line, a full row off near the bottom when cell_height != 1.
#[test]
fn pane_metrics_line_height_tracks_cell_h_not_font_line_height() {
    let base = metrics_for(16.0, 1.0); // line_height = 20.0
    let cell_h = base.line_height * 1.4; // e.g. cfg.cell_height = 1.4
    let pm = pane_metrics(base.font_size, cell_h);
    assert_eq!(pm.line_height, cell_h, "text line step must equal cell_h");
    assert_eq!(
        pm.font_size, base.font_size,
        "glyph size must NOT be scaled by cell_height"
    );
    // At cell_height == 1.0 the pane line height equals the base metric.
    let pm1 = pane_metrics(base.font_size, base.line_height);
    assert_eq!(pm1.line_height, base.line_height);
}

/// Core invariant: a logical font size renders at
/// `font_size × scale` physical pixels. If the metrics ignore `scale`, a
/// 13pt font draws at ~6.5px on a 200%-scaled (2×) Windows 11 display.
#[test]
fn metrics_scale_with_dpi_factor() {
    // 1× display: physical == logical.
    let m1 = metrics_for(13.0, 1.0);
    assert!((m1.font_size - 13.0).abs() < f32::EPSILON);
    assert!((m1.line_height - 13.0 * 1.25).abs() < f32::EPSILON);
    // 2× (200% Windows scaling / Retina): physical is doubled.
    let m2 = metrics_for(13.0, 2.0);
    assert!((m2.font_size - 26.0).abs() < f32::EPSILON);
    assert!((m2.line_height - 26.0 * 1.25).abs() < f32::EPSILON);
    // 1.5× (150%, common Surface scaling).
    let m15 = metrics_for(20.0, 1.5);
    assert!((m15.font_size - 30.0).abs() < f32::EPSILON);
}

/// A bogus scale (0, negative, NaN, inf) must not zero or NaN the cell —
/// it falls back to 1× rather than producing degenerate metrics.
#[test]
fn metrics_sanitize_bad_scale() {
    for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
        let m = metrics_for(13.0, bad);
        assert!((m.font_size - 13.0).abs() < f32::EPSILON, "scale {bad}");
    }
}

/// End-to-end: the measured cell box scales (≈) with the DPI factor, so
/// the grid (cols×rows from physical window size ÷ physical cell) stays
/// consistent. Uses the embedded font — no GPU required.
#[test]
fn measured_cell_doubles_at_2x() {
    let mut fs = FontSystem::new();
    for face in kettle_config::font::all() {
        crate::load_bundled_font(&mut fs, face);
    }
    let fam = "JetBrains Mono";
    let m1 = metrics_for(16.0, 1.0);
    let mut b1 = TextBuffer::new(&mut fs, m1);
    let (w1, h1) = measure_cell(&mut fs, &mut b1, fam, m1);
    let m2 = metrics_for(16.0, 2.0);
    let mut b2 = TextBuffer::new(&mut fs, m2);
    let (w2, h2) = measure_cell(&mut fs, &mut b2, fam, m2);
    // Allow a little slack for hinting/rounding, but it must be ~2×, not 1×.
    assert!(
        (w2 / w1 - 2.0).abs() < 0.15,
        "cell width should ≈ double at 2× scale: {w1} → {w2}"
    );
    assert!(
        (h2 / h1 - 2.0).abs() < 0.15,
        "cell height should ≈ double at 2× scale: {h1} → {h2}"
    );
}

/// At a large font on a high-DPI display the 10-glyph
/// measure probe is ~1300px wide (72pt×3). A fixed 1000px measure box
/// wraps it, so `cell_w` comes out too narrow and mis-grids the terminal.
/// With the metrics-relative box, `cell_w` must scale linearly.
#[test]
fn measured_cell_does_not_wrap_at_large_font_highdpi() {
    let mut fs = FontSystem::new();
    for face in kettle_config::font::all() {
        crate::load_bundled_font(&mut fs, face);
    }
    let fam = "JetBrains Mono";
    let m1 = metrics_for(72.0, 1.0);
    let mut b1 = TextBuffer::new(&mut fs, m1);
    let (w1, _) = measure_cell(&mut fs, &mut b1, fam, m1);
    // 72pt × 3 = 216px physical; the ~1300px probe would wrap a fixed
    // 1000px box. Width must still scale ~3×.
    let m3 = metrics_for(72.0, 3.0);
    let mut b3 = TextBuffer::new(&mut fs, m3);
    let (w3, _) = measure_cell(&mut fs, &mut b3, fam, m3);
    assert!(
        (w3 / w1 - 3.0).abs() < 0.15,
        "cell width must scale ~3× without wrapping: {w1} → {w3}"
    );
}
