//! Structural admission: what an SVG document costs once usvg expands its
//! references, counted on the parsed document before usvg sees it.
//!
//! Each element costs one unit, plus the numbers in its path data or point
//! list and the characters of its text. A reference adds its target's whole
//! cost every time it is used: `use` (whose copy inherits from the `use`),
//! and paint servers, clips, masks, filters, `feImage`, text paths and the
//! templates gradients, patterns and filters link to (which inherit from
//! where they are defined). A shape that can carry markers adds, per vertex,
//! the cost of the most expensive marker. Nothing usvg expands is left
//! uncounted, so the total bounds the tree usvg will build.
//!
//! The walk refuses a reference cycle, a document whose expanded nesting
//! passes `MAX_SVG_DEPTH` (usvg and resvg recurse that deep), more than
//! `MAX_SVG_ELEMENTS` elements, an expanded cost over `MAX_SVG_WORK`, style
//! sheets whose matching against every element would cost more than ten
//! times that, and markers that could hold markers: a marker whose content
//! (or what it references) sets a marker property, or that inherits one, or
//! that holds a shape while a style sheet sets marker properties. usvg
//! allows such nesting, and it multiplies per vertex at every level.

use std::collections::HashMap;

use kettle_media::{FailureCode, MAX_SVG_DEPTH, MAX_SVG_ELEMENTS, MAX_SVG_WORK};
use roxmltree::{Document, Node};

use super::sanitize::{self, Urls};

/// Style rules times elements: the selector matching usvg does.
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
    own: u64,
    /// For an element that can carry markers, its vertices.
    vertices: Option<u64>,
    declares_marker: bool,
    marker: bool,
    ancestor_declares: bool,
}

/// The fragment references an element makes, by property or `href`.
#[derive(Default)]
struct References<'a> {
    inherited: Vec<&'a str>,
    referenced: Vec<&'a str>,
    declares_marker: bool,
}

/// The properties whose `url(#id)` usvg follows to render something.
fn rendering_property(name: &str) -> bool {
    matches!(name, "fill" | "stroke" | "clip-path" | "mask" | "filter")
}

fn references<'a>(node: Node<'a, '_>) -> Result<References<'a>, FailureCode> {
    let mut found = References::default();
    let property = |name: &str, value: &'a str, found: &mut References<'a>| {
        if sanitize::marker_property(name) {
            found.declares_marker = true;
        } else if rendering_property(name)
            && let Urls::Local(ids) = sanitize::urls(value)?
        {
            found.referenced.extend(ids);
        }
        Ok::<(), FailureCode>(())
    };
    for attribute in node.attributes() {
        if attribute.namespace().is_some() {
            continue;
        }
        if attribute.name() == "style" {
            for declaration in sanitize::declarations(attribute.value())? {
                property(declaration.name, declaration.value, &mut found)?;
            }
        } else {
            property(attribute.name(), attribute.value(), &mut found)?;
        }
    }
    if let Some(id) = sanitize::href(node) {
        match node.tag_name().name() {
            "use" => found.inherited.push(id),
            "feImage" | "textPath" | "tref" | "pattern" | "linearGradient" | "radialGradient"
            | "filter" => found.referenced.push(id),
            _ => {}
        }
    }
    Ok(found)
}

/// The numbers in path data or a point list, counted the way a parser
/// splits them (`1-2.5.5e-1` is three).
fn numbers(text: &str) -> u64 {
    let mut count = 0u64;
    let mut in_number = false;
    let mut dot = false;
    let mut exponent = false;
    let mut previous = b' ';
    for &byte in text.as_bytes() {
        match byte {
            b'0'..=b'9' => {
                if !in_number {
                    count += 1;
                    in_number = true;
                    dot = false;
                    exponent = false;
                }
            }
            b'.' => {
                if !in_number || dot || exponent {
                    count += 1;
                    exponent = false;
                }
                in_number = true;
                dot = true;
            }
            b'+' | b'-' => {
                if !(in_number && matches!(previous, b'e' | b'E')) {
                    count += 1;
                    dot = false;
                    exponent = false;
                }
                in_number = true;
            }
            b'e' | b'E' if in_number => exponent = true,
            _ => in_number = false,
        }
        previous = byte;
    }
    count
}

fn vertices(node: Node<'_, '_>) -> Option<u64> {
    match node.tag_name().name() {
        "path" => Some(numbers(node.attribute("d").unwrap_or_default()) / 2),
        "polyline" | "polygon" => Some(numbers(node.attribute("points").unwrap_or_default()) / 2),
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
        if let Some(id) = node.attribute("id") {
            ids.entry(id).or_default().push(index);
        }
    }
    let resolve = |targets: Vec<&str>| -> Vec<usize> {
        targets
            .into_iter()
            .flat_map(|id| ids.get(id).into_iter().flatten().copied())
            .collect()
    };

    let mut elements: Vec<Element> = Vec::with_capacity(nodes.len());
    let mut style_marks = false;
    let mut style_rules = 0u64;
    for (index, node) in nodes.iter().enumerate() {
        let references = references(*node)?;
        let text: u64 = node
            .children()
            .filter_map(|child| child.text())
            .map(|text| text.chars().count() as u64)
            .sum();
        if node.tag_name().name() == "style" {
            let css: String = node.children().filter_map(|child| child.text()).collect();
            style_marks |= sanitize::check_style_sheet(&css)?;
            style_rules += css.matches('{').count() as u64;
        }
        let ancestor_declares = parents[index].is_some_and(|parent| {
            elements[parent].ancestor_declares || elements[parent].declares_marker
        });
        let mut element = Element {
            inherited: resolve(references.inherited),
            referenced: resolve(references.referenced),
            own: 1u64
                .saturating_add(numbers(node.attribute("d").unwrap_or_default()))
                .saturating_add(numbers(node.attribute("points").unwrap_or_default()))
                .saturating_add(text),
            vertices: vertices(*node),
            declares_marker: references.declares_marker,
            marker: node.tag_name().name() == "marker",
            ancestor_declares,
            ..Element::default()
        };
        element.own = element.own.min(CAP);
        elements.push(element);
        if let Some(parent) = parents[index] {
            elements[parent].children.push(index);
        }
    }
    if style_rules.saturating_mul(all as u64) > MAX_STYLE_MATCHING {
        return Err(FailureCode::RenderResource);
    }

    let order = post_order(&elements)?;

    // Whether an element, or what it renders, sets a marker property or
    // holds a shape.
    let mut declares = vec![false; elements.len()];
    let mut shape = vec![false; elements.len()];
    for &index in &order {
        let element = &elements[index];
        let in_place = element.children.iter().chain(&element.inherited);
        declares[index] = element.declares_marker
            || in_place.clone().any(|&target| declares[target])
            || element
                .referenced
                .iter()
                .any(|&target| declares[target] || elements[target].ancestor_declares);
        shape[index] = element.vertices.is_some()
            || in_place
                .chain(&element.referenced)
                .any(|&target| shape[target]);
    }
    for (index, element) in elements.iter().enumerate() {
        if element.marker
            && (element.ancestor_declares || declares[index] || (style_marks && shape[index]))
        {
            return Err(FailureCode::RenderResource);
        }
    }

    let first = costs(&elements, &order, style_marks, (0, 0));
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
        costs(&elements, &order, style_marks, markers)
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
            let in_place = element.children.len() + element.inherited.len();
            let edge = if next < element.children.len() {
                Some(element.children[next])
            } else if next < in_place {
                Some(element.inherited[next - element.children.len()])
            } else {
                element.referenced.get(next - in_place).copied()
            };
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
    style_marks: bool,
    (marker_work, marker_depth): (u64, usize),
) -> Vec<([u64; 2], [usize; 2])> {
    let mut costs = vec![([0u64; 2], [0usize; 2]); elements.len()];
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
                let defined = usize::from(elements[target].ancestor_declares || style_marks);
                work = work.saturating_add(costs[target].0[defined]);
                depth = depth.max(costs[target].1[defined]);
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
        assert_eq!(numbers("M10-20.5.5e-1L1e+2 3"), 5);
        assert_eq!(numbers("1,2 3,4"), 4);
        assert_eq!(numbers(""), 0);
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
    fn style_matching_is_bounded() {
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
