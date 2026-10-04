//! Structural admission: what an SVG document costs once usvg expands its
//! references, counted on the parsed document before usvg sees it.
//!
//! Each element costs one unit, plus every number its attributes hold (path
//! data, point lists, filter tables alike), a unit per `ATTRIBUTE_BYTES` of
//! attribute text, and the characters of its text. A reference adds its target's whole
//! cost every time it is used: `use` (whose copy inherits from the `use`),
//! and paint servers, clips, masks, filters, `feImage`, text paths and the
//! templates gradients, patterns and filters link to (which inherit from
//! where they are defined). A shape that can carry markers adds, per vertex,
//! the cost of the most expensive marker. The total bounds the tree usvg
//! will build.
//!
//! This runs on the sanitized text, where CSS has already been resolved into
//! presentation attributes, so it reads attributes only. Paint is modelled
//! without modelling which declaration wins: the paint an element can be
//! drawn with is one declared on it or on an ancestor. Every element has a paint context
//! that reaches all of those: its own paint references, then its parent's
//! context. A shape or text is charged for, and linked to, its context; a
//! `use` links its copy to its own context, charged once per painted
//! element in the copy; `context-fill` and `context-stroke` reach every
//! paint server the document uses. An `id` resolves to its first and its
//! last element, as usvg resolves `use` by the first and other references
//! by the last.
//!
//! The walk refuses a reference cycle (through paint contexts as well: usvg
//! would recurse through a pattern drawn with itself without end), an
//! expanded nesting past `MAX_SVG_DEPTH` (usvg and resvg recurse that
//! deep), more than `MAX_SVG_ELEMENTS` elements, an expanded cost over
//! `MAX_SVG_WORK`, more than `MAX_VIEWPORTS` viewports nested (references
//! followed), and markers that could hold
//! markers: a marker whose content (or what it references) sets a marker
//! property, or that inherits one. usvg allows such nesting, and it
//! multiplies per vertex at every level.

use std::collections::HashMap;

use kettle_media::{FailureCode, MAX_SVG_DEPTH, MAX_SVG_ELEMENTS, MAX_SVG_WORK};
use roxmltree::{Document, Node};

use super::sanitize::{self, Urls};

/// Viewports (`svg`, `symbol`) nested in one another, references followed:
/// a percentage length takes its size from the viewport around it, so each
/// level can multiply the next.
const MAX_VIEWPORTS: usize = 8;
/// Vertices charged for a shape whose outline is not listed: a line,
/// rectangle, circle or ellipse (a rounded rectangle has at most 13 points).
const BASIC_SHAPE_VERTICES: u64 = 16;
/// The primitives a filter may hold: usvg looks up each primitive's input
/// among those before it, which is quadratic in their number.
const MAX_FILTER_PRIMITIVES: usize = 64;
/// The bytes of attribute text one unit of work stands for.
const ATTRIBUTE_BYTES: u64 = 64;
/// The copies of its paint usvg makes per character of text: for the span,
/// its laid-out chunk and its flattened outline, each with up to three
/// decorations beside the glyph.
const TEXT_PAINT_COPIES: u64 = 12;
/// Costs stop counting just past the limit, so sums cannot overflow.
const CAP: u64 = MAX_SVG_WORK + 1;

#[derive(Default)]
struct Element {
    children: Vec<usize>,
    /// Targets rendered in place that inherit from this element (`use`).
    inherited: Vec<usize>,
    /// Targets rendered with the inheritance of where they are defined.
    referenced: Vec<usize>,
    /// The paint context this element draws with: for a shape once, for
    /// text once per character, and for a `use` once per painted element in
    /// its copy.
    copy_paint: Vec<usize>,
    /// How many times a shape or text draws its paint: once for a shape,
    /// once per character for text (0 for a `use`, which counts its copy).
    pieces: u64,
    own: u64,
    /// For an element that can carry markers, its vertices.
    vertices: Option<u64>,
    /// An `svg` or `symbol`: a viewport for percentages inside it.
    viewport: bool,
    declares_marker: bool,
    marker: bool,
    ancestor_declares: bool,
    /// Bookkeeping (a paint context), not an element: adds no nesting.
    linking: bool,
}

/// What an element declares, in its attributes and `style` together.
#[derive(Default)]
struct Declared {
    /// Fragments its `fill` and `stroke` name.
    paint: Vec<String>,
    /// Whether its `fill` or `stroke` is the marker's context paint.
    context_paint: bool,
    /// Fragments its clips, masks and filters name.
    other: Vec<String>,
    declares_marker: bool,
}

fn declared(node: Node<'_, '_>) -> Result<Declared, FailureCode> {
    fn property(name: &str, value: &str, found: &mut Declared) -> Result<(), FailureCode> {
        // A value naming anything outside the document is removed by the
        // writer, so usvg never sees it: it declares nothing.
        let ids = || -> Result<Vec<String>, FailureCode> {
            Ok(match sanitize::urls(value)? {
                Urls::Local(ids) => ids.into_iter().map(str::to_owned).collect(),
                Urls::External => Vec::new(),
            })
        };
        match name {
            _ if sanitize::marker_property(name) => found.declares_marker = true,
            "fill" | "stroke" => {
                found.paint.extend(ids()?);
                found.context_paint |=
                    value.contains("context-fill") || value.contains("context-stroke");
            }
            "clip-path" | "mask" | "filter" => found.other.extend(ids()?),
            _ => {}
        }
        Ok(())
    }
    let mut found = Declared::default();
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
    // Ids must be unique: usvg resolves a duplicate differently by kind of
    // reference and by which elements it keeps, past what this can follow.
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(id) = sanitize::plain(*node, "id")
            && ids.insert(id, index).is_some()
        {
            return Err(FailureCode::RenderParse);
        }
    }
    let resolve = |targets: &[String]| -> Vec<usize> {
        targets
            .iter()
            .filter_map(|id| ids.get(id.as_str()).copied())
            .collect()
    };

    // Whether an element is text or inside it, and how many text elements
    // (itself included) it sits in: usvg copies a list of what each of them
    // sets into every positioned piece.
    let mut in_text = Vec::with_capacity(nodes.len());
    let mut text_depth: Vec<u64> = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let name = node.tag_name().name();
        let parent = parents[index];
        let inside = parent.is_some_and(|parent: usize| in_text[parent]) || name == "text";
        in_text.push(inside);
        let outer = parent.map_or(0, |parent| text_depth[parent]);
        text_depth
            .push(outer + u64::from(inside && matches!(name, "text" | "tspan" | "textPath" | "a")));
        if name == "filter"
            && node
                .children()
                .filter(|child| child.is_element() && child.tag_name().name().starts_with("fe"))
                .count()
                > MAX_FILTER_PRIMITIVES
        {
            return Err(FailureCode::RenderResource);
        }
    }

    // Indices: the elements, then each element's paint context, then the
    // stand-in for context paint.
    let count = nodes.len();
    let context_of = |index: usize| count + index;
    let context_paint = 2 * count;
    let mut elements: Vec<Element> = Vec::with_capacity(2 * count + 1);
    let mut contexts: Vec<Element> = Vec::with_capacity(count);
    let mut all_paint = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let name = node.tag_name().name();
        let found = declared(*node)?;
        let parent = parents[index];
        let paint = resolve(&found.paint);
        all_paint.extend(paint.iter().copied());
        // Context paint inherits like any paint: a descendant drawn with it
        // reaches whatever the marker's shape is painted with.
        contexts.push(Element {
            referenced: paint
                .into_iter()
                .chain(parent.map(context_of))
                .chain(found.context_paint.then_some(context_paint))
                .collect(),
            linking: true,
            ..Element::default()
        });
        let text: u64 = node
            .children()
            .filter_map(|child| child.text())
            .map(|text| text.chars().count() as u64)
            .sum();
        let mut referenced = resolve(&found.other);
        let mut paint_pieces = 0u64;
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
        let text_like = in_text[index] && matches!(name, "text" | "tspan" | "textPath" | "a");
        if text_like || painted(name) {
            // Positioned text is drawn in pieces down to a character each,
            // and each piece copies its paint: charge it per character.
            paint_pieces = if text_like {
                text.max(1).saturating_mul(TEXT_PAINT_COPIES)
            } else {
                1
            };
            copy_paint.push(context_of(index));
        } else if name == "use" {
            copy_paint.push(context_of(index));
        }
        let ancestor_declares = parent.is_some_and(|parent| {
            elements[parent].ancestor_declares || elements[parent].declares_marker
        });
        // Every number an attribute holds is parsed and kept, and every
        // attribute is copied where its element is: charge numbers and bytes.
        let attributes = node
            .attributes()
            .filter(|attribute| attribute.namespace().is_none());
        let (numbers, bytes) = attributes.fold((0u64, 0u64), |(numbers, bytes), attribute| {
            let counted = if sanitize::identifier(attribute.name()) {
                0
            } else {
                count_numbers(attribute.value())
            };
            (
                numbers.saturating_add(counted),
                bytes.saturating_add(attribute.value().len() as u64),
            )
        });
        let own = 1u64
            .saturating_add(numbers)
            .saturating_add(bytes / ATTRIBUTE_BYTES)
            .saturating_add(text.saturating_mul(text_depth[index].max(1)))
            .min(CAP);
        elements.push(Element {
            inherited,
            referenced,
            copy_paint,
            pieces: paint_pieces,
            own,
            vertices: vertices(*node),
            viewport: matches!(name, "svg" | "symbol"),
            declares_marker: found.declares_marker,
            marker: name == "marker",
            ancestor_declares,
            ..Element::default()
        });
        if let Some(parent) = parent {
            elements[parent].children.push(index);
        }
    }
    elements.extend(contexts);
    all_paint.sort_unstable();
    all_paint.dedup();
    elements.push(Element {
        referenced: all_paint,
        linking: true,
        ..Element::default()
    });

    let order = post_order(&elements)?;

    // Whether an element, or what it renders, sets a marker property; how
    // many pieces of paint it draws in place; and how many viewports nest
    // in what it renders.
    let mut declares = vec![false; elements.len()];
    let mut painted_count = vec![0u64; elements.len()];
    let mut viewports = vec![0usize; elements.len()];
    for &index in &order {
        let element = &elements[index];
        let in_place = element.children.iter().chain(&element.inherited);
        let mut elsewhere = element.referenced.iter().chain(&element.copy_paint);
        declares[index] = element.declares_marker
            || in_place.clone().any(|&target| declares[target])
            || elsewhere.any(|&target| declares[target] || elements[target].ancestor_declares);
        painted_count[index] = in_place
            .clone()
            .fold(element.pieces, |count, &target| {
                count.saturating_add(painted_count[target])
            })
            .min(CAP);
        viewports[index] = usize::from(element.viewport)
            + in_place
                .chain(&element.referenced)
                .chain(&element.copy_paint)
                .map(|&target| viewports[target])
                .max()
                .unwrap_or(0);
        if viewports[index] > MAX_VIEWPORTS {
            return Err(FailureCode::RenderResource);
        }
    }
    for (index, element) in elements.iter().enumerate() {
        if element.marker && (element.ancestor_declares || declares[index]) {
            return Err(FailureCode::RenderResource);
        }
    }

    let first = costs(&elements, &order, &painted_count, (0, 0));
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
        costs(&elements, &order, &painted_count, markers)
    };
    let (work, depth) = costs[0];
    if work[0] > MAX_SVG_WORK || depth[0] > MAX_SVG_DEPTH {
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
    (marker_work, marker_depth): (u64, usize),
) -> Vec<([u64; 2], [usize; 2])> {
    let mut costs = vec![([0u64; 2], [0usize; 2]); elements.len()];
    let defined = |target: usize| usize::from(elements[target].ancestor_declares);
    for &index in order {
        let element = &elements[index];
        for context in 0..2 {
            let inner = usize::from(context == 1 || element.declares_marker);
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
            // A shape draws its paint once, text once per character, and a
            // `use` once per painted element in its copy.
            let draws = if element.inherited.is_empty() {
                element.pieces
            } else {
                element.inherited.iter().fold(0u64, |count, &target| {
                    count.saturating_add(painted_count[target])
                })
            };
            for &target in &element.copy_paint {
                let paint = costs[target].0[defined(target)];
                work = work.saturating_add(draws.saturating_mul(paint));
                depth = depth.max(costs[target].1[defined(target)]);
            }
            if let Some(vertices) = element.vertices
                && inner == 1
            {
                work = work.saturating_add(vertices.saturating_add(2).saturating_mul(marker_work));
                depth = depth.max(marker_depth);
            }
            costs[index].0[context] = work.min(CAP);
            costs[index].1[context] =
                (depth + usize::from(!element.linking)).min(MAX_SVG_DEPTH + 1);
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
        // Admission runs on the sanitized text, as in `render`.
        let sanitized = sanitize::write(&sanitize::parse(&svg)?)?;
        admit(&sanitize::parse(&sanitized)?)
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
        // ...or inherited through a `use`.
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><defs><polyline id="p" points="{points}"/></defs><g style="marker-mid:url(#m)"><use href="#p"/></g>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // ...or set by a style sheet, which reaches the count as attributes.
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
            // Context paint reaches a pattern whose shape carries a marker.
            r##"<marker id="a"><rect fill="context-fill"/></marker><pattern id="p"><path d="M0 0L1 1" marker-end="url(#a)"/></pattern><path d="M0 0L9 9" fill="url(#p)" marker-end="url(#a)"/>"##,
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
                r##"<marker id="arrow"><path d="M0 0L10 5L0 10z" fill="context-stroke"/></marker>{paths}"##
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
    fn comments_do_not_hide_marker_declarations() {
        let marker: String = (0..100).map(|_| "<rect/>").collect();
        let points: String = (0..12_000).map(|i| format!("{i},{i} ")).collect();
        assert_eq!(
            admitted(&format!(
                r##"<marker id="m">{marker}</marker><polyline points="{points}" style="/*c:d*/marker-mid:url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }
    #[test]
    fn paint_contexts_close_cycles() {
        for body in [
            // The pattern's rectangle inherits the fill naming the pattern.
            r##"<g fill="url(#p)"><pattern id="p" width="8" height="8"><rect width="4" height="4"/></pattern><rect width="8" height="8"/></g>"##,
            // ...whatever it declares itself: `inherit`, a colour, or a
            // reference the writer removes.
            r##"<g fill="url(#p)"><pattern id="p"><rect fill="inherit"/></pattern></g>"##,
            r##"<g fill="url(#p)"><pattern id="p"><rect fill="red"/></pattern></g>"##,
            r##"<g fill="url(#p)"><pattern id="p"><rect fill="url(http://x/)"/></pattern></g>"##,
            // Three patterns chained through inherited fills.
            r##"<g fill="url(#p2)"><pattern id="p1"><rect/></pattern></g><g fill="url(#p3)"><pattern id="p2"><rect/></pattern></g><g fill="url(#p1)"><pattern id="p3"><rect/></pattern></g>"##,
            // A use inside the pattern copies a shape that inherits it.
            r##"<g fill="url(#p)"><pattern id="p" width="8" height="8"><use href="#r"/></pattern></g><rect id="r" width="4" height="4"/>"##,
        ] {
            assert_eq!(
                admitted(body).unwrap_err(),
                FailureCode::RenderParse,
                "{body}"
            );
        }
        // A style sheet naming a pattern is refused before any of this.
        assert_eq!(
            admitted(r##"<style>.a { fill: url(#p) }</style><pattern id="p"><rect class="a"/></pattern>"##)
                .unwrap_err(),
            FailureCode::RenderParse
        );
        // Patterns in definitions, filling shapes elsewhere, are ordinary.
        assert!(
            admitted(r##"<defs><pattern id="p"><rect fill="red"/></pattern><linearGradient id="g"><stop offset="0"/></linearGradient></defs><rect fill="url(#p)" stroke="url(#g)"/>"##)
                .is_ok()
        );
    }

    #[test]
    fn text_pieces_are_charged_through_uses_and_links() {
        let content: String = (0..1_000).map(|_| "<rect/>").collect();
        let pattern = format!(r#"<defs><pattern id="p">{content}</pattern></defs>"#);
        let text = "x".repeat(1_000);
        // A hundred copies of thousand-character text, each piece copying
        // the pattern its use passes down.
        let uses: String = (0..100).map(|_| r##"<use href="#t"/>"##).collect();
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<defs><text id="t">{text}</text></defs><g fill="url(#p)">{uses}</g>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // A link inside text is text too.
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<text><a fill="url(#p)">{text}</a></text>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn viewports_nest_eight_deep_at_most() {
        let nested = |depth: usize| format!("{}{}", "<svg>".repeat(depth), "</svg>".repeat(depth));
        // The root is one viewport already.
        assert!(admitted(&nested(7)).is_ok());
        assert_eq!(
            admitted(&nested(8)).unwrap_err(),
            FailureCode::RenderResource
        );
        // Symbols reached through uses count as they are drawn.
        let symbols = r##"<symbol id="a"><use href="#b"/></symbol><symbol id="b"><use href="#c"/></symbol><symbol id="c"><use href="#d"/></symbol><symbol id="d"><use href="#e"/></symbol><symbol id="e"><use href="#f"/></symbol><symbol id="f"><use href="#g"/></symbol><symbol id="g"><use href="#h"/></symbol><symbol id="h"><rect/></symbol><use href="#a"/>"##;
        assert_eq!(admitted(symbols).unwrap_err(), FailureCode::RenderResource);
    }

    #[test]
    fn attribute_payloads_are_charged_per_use() {
        let rects: String = (0..2_000)
            .map(|_| r##"<rect width="1" height="1" filter="url(#f)"/>"##)
            .collect();
        // A filter's table of 1,000 numbers (2,000 bytes), kept for every
        // element using it: the numbers, not the bytes, pass the limit.
        let table = "0 1 ".repeat(500);
        assert_eq!(
            admitted(&format!(
                r#"<filter id="f"><feComponentTransfer><feFuncR type="table" tableValues="{table}"/></feComponentTransfer></filter>{rects}"#
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // A result name of 200,000 bytes, copied likewise.
        let name = "a".repeat(200_000);
        assert_eq!(
            admitted(&format!(
                r#"<filter id="f"><feFlood result="{name}"/></filter>{rects}"#
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn decorated_positioned_text_is_charged_per_copy() {
        let content: String = (0..1_000).map(|_| "<rect/>").collect();
        let pattern = format!(r#"<defs><pattern id="p">{content}</pattern></defs>"#);
        let text = "A".repeat(400);
        let xs: Vec<String> = (0..400).map(|i| i.to_string()).collect();
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<text x="{}" fill="url(#p)" text-decoration="underline overline line-through">{text}</text>"##,
                xs.join(" ")
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn filters_hold_sixty_four_primitives_at_most() {
        let filter = |count: usize| {
            let primitives = r#"<feOffset in="missing"/>"#.repeat(count);
            format!(r##"<filter id="f">{primitives}</filter><rect filter="url(#f)"/>"##)
        };
        assert!(admitted(&filter(64)).is_ok());
        assert_eq!(
            admitted(&filter(65)).unwrap_err(),
            FailureCode::RenderResource
        );
    }

    #[test]
    fn deeply_nested_text_is_charged_per_level() {
        // usvg copies what each enclosing text element sets into every
        // positioned piece: 10,000 characters two hundred levels deep.
        let text = "x".repeat(10_000);
        let nested = format!(
            "<text>{}{text}{}</text>",
            "<tspan>".repeat(200),
            "</tspan>".repeat(200)
        );
        assert_eq!(admitted(&nested).unwrap_err(), FailureCode::RenderResource);
        assert!(admitted(&format!("<text><tspan>{text}</tspan></text>")).is_ok());
    }

    #[test]
    fn duplicate_ids_are_refused() {
        // usvg resolves a duplicate by the first or the last element
        // depending on the reference.
        assert_eq!(
            admitted(r##"<rect id="p"/><pattern id="p"><rect/></pattern><rect fill="url(#p)"/>"##)
                .unwrap_err(),
            FailureCode::RenderParse
        );
        // An id on a style sheet, which the writer drops, is no duplicate.
        assert!(
            admitted(r##"<style id="p"/><pattern id="p"><rect/></pattern><rect fill="url(#p)"/>"##)
                .is_ok()
        );
    }

    #[test]
    fn context_paint_and_text_are_charged_per_drawing() {
        let content: String = (0..1_000).map(|_| "<rect/>").collect();
        let pattern = format!(r#"<defs><pattern id="p">{content}</pattern></defs>"#);
        // Context paint inherited by the marker's content still copies the
        // shape's pattern per vertex.
        let steps: String = (0..10_000).map(|i| format!("L{i} 0")).collect();
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<marker id="m"><g fill="context-fill"><rect/></g></marker><path d="M0 0{steps}" fill="url(#p)" marker-mid="url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
        );
        // Text drawn in pieces copies its paint per character.
        let text = "x".repeat(2_000);
        assert_eq!(
            admitted(&format!(r##"{pattern}<text fill="url(#p)">{text}</text>"##)).unwrap_err(),
            FailureCode::RenderResource
        );
        assert!(admitted(&format!(r##"{pattern}<text fill="url(#p)">short</text>"##)).is_ok());
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
        // A marker drawn with context paint copies the shape's pattern for
        // every vertex.
        let steps: String = (0..10_000).map(|i| format!("L{i} 0")).collect();
        assert_eq!(
            admitted(&format!(
                r##"{pattern}<marker id="m"><rect fill="context-fill"/></marker><path d="M0 0{steps}" fill="url(#p)" marker-mid="url(#m)"/>"##
            ))
            .unwrap_err(),
            FailureCode::RenderResource
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
