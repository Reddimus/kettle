//! Layer admission: the pixels resvg will allocate while rendering a tree,
//! counted on the tree before any pixmap exists.
//!
//! The walk follows resvg 0.48.1's own traversal (`render.rs`, `clip.rs`,
//! `mask.rs`, `path.rs`, `filter/mod.rs`) and charges every allocation it
//! would make, each time it would make it:
//!
//! - an isolated group's layer: its transformed layer bounds, floored and
//!   ceiled, widened by 2 pixels a side without filters, and clamped to the
//!   region resvg clamps to (from -2 to +3 canvas widths and heights: up to
//!   25 times the canvas area, whatever resvg's comment says);
//! - every filter primitive's result at the layer's size (an input that is
//!   the source graphic is a copy of the whole layer, and every result lives
//!   until the filter ends), and what an `feImage` renders;
//! - a clip path's canvas and mask at the layer's size, again for each clip
//!   in a chain and for each clipped group inside one;
//! - a mask's canvas and masks at the layer's size, its content, and again
//!   for each mask in a chain;
//! - a pattern tile at its own transformed size (not clamped to anything),
//!   for every fill and stroke that uses it, and its content.
//!
//! The total may not pass `MAX_SVG_LAYER_PIXELS`. A transform that is not
//! finite is refused, and so is a filter rectangle reaching past
//! `MAX_FILTER_COORDINATE` on its layer (tiny-skia's conversion of it
//! unwraps), and an image node, which nothing here may load.
//! The walk also counts the nodes it visits, which is the drawing resvg will
//! do, against `MAX_SVG_WORK`: a style sheet can give every path a pattern
//! without any reference the structural pass sees. Pixels are not time, and
//! filter scratch buffers and path tessellation are not counted, so the
//! worker's deadline and the client's memory limit still stand behind this.

use kettle_media::{FailureCode, MAX_SVG_LAYER_PIXELS, MAX_SVG_WORK};
use resvg::tiny_skia::{IntRect, Transform};

#[derive(Default)]
struct Budget {
    pixels: u64,
    steps: u64,
}

impl Budget {
    fn charge(&mut self, width: u32, height: u32) -> Result<(), FailureCode> {
        self.charge_pixels(u64::from(width) * u64::from(height))
    }

    fn charge_pixels(&mut self, pixels: u64) -> Result<(), FailureCode> {
        self.pixels = self.pixels.saturating_add(pixels);
        if self.pixels > MAX_SVG_LAYER_PIXELS {
            return Err(FailureCode::RenderResource);
        }
        Ok(())
    }

    fn step(&mut self) -> Result<(), FailureCode> {
        self.steps += 1;
        if self.steps > MAX_SVG_WORK {
            return Err(FailureCode::RenderResource);
        }
        Ok(())
    }
}

/// What resvg will do next, kept on an explicit stack: a tree may nest deeper
/// than a thread's stack allows recursion.
enum Task<'a> {
    /// Render a group's children (`render_nodes`), with nested layers clamped
    /// to `bounds`.
    Children {
        group: &'a usvg::Group,
        transform: Transform,
        bounds: IntRect,
    },
    /// Draw a clip path's children onto a layer-sized canvas.
    ClipChildren {
        group: &'a usvg::Group,
        transform: Transform,
        layer: (u32, u32),
    },
    /// Apply a clip path to a layer.
    Clip {
        clip: &'a usvg::ClipPath,
        transform: Transform,
        layer: (u32, u32),
    },
    /// Apply a mask to a layer.
    Mask {
        mask: &'a usvg::Mask,
        transform: Transform,
        layer: (u32, u32),
        bounds: IntRect,
    },
}

/// The region resvg clamps layers to for a `width` by `height` canvas.
fn canvas_bounds(width: u32, height: u32) -> IntRect {
    IntRect::from_xywh(
        i32::try_from(width).unwrap_or(i32::MAX).saturating_mul(-2),
        i32::try_from(height).unwrap_or(i32::MAX).saturating_mul(-2),
        width.saturating_mul(5),
        height.saturating_mul(5),
    )
    .or_else(|| IntRect::from_ltrb(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2))
    .unwrap_or_else(|| unreachable!("a fixed, valid rectangle"))
}

/// The farthest a filter rectangle may reach on its layer: past any canvas,
/// and near enough that tiny-skia's integer conversion (which unwraps) and
/// resvg's translations of the result cannot overflow.
const MAX_FILTER_COORDINATE: f32 = 16_777_216.0;

/// A filter or primitive rectangle on the layer, as resvg converts it: `None`
/// where resvg finds none, refused where converting it would overflow.
fn filter_rect(
    rect: usvg::NonZeroRect,
    transform: Transform,
) -> Result<Option<IntRect>, FailureCode> {
    let Some(rect) = rect.transform(transform) else {
        return Ok(None);
    };
    if [rect.left(), rect.top(), rect.right(), rect.bottom()]
        .iter()
        .any(|edge| edge.abs() > MAX_FILTER_COORDINATE)
    {
        return Err(FailureCode::RenderParse);
    }
    Ok(Some(rect.to_int_rect()))
}

/// resvg's `fit_to_rect`: `rect` cut to `bounds`.
fn fit(rect: IntRect, bounds: IntRect) -> Option<IntRect> {
    IntRect::from_ltrb(
        rect.left().max(bounds.left()),
        rect.top().max(bounds.top()),
        rect.right().min(bounds.right()),
        rect.bottom().min(bounds.bottom()),
    )
}

fn finite(transform: Transform) -> Result<Transform, FailureCode> {
    if transform.is_finite() {
        Ok(transform)
    } else {
        Err(FailureCode::RenderParse)
    }
}

/// Admit rendering `tree` with `transform` onto a `width` by `height` canvas,
/// or refuse it.
pub(super) fn admit(
    tree: &usvg::Tree,
    transform: Transform,
    width: u32,
    height: u32,
) -> Result<(), FailureCode> {
    let mut budget = Budget::default();
    let mut tasks = vec![Task::Children {
        group: tree.root(),
        transform: finite(transform)?,
        bounds: canvas_bounds(width, height),
    }];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Children {
                group,
                transform,
                bounds,
            } => {
                for node in group.children() {
                    budget.step()?;
                    match node {
                        usvg::Node::Group(group) => {
                            isolate(group, transform, bounds, &mut budget, &mut tasks)?;
                        }
                        usvg::Node::Path(path) => {
                            patterns(path, transform, bounds, &mut budget, &mut tasks, true)?;
                        }
                        usvg::Node::Text(text) => {
                            isolate(text.flattened(), transform, bounds, &mut budget, &mut tasks)?;
                        }
                        usvg::Node::Image(_) => return Err(FailureCode::RenderParse),
                    }
                }
            }
            Task::ClipChildren {
                group,
                transform,
                layer,
            } => {
                for node in group.children() {
                    budget.step()?;
                    match node {
                        usvg::Node::Path(path) => {
                            // A clip path draws fills only, with nested
                            // layers clamped to a single pixel.
                            let one = IntRect::from_xywh(0, 0, 1, 1)
                                .unwrap_or_else(|| unreachable!("a valid rectangle"));
                            patterns(path, transform, one, &mut budget, &mut tasks, false)?;
                        }
                        usvg::Node::Text(text) => tasks.push(Task::ClipChildren {
                            group: text.flattened(),
                            transform,
                            layer,
                        }),
                        usvg::Node::Group(group) => {
                            let transform = finite(transform.pre_concat(group.transform()))?;
                            if let Some(clip) = group.clip_path() {
                                // Drawn on its own layer-sized canvas, clipped.
                                budget.charge(layer.0, layer.1)?;
                                tasks.push(Task::Clip {
                                    clip,
                                    transform,
                                    layer,
                                });
                            }
                            tasks.push(Task::ClipChildren {
                                group,
                                transform,
                                layer,
                            });
                        }
                        usvg::Node::Image(_) => return Err(FailureCode::RenderParse),
                    }
                }
            }
            Task::Clip {
                clip,
                transform,
                layer,
            } => {
                // The clip canvas, then its alpha mask (a byte a pixel).
                budget.charge(layer.0, layer.1)?;
                budget.charge_pixels((u64::from(layer.0) * u64::from(layer.1)).div_ceil(4))?;
                tasks.push(Task::ClipChildren {
                    group: clip.root(),
                    transform: finite(transform.pre_concat(clip.transform()))?,
                    layer,
                });
                if let Some(clip) = clip.clip_path() {
                    tasks.push(Task::Clip {
                        clip,
                        transform,
                        layer,
                    });
                }
            }
            Task::Mask {
                mask,
                transform,
                layer,
                bounds,
            } => {
                if mask.root().children().is_empty() {
                    continue;
                }
                // The mask canvas, its region mask and the mask made from it.
                budget.charge(layer.0, layer.1)?;
                budget.charge_pixels((u64::from(layer.0) * u64::from(layer.1)).div_ceil(2))?;
                tasks.push(Task::Children {
                    group: mask.root(),
                    transform,
                    bounds,
                });
                if let Some(mask) = mask.mask() {
                    tasks.push(Task::Mask {
                        mask,
                        transform,
                        layer,
                        bounds,
                    });
                }
            }
        }
    }
    Ok(())
}

/// resvg's `render_group`: a group that isolates gets a layer, its filters,
/// clip and mask; one that does not draws its children in place.
fn isolate<'a>(
    group: &'a usvg::Group,
    transform: Transform,
    bounds: IntRect,
    budget: &mut Budget,
    tasks: &mut Vec<Task<'a>>,
) -> Result<(), FailureCode> {
    let transform = finite(transform.pre_concat(group.transform()))?;
    if !group.should_isolate() {
        tasks.push(Task::Children {
            group,
            transform,
            bounds,
        });
        return Ok(());
    }
    // Where resvg finds no layer, it draws nothing and allocates nothing.
    let Some(bbox) = group.layer_bounding_box().transform(transform) else {
        return Ok(());
    };
    // The same float-to-integer conversions resvg makes, saturating included.
    let layer = if group.filters().is_empty() {
        (bbox.x().floor() as i32).checked_sub(2).and_then(|x| {
            (bbox.y().floor() as i32).checked_sub(2).and_then(|y| {
                IntRect::from_xywh(
                    x,
                    y,
                    (bbox.width().ceil() as u32).checked_add(4)?,
                    (bbox.height().ceil() as u32).checked_add(4)?,
                )
            })
        })
    } else {
        IntRect::from_xywh(
            bbox.x().floor() as i32,
            bbox.y().floor() as i32,
            bbox.width().ceil().max(1.0) as u32,
            bbox.height().ceil().max(1.0) as u32,
        )
    };
    // A list of filters runs on one layer with results of different sizes,
    // which resvg's lighting and displacement index past; the sanitizer
    // allows one filter, and this holds whatever usvg built.
    if group.filters().len() > 1 {
        return Err(FailureCode::RenderParse);
    }
    let Some(layer) = layer.and_then(|layer| fit(layer, bounds)) else {
        return Ok(());
    };
    budget.charge(layer.width(), layer.height())?;
    let size = (layer.width(), layer.height());
    let transform = finite(
        Transform::from_translate(-(layer.x() as f32), -(layer.y() as f32)).pre_concat(transform),
    )?;
    let whole = IntRect::from_xywh(0, 0, size.0, size.1)
        .unwrap_or_else(|| unreachable!("a layer is never empty"));
    for filter in group.filters() {
        // Every rectangle resvg will convert must convert without panicking,
        // whether or not resvg reaches it.
        let mut subregions = Vec::with_capacity(filter.primitives().len());
        for primitive in filter.primitives() {
            subregions.push(filter_rect(primitive.rect(), transform)?);
        }
        let Some(region) = filter_rect(filter.rect(), transform)?.and_then(|rect| fit(rect, whole))
        else {
            continue;
        };
        for (primitive, subregion) in filter.primitives().iter().zip(subregions) {
            // resvg ends the filter at a subregion it cannot form.
            let Some(subregion) = subregion else {
                break;
            };
            budget.step()?;
            // A result is a copy of the whole layer whenever its input is the
            // source graphic, and every result lives until the filter ends.
            budget.charge(size.0, size.1)?;
            if let usvg::filter::Kind::Image(image) = primitive.kind() {
                let (sx, sy) = transform.get_scale();
                tasks.push(Task::Children {
                    group: image.root(),
                    transform: finite(Transform::from_row(
                        sx,
                        0.0,
                        0.0,
                        sy,
                        subregion.x() as f32,
                        subregion.y() as f32,
                    ))?,
                    bounds: IntRect::from_xywh(0, 0, region.width(), region.height())
                        .unwrap_or_else(|| unreachable!("a region is never empty")),
                });
            }
        }
    }
    if let Some(clip) = group.clip_path() {
        tasks.push(Task::Clip {
            clip,
            transform,
            layer: size,
        });
    }
    if let Some(mask) = group.mask() {
        tasks.push(Task::Mask {
            mask,
            transform,
            layer: size,
            bounds,
        });
    }
    tasks.push(Task::Children {
        group,
        transform,
        bounds,
    });
    Ok(())
}

/// resvg's pattern tiles for a path's fill (and its stroke, outside a clip
/// path), each at its own transformed size.
fn patterns<'a>(
    path: &'a usvg::Path,
    transform: Transform,
    bounds: IntRect,
    budget: &mut Budget,
    tasks: &mut Vec<Task<'a>>,
    stroke: bool,
) -> Result<(), FailureCode> {
    if !path.is_visible() {
        return Ok(());
    }
    let fill = path.fill().map(usvg::Fill::paint);
    let stroke = path.stroke().filter(|_| stroke).map(usvg::Stroke::paint);
    for paint in fill.into_iter().chain(stroke) {
        let usvg::Paint::Pattern(pattern) = paint else {
            continue;
        };
        let (sx, sy) = finite(transform.pre_concat(pattern.transform()))?.get_scale();
        let rect = pattern.rect();
        let width = (rect.width() * sx).round() as u32;
        let height = (rect.height() * sy).round() as u32;
        if width == 0 || height == 0 {
            continue;
        }
        budget.charge(width, height)?;
        tasks.push(Task::Children {
            group: pattern.root(),
            transform: finite(Transform::from_scale(sx, sy))?,
            bounds,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(body: &str) -> usvg::Tree {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">{body}</svg>"#
        );
        usvg::Tree::from_str(&svg, &super::super::options()).unwrap()
    }

    fn patterned(rects: usize) -> usvg::Tree {
        let content: String = (0..1000)
            .map(|_| r#"<rect width="1" height="1" fill="red"/>"#)
            .collect();
        let rects: String = (0..rects)
            .map(|_| r#"<rect width="2" height="2"/>"#)
            .collect();
        tree(&format!(
            r#"<style>.p {{ fill: url(#p) }}</style><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse">{content}</pattern><g class="p">{rects}</g>"#
        ))
    }

    #[test]
    fn a_filter_list_is_refused_whatever_the_sanitizer_allowed() {
        let tree = tree(
            r#"<filter id="a"><feOffset/></filter><filter id="b"><feOffset/></filter><rect width="8" height="8" filter="url(#a) url(#b)"/>"#,
        );
        assert_eq!(
            admit(&tree, Transform::identity(), 64, 64).unwrap_err(),
            FailureCode::RenderParse
        );
    }

    #[test]
    fn drawing_work_is_bounded_whatever_the_structural_pass_allowed() {
        // Built straight from usvg: a style sheet gives rectangles a pattern
        // of a thousand shapes, drawn for each.
        assert_eq!(
            admit(&patterned(2000), Transform::identity(), 64, 64).unwrap_err(),
            FailureCode::RenderResource
        );
        assert!(admit(&patterned(200), Transform::identity(), 64, 64).is_ok());
    }
}
