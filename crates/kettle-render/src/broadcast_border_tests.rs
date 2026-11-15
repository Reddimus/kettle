//! Broadcast outlines: the pane that sends input and the panes that receive
//! it, resolved from each `PaneView::broadcast`.

use crate::chrome::ICON_CONTRAST;
use crate::geometry::{broadcast_dash, dashed_outline_rects, fitted_dashes};
use crate::gpu_tests::gpu_test_guard;
use crate::headless_tests::{capture, capture_with_tabs, focused, pane, renderer, snapshot_of};
use crate::quad::QuadInstance;
use crate::theme::{PaneBorderStyle, broadcast_border_color, pane_border_style};
use crate::{PaneBroadcast, PaneSnapshot, PaneView, Rect4, TabBar, color, rect};
use kettle_config::{Config, Rgb, Theme};

const DIVIDER: Rgb = Rgb::new(0x12, 0x34, 0x56);
const FOCUSED_SPLIT: Rgb = Rgb::new(0x65, 0x43, 0x21);

/// With broadcast off a pane's outline is the one Kettle always drew: the
/// focused pane's `focused-split-color` or accent, the others'
/// `split-divider-color` or `palette[8]`, all solid.
#[test]
fn outlines_without_broadcast_are_unchanged() {
    let mut cfg = Config::default();
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let accent = cfg.resolved_accent(&theme);
        for (focused_split, divider) in [(None, None), (Some(FOCUSED_SPLIT), Some(DIVIDER))] {
            cfg.focused_split_color = focused_split;
            cfg.split_divider_color = divider;
            let style = |focused| {
                pane_border_style(
                    &cfg,
                    &theme,
                    accent,
                    theme.background,
                    focused,
                    PaneBroadcast::Off,
                )
            };
            assert_eq!(
                style(true),
                PaneBorderStyle {
                    color: focused_split.unwrap_or(accent),
                    dashed: false
                },
                "{name}"
            );
            assert_eq!(
                style(false),
                PaneBorderStyle {
                    color: divider.unwrap_or(theme.palette[8]),
                    dashed: false
                },
                "{name}"
            );
        }
    }
}

/// The sender and every receiver share broadcast's colour, which overrides
/// both split colours as the sender's yellow always has. The sender is solid
/// and a receiver dashed, so the two differ in shape, not only in colour. A
/// pane the broadcast does not reach keeps `split-divider-color`.
#[test]
fn senders_are_solid_receivers_dashed_and_other_panes_keep_the_divider() {
    let cfg = Config {
        focused_split_color: Some(FOCUSED_SPLIT),
        split_divider_color: Some(DIVIDER),
        ..Config::default()
    };
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        let accent = cfg.resolved_accent(&theme);
        let outline = broadcast_border_color(&theme, theme.background);
        let style = |focused, role| {
            pane_border_style(&cfg, &theme, accent, theme.background, focused, role)
        };
        let sender = style(true, PaneBroadcast::Sending);
        let receiver = style(false, PaneBroadcast::Receiving);
        assert_eq!(
            sender,
            PaneBorderStyle {
                color: outline,
                dashed: false
            },
            "{name}"
        );
        assert_eq!(
            receiver,
            PaneBorderStyle {
                color: outline,
                dashed: true
            },
            "{name}"
        );
        assert_ne!(sender, receiver, "{name}: the sender stays distinct");
        assert_eq!(
            style(false, PaneBroadcast::Off).color,
            DIVIDER,
            "{name}: a pane outside the scope keeps split-divider-color"
        );
        // A focused pane another window's group broadcast reaches receives;
        // it does not send.
        assert_eq!(style(true, PaneBroadcast::Receiving), receiver, "{name}");
    }
}

/// The broadcast outline keeps 3:1 non-text contrast against the pane
/// background in every bundled theme and against the backgrounds a program
/// can set (OSC 11), in `palette[3]`'s own hue, and is the theme's yellow
/// itself wherever that already passes. Raw `palette[3]` is about 2.2:1 on
/// Catppuccin Latte.
#[test]
fn broadcast_outlines_keep_3_to_1_on_every_bundled_theme() {
    let mut failures = Vec::new();
    let mut lifted = 0;
    for name in Theme::list() {
        let theme = Theme::by_name(name);
        for background in [
            theme.background,
            Rgb::new(255, 255, 255),
            Rgb::new(0, 0, 0),
            Rgb::new(0x80, 0x80, 0x80),
        ] {
            let outline = broadcast_border_color(&theme, background);
            let ratio = color::contrast_ratio(outline, background);
            if ratio < ICON_CONTRAST - 1e-6 {
                failures.push(format!("{name} on {background:?}: {ratio:.2}:1"));
            }
            if color::contrast_ratio(theme.palette[3], background) >= ICON_CONTRAST {
                assert_eq!(
                    outline, theme.palette[3],
                    "{name}: unchanged when it passes"
                );
            } else {
                lifted += 1;
                let (hue, saturation, _) = color::hsl(theme.palette[3]);
                let (lifted_hue, _, _) = color::hsl(outline);
                if saturation > 0.2 && outline != Rgb::new(0, 0, 0) {
                    let gap = (hue - lifted_hue)
                        .abs()
                        .min(360.0 - (hue - lifted_hue).abs());
                    assert!(gap < 12.0, "{name}: hue {hue} became {lifted_hue}");
                }
            }
        }
    }
    assert!(lifted > 0, "some bundled yellow needs lifting");
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

const W: f32 = 200.0;
const H: f32 = 200.0;

fn leaf(i: usize) -> Rect4 {
    (W * i as f32, 0.0, W, H)
}

/// Three side-by-side panes, the first focused, with `roles`.
fn three_panes(snap: &PaneSnapshot, roles: [PaneBroadcast; 3], titled: bool) -> Vec<PaneView<'_>> {
    (0..3)
        .map(|i| {
            let mut view = pane(snap, W as u32, H as u32);
            let (x, y, w, h) = leaf(i);
            view.id = i as u64 + 1;
            view.rect = (x, y, w, h);
            view.focused = i == 0;
            view.broadcast = roles[i];
            if titled {
                view.titlebar = Some((x, y, w, 24.0));
                view.terminal = (x, y + 24.0, w, h - 24.0);
            } else {
                view.terminal = view.rect;
            }
            view
        })
        .collect()
}

fn inside(quad: &QuadInstance, (x, y, w, h): Rect4) -> bool {
    quad.pos[0] >= x
        && quad.pos[1] >= y
        && quad.pos[0] + quad.size[0] <= x + w
        && quad.pos[1] + quad.size[1] <= y + h
}

fn quad_rect(quad: &QuadInstance) -> Rect4 {
    (quad.pos[0], quad.pos[1], quad.size[0], quad.size[1])
}

fn colored(color: Rgb) -> [f32; 4] {
    rect(0.0, 0.0, 0.0, 0.0, color, 1.0).color
}

/// A frame's outlines as it sends them to the GPU: the sending pane's four
/// solid strips and the receiving pane's dashes in broadcast's colour, and
/// the divider's four strips around the pane the broadcast does not reach.
/// Stopping broadcast takes broadcast's colour off every pane.
#[test]
fn a_frame_outlines_the_sender_solid_and_each_receiver_dashed() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer((3.0 * W) as u32, H as u32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 8, b"");
    let theme = &cfg.theme;
    let outline = colored(broadcast_border_color(theme, theme.background));
    let divider = colored(cfg.split_divider_color.unwrap_or(theme.palette[8]));
    let strips = |(x, y, w, h): Rect4| {
        vec![
            (x, y, w, 1.0),
            (x, y + h - 1.0, w, 1.0),
            (x, y, 1.0, h),
            (x + w - 1.0, y, 1.0, h),
        ]
    };
    let uploaded = |renderer: &crate::Renderer| -> Vec<QuadInstance> {
        renderer
            .quads
            .uploaded()
            .expect("the retained copy")
            .collect()
    };
    let in_pane = |quads: &[QuadInstance], color: [f32; 4], i: usize| -> Vec<Rect4> {
        quads
            .iter()
            .filter(|quad| quad.color == color && inside(quad, leaf(i)))
            .map(quad_rect)
            .collect()
    };

    let panes = three_panes(
        &snap,
        [
            PaneBroadcast::Sending,
            PaneBroadcast::Receiving,
            PaneBroadcast::Off,
        ],
        false,
    );
    capture(&mut renderer, &cfg, &panes, &focused(true));
    let quads = uploaded(&renderer);
    assert_eq!(in_pane(&quads, outline, 0), strips(leaf(0)), "the sender");
    let dashes: Vec<Rect4> = dashed_outline_rects(leaf(1), 1.0, broadcast_dash(1.0, 1.0)).collect();
    assert!(dashes.len() > 4);
    assert_eq!(in_pane(&quads, outline, 1), dashes, "the receiver");
    assert!(in_pane(&quads, outline, 2).is_empty(), "outside the scope");
    assert_eq!(in_pane(&quads, divider, 2), strips(leaf(2)));
    // Unfocused panes recede under a dim; the receiver's outline does not,
    // or it would lose the 3:1 it was lifted to. The pane outside the scope
    // is dimmed edge to edge as before.
    let dims: Vec<Rect4> = renderer
        .pane_dimming
        .uploaded()
        .expect("the retained copy")
        .map(|quad| quad_rect(&quad))
        .collect();
    let (x, y, w, h) = leaf(1);
    assert!(
        dims.contains(&(x + 1.0, y + 1.0, w - 2.0, h - 2.0)),
        "{dims:?}"
    );
    assert!(dims.contains(&leaf(2)), "{dims:?}");
    let overlaps = |a: Rect4, b: Rect4| crate::geometry::rects_overlap(a, b);
    for dash in &dashes {
        assert!(
            !dims.iter().any(|dim| overlaps(*dim, *dash)),
            "{dash:?} is dimmed"
        );
    }

    // Broadcast stops: no pane keeps broadcast's colour.
    let panes = three_panes(&snap, [PaneBroadcast::Off; 3], false);
    capture(&mut renderer, &cfg, &panes, &focused(true));
    let quads = uploaded(&renderer);
    for i in 0..3 {
        assert!(in_pane(&quads, outline, i).is_empty(), "pane {i}");
    }
    assert_eq!(in_pane(&quads, divider, 1), strips(leaf(1)));
    assert_eq!(in_pane(&quads, divider, 2), strips(leaf(2)));
}

/// Per-pane titlebars follow the same roles: under a group scope a pane
/// outside the group keeps the inactive bar even though the window
/// broadcasts, and a receiving pane takes the receive bar.
#[test]
fn titlebars_mark_only_the_panes_the_broadcast_reaches() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer((3.0 * W) as u32, H as u32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.show_titlebar = true;
    let snap = snapshot_of(20, 8, b"");
    let theme = cfg.theme.clone();
    let receive = colored(
        cfg.title_receive_bg_color
            .unwrap_or(cfg.resolved_accent(&theme)),
    );
    let inactive = colored(cfg.title_inactive_bg_color.unwrap_or(theme.palette[8]));
    assert_ne!(receive, inactive);
    let panes = three_panes(
        &snap,
        [
            PaneBroadcast::Sending,
            PaneBroadcast::Receiving,
            PaneBroadcast::Off,
        ],
        true,
    );
    let mut tabs = TabBar::hidden();
    tabs.broadcast = true;
    capture_with_tabs(&mut renderer, &cfg, &panes, &focused(true), &tabs);
    let quads: Vec<QuadInstance> = renderer.quads.uploaded().expect("retained").collect();
    let bar = |i: usize| {
        let (x, y, w, _) = leaf(i);
        quads
            .iter()
            .find(|quad| quad_rect(quad) == (x + 1.0, y + 1.0, w - 2.0, 23.0))
            .map(|quad| quad.color)
            .unwrap_or_else(|| panic!("pane {i} has a titlebar quad"))
    };
    assert_eq!(bar(1), receive, "the receiving pane");
    assert_eq!(bar(2), inactive, "the pane outside the group");
}

/// On a decorated macOS window the panes at the bottom corners are drawn by
/// the rounded outline shader instead. A receiving pane's outline there
/// carries the same dash as the four-quad path and the GPU draws it with
/// gaps; the sender's stays solid.
#[test]
fn rounded_outlines_dash_receivers_too() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer((3.0 * W) as u32, H as u32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    renderer.set_rounded_window_corners(true);
    let snap = snapshot_of(20, 8, b"");
    let panes = three_panes(
        &snap,
        [
            PaneBroadcast::Sending,
            PaneBroadcast::Off,
            PaneBroadcast::Receiving,
        ],
        false,
    );
    let frame = capture(&mut renderer, &cfg, &panes, &focused(true));
    let outlines: Vec<_> = renderer
        .pane_outlines
        .uploaded()
        .expect("the retained copy")
        .collect();
    let at = |x: f32| {
        outlines
            .iter()
            .find(|outline| outline.pos[0] == x)
            .copied()
            .unwrap_or_else(|| panic!("an outline at x {x}"))
    };
    let (dash, gap) = broadcast_dash(1.0, 1.0);
    let sender = at(leaf(0).0);
    let receiver = at(leaf(2).0);
    assert_eq!((sender.dash, sender.gap), (0.0, 0.0));
    assert_eq!((receiver.dash, receiver.gap), (dash, gap));
    assert_eq!(receiver.color, sender.color);

    // Along each pane's straight top edge: one solid run for the sender, one
    // run per fitted dash for the receiver.
    let outline = broadcast_border_color(&cfg.theme, cfg.theme.background);
    let runs = |i: usize| {
        let (x, _, w, _) = leaf(i);
        let mut runs = 0;
        let mut previous = false;
        for px in x as u32..(x + w) as u32 {
            let p = frame.get_pixel(px, 0);
            let on = [p[0], p[1], p[2]]
                .iter()
                .zip([outline.r, outline.g, outline.b])
                .all(|(a, b)| a.abs_diff(b) <= 3);
            runs += usize::from(on && !previous);
            previous = on;
        }
        runs
    };
    assert_eq!(runs(0), 1, "the sender's top edge is solid");
    assert_eq!(
        runs(2),
        fitted_dashes(W, dash, gap).count(),
        "the receiver's is dashed"
    );
}
