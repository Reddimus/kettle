//! Structural admission: what an SVG document costs once usvg expands its
//! references, counted on the parsed document before usvg sees it.
//!
//! Each element costs one unit, plus the numbers in its path data or point
//! list and the characters of its text. A reference adds its target's whole
//! cost every time it is used: `use` (whose copy inherits from the `use`),
//! and paint servers, clips, masks, filters, `feImage`, text paths and the
//! templates gradients, patterns and filters link to (which inherit from
//! where they are defined). Paint counts where it is drawn: a shape or text
//! is charged for the `fill` and `stroke` it inherits as well as its own, and
//! a `use` for the paint its copy inherits, once per painted element in the
//! copy. A style sheet may apply any of its references to any element, so
//! every painted element is charged for the sheet's paint, and every
//! graphics element for its clips, masks and filters. A shape that can carry
//! markers adds, per vertex, the cost of the most expensive marker. The
//! total bounds the tree usvg will build.
//!
//! The walk refuses a reference cycle (inherited and style-sheet paint
//! included: usvg would recurse through one without end), a document whose
//! expanded nesting passes `MAX_SVG_DEPTH` (usvg and resvg recurse that
//! deep), more than `MAX_SVG_ELEMENTS` elements, an expanded cost over
//! `MAX_SVG_WORK`, style sheets whose selectors matched against every
//! element would cost more than ten times that, and markers that could hold
//! markers: a marker whose content (or what it references) sets a marker
//! property, or that inherits one, or that holds a shape while a style sheet
//! sets marker properties. usvg allows such nesting, and it multiplies per
//! vertex at every level.

use std::collections::HashMap;

use kettle_media::{FailureCode, MAX_SVG_DEPTH, MAX_SVG_ELEMENTS, MAX_SVG_WORK};
use roxmltree::{Document, Node};

use super::sanitize::{self, Urls};

/// Selectors times elements: the matching usvg does.
const MAX_STYLE_MATCHING: u64 = 10 * MAX_SVG_WORK;
/// Vertices charged for a shape whose outline is not listed: a line,
/// rectangle, circle or ellipse (a rounded rectangle has at most 13 points).
const BASIC_SHAPE_VERTICES: u64 = 16;
/// Costs stop counting just past the limit, so sums cannot overflow.
const CAP: u64 = MAX_SVG_WORK + 1;

#[derive(Default)]
struct Element {
    children: Vec<usize>,
    /// Targets rendered in place that inherit from this element (`use`).
    inherited: Vec<usize>,
    /// Targets rendered with the inheritance of where they are defined.
    referenced: Vec<usize>,
    /// For a `use`: the paint its copy inherits, drawn by every painted
    /// element in the copy.
    copy_paint: Vec<usize>,
    own: u64,
    /// For an element that can carry markers, its vertices.
    vertices: Option<u64>,
    painted: bool,
    declares_marker: bool,
    marker: bool,
    ancestor_declares: bool,
}

impl Element {
    fn edges(&self) -> impl Iterator<Item = &usize> + Clone {
        self.children
            .iter()
            .chain(&self.inherited)
            .chain(&self.referenced)
            .chain(&self.copy_paint)
    }
}

/// What an element declares: its paint (when it declares `fill` or `stroke`
/// at all, the fragments they name), other rendering references, and markers.
#[derive(Default)]
struct Declared {
    fill: Option<Vec<String>>,
    stroke: Option<Vec<String>>,
    other: Vec<String>,
    declares_marker: bool,
}

fn declared(node: Node<'_, '_>) -> Result<Declared, FailureCode> {
    fn property(name: &str, value: &str, found: &mut Declared) -> Result<(), FailureCode> {
        let ids = || -> Result<Vec<String>, FailureCode> {
            Ok(match sanitize::urls(value)? {
                Urls::Local(ids) => ids.into_iter().map(str::to_owned).collect(),
                // Removed by the writer: usvg never sees it.
                Urls::External => Vec::new(),
            })
        };
        match name {
            _ if sanitize::marker_property(name) => found.declares_marker = true,
            "fill" => found.fill = Some(ids()?),
            "stroke" => found.stroke = Some(ids()?),
            "clip-path" | "mask" | "filter" => found.other.extend(ids()?),
            _ => {}
        }
        Ok(())
    }
    let mut found = Declared::default();
    // Presentation attributes first: a `style` declaration overrides one.
    for attribute in node.attributes() {
        if attribute.namespace().is_none() && attribute.name() != "style" {
            property(attribute.name(), attribute.value(), &mut found)?;
        }
    }
    if let Some(style) = sanitize::plain(node, "style") {
        let style = sanitize::strip_comments(style)?;
        for declaration in sanitize::declarations(&style)? {
            property(declaration.name, declaration.value, &mut found)?;
        }
    }
    Ok(found)
}

/// Elements drawn with `fill` and `stroke`.
fn painted(name: &str) -> bool {
    matches!(
        name,
        "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "textPath"
            | "tref"
    )
}

/// Elements a style sheet's clip, mask or filter can apply to.
fn graphic(name: &str) -> bool {
    painted(name)
        || matches!(
            name,
            "g" | "svg" | "a" | "switch" | "use" | "symbol" | "image"
        )
}

fn count_numbers(text: &str) -> u64 {
    let mut count = 0u64;
    sanitize::numbers(text, |_| count += 1);
    count
}

fn vertices(node: Node<'_, '_>) -> Option<u64> {
    let count = |name| count_numbers(sanitize::plain(node, name).unwrap_or_default());
    match node.tag_name().name() {
        // An `H` or `V` takes one number per vertex: count every number.
        "path" => Some(count("d")),
        "polyline" | "polygon" => Some(count("points") / 2),
        "line" | "rect" | "circle" | "ellipse" => Some(BASIC_SHAPE_VERTICES),
        _ => None,
    }
}

/// Admit `document`'s structure, or refuse it.
pub(super) fn admit(document: &Document<'_>) -> Result<(), FailureCode> {
    let all = document.descendants().filter(Node::is_element).count();
    if all > MAX_SVG_ELEMENTS {
        return Err(FailureCode::RenderResource);
    }
    let root = document.root_element();
    if !sanitize::kept(root) {
        return Err(FailureCode::RenderParse);
    }

    // The kept elements in document order, each with its parent's index.
    let mut nodes: Vec<Node<'_, '_>> = Vec::new();
    let mut parents: Vec<Option<usize>> = Vec::new();
    let mut stack = vec![(root, None)];
    while let Some((node, parent)) = stack.pop() {
        let index = nodes.len();
        nodes.push(node);
        parents.push(parent);
        stack.extend(
            node.children()
                .filter(|child| sanitize::kept(*child))
                .rev()
                .map(|child| (child, Some(index))),
        );
    }
    let mut ids: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(id) = sanitize::plain(*node, "id") {
            ids.entry(id).or_default().push(index);
        }
    }
    let resolve = |targets: &[String]| -> Vec<usize> {
        targets
            .iter()
            .flat_map(|id| ids.get(id.as_str()).into_iter().flatten().copied())
            .collect()
    };

    // What the style sheets apply, and two stand-ins for "whatever a rule
    // may apply to": one for their paint, one for their other references.
    let mut sheet_paint = Vec::new();
    let mut sheet_other = Vec::new();
    let mut style_marks = false;
    let mut selectors = 0u64;
    for node in &nodes {
        if node.tag_name().name() == "style" {
            let css: String = node.children().filter_map(|child| child.text()).collect();
            let sheet = sanitize::check_style_sheet(&css)?;
            style_marks |= sheet.marks;
            selectors = selectors.saturating_add(sheet.selectors);
            sheet_paint.extend(sheet.paint);
            sheet_other.extend(sheet.other);
        }
    }
    if selectors.saturating_mul(all as u64) > MAX_STYLE_MATCHING {
        return Err(FailureCode::RenderResource);
    }
    let sheet_paint_node = nodes.len();
    let sheet_other_node = nodes.len() + 1;

    let mut elements: Vec<Element> = Vec::with_capacity(nodes.len() + 2);
    // The nearest element (itself included) declaring `fill` or `stroke`.
    let mut fill_source: Vec<Option<usize>> = Vec::with_capacity(nodes.len());
    let mut stroke_source: Vec<Option<usize>> = Vec::with_capacity(nodes.len());
    let mut declarations: Vec<Declared> = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let name = node.tag_name().name();
        let found = declared(*node)?;
        let parent = parents[index];
        fill_source.push(if found.fill.is_some() {
            Some(index)
        } else {
            parent.and_then(|parent| fill_source[parent])
        });
        stroke_source.push(if found.stroke.is_some() {
            Some(index)
        } else {
            parent.and_then(|parent| stroke_source[parent])
        });
        // The paint in effect here, inherited or declared.
        let mut paint = Vec::new();
        for (source, pick) in [(fill_source[index], 0), (stroke_source[index], 1)] {
            if let Some(source) = source {
                let declaration = if source == index {
                    &found
                } else {
                    &declarations[source]
                };
                let targets = if pick == 0 {
                    &declaration.fill
                } else {
                    &declaration.stroke
                };
                paint.extend(resolve(targets.as_deref().unwrap_or_default()));
            }
        }
        if !sheet_paint.is_empty() {
            paint.push(sheet_paint_node);
        }
        let mut referenced = resolve(&found.other);
        let mut inherited = Vec::new();
        let mut copy_paint = Vec::new();
        if let Some(id) = sanitize::href(*node) {
            let targets = resolve(&[id.to_owned()]);
            match name {
                "use" => inherited = targets,
                "feImage" | "textPath" | "tref" | "pattern" | "linearGradient"
                | "radialGradient" | "filter" => referenced.extend(targets),
                _ => {}
            }
        }
        if painted(name) {
            referenced.extend(paint);
        } else if name == "use" {
            copy_paint = paint;
        }
        if graphic(name) && !sheet_other.is_empty() {
            referenced.push(sheet_other_node);
        }
        let text: u64 = node
            .children()
            .filter_map(|child| child.text())
            .map(|text| text.chars().count() as u64)
            .sum();
        let ancestor_declares = parent.is_some_and(|parent| {
            elements[parent].ancestor_declares || elements[parent].declares_marker
        });
        let own = 1u64
            .saturating_add(count_numbers(
                sanitize::plain(*node, "d").unwrap_or_default(),
            ))
            .saturating_add(count_numbers(
                sanitize::plain(*node, "points").unwrap_or_default(),
            ))
            .saturating_add(text)
            .min(CAP);
        elements.push(Element {
            inherited,
            referenced,
            copy_paint,
            own,
            vertices: vertices(*node),
            painted: painted(name),
            declares_marker: found.declares_marker,
            marker: name == "marker",
            ancestor_declares,
            ..Element::default()
        });
        declarations.push(found);
        if let Some(parent) = parent {
            elements[parent].children.push(index);
        }
    }
    for targets in [&sheet_paint, &sheet_other] {
        elements.push(Element {
            referenced: resolve(targets),
            ..Element::default()
        });
    }

    let order = post_order(&elements)?;

    // Whether an element, or what it renders, sets a marker property or
    // holds a shape; and how many painted elements it draws in place.
    let mut declares = vec![false; elements.len()];
    let mut shape = vec![false; elements.len()];
    let mut painted_count = vec![0u64; elements.len()];
    for &index in &order {
        let element = &elements[index];
        let in_place = element.children.iter().chain(&element.inherited);
        let elsewhere = element.referenced.iter().chain(&element.copy_paint);
        declares[index] = element.declares_marker
            || in_place.clone().any(|&target| declares[target])
            || elsewhere
                .clone()
                .any(|&target| declares[target] || elements[target].ancestor_declares);
        shape[index] = element.vertices.is_some() || element.edges().any(|&target| shape[target]);
        painted_count[index] = in_place
            .fold(u64::from(element.painted), |count, &target| {
                count.saturating_add(painted_count[target])
            })
            .min(CAP);
    }
    for (index, element) in elements.iter().enumerate() {
        if element.marker
            && (element.ancestor_declares || declares[index] || (style_marks && shape[index]))
        {
            return Err(FailureCode::RenderResource);
        }
    }

    let first = costs(&elements, &order, &painted_count, style_marks, (0, 0));
    let markers = elements
        .iter()
        .enumerate()
        .filter(|(_, element)| element.marker)
        .fold((0, 0), |(work, depth), (index, _)| {
            (work.max(first[index].0[0]), depth.max(first[index].1[0]))
        });
    let costs = if markers == (0, 0) {
        first
    } else {
        costs(&elements, &order, &painted_count, style_marks, markers)
    };
    let (work, depth) = costs[0];
    let context = usize::from(style_marks);
    if work[context] > MAX_SVG_WORK || depth[context] > MAX_SVG_DEPTH {
        return Err(FailureCode::RenderResource);
    }
    Ok(())
}

/// Every element after everything it contains or references, refusing a
/// cycle. Iterative: a document may nest deeper than a thread's stack.
fn post_order(elements: &[Element]) -> Result<Vec<usize>, FailureCode> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    let mut marks = vec![Mark::New; elements.len()];
    let mut order = Vec::with_capacity(elements.len());
    for start in 0..elements.len() {
        if marks[start] != Mark::New {
            continue;
        }
        // (element, next edge to follow)
        let mut stack = vec![(start, 0usize)];
        marks[start] = Mark::Open;
        while let Some(top) = stack.last_mut() {
            let (index, next) = *top;
            top.1 += 1;
            let element = &elements[index];
            let lists = [
                &element.children,
                &element.inherited,
                &element.referenced,
                &element.copy_paint,
            ];
            let mut offset = next;
            let mut edge = None;
            for list in lists {
                if offset < list.len() {
                    edge = Some(list[offset]);
                    break;
                }
                offset -= list.len();
            }
            match edge {
                Some(target) => match marks[target] {
                    Mark::New => {
                        marks[target] = Mark::Open;
                        stack.push((target, 0));
                    }
                    Mark::Open => return Err(FailureCode::RenderParse),
                    Mark::Done => {}
                },
                None => {
                    marks[index] = Mark::Done;
                    order.push(index);
                    stack.pop();
                }
            }
        }
    }
    Ok(order)
}

/// Each element's expanded cost and nesting, in a context without and with
/// an inherited marker property, given the most expensive marker's cost and
/// nesting.
#[allow(clippy::type_complexity)]
fn costs(
    elements: &[Element],
    order: &[usize],
    painted_count: &[u64],
    style_marks: bool,
    (marker_work, marker_depth): (u64, usize),
) -> Vec<([u64; 2], [usize; 2])> {
    let mut costs = vec![([0u64; 2], [0usize; 2]); elements.len()];
    let defined = |target: usize| usize::from(elements[target].ancestor_declares || style_marks);
    for &index in order {
        let element = &elements[index];
        for context in 0..2 {
            let inner = usize::from(context == 1 || element.declares_marker || style_marks);
            let mut work = element.own;
            let mut depth = 0;
            for &target in element.children.iter().chain(&element.inherited) {
                work = work.saturating_add(costs[target].0[inner]);
                depth = depth.max(costs[target].1[inner]);
            }
            for &target in &element.referenced {
                work = work.saturating_add(costs[target].0[defined(target)]);
                depth = depth.max(costs[target].1[defined(target)]);
            }
            // A copy's painted elements each draw the paint it inherits.
            let painters = element.inherited.iter().fold(0u64, |count, &target| {
                count.saturating_add(painted_count[target])
            });
            for &target in &element.copy_paint {
                work =
                    work.saturating_add(painters.saturating_mul(costs[target].0[defined(target)]));
                depth = depth.max(costs[target].1[defined(target)]);
            }
            if let Some(vertices) = element.vertices
                && inner == 1
            {
                work = work.saturating_add(vertices.saturating_add(2).saturating_mul(marker_work));
                depth = depth.max(marker_depth);
            }
            costs[index].0[context] = work.min(CAP);
            costs[index].1[context] = (depth + 1).min(MAX_SVG_DEPTH + 1);
        }
    }
    costs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(body: &str) -> Result<(), FailureCode> {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">{body}</svg>"#
        );
        admit(&sanitize::parse(&svg)?)
    }

    #[test]
    fn numbers_split_as_a_parser_splits_them() {
        // 10, -20.5, .5e-1, 1e+2 and 3.
        assert_eq!(count_numbers("M10-20.5.5e-1L1e+2 3"), 5);
        assert_eq!(count_numbers("1,2 3,4"), 4);
        assert_eq!(count_numbers(""), 0);
    }

    #[test]
    fn shared_definitions_are_charged_per_use() {
        // A group of 1000 elements used 999 times is about a million units.
        let group: String = (0..1000).map(|_| "<rect/>").collect();
        let uses: String = (0..999).map(|_| r##"<use href="#g"/>"##).collect();
        assert_eq!(
            admitted(&format!(r#"<defs><g id="g">{group}</g></defs>{uses}"#)).unwrap_err(),
            FailureCode::RenderResource
        );
        let uses: String = (0..900).map(|_| r##"<use href="#g"/>"##).collect();
        assert!(admitted(&format!(r#"<defs><g id="g">{group}</g></defs>{uses}"#)).is_ok());
    }

    #[test]
    fn nested_uses_multiply() {
        // Ten levels of ten uses each: ten billion copies.
        let mut body = String::from(r#"<rect id="l0"/>"#);
        for level in 1..=10 {
            body.push_str(&format!(r#"<g id="l{level}">"#));
            for _ in 0..10 {
                body.push_str(&format!(r##"<use href="#l{}"/>"##, level - 1));
            }
            body.push_str("</g>");
        }
        assert_eq!(admitted(&body).unwrap_err(), FailureCode::RenderResource);
    }

    #[test]
    fn reference_cycles_are_refused() {
        for body in [
            r##"<g id="a"><use href="#a"/></g>"##,
            r##"<g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g>"##,
            r##"<pattern id="p"><rect fill="url(#p)"/></pattern>"##,
        ] {
            assert_eq!(
                admitted(body).unwrap_err(),
                FailureCode::RenderParse,
                "{body}"
            );
        }
        // A link to an ancestor is not rendering.
        assert!(admitted(r##"<g id="top"><a href="#top"><rect/></a></g>"##).is_ok());
    }

    #[test]
    fn markers_are_charged_per_vertex_wherever_they_come_from() {
        let marker: String = (0..100).map(|_| "<rect/>").collect();
        let points: String = (0..12_000).map(|i| format!("{i},{i} ")).collect();
        // 12,000 vertices times a 100-element marker, directly...
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><polyline points="{points}" marker-mid="url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // ...inherited through a `use`...
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><defs><polyline id="p" points="{points}"/></defs><g style="marker-mid:url(#m)"><use href="#p"/></g>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // ...or from a style sheet.
        assert_eq!(
            admitted(&format!(
                r##"<style>polyline {{ marker-mid: url(#m) }}</style><marker id="m">{marker}</marker><polyline points="{points}"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // Without a marker property in effect the same polyline is cheap.
        assert!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><polyline points="{points}"/>"##
            ))
            .is_ok()
        );
    }

    #[test]
    fn markers_that_could_hold_markers_are_refused() {
        for body in [
            r##"<marker id="a"><path d="M0 0L1 1" marker-end="url(#b)"/></marker><marker id="b"><rect/></marker>"##,
            r##"<g marker-end="url(#b)"><marker id="a"><path d="M0 0L1 1"/></marker></g><marker id="b"/>"##,
            r##"<marker id="a"><use href="#p"/></marker><path id="p" d="M0 0L1 1" marker-end="url(#a)"/>"##,
            r##"<style>path { marker-end: url(#a) }</style><marker id="a"><path d="M0 0L1 1"/></marker>"##,
        ] {
            assert_eq!(
                admitted(body).unwrap_err(),
                FailureCode::RenderResource,
                "{body}"
            );
        }
        // An arrowhead marker on many short paths is ordinary.
        let paths: String = (0..200)
            .map(|_| r##"<path d="M0 0L10 10L20 0" marker-end="url(#arrow)"/>"##)
            .collect();
        assert!(
            admitted(&format!(
                r##"<marker id="arrow"><path d="M0 0L10 5L0 10z"/></marker>{paths}"##
            ))
            .is_ok()
        );
    }

    #[test]
    fn tree_size_and_depth_are_bounded() {
        let many: String = (0..MAX_SVG_ELEMENTS).map(|_| "<g/>").collect();
        assert_eq!(admitted(&many).unwrap_err(), FailureCode::RenderResource);
        let deep = format!(
            "{}{}",
            "<g>".repeat(MAX_SVG_DEPTH),
            "</g>".repeat(MAX_SVG_DEPTH)
        );
        assert_eq!(admitted(&deep).unwrap_err(), FailureCode::RenderResource);
        let path: String = (0..MAX_SVG_WORK).map(|i| format!("L{} ", i % 10)).collect();
        assert_eq!(
            admitted(&format!(r#"<path d="M0 0{path}"/>"#)).unwrap_err(),
            FailureCode::RenderResource
        );
        let shallow = format!(
            "{}{}",
            "<g>".repeat(MAX_SVG_DEPTH - 2),
            "</g>".repeat(MAX_SVG_DEPTH - 2)
        );
        assert!(admitted(&shallow).is_ok());
    }

    #[test]
    fn horizontal_and_vertical_steps_are_vertices() {
        // One number per vertex: 12,000 of them times a 100-element marker.
        let marker: String = (0..100).map(|_| "<rect/>").collect();
        let steps: String = (0..6_000).map(|i| format!("H{i}V{i}")).collect();
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><path d="M0 0{steps}" marker-mid="url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn the_href_usvg_follows_is_the_one_counted() {
        // usvg prefers the plain `href`: the large target is what it copies.
        let large: String = (0..2_000).map(|_| "<rect/>").collect();
        let uses: String = (0..600)
            .map(|_| r##"<use xlink:href="#small" href="#large"/>"##)
            .collect();
        assert_eq!(
            admitted(&format!(
                r#"<defs><rect id="small"/><g id="large">{large}</g></defs>{uses}"#
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn namespaced_attributes_neither_count_nor_shadow() {
        let points: String = (0..12_000).map(|i| format!("{i},{i} ")).collect();
        let marker: String = (0..100).map(|_| "<rect/>").collect();
        // An empty namespaced `points` before the real one does not hide it.
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><polyline xmlns:o="urn:o" o:points="" points="{points}" marker-mid="url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn comments_do_not_hide_marker_rules() {
        let marker: String = (0..100).map(|_| "<rect/>").collect();
        let points: String = (0..12_000).map(|i| format!("{i},{i} ")).collect();
        for style in ["<style>polyline { /*c*/ marker-mid: url(#m) }</style>", ""] {
            let inline = if style.is_empty() {
                r#" style="/*c*/marker-mid:url(#m)""#
            } else {
                ""
            };
            assert_eq!(
                admitted(&format!(
                    r##"{style}<marker id="m">{marker}</marker><polyline points="{points}"{inline}/>"##
                ))
                .unwrap_err(),
                FailureCode::RenderResource,
                "{style}{inline}"
            );
        }
    }

    #[test]
    fn inherited_and_style_sheet_paint_close_cycles() {
        for body in [
            // The pattern's rectangle inherits the fill naming the pattern.
            r##"<g fill="url(#p)"><pattern id="p" width="8" height="8"><rect width="4" height="4"/></pattern><rect width="8" height="8"/></g>"##,
            // A use inside the pattern copies a shape that inherits it.
            r##"<g fill="url(#p)"><pattern id="p" width="8" height="8"><use href="#r"/></pattern></g><rect id="r" width="4" height="4"/>"##,
            // A style sheet's fills chain three patterns into a loop.
            r##"<style>.a { fill: url(#p2) } .b { fill: url(#p3) } .c { fill: url(#p1) }</style><pattern id="p1"><rect class="a"/></pattern><pattern id="p2"><rect class="b"/></pattern><pattern id="p3"><rect class="c"/></pattern><rect fill="url(#p1)"/>"##,
        ] {
            assert_eq!(
                admitted(body).unwrap_err(),
                FailureCode::RenderParse,
                "{body}"
            );
        }
        // A style sheet's gradient, whose stops are not painted, is ordinary.
        assert!(
            admitted(r##"<style>.a { fill: url(#g) }</style><linearGradient id="g"><stop offset="0"/><stop offset="1"/></linearGradient><rect class="a"/>"##)
                .is_ok()
        );
    }

    #[test]
    fn inherited_paint_is_charged_where_it_is_drawn() {
        // A thousand-element pattern inherited by a thousand rectangles,
        // directly or through a use: each draws it.
        let content: String = (0..1_000).map(|_| "<rect/>").collect();
        let rects: String = (0..1_000).map(|_| "<rect/>").collect();
        let pattern = format!(r#"<defs><pattern id="p">{content}</pattern></defs>"#);
        assert_eq!(
            admitted(&format!(r##"{pattern}<g fill="url(#p)">{rects}</g>"##)).unwrap_err(),
            FailureCode::RenderResource
        );
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<defs><g id="group">{rects}</g></defs><g fill="url(#p)"><use href="#group"/></g>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // ...and a style sheet's paint is charged to every painted element
        // (a gradient here: a sheet naming a pattern that holds painted
        // shapes is a cycle, refused as one).
        let stops: String = (0..1_000).map(|_| r#"<stop offset="0"/>"#).collect();
        assert_eq!(
            admitted(&format!(
                r##"<style>.x {{ fill: url(#g) }}</style><linearGradient id="g">{stops}</linearGradient>{rects}"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        assert_eq!(
            admitted(&format!(
                r##"<style>.x {{ fill: url(#p) }}</style>{pattern}{rects}"##
            ))
            .unwrap_err(),
            FailureCode::RenderParse
        );
    }

    #[test]
    fn style_matching_is_bounded() {
        // One rule with ten thousand selectors is ten thousand rules.
        let selectors: Vec<String> = (0..10_000).map(|i| format!(".c{i}")).collect();
        let elements: String = (0..2_000).map(|_| "<rect/>").collect();
        assert_eq!(
            admitted(&format!(
                "<style>{} {{ fill: red }}</style>{elements}",
                selectors.join(",")
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        let rules: String = (0..2_000)
            .map(|i| format!(".c{i} {{ fill: red }}"))
            .collect();
        let elements: String = (0..6_000).map(|_| "<rect/>").collect();
        assert_eq!(
            admitted(&format!("<style>{rules}</style>{elements}")).unwrap_err(),
            FailureCode::RenderResource
        );
    }
}
