//! A pane's share of its split leaf: the terminal, the pane's titlebar, and
//! the preview lane carved beside them. One computation feeds everything
//! that needs a pane's geometry (PTY sizing, the renderer, pointer and IME
//! input, accessibility, automation), so none of them can disagree about
//! where the terminal is.
//!
//! The leaf is the split tree's allocation. The titlebar takes a strip at the
//! top or bottom of it; the lane takes the bottom or right of what is left,
//! in whole terminal cells, and the terminal keeps the rest with its padding
//! inside. A lane never takes the terminal below its floor: when it cannot
//! fit expanded it shrinks to a one-cell strip, and when even that would cost
//! the floor it is only a badge in the pane's chrome, leaving the terminal
//! as it was. Only the user opens a lane; media an agent sends waits on
//! the pane's shelf.

use crate::mux::Rect;

/// The side of its pane a lane sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneSide {
    Bottom,
    Right,
}

/// A lane a pane asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaneRequest {
    pub(crate) side: LaneSide,
    /// The share of the pane's body the lane wants when expanded, kept
    /// within [`MIN_FRACTION`] and [`MAX_FRACTION`].
    fraction: f32,
    /// `false` while collapsed: the lane wants only its strip.
    pub(crate) expanded: bool,
}

/// The share a lane takes when nothing else is asked.
pub(crate) const DEFAULT_FRACTION: f32 = 0.4;
/// The least and most of its pane's body a lane may ask for.
pub(crate) const MIN_FRACTION: f32 = 0.1;
pub(crate) const MAX_FRACTION: f32 = 0.9;

impl LaneRequest {
    /// A request for `fraction` of the pane, held within the allowed range;
    /// a fraction that is not a number asks for the default.
    pub(crate) fn new(side: LaneSide, fraction: f32, expanded: bool) -> Self {
        let fraction = if fraction.is_finite() {
            fraction.clamp(MIN_FRACTION, MAX_FRACTION)
        } else {
            DEFAULT_FRACTION
        };
        Self {
            side,
            fraction,
            expanded,
        }
    }

    #[cfg(test)]
    pub(crate) fn fraction(self) -> f32 {
        self.fraction
    }
}

/// The fixed sizes a partition is made from, checked once so the partition
/// itself cannot fail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    cell: (f32, f32),
    padding: (f32, f32),
    titlebar: f32,
    title_at_bottom: bool,
}

impl Metrics {
    /// Metrics for cells of `cell` pixels, terminal `padding` on each side,
    /// and a titlebar strip `titlebar` pixels tall (zero for none) at the top
    /// or bottom. `None` unless everything is finite, the cell positive and
    /// the titlebar not negative. Padding may be negative, as the
    /// configuration allows.
    pub(crate) fn new(
        cell: (f32, f32),
        padding: (f32, f32),
        titlebar: f32,
        title_at_bottom: bool,
    ) -> Option<Self> {
        let positive = |value: f32| value.is_finite() && value > 0.0;
        (positive(cell.0)
            && positive(cell.1)
            && padding.0.is_finite()
            && padding.1.is_finite()
            && titlebar.is_finite()
            && titlebar >= 0.0)
            .then_some(Self {
                cell,
                padding,
                titlebar,
                title_at_bottom,
            })
    }

    #[cfg(test)]
    pub(crate) fn titlebar(self) -> f32 {
        self.titlebar
    }

    #[cfg(test)]
    pub(crate) fn title_at_bottom(self) -> bool {
        self.title_at_bottom
    }

    /// The pixels `cells` whole cells and the padding on both sides take
    /// along the vertical (`rows`) or horizontal axis.
    fn extent(self, cells: f32, vertical: bool) -> f32 {
        if vertical {
            cells * self.cell.1 + 2.0 * self.padding.1
        } else {
            cells * self.cell.0 + 2.0 * self.padding.0
        }
    }

    /// The whole cells that fit in `pixels` along an axis, after padding.
    fn cells_in(self, pixels: f32, vertical: bool) -> f32 {
        let (cell, padding) = if vertical {
            (self.cell.1, self.padding.1)
        } else {
            (self.cell.0, self.padding.0)
        };
        ((pixels - 2.0 * padding) / cell).floor().max(0.0)
    }
}

/// How a window measures its panes: its cells and padding, and the titlebar
/// a pane shows while its tab shows more than one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayoutStyle {
    shown: Metrics,
}

impl LayoutStyle {
    /// A style for cells of `cell` pixels and terminal `padding`, whose panes
    /// show a titlebar `titlebar` pixels tall while their tab shows several
    /// (`None` when titlebars are off). `None` for numbers [`Metrics::new`]
    /// refuses.
    pub(crate) fn new(
        cell: (f32, f32),
        padding: (f32, f32),
        titlebar: Option<f32>,
        title_at_bottom: bool,
    ) -> Option<Self> {
        Metrics::new(cell, padding, titlebar.unwrap_or(0.0), title_at_bottom)
            .map(|shown| Self { shown })
    }

    /// Cells of 8 by 16 pixels, no padding and no titlebar: the style a
    /// window falls back to when its own numbers are refused.
    pub(crate) fn fallback() -> Self {
        Self {
            shown: Metrics {
                cell: (8.0, 16.0),
                padding: (0.0, 0.0),
                titlebar: 0.0,
                title_at_bottom: false,
            },
        }
    }

    /// The metrics for a tab showing `panes` panes: a lone pane has no
    /// titlebar.
    pub(crate) fn metrics(self, panes: usize) -> Metrics {
        if panes > 1 {
            self.shown
        } else {
            Metrics {
                titlebar: 0.0,
                ..self.shown
            }
        }
    }
}

/// The terminal a lane leaves at least: columns and rows.
pub(crate) const FLOOR: (f32, f32) = (20.0, 5.0);
/// The least an expanded lane takes, in cells: columns on the right, room
/// for its header's title and controls, and rows at the bottom, room for its
/// header and a little content below it.
pub(crate) const LANE_MIN_CELLS: (f32, f32) = (16.0, 4.0);

/// How a pane's lane shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LaneShare {
    /// The lane in full.
    Expanded(Rect),
    /// Collapsed, or with no room to expand: a strip one row tall along the
    /// bottom of the pane's body, whichever side the lane opened on, so its
    /// header always has the pane's width for its controls.
    Strip(Rect),
    /// No room even for the strip: a badge in the pane's chrome, the
    /// terminal unchanged.
    Badge,
}

impl LaneShare {
    /// The pixels the lane takes from the pane, if any.
    pub(crate) fn rect(self) -> Option<Rect> {
        match self {
            Self::Expanded(rect) | Self::Strip(rect) => Some(rect),
            Self::Badge => None,
        }
    }
}

/// A pane's share of its leaf.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanePartition {
    /// The split tree's allocation, which everything else tiles.
    pub(crate) leaf: Rect,
    /// The titlebar strip, when the pane shows one.
    pub(crate) titlebar: Option<Rect>,
    /// The terminal, its padding inside.
    pub(crate) terminal: Rect,
    /// The lane, when the pane has one.
    pub(crate) lane: Option<LaneShare>,
}

/// Divide `leaf` between its titlebar, terminal and the lane `request` asks
/// for. Never fails: a leaf too small for anything still gets a terminal,
/// at least zero by zero, and no lane.
pub(crate) fn partition_leaf(
    leaf: Rect,
    metrics: Metrics,
    request: Option<LaneRequest>,
) -> PanePartition {
    let leaf = finite_rect(leaf);
    let (x, y, w, h) = leaf;
    let titlebar_h = metrics.titlebar.min(h);
    let (titlebar, body) = if titlebar_h <= 0.0 {
        (None, leaf)
    } else if metrics.title_at_bottom {
        (
            Some((x, y + h - titlebar_h, w, titlebar_h)),
            (x, y, w, h - titlebar_h),
        )
    } else {
        (
            Some((x, y, w, titlebar_h)),
            (x, y + titlebar_h, w, h - titlebar_h),
        )
    };
    let Some(request) = request else {
        return PanePartition {
            leaf,
            titlebar,
            terminal: body,
            lane: None,
        };
    };
    let (terminal, lane) = carve(body, metrics, request);
    PanePartition {
        leaf,
        titlebar,
        terminal,
        lane: Some(lane),
    }
}

/// A rectangle with every number finite and its size not negative, so the
/// arithmetic below stays in range.
fn finite_rect((x, y, w, h): Rect) -> Rect {
    let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
    (finite(x), finite(y), finite(w).max(0.0), finite(h).max(0.0))
}

/// Split `body` between the terminal and the lane `request` asks for.
fn carve(body: Rect, metrics: Metrics, request: LaneRequest) -> (Rect, LaneShare) {
    let (x, y, w, h) = body;
    let vertical = request.side == LaneSide::Bottom;
    let along = if vertical { h } else { w };
    // The least the terminal keeps along the lane's axis.
    let floor = metrics.extent(if vertical { FLOOR.1 } else { FLOOR.0 }, vertical);
    let lane_min = if vertical {
        LANE_MIN_CELLS.1 * metrics.cell.1
    } else {
        LANE_MIN_CELLS.0 * metrics.cell.0
    };
    // The terminal keeps whole cells; the lane takes the pixels after them.
    let terminal_for = |lane: f32| -> f32 {
        let cells = metrics.cells_in(along - lane, vertical);
        metrics.extent(cells, vertical).min(along)
    };
    let split = |terminal: f32| -> (Rect, Rect) {
        let lane = along - terminal;
        if vertical {
            ((x, y, w, terminal), (x, y + terminal, w, lane))
        } else {
            ((x, y, terminal, h), (x + terminal, y, lane, h))
        }
    };
    if request.expanded {
        let wanted = (along * request.fraction).max(lane_min);
        let terminal = terminal_for(wanted);
        if terminal >= floor && along - terminal >= lane_min {
            let (terminal, lane) = split(terminal);
            return (terminal, LaneShare::Expanded(lane));
        }
    }
    // The strip is one row along the bottom, whatever the side, and the
    // terminal above it keeps its whole rows and every column.
    let row = metrics.cell.1;
    let above = metrics.extent(metrics.cells_in(h - row, true), true).min(h);
    if above >= metrics.extent(FLOOR.1, true) && h - above >= row {
        return (
            (x, y, w, above),
            LaneShare::Strip((x, y + above, w, h - above)),
        );
    }
    (body, LaneShare::Badge)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lone pane has no titlebar; two or more show the style's.
    #[test]
    fn a_titlebar_shows_only_beside_another_pane() {
        let style = LayoutStyle::new((9.0, 18.0), (4.0, 2.0), Some(24.0), true).unwrap();
        assert_eq!(style.metrics(1).titlebar(), 0.0);
        assert_eq!(style.metrics(2).titlebar(), 24.0);
        assert!(style.metrics(2).title_at_bottom());
        let off = LayoutStyle::new((9.0, 18.0), (4.0, 2.0), None, false).unwrap();
        assert_eq!(off.metrics(3).titlebar(), 0.0);
        assert_eq!(LayoutStyle::new((0.0, 18.0), (0.0, 0.0), None, false), None);
    }

    fn metrics(titlebar: f32, at_bottom: bool) -> Metrics {
        Metrics::new((9.0, 18.0), (4.0, 2.0), titlebar, at_bottom).unwrap()
    }

    fn area((_, _, w, h): Rect) -> f64 {
        f64::from(w) * f64::from(h)
    }

    fn contains(outer: Rect, inner: Rect) -> bool {
        let eps = 1e-3;
        inner.0 >= outer.0 - eps
            && inner.1 >= outer.1 - eps
            && inner.0 + inner.2 <= outer.0 + outer.2 + eps
            && inner.1 + inner.3 <= outer.1 + outer.3 + eps
    }

    fn overlap(a: Rect, b: Rect) -> f64 {
        let w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
        let h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
        if w > 0.0 && h > 0.0 {
            f64::from(w) * f64::from(h)
        } else {
            0.0
        }
    }

    /// Every part lies in the leaf, no two overlap, and together they cover
    /// it: the terminal, titlebar and lane tile the leaf exactly, for
    /// fractional origins and cells, both titlebar positions, both sides,
    /// expanded and collapsed, and sizes around every floor.
    #[test]
    fn partitions_tile_the_leaf_without_overlap() {
        let mut checked = 0;
        for &(x, y) in &[(0.0, 0.0), (13.5, 7.25)] {
            for w in [1.0, 40.0, 190.0, 191.0, 260.0, 777.5, 1500.0] {
                for h in [0.0, 30.0, 100.0, 109.0, 110.0, 290.0, 333.3, 900.0] {
                    for titlebar in [0.0, 24.0] {
                        for at_bottom in [false, true] {
                            for side in [LaneSide::Bottom, LaneSide::Right] {
                                {
                                    for expanded in [true, false] {
                                        let leaf = (x, y, w, h);
                                        let request = LaneRequest::new(side, 0.4, expanded);
                                        let p = partition_leaf(
                                            leaf,
                                            metrics(titlebar, at_bottom),
                                            Some(request),
                                        );
                                        let mut parts = vec![p.terminal];
                                        parts.extend(p.titlebar);
                                        parts.extend(p.lane.and_then(LaneShare::rect));
                                        let total: f64 = parts.iter().copied().map(area).sum();
                                        assert!(
                                            (total - area(leaf)).abs() < 1e-2,
                                            "{p:?} covers {total}, leaf {}",
                                            area(leaf)
                                        );
                                        for (i, a) in parts.iter().enumerate() {
                                            assert!(contains(leaf, *a), "{p:?}");
                                            for b in &parts[i + 1..] {
                                                assert!(overlap(*a, *b) < 1e-3, "{p:?}");
                                            }
                                        }
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 1000);
    }

    /// With no lane, the terminal is the leaf less its titlebar, which keeps
    /// every size the panes had before lanes existed.
    #[test]
    fn no_lane_leaves_the_terminal_as_before() {
        let leaf = (10.0, 20.0, 400.0, 300.0);
        let top = partition_leaf(leaf, metrics(24.0, false), None);
        assert_eq!(top.titlebar, Some((10.0, 20.0, 400.0, 24.0)));
        assert_eq!(top.terminal, (10.0, 44.0, 400.0, 276.0));
        assert_eq!(top.lane, None);
        let bottom = partition_leaf(leaf, metrics(24.0, true), None);
        assert_eq!(bottom.titlebar, Some((10.0, 296.0, 400.0, 24.0)));
        assert_eq!(bottom.terminal, (10.0, 20.0, 400.0, 276.0));
        let bare = partition_leaf(leaf, metrics(0.0, false), None);
        assert_eq!(bare.titlebar, None);
        assert_eq!(bare.terminal, leaf);
    }

    /// A user's lane at the bottom keeps the terminal's columns, takes about
    /// the share it asks for, and leaves the terminal whole rows.
    #[test]
    fn a_user_lane_takes_its_share_and_leaves_whole_rows() {
        let m = metrics(0.0, false);
        let leaf = (0.0, 0.0, 800.0, 600.0);
        let p = partition_leaf(leaf, m, Some(LaneRequest::new(LaneSide::Bottom, 0.4, true)));
        let LaneShare::Expanded(lane) = p.lane.unwrap() else {
            panic!("{p:?}");
        };
        assert_eq!(p.terminal.2, 800.0, "columns kept");
        let rows = (p.terminal.3 - 4.0) / 18.0;
        assert_eq!(rows, rows.floor(), "whole rows: {p:?}");
        assert!(
            lane.3 >= 0.4 * 600.0 && lane.3 < 0.4 * 600.0 + 18.0,
            "{p:?}"
        );
    }

    /// However much a lane asks for, the terminal keeps 20 columns
    /// and 5 rows; with less room the lane collapses to its strip or badge.
    #[test]
    fn a_lane_never_takes_the_terminal_below_twenty_by_five() {
        let m = metrics(0.0, false);
        for side in [LaneSide::Bottom, LaneSide::Right] {
            for size in (0..60).map(|step| step as f32 * 12.5) {
                let leaf = (0.0, 0.0, size * 1.7, size);
                let p = partition_leaf(leaf, m, Some(LaneRequest::new(side, 0.9, true)));
                if p.lane.and_then(LaneShare::rect).is_none() {
                    assert_eq!(p.terminal, leaf, "a badge changes nothing");
                    continue;
                }
                let floor = match side {
                    LaneSide::Bottom => m.extent(FLOOR.1, true) - 1e-3,
                    LaneSide::Right => m.extent(FLOOR.0, false) - 1e-3,
                };
                let kept = if side == LaneSide::Bottom {
                    p.terminal.3
                } else {
                    p.terminal.2
                };
                assert!(kept >= floor, "{side:?} {p:?}");
            }
        }
    }

    /// A collapsed lane is one cell across when the floor allows it, and a
    /// badge, the terminal untouched, when the strip would take the floor's
    /// last row.
    #[test]
    fn a_collapsed_lane_is_a_strip_or_a_badge() {
        let m = metrics(0.0, false);
        let request = LaneRequest::new(LaneSide::Bottom, 0.4, false);
        let roomy = partition_leaf((0.0, 0.0, 400.0, 400.0), m, Some(request));
        let Some(LaneShare::Strip(strip)) = roomy.lane else {
            panic!("{roomy:?}");
        };
        assert!(strip.3 >= 18.0 && strip.3 < 36.0, "{roomy:?}");
        // Five rows and padding exactly: the strip would cost the fifth row.
        let tight = (0.0, 0.0, 400.0, m.extent(FLOOR.1, true));
        let p = partition_leaf(tight, m, Some(request));
        assert_eq!(p.lane, Some(LaneShare::Badge));
        assert_eq!(p.terminal, tight);
        // One row more and the strip fits.
        let room = (0.0, 0.0, 400.0, m.extent(FLOOR.1 + 1.0, true));
        let p = partition_leaf(room, m, Some(request));
        assert!(matches!(p.lane, Some(LaneShare::Strip(_))), "{p:?}");
    }

    /// A collapsed or cramped right lane is a strip along the bottom with the
    /// pane's whole width, so its header has room for its controls, and the
    /// terminal keeps every column.
    #[test]
    fn a_collapsed_right_lane_is_a_strip_along_the_bottom() {
        let m = metrics(0.0, false);
        let leaf = (0.0, 0.0, 800.0, 400.0);
        let collapsed =
            partition_leaf(leaf, m, Some(LaneRequest::new(LaneSide::Right, 0.4, false)));
        let Some(LaneShare::Strip(strip)) = collapsed.lane else {
            panic!("{collapsed:?}");
        };
        assert_eq!((strip.0, strip.2), (0.0, 800.0), "{strip:?}");
        assert_eq!(collapsed.terminal.2, 800.0);
        assert_eq!(strip.1, collapsed.terminal.3);
        // Room for the terminal's floor but not for an expanded right lane
        // wide enough for its controls: the same strip.
        let narrow = (0.0, 0.0, m.extent(FLOOR.0 + 10.0, false), 400.0);
        let cramped = partition_leaf(
            narrow,
            m,
            Some(LaneRequest::new(LaneSide::Right, 0.1, true)),
        );
        assert!(
            matches!(cramped.lane, Some(LaneShare::Strip(_))),
            "{cramped:?}"
        );
    }

    /// A right lane keeps the terminal's rows and takes columns.
    #[test]
    fn a_right_lane_keeps_the_rows() {
        let m = metrics(24.0, false);
        let leaf = (5.0, 5.0, 1000.0, 500.0);
        let p = partition_leaf(leaf, m, Some(LaneRequest::new(LaneSide::Right, 0.3, true)));
        let LaneShare::Expanded(lane) = p.lane.unwrap() else {
            panic!("{p:?}");
        };
        assert_eq!(p.terminal.3, 476.0);
        assert_eq!(lane.1, p.terminal.1);
        assert_eq!(lane.0, p.terminal.0 + p.terminal.2);
    }

    /// Closing a lane gives back exactly what the pane had.
    #[test]
    fn closing_a_lane_restores_the_partition_exactly() {
        let m = metrics(24.0, true);
        let leaf = (3.25, 11.5, 641.0, 487.0);
        let before = partition_leaf(leaf, m, None);
        for _ in 0..3 {
            let open = partition_leaf(leaf, m, Some(LaneRequest::new(LaneSide::Bottom, 0.5, true)));
            assert_ne!(open.terminal, before.terminal);
            assert_eq!(partition_leaf(leaf, m, None), before);
        }
    }

    /// Bad numbers are refused where they come in, never reaching the
    /// partition, and a degenerate leaf keeps its pane.
    #[test]
    fn bad_numbers_are_refused_and_no_pane_is_dropped() {
        assert_eq!(Metrics::new((0.0, 18.0), (0.0, 0.0), 0.0, false), None);
        assert_eq!(Metrics::new((9.0, f32::NAN), (0.0, 0.0), 0.0, false), None);
        assert_eq!(Metrics::new((9.0, 18.0), (f32::NAN, 0.0), 0.0, false), None);
        assert_eq!(Metrics::new((9.0, 18.0), (0.0, 0.0), -1.0, false), None);
        assert!(Metrics::new((9.0, 18.0), (-1.0, 0.0), 0.0, false).is_some());
        assert_eq!(
            Metrics::new((9.0, 18.0), (0.0, 0.0), f32::INFINITY, false),
            None
        );
        assert_eq!(
            LaneRequest::new(LaneSide::Bottom, f32::NAN, true).fraction(),
            DEFAULT_FRACTION
        );
        assert_eq!(
            LaneRequest::new(LaneSide::Bottom, 5.0, true).fraction(),
            MAX_FRACTION
        );
        let p = partition_leaf(
            (f32::NAN, 0.0, -5.0, f32::INFINITY),
            metrics(24.0, false),
            Some(LaneRequest::new(LaneSide::Bottom, 0.4, true)),
        );
        assert_eq!(p.leaf, (0.0, 0.0, 0.0, 0.0));
        assert_eq!(p.lane, Some(LaneShare::Badge));
    }
}
