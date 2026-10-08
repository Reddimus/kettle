//! Card draw lists derived from owned geometry; no terminal locks or decoders.

use crate::quad::QuadInstance;
use crate::{
    PaneSnapshot,
    imgpipe::ImageItem,
    inline_cards::{CardFrame, CardVisual, InlineCards},
};
use kettle_config::Rgb;

#[derive(Default)]
pub(crate) struct CardScene {
    pub base: Vec<QuadInstance>,
    pub posters: Vec<ImageItem>,
    poster_failures: Vec<Option<CardLabel>>,
    pub decoration: Vec<QuadInstance>,
    pub cursors: Vec<QuadInstance>,
    pub labels: Vec<CardLabel>,
}

#[derive(Clone, Copy)]
pub(crate) struct CardLabel {
    pub rect: [f32; 4],
    pub kind: CardLabelKind,
    pub tr: kettle_i18n::Translator,
    pub scale: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardLabelKind {
    Brand,
    Claude,
    Pending,
    Unavailable,
}
impl CardLabel {
    pub fn text(&self) -> &'static str {
        match self.kind {
            CardLabelKind::Brand => "Kettle",
            CardLabelKind::Claude => "Claude",
            CardLabelKind::Pending => self.tr.text(kettle_i18n::Text::InlineCardPending),
            CardLabelKind::Unavailable => self.tr.text(kettle_i18n::Text::InlineCardUnavailable),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardBadgeState {
    Ready,
    Pending,
    Failed,
}

pub(crate) struct CardGeometry {
    pub grid_origin: [f32; 2],
    pub cell: [f32; 2],
    pub clip: [f32; 4],
    pub tr: kettle_i18n::Translator,
}

pub(crate) struct CardColors {
    pub background: Rgb,
    pub frame: Rgb,
    pub selection: Rgb,
}

#[derive(Clone, Copy)]
pub(crate) struct FallbackRun {
    line: i32,
    first_column: usize,
    last_column: usize,
    index: usize,
    color: Rgb,
    rect: [f32; 4],
}

/// Merge grid-adjacent cells before clipping, preserving partial edge cells.
pub(crate) fn append_fallback_background(
    base: &mut Vec<QuadInstance>,
    run: &mut Option<FallbackRun>,
    line: i32,
    column: usize,
    color: Rgb,
    rect: [f32; 4],
    clip: [f32; 4],
) {
    if let Some(previous) = run
        && previous.line == line
        && previous.last_column.checked_add(1) == Some(column)
        && previous.color == color
    {
        let mut extended = previous.rect;
        extended[2] = (column - previous.first_column + 1) as f32 * rect[2];
        if let Some(clipped) = intersect(extended, clip) {
            base[previous.index] = quad(clipped, color, 1.0);
            previous.last_column = column;
            return;
        }
    }
    *run = None;
    if let Some(clipped) = intersect(rect, clip) {
        let index = base.len();
        base.push(quad(clipped, color, 1.0));
        *run = Some(FallbackRun {
            line,
            first_column: column,
            last_column: column,
            index,
            color,
            rect,
        });
    }
}

impl CardScene {
    pub fn clear(&mut self) {
        self.base.clear();
        self.posters.clear();
        self.poster_failures.clear();
        self.decoration.clear();
        self.cursors.clear();
        self.labels.clear();
    }

    pub fn apply_upload_results(&mut self, drawn: impl Iterator<Item = usize>) {
        let mut drawn = drawn.peekable();
        for (index, failure) in self.poster_failures.iter().enumerate() {
            if drawn.peek().copied() == Some(index) {
                drawn.next();
            } else if let Some(failure) = failure {
                self.labels.push(*failure);
            }
        }
    }

    /// Append one pane's accepted blocks. A failed GPU upload still leaves its
    /// neutral background and owned badge; no stale terminal image is revealed.
    pub fn append(
        &mut self,
        cards: &InlineCards,
        frame: &CardFrame,
        snap: &PaneSnapshot,
        geometry: &CardGeometry,
        colors: &CardColors,
    ) {
        let [cw, ch] = geometry.cell;
        if !cw.is_finite() || !ch.is_finite() || cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let Ok(offset) = i32::try_from(snap.display_offset) else {
            return;
        };
        for block in &frame.blocks {
            let Some(visual) = cards.visual(block.nonce) else {
                continue;
            };
            let Some(row) = block.line.checked_add(offset) else {
                continue;
            };
            let rect = [
                geometry.grid_origin[0] + block.column as f32 * cw,
                geometry.grid_origin[1] + row as f32 * ch,
                f32::from(block.columns) * cw,
                f32::from(block.rows) * ch,
            ];
            let Some(clipped) = intersect(rect, geometry.clip) else {
                continue;
            };
            self.base.push(quad(clipped, colors.background, 1.0));
            let state = match visual {
                CardVisual::Ready(image) => {
                    if let Some(target) = letterbox(rect, image.width, image.height) {
                        self.posters.push(ImageItem::placement(
                            target,
                            image.clone(),
                            None,
                            None,
                            geometry.clip,
                        ));
                        let failure = status_label_rect(clipped, ch, false)
                            .filter(|_| intersect(target, geometry.clip).is_some())
                            .map(|rect| CardLabel {
                                rect,
                                kind: CardLabelKind::Unavailable,
                                tr: geometry.tr,
                                scale: 1.0,
                            });
                        self.poster_failures.push(failure);
                    }
                    CardBadgeState::Ready
                }
                CardVisual::Pending => CardBadgeState::Pending,
                CardVisual::Failed => CardBadgeState::Failed,
            };
            if state == CardBadgeState::Pending {
                for skeleton in skeleton_bars(rect, ch).into_iter().flatten() {
                    if let Some(skeleton) = intersect(skeleton, geometry.clip) {
                        self.base.push(quad(skeleton, colors.frame, 0.18));
                    }
                }
            }
            let badge = [
                geometry.grid_origin[0],
                rect[1],
                block.column as f32 * cw,
                rect[3],
            ];
            if let Some(badge) = intersect(badge, geometry.clip) {
                self.decoration.push(quad(badge, colors.background, 1.0));
                for (row, kind) in [CardLabelKind::Brand, CardLabelKind::Claude]
                    .into_iter()
                    .enumerate()
                {
                    if row > 0 && badge[3] < (row + 1) as f32 * ch {
                        continue;
                    }
                    if let Some(label) = intersect(
                        [
                            badge[0],
                            badge[1] + row as f32 * ch,
                            badge[2],
                            ch.min(badge[3]),
                        ],
                        geometry.clip,
                    ) {
                        self.labels.push(CardLabel {
                            rect: label,
                            kind,
                            tr: geometry.tr,
                            scale: 0.8,
                        });
                    }
                }
            }
            if state != CardBadgeState::Ready {
                let kind = if state == CardBadgeState::Pending {
                    CardLabelKind::Pending
                } else {
                    CardLabelKind::Unavailable
                };
                if let Some(label) =
                    status_label_rect(clipped, ch, state == CardBadgeState::Pending)
                {
                    self.labels.push(CardLabel {
                        rect: label,
                        kind,
                        tr: geometry.tr,
                        scale: 1.0,
                    });
                }
            }
            if let Some(selection) = snap.selection {
                for r in 0..usize::from(block.rows) {
                    let line = block.line + r as i32;
                    let mut run_start = None;
                    for col in 0..=usize::from(block.columns) {
                        let selected = col < usize::from(block.columns)
                            && selection.contains(kettle_core::Point::new(
                                kettle_core::Line(line),
                                kettle_core::Column(block.column + col),
                            ));
                        if selected && run_start.is_none() {
                            run_start = Some(col);
                        }
                        if !selected && let Some(start) = run_start.take() {
                            let tint = [
                                rect[0] + start as f32 * cw,
                                rect[1] + r as f32 * ch,
                                (col - start) as f32 * cw,
                                ch,
                            ];
                            if let Some(tint) = intersect(tint, geometry.clip) {
                                self.decoration.push(quad(tint, colors.selection, 0.25));
                            }
                        }
                    }
                }
            }
            for edge in borders(rect) {
                if let Some(edge) = intersect(edge, geometry.clip) {
                    self.decoration.push(quad(edge, colors.frame, 1.0));
                }
            }
        }
    }
}

// Keep status text in the visible body when either end scrolls out of view.
fn status_label_rect(
    [x, y, width, height]: [f32; 4],
    line_height: f32,
    pending: bool,
) -> Option<[f32; 4]> {
    if width <= 2.0 {
        return None;
    }
    let spare = (height - line_height).max(0.0);
    Some([
        x + 1.0,
        y + if pending { spare } else { spare * 0.5 },
        width - 2.0,
        line_height.min(height),
    ])
}

fn skeleton_bars([x, y, width, height]: [f32; 4], line_height: f32) -> [Option<[f32; 4]>; 3] {
    let available = height - line_height - 4.0;
    if available < line_height || width < 12.0 {
        return [None; 3];
    }
    let step = available / 4.0;
    let thickness = (step * 0.4).min(3.0);
    std::array::from_fn(|index| {
        let fraction = [0.8, 0.6, 0.45][index];
        Some([
            x + 4.0,
            y + 2.0 + step * (index + 1) as f32 - thickness * 0.5,
            (width - 8.0) * fraction,
            thickness,
        ])
    })
}

fn quad([x, y, width, height]: [f32; 4], color: Rgb, alpha: f32) -> QuadInstance {
    QuadInstance {
        pos: [x, y],
        size: [width, height],
        color: [
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            alpha,
        ],
    }
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> Option<[f32; 4]> {
    if a.into_iter().chain(b).any(|v| !v.is_finite())
        || a[2] <= 0.0
        || a[3] <= 0.0
        || b[2] <= 0.0
        || b[3] <= 0.0
    {
        return None;
    }
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    let width = (a[0] + a[2]).min(b[0] + b[2]) - x;
    let height = (a[1] + a[3]).min(b[1] + b[3]) - y;
    (width > 0.0 && height > 0.0).then_some([x, y, width, height])
}

fn borders([x, y, w, h]: [f32; 4]) -> [[f32; 4]; 4] {
    let width = 1.0_f32.min(w);
    let height = 1.0_f32.min(h);
    [
        [x, y, w, height],
        [x, y + h - height, w, height],
        [x, y, width, h],
        [x + w - width, y, width, h],
    ]
}

fn letterbox([x, y, w, h]: [f32; 4], source_width: u32, source_height: u32) -> Option<[f32; 4]> {
    if source_width == 0 || source_height == 0 || w <= 2.0 || h <= 2.0 {
        return None;
    }
    let scale = ((w - 2.0) / source_width as f32).min((h - 2.0) / source_height as f32);
    let width = source_width as f32 * scale;
    let height = source_height as f32 * scale;
    Some([x + (w - width) * 0.5, y + (h - height) * 0.5, width, height])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_fallback_cells_share_one_clipped_background() {
        let mut base = Vec::new();
        let mut run = None;
        let color = Rgb::new(10, 20, 30);
        for column in 0..256 {
            append_fallback_background(
                &mut base,
                &mut run,
                0,
                column,
                color,
                [column as f32 * 8.0, 0.0, 8.0, 16.0],
                [0.0, 0.0, 2048.0, 16.0],
            );
        }
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].size, [2048.0, 16.0]);
        base.clear();
        run = None;
        for column in 0..4 {
            append_fallback_background(
                &mut base,
                &mut run,
                0,
                column,
                color,
                [column as f32 * 8.0, 0.0, 8.0, 16.0],
                [3.0, 2.0, 17.0, 10.0],
            );
        }
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].pos, [3.0, 2.0]);
        assert_eq!(base[0].size, [17.0, 10.0]);
    }

    #[test]
    fn fallback_runs_stop_at_color_row_or_column_gaps() {
        let mut base = Vec::new();
        let mut run = None;
        let a = Rgb::new(1, 2, 3);
        let b = Rgb::new(4, 5, 6);
        for (line, column, color) in [(0, 0, a), (0, 1, b), (0, 3, b), (1, 4, b)] {
            append_fallback_background(
                &mut base,
                &mut run,
                line,
                column,
                color,
                [column as f32 * 8.0, line as f32 * 16.0, 8.0, 16.0],
                [0.0, 0.0, 100.0, 100.0],
            );
        }
        assert_eq!(base.len(), 4);
        assert!(base.iter().all(|quad| quad.size == [8.0, 16.0]));
    }

    #[test]
    fn pending_skeleton_is_static_and_never_enters_status_label_or_short_card() {
        let bars = skeleton_bars([40.0, 16.0, 96.0, 48.0], 16.0);
        assert!(bars.iter().all(Option::is_some));
        for bar in bars.into_iter().flatten() {
            assert!(bar[0] >= 40.0 && bar[0] + bar[2] <= 136.0);
            assert!(bar[1] >= 16.0 && bar[1] + bar[3] < 48.0);
        }
        assert_eq!(skeleton_bars([0.0, 0.0, 96.0, 16.0], 16.0), [None; 3]);
    }

    #[test]
    fn portrait_and_landscape_posters_letterbox_inside_the_permanent_frame() {
        assert_eq!(
            letterbox([10.0, 20.0, 102.0, 52.0], 200, 100),
            Some([11.0, 21.0, 100.0, 50.0])
        );
        assert_eq!(
            letterbox([10.0, 20.0, 102.0, 52.0], 100, 200),
            Some([48.5, 21.0, 25.0, 50.0])
        );
        assert!(letterbox([0.0, 0.0, 2.0, 20.0], 1, 1).is_none());
    }
    #[test]
    fn card_clip_never_expands_into_adjacent_panes() {
        assert_eq!(
            intersect([10.0, 20.0, 100.0, 60.0], [0.0, 0.0, 50.0, 50.0]),
            Some([10.0, 20.0, 40.0, 30.0])
        );
        assert!(intersect([50.0, 50.0, 20.0, 20.0], [0.0, 0.0, 50.0, 50.0]).is_none());
        assert!(intersect([f32::NAN, 0.0, 10.0, 10.0], [0.0, 0.0, 50.0, 50.0]).is_none());
    }
    #[test]
    fn selected_pending_card_keeps_the_full_gutter_and_frame_above_tint() {
        let (cards, mut snap, _) = crate::inline_cards::tests::fixture();
        let mut frame = CardFrame::default();
        cards.recognize_into(&snap, &mut frame);
        snap.selection = Some(alacritty_terminal::selection::SelectionRange::new(
            kettle_core::Point::new(kettle_core::Line(1), kettle_core::Column(5)),
            kettle_core::Point::new(kettle_core::Line(3), kettle_core::Column(16)),
            true,
        ));
        let mut scene = CardScene::default();
        scene.append(
            &cards,
            &frame,
            &snap,
            &CardGeometry {
                grid_origin: [0.0, 0.0],
                cell: [8.0, 16.0],
                clip: [0.0, 0.0, 640.0, 128.0],
                tr: kettle_i18n::Translator::default(),
            },
            &CardColors {
                background: Rgb::new(10, 10, 10),
                frame: Rgb::new(255, 0, 0),
                selection: Rgb::new(0, 0, 255),
            },
        );
        assert_eq!(scene.labels.len(), 3);
        assert_eq!(scene.labels[0].kind, CardLabelKind::Brand);
        assert_eq!(scene.labels[0].rect, [0.0, 16.0, 40.0, 16.0]);
        assert_eq!(scene.labels[1].kind, CardLabelKind::Claude);
        assert_eq!(scene.labels[1].rect, [0.0, 32.0, 40.0, 16.0]);
        assert_eq!(scene.labels[2].kind, CardLabelKind::Pending);
        assert_eq!(scene.labels[2].rect, [41.0, 48.0, 94.0, 16.0]);
        assert_eq!(scene.base.len(), 4);
        assert!(
            scene.base[1..]
                .iter()
                .all(|bar| bar.pos[1] + bar.size[1] < scene.labels[2].rect[1])
        );
        assert!(scene.posters.is_empty());
        assert_eq!(scene.decoration.len(), 8); // Gutter, three tint runs, four edges.
        assert!(
            scene.decoration[1..4]
                .iter()
                .all(|quad| quad.color[3] == 0.25)
        );
        assert!(
            scene.decoration[4..]
                .iter()
                .all(|quad| quad.color == [1.0, 0.0, 0.0, 1.0])
        );
        let capacities = (
            scene.base.capacity(),
            scene.decoration.capacity(),
            scene.labels.capacity(),
        );
        scene.clear();
        assert!(scene.base.is_empty() && scene.decoration.is_empty() && scene.labels.is_empty());
        assert_eq!(
            capacities,
            (
                scene.base.capacity(),
                scene.decoration.capacity(),
                scene.labels.capacity()
            )
        );
    }

    #[test]
    fn declined_upload_has_visible_status_and_accepted_upload_does_not() {
        let mut scene = CardScene {
            poster_failures: vec![Some(CardLabel {
                rect: [1.0, 2.0, 80.0, 16.0],
                kind: CardLabelKind::Unavailable,
                tr: kettle_i18n::Translator::default(),
                scale: 1.0,
            })],
            ..CardScene::default()
        };
        scene.apply_upload_results(std::iter::empty());
        assert_eq!(scene.labels.len(), 1);
        assert_eq!(scene.labels[0].text(), "Unavailable");
        scene.labels.clear();
        scene.apply_upload_results(std::iter::once(0));
        assert!(scene.labels.is_empty());
    }

    #[test]
    fn clipped_letterbox_margin_is_not_an_upload_failure() {
        let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
        cards.set_poster(
            nonce,
            kettle_core::ImageData::new_with_budget(
                1,
                2,
                vec![255; 8],
                &kettle_core::GraphicsBudget::previews(),
            ),
        );
        let mut frame = CardFrame::default();
        cards.recognize_into(&snap, &mut frame);
        for (clip, should_fail) in [
            ([40.0, 32.0, 20.0, 16.0], false),
            ([75.0, 32.0, 20.0, 16.0], true),
        ] {
            let mut scene = CardScene::default();
            scene.append(
                &cards,
                &frame,
                &snap,
                &CardGeometry {
                    grid_origin: [0.0, 0.0],
                    cell: [8.0, 16.0],
                    clip,
                    tr: kettle_i18n::Translator::default(),
                },
                &CardColors {
                    background: Rgb::new(10, 10, 10),
                    frame: Rgb::new(255, 0, 0),
                    selection: Rgb::new(0, 0, 255),
                },
            );
            assert_eq!(scene.posters.len(), 1);
            scene.apply_upload_results(std::iter::empty());
            assert_eq!(
                scene
                    .labels
                    .iter()
                    .any(|label| label.kind == CardLabelKind::Unavailable),
                should_fail
            );
        }
    }

    #[test]
    fn decoded_poster_is_owned_by_scene_and_released_when_lists_clear() {
        let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
        let account = kettle_core::GraphicsBudget::independent(kettle_core::GraphicsLimits {
            image_bytes: 8,
            retained_bytes: 8,
            process_cpu_bytes: 8,
            process_gpu_bytes: 8,
            ..kettle_core::GraphicsLimits::default()
        })
        .unwrap();
        let poster = kettle_core::ImageData::new_with_budget(2, 1, vec![255; 8], &account).unwrap();
        cards.set_poster(nonce, Some(poster));
        let mut frame = CardFrame::default();
        cards.recognize_into(&snap, &mut frame);
        let mut scene = CardScene::default();
        scene.append(
            &cards,
            &frame,
            &snap,
            &CardGeometry {
                grid_origin: [0.0, 0.0],
                cell: [8.0, 16.0],
                clip: [0.0, 0.0, 640.0, 128.0],
                tr: kettle_i18n::Translator::default(),
            },
            &CardColors {
                background: Rgb::new(10, 10, 10),
                frame: Rgb::new(255, 0, 0),
                selection: Rgb::new(0, 0, 255),
            },
        );
        assert_eq!(scene.posters.len(), 1);
        assert_eq!(scene.labels.len(), 2);
        assert!(
            scene
                .labels
                .iter()
                .all(|label| matches!(label.kind, CardLabelKind::Brand | CardLabelKind::Claude))
        );
        assert!(kettle_core::ImageData::new_with_budget(1, 1, vec![0; 4], &account).is_none());
        drop(cards);
        assert!(kettle_core::ImageData::new_with_budget(1, 1, vec![0; 4], &account).is_none());
        scene.clear();
        assert!(kettle_core::ImageData::new_with_budget(2, 1, vec![0; 8], &account).is_some());
    }

    fn partial_card_keeps_visible_brand_and_status(line: i32) {
        for state in [
            CardBadgeState::Pending,
            CardBadgeState::Failed,
            CardBadgeState::Ready,
        ] {
            let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
            match state {
                CardBadgeState::Pending => {}
                CardBadgeState::Failed => cards.set_poster(nonce, None),
                CardBadgeState::Ready => cards.set_poster(
                    nonce,
                    Some(
                        kettle_core::ImageData::new_with_budget(
                            2,
                            1,
                            vec![255; 8],
                            &kettle_core::GraphicsBudget::previews(),
                        )
                        .unwrap(),
                    ),
                ),
            }
            let mut frame = CardFrame::default();
            cards.recognize_into(&snap, &mut frame);
            assert_eq!(frame.blocks.len(), 1);
            frame.blocks[0].line = line;
            let mut scene = CardScene::default();
            scene.append(
                &cards,
                &frame,
                &snap,
                &CardGeometry {
                    grid_origin: [0.0, 0.0],
                    cell: [8.0, 16.0],
                    clip: [0.0, 0.0, 640.0, 128.0],
                    tr: kettle_i18n::Translator::default(),
                },
                &CardColors {
                    background: Rgb::new(10, 10, 10),
                    frame: Rgb::new(255, 0, 0),
                    selection: Rgb::new(0, 0, 255),
                },
            );
            if state == CardBadgeState::Ready {
                assert_eq!(scene.posters.len(), 1);
                scene.apply_upload_results(std::iter::empty());
            }
            let y = if line < 0 { 0.0 } else { 112.0 };
            let brand = scene
                .labels
                .iter()
                .find(|label| label.kind == CardLabelKind::Brand)
                .unwrap_or_else(|| {
                    panic!("visible card lost Kettle badge at line {line} for {state:?}")
                });
            assert_eq!(brand.rect, [0.0, y, 40.0, 16.0]);
            let kind = if state == CardBadgeState::Pending {
                CardLabelKind::Pending
            } else {
                CardLabelKind::Unavailable
            };
            let status = scene
                .labels
                .iter()
                .find(|label| label.kind == kind)
                .unwrap_or_else(|| {
                    panic!("visible card lost {kind:?} at line {line} for {state:?}")
                });
            assert_eq!(status.rect, [41.0, y, 94.0, 16.0]);
            assert!(
                scene
                    .labels
                    .iter()
                    .all(|label| label.rect[1] >= 0.0 && label.rect[1] + label.rect[3] <= 128.0)
            );
        }
    }

    #[test]
    fn card_with_only_last_row_visible_keeps_brand_and_typed_status() {
        partial_card_keeps_visible_brand_and_status(-2);
    }

    #[test]
    fn card_with_only_first_row_visible_keeps_brand_and_typed_status() {
        partial_card_keeps_visible_brand_and_status(7);
    }
}
