//! The scrollbar the pointer grabs and AccessKit announces is the one the
//! renderer paints: the same thumb, the same full-height channel, and a hit
//! strip that still runs to the pane's edge.

use super::accessibility::scrollback_bar_accessibility_node;
use super::{pane_scrollbar, production_source, scrollbar_drag_target, scrollbar_hit_contains};
use crate::mux::Mux;
use crate::window_state::WindowState;
use kettle_config::{Config, ScrollbarMode};

const PANE: u64 = 7;
/// The 6.0 comparison's window: 51 rows and 10,000 lines of history over a
/// 920 px terminal.
const ROWS: usize = 51;
const HISTORY: usize = 10_000;
const SCROLLED_BACK: usize = 360;

fn terminal(scale: f32) -> (f32, f32, f32, f32) {
    (40.0 * scale, 30.0 * scale, 800.0 * scale, 920.0 * scale)
}

/// Rest, hovered and scrolled back: the pane's scrollbar is the renderer's
/// for the same pointer state, its 40 logical px thumb is where a press
/// takes hold without moving the view, and a press beside it moves the
/// view. The hit strip covers the drawn bar and reaches the pane's edge.
#[test]
fn pointer_grabs_the_thumb_the_painter_draws() {
    let cfg = Config::default();
    let mut ws = WindowState::new(1, Mux::new());
    for scale in [1.0, 2.0] {
        let terminal = terminal(scale);
        let (x, y, w, h) = terminal;
        for (hover, off) in [(None, 0), (Some(PANE), 0), (None, SCROLLED_BACK)] {
            ws.scrollbar_hover = hover;
            let context = format!("scale {scale} hover {hover:?} offset {off}");
            let bar = pane_scrollbar(&cfg, &ws, PANE, terminal, scale, (ROWS, HISTORY, off));
            let painted = kettle_render::pane_scrollbar_geometry(
                terminal,
                scale,
                &cfg,
                ROWS,
                HISTORY,
                off,
                hover == Some(PANE),
            );
            assert_eq!(bar, painted, "{context}");
            let (_, top, _, height) = bar.thumb_rect.expect("a thumb");
            assert_eq!(height, 40.0 * scale, "{context}");
            for py in [top, top + height / 2.0, top + height] {
                let (grab, target) =
                    scrollbar_drag_target(&bar, terminal, py, HISTORY, None).expect("a thumb");
                assert_eq!(target, off, "{context}: a press at {py} on the thumb");
                assert!((grab - (py - top)).abs() < 1e-3, "{context}");
            }
            let (_, beside) =
                scrollbar_drag_target(&bar, terminal, top - 4.0 * scale, HISTORY, None).unwrap();
            assert!(
                beside > off,
                "{context}: a press above the thumb scrolls back"
            );
            let (tx, _, tw, _) = bar.track_rect;
            for px in [tx, tx + tw, x + w] {
                assert!(
                    scrollbar_hit_contains(bar.hit_rect, px, y + h / 2.0),
                    "{context}: {px} is on the strip"
                );
            }
        }
    }
}

/// Hovering a pane that is not focused widens only that pane's bar; a drag
/// widens the focused pane's.
#[test]
fn pointer_state_widens_the_bar_the_renderer_widens() {
    let cfg = Config::default();
    let mut ws = WindowState::new(1, Mux::new());
    let lines = (ROWS, HISTORY, 0);
    let width = |ws: &WindowState, pane| pane_scrollbar(&cfg, ws, pane, terminal(1.0), 1.0, lines);
    assert_eq!(width(&ws, PANE).track_rect.2, 6.0);
    ws.scrollbar_hover = Some(PANE);
    assert_eq!(width(&ws, PANE).track_rect.2, 10.0);
    assert_eq!(width(&ws, PANE + 1).track_rect.2, 6.0);
    ws.scrollbar_hover = None;
    // No pane is focused in an empty mux, so a drag widens none.
    ws.scrollbar_drag_offset = Some(4.0);
    assert_eq!(width(&ws, PANE).track_rect.2, 6.0);
}

/// The ScrollBar node covers the painted channel, full height and as wide as
/// the bar, at rest, hovered and scrolled back, at 1x and 2x; with no
/// history it still marks where the bar would be.
#[test]
fn accessibility_scrollbar_bounds_match_paint() {
    let tr = kettle_i18n::Translator::default();
    for mode in [ScrollbarMode::Auto, ScrollbarMode::Always] {
        let cfg = Config {
            scrollbar: mode,
            ..Config::default()
        };
        let mut ws = WindowState::new(1, Mux::new());
        for scale in [1.0, 2.0] {
            let terminal = terminal(scale);
            for (hover, hist, off) in [
                (None, HISTORY, 0),
                (Some(PANE), HISTORY, 0),
                (None, HISTORY, SCROLLED_BACK),
                (None, 0, 0),
            ] {
                ws.scrollbar_hover = hover;
                let bar = pane_scrollbar(&cfg, &ws, PANE, terminal, scale, (ROWS, hist, off));
                let node = scrollback_bar_accessibility_node(hist, off, ROWS, bar.track_rect, &tr);
                let bounds = node.bounds().expect("bounds");
                let (x, y, w, h) = bar.track_rect;
                let context = format!("{mode:?} scale {scale} hover {hover:?} offset {off}");
                assert_eq!(
                    (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
                    (
                        f64::from(x),
                        f64::from(y),
                        f64::from(x + w),
                        f64::from(y + h)
                    ),
                    "{context}"
                );
                assert_eq!(h, terminal.3, "{context}: full height");
                let wide =
                    hist > 0 && (hover.is_some() || off > 0 || mode == ScrollbarMode::Always);
                assert_eq!(w, if wide { 10.0 } else { 6.0 } * scale, "{context}");
            }
        }
    }
}

/// The pointer, AccessKit and `kettle ctl ui_geometry` take the bar and its
/// command ticks from that one geometry.
#[test]
fn the_pointer_and_accessibility_read_the_painted_bar() {
    let source: String = production_source().split_whitespace().collect();
    for needle in [
        "letbar=pane_scrollbar(&self.cfg,ws,pane,terminal,scale,(rows,hist,off));",
        "scrollbar_drag_target(&bar,terminal,py,hist,grab_offset)?;",
        "scrollback_bar_accessibility_node(history,offset,screen,bar.track_rect,&self.ui_text,)",
        "letbar=super::pointer::pane_scrollbar(&self.cfg,target,*id,partition.terminal,scale,lines,);",
    ] {
        assert!(source.contains(needle), "missing: {needle}");
    }
    assert_eq!(
        source
            .matches("letcolumn=(bar.track_rect.0,bar.track_rect.2);")
            .count(),
        2,
        "AccessKit and ui_geometry lay ticks across the bar"
    );
    assert!(!source.contains("scrollbar_width*scale).max(1.0)"));
}
