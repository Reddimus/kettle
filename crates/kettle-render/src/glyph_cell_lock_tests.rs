use super::{cell_locked_pen_x, glyph_grid_col};

/// `char index == grid column` because `build_pane` writes one char per cell
/// (wide-char spacer included). The cluster→column map must resolve each
/// glyph's start byte to its char's column, including a multi-byte char's
/// interior byte and the spacer that follows a wide glyph.
#[test]
fn glyph_grid_col_maps_cluster_byte_to_column() {
    // Row text "a你 b": 'a'@0 (1B), '你'@1 (3B), spacer ' '@4 (1B), 'b'@5.
    // One char per cell ⇒ a=col0, 你=col1, spacer=col2, b=col3.
    let starts = [0u32, 1, 4, 5];
    assert_eq!(glyph_grid_col(&starts, 0), 0, "'a' → col 0");
    assert_eq!(glyph_grid_col(&starts, 1), 1, "'你' → col 1");
    assert_eq!(glyph_grid_col(&starts, 4), 2, "spacer → col 2");
    assert_eq!(
        glyph_grid_col(&starts, 5),
        3,
        "'b' → col 3 (past the wide cell)"
    );
    // A cluster byte inside the multi-byte char still maps to its column.
    assert_eq!(
        glyph_grid_col(&starts, 3),
        1,
        "interior byte of '你' → col 1"
    );
    // Defensive: a cluster that starts before the first char clamps to
    // column 0 rather than underflowing.
    assert_eq!(glyph_grid_col(&[2, 3], 0), 0);
}

/// The pen is pinned to the cell and snapped to an integer pixel; a
/// primary-face glyph (x_offset 0) lands exactly on `cell_left`, and a
/// combining mark's offset is preserved before snapping.
#[test]
fn cell_locked_pen_snaps_to_the_cell() {
    assert_eq!(
        cell_locked_pen_x(40.0, 0.0),
        40.0,
        "integer cell_left is unchanged"
    );
    assert_eq!(cell_locked_pen_x(40.4, 0.0), 40.0, "snaps down");
    assert_eq!(cell_locked_pen_x(40.6, 0.0), 41.0, "snaps up");
    assert_eq!(
        cell_locked_pen_x(40.0, 2.0),
        42.0,
        "intra-cluster x_offset kept"
    );
    // The whole point: column N pins to N*cw regardless of upstream drift.
    let cw = 8.4_f32;
    for col in 0..80u32 {
        let cell_left = 10.0 + col as f32 * cw;
        assert_eq!(
            cell_locked_pen_x(cell_left, 0.0),
            cell_left.round(),
            "column {col} pen must be its own cell, never drifted by neighbors"
        );
    }
}

/// Wiring drift guards: the grid path must NOT push a pane TextArea (that
/// would double-draw via glyphon), the cell-locked draw must sit in the
/// shared scene pass, and the emit must be gated on grid mode.
#[test]
fn grid_path_wiring_is_present() {
    let src = super::production_source();
    assert!(
        src.contains("if cfg.text_renderer == TextRendererMode::Legacy {"),
        "pane TextArea must be pushed to glyphon ONLY in legacy mode"
    );
    assert!(
        src.contains("let target_size = [self.config.width.max(1), self.config.height.max(1)];")
            && src.contains("fn encode_scene_pass(")
            && src.contains("self.glyph_pipeline")
            && src.contains(".draw(&mut pass, &self.glyph_clips, target_size);"),
        "the cell-locked glyph pipeline must draw against the live target size"
    );
    assert!(
        src.contains("if cfg.text_renderer == TextRendererMode::Grid {")
            && src.contains("self.emit_pane_glyphs("),
        "glyph emission must be gated on grid mode"
    );
    assert!(
        src.contains("buf.set_wrap(Wrap::None);"),
        "pane buffers must be Wrap::None so a row is one run + char==column holds"
    );
}

/// Cursor blink may force the separate cursor-glyph prepare, but it must not
/// be part of the pane grid upload gate. Otherwise blink can clear/stale-draw
/// ordinary prompt glyphs. Pane grid uploads are allowed only for text
/// content damage or layout/style damage.
#[test]
fn grid_upload_damage_excludes_cursor_blink() {
    let src = super::production_source();
    assert!(
        src.contains("last_text_layout_key: Option<u64>"),
        "renderer must keep a layout damage key for cached text/grid vertices"
    );
    assert!(
        src.contains("grid_glyphs_dirty: bool"),
        "renderer must force a grid upload after clearing the glyph pipeline"
    );
    assert!(
            src.contains("let grid_upload_needed =")
                && src.contains(
                    "self.grid_glyphs_dirty || any_pane_text_changed || text_layout_changed",
                ),
            "grid glyph upload must be gated by forced dirtiness, pane text, or layout damage"
        );
    assert!(
        src.contains("self.glyph_pipeline.clear();")
            && src.contains("self.grid_glyphs_dirty = true;"),
        "clearing the grid glyph pipeline must dirty the next upload"
    );
    let gate = src
        .split("let grid_upload_needed =")
        .nth(1)
        .and_then(|s| s.split("if let Some((gx, gy, gcolor,").next())
        .expect("grid upload block present before cursor-glyph prepare");
    assert!(
        !gate.contains("cursor_char_changed") && !gate.contains("cursor_visible"),
        "grid upload gate/block must not depend on cursor blink state"
    );
    // Nor may any upload after it: a blink edge must write no GPU buffer.
    let uploads = src
        .split("let grid_upload_needed =")
        .nth(1)
        .and_then(|s| s.split("let target_size = [").next())
        .expect("the frame's upload block");
    assert!(
        uploads.contains("self.menu_quads") && uploads.contains("self.starfield.upload("),
        "the block runs through every pipeline's upload"
    );
    for blink in ["cursor_visible", "cursor_on", "blink"] {
        assert!(
            !uploads.contains(blink),
            "the frame's uploads must not depend on the blink phase ({blink})"
        );
    }
}

/// The cell-locked emit loop must live in ONE free function,
/// `emit_cell_locked_glyphs`, called from both production sites (live panes
/// and the screenshot path). Hand-copied loops could silently drift so the
/// README imagery no longer matches the live renderer.
#[test]
fn cell_lock_emit_is_a_single_shared_fn() {
    let src = super::production_source();
    assert!(
        src.contains("fn emit_cell_locked_glyphs("),
        "the shared cell-locked emit function must exist"
    );
    // Exactly one definition plus the two production calls. The GPU blink
    // fixtures are deliberately absent from `production_source`.
    let calls = src.matches("emit_cell_locked_glyphs(").count();
    assert_eq!(
        calls, 3,
        "emit_cell_locked_glyphs must be the single emit shared by \
             emit_pane_glyphs and the screenshot path (definition + 2 calls); \
             found {calls} occurrences"
    );
}

/// Offscreen captures honor `cfg.text_renderer`: Grid cell-locks pane text;
/// Legacy uses glyphon. Annotation and menu chrome use glyphon in both modes.
#[test]
fn screenshot_routes_pane_text_by_renderer_mode() {
    let src = super::production_source();
    // The capture path reads the renderer mode.
    assert!(
        src.contains("let grid = cfg.text_renderer == TextRendererMode::Grid;"),
        "capture_png path must branch on the configured text-renderer"
    );
    // Pane TextAreas only go to glyphon in Legacy mode.
    assert!(
        src.contains("if !grid {"),
        "pane body TextAreas must be glyphon-only in Legacy (`if !grid`) mode"
    );
    // Grid mode builds a GlyphPipeline and draws it in the pass.
    assert!(
        src.contains("let mut grid_glyphs =") && src.contains("GlyphPipeline::new_with_budget("),
        "Grid screenshot path must build a GlyphPipeline"
    );
    assert!(
        src.contains("grid_glyphs.draw(&mut pass, &grid_clips, [w, h]);"),
        "Grid screenshot path must draw the cell-locked pane glyphs in the pass"
    );
    // The annotation buffer (chrome) still goes through glyphon in both modes.
    assert!(
        src.contains("buffer: &annotate_buf,"),
        "the annotation chrome must still render via glyphon"
    );
}

#[test]
fn gpu_recovery_escalates_to_software() {
    assert_eq!(
        super::escalation_for_attempt(0),
        super::AdapterEscalation::AlternateBackend
    );
    assert_eq!(
        super::escalation_for_attempt(1),
        super::AdapterEscalation::SurfacePreferred
    );
    assert_eq!(
        super::escalation_for_attempt(2),
        super::AdapterEscalation::AnyHardware
    );
    assert_eq!(
        super::escalation_for_attempt(3),
        super::AdapterEscalation::ForceSoftware
    );
    assert_eq!(
        super::escalation_for_attempt(99),
        super::AdapterEscalation::ForceSoftware
    );
}

#[test]
fn gpu_fault_latch_keeps_first_error_and_bounds_message() {
    let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fault = std::sync::Arc::new(std::sync::Mutex::new(None));
    let long = format!("first\n{}", "x".repeat(3000));

    super::latch_gpu_fault(&lost, &fault, "device_lost", long);
    super::latch_gpu_fault(&lost, &fault, "internal", "second".to_string());

    assert!(lost.load(std::sync::atomic::Ordering::Acquire));
    let actual = fault.lock().unwrap().clone().expect("fault latched");
    assert_eq!(actual.kind, "device_lost");
    assert!(!actual.message.contains('\n'));
    assert_eq!(actual.message.chars().count(), 2048);
    assert!(!actual.message.contains("second"));
}

#[test]
fn terminal_cursor_is_suppressed_while_window_is_unfocused() {
    assert!(super::cursor_focus_gate(true, true));
    assert!(!super::cursor_focus_gate(false, true));
    assert!(!super::cursor_focus_gate(true, false));
    assert!(!super::cursor_focus_gate(false, false));
}
