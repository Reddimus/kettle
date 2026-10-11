//! Style sheets and `style` attributes, resolved here so usvg never sees CSS.
//!
//! usvg runs its own CSS engine, whose reach (which elements a selector
//! matches, how often it matches again in `use` copies, how it splits a
//! property name or an `!important`) is more than the structural count can
//! follow. So the writer applies CSS itself, the way usvg would, and writes
//! the result as presentation attributes: no `<style>` and none of the
//! document's own CSS reach usvg (the one `style` attribute it may see is
//! composed here, of checked keywords for the three properties usvg reads
//! only from CSS), and the structural count sees exactly what usvg draws.
//!
//! The order is usvg's: an element's presentation attributes, then the
//! matching rules in ascending specificity (a later rule winning a tie), then
//! its `style` attribute, each declaration replacing the value before it.
//! Only presentation properties apply, as in usvg; the `marker` shorthand
//! sets all three marker properties and the `font` shorthand its parts.
//!
//! Selectors are simple only: a type or `*`, then any `.class` and `#id`
//! parts, in comma-separated lists. A combinator, pseudo-class, attribute
//! selector or at-rule refuses the document, as does any `!` in a value; so
//! the matching cost is rules times elements, bounded before it is paid.

use std::rc::Rc;

use kettle_media::{FailureCode, MAX_SVG_WORK};
use roxmltree::Node;

use super::sanitize;

/// Rules and the declarations they apply, times elements: the matching and
/// applying done here.
const MAX_MATCHING: u64 = 10 * MAX_SVG_WORK;

/// A declaration, shared by every selector of its rule and every element it
/// applies to rather than copied.
pub(super) type Applied = (Rc<str>, Rc<str>);

/// The properties usvg takes from CSS (its `is_presentation` list).
pub(super) fn presentation(name: &str) -> bool {
    matches!(
        name,
        "alignment-baseline"
            | "baseline-shift"
            | "background-color"
            | "clip-path"
            | "clip-rule"
            | "color"
            | "color-interpolation"
            | "color-interpolation-filters"
            | "color-rendering"
            | "direction"
            | "display"
            | "dominant-baseline"
            | "fill"
            | "fill-opacity"
            | "fill-rule"
            | "filter"
            | "flood-color"
            | "flood-opacity"
            | "font-family"
            | "font-kerning"
            | "font-optical-sizing"
            | "font-size"
            | "font-size-adjust"
            | "font-stretch"
            | "font-style"
            | "font-variant"
            | "font-weight"
            | "font-variation-settings"
            | "glyph-orientation-horizontal"
            | "glyph-orientation-vertical"
            | "image-rendering"
            | "isolation"
            | "letter-spacing"
            | "lighting-color"
            | "marker-end"
            | "marker-mid"
            | "marker-start"
            | "mask"
            | "mask-type"
            | "mix-blend-mode"
            | "opacity"
            | "overflow"
            | "paint-order"
            | "shape-rendering"
            | "stop-color"
            | "stop-opacity"
            | "stroke"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stroke-opacity"
            | "stroke-width"
            | "text-anchor"
            | "text-decoration"
            | "text-overflow"
            | "text-rendering"
            | "transform"
            | "transform-origin"
            | "unicode-bidi"
            | "vector-effect"
            | "visibility"
            | "white-space"
            | "word-spacing"
            | "writing-mode"
    )
}

/// The properties usvg reads from CSS but ignores as attributes: written back
/// in a `style` attribute composed here, from a keyword checked here.
pub(super) fn css_only(name: &str) -> bool {
    matches!(name, "mix-blend-mode" | "isolation" | "font-kerning")
}

/// Whether `value` is a keyword usvg reads for a CSS-only property.
fn css_only_value(name: &str, value: &str) -> bool {
    match name {
        "mix-blend-mode" => matches!(
            value,
            "normal"
                | "multiply"
                | "screen"
                | "overlay"
                | "darken"
                | "lighten"
                | "color-dodge"
                | "color-burn"
                | "hard-light"
                | "soft-light"
                | "difference"
                | "exclusion"
                | "hue"
                | "saturation"
                | "color"
                | "luminosity"
        ),
        "isolation" => matches!(value, "auto" | "isolate"),
        "font-kerning" => matches!(value, "auto" | "normal" | "none"),
        _ => false,
    }
}

/// One simple selector: an optional type, then classes and ids.
#[derive(Debug, PartialEq)]
struct Selector {
    /// `None` for `*` or no type.
    element: Option<String>,
    classes: Vec<String>,
    ids: Vec<String>,
}

impl Selector {
    /// usvg's (simplecss's) specificity: ids, then classes, then types.
    fn specificity(&self) -> (usize, usize, usize) {
        (
            self.ids.len(),
            self.classes.len(),
            usize::from(self.element.is_some()),
        )
    }

    fn matches(&self, node: Node<'_, '_>) -> bool {
        if self
            .element
            .as_ref()
            .is_some_and(|element| element != node.tag_name().name())
        {
            return false;
        }
        let id = sanitize::plain(node, "id");
        if !self.ids.iter().all(|wanted| id == Some(wanted.as_str())) {
            return false;
        }
        let classes = sanitize::plain(node, "class").unwrap_or_default();
        self.classes.iter().all(|wanted| {
            classes
                .split_ascii_whitespace()
                .any(|class| class == wanted)
        })
    }
}

fn identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Parse a simple selector, or refuse it.
fn selector(text: &str) -> Result<Selector, FailureCode> {
    let text = text.trim();
    let mut selector = Selector {
        element: None,
        classes: Vec::new(),
        ids: Vec::new(),
    };
    let mut rest = text;
    if let Some(after) = rest.strip_prefix('*') {
        rest = after;
    } else {
        let end = rest
            .find(|c: char| !identifier_char(c))
            .unwrap_or(rest.len());
        if end > 0 {
            selector.element = Some(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    while let Some(kind) = rest.chars().next() {
        let after = &rest[kind.len_utf8()..];
        let end = after
            .find(|c: char| !identifier_char(c))
            .unwrap_or(after.len());
        if end == 0 {
            return Err(FailureCode::RenderParse);
        }
        let name = after[..end].to_owned();
        match kind {
            '.' => selector.classes.push(name),
            '#' => selector.ids.push(name),
            _ => return Err(FailureCode::RenderParse),
        }
        rest = &after[end..];
    }
    if text.is_empty() {
        return Err(FailureCode::RenderParse);
    }
    Ok(selector)
}

/// A rule: one selector and its checked declarations.
struct Rule {
    selector: Selector,
    declarations: Rc<[Applied]>,
}

/// The style sheets of a document, parsed and in usvg's order.
pub(super) struct StyleSheets {
    rules: Vec<Rule>,
}

/// A property name: lower-case letters and hyphens only (simplecss would
/// strip a leading `*`, or take others, that this would not).
fn property_name(name: &str) -> Result<&str, FailureCode> {
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return Err(FailureCode::RenderParse);
    }
    Ok(name)
}

/// The declarations of a rule body or a `style` attribute, checked, with
/// shorthands expanded and non-presentation properties left out.
pub(super) fn declarations(body: &str) -> Result<Vec<(String, String)>, FailureCode> {
    let mut out = Vec::new();
    for declaration in sanitize::declarations(body)? {
        let name = property_name(declaration.name)?;
        // `check_declaration` refuses any `!` or backslash in what applies.
        let value = declaration.value.trim();
        // A value naming anything outside the document is left out.
        if sanitize::urls(value)? == sanitize::Urls::External {
            continue;
        }
        match name {
            "marker" => {
                let value = sanitize::check_declaration("marker", value)?;
                for part in ["marker-start", "marker-mid", "marker-end"] {
                    out.push((part.to_owned(), value.clone()));
                }
            }
            "font" => out.extend(font(value)?),
            // Kept only as one of the keywords usvg reads.
            _ if css_only(name) => {
                let keyword = value.to_ascii_lowercase();
                if css_only_value(name, &keyword) {
                    out.push((name.to_owned(), keyword));
                }
            }
            _ if presentation(name) => {
                out.push((name.to_owned(), sanitize::check_declaration(name, value)?));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The `font` shorthand, as its parts: style, variant, weight and stretch
/// keywords, then a size (absolute, with an optional `/line-height`), then
/// the family. A value that does not read so is ignored, as usvg ignores it.
fn font(value: &str) -> Result<Vec<(String, String)>, FailureCode> {
    let mut parts = vec![
        ("font-style", "normal".to_owned()),
        ("font-variant", "normal".to_owned()),
        ("font-weight", "normal".to_owned()),
        ("font-stretch", "normal".to_owned()),
    ];
    let mut rest = value.trim();
    loop {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..end];
        let lower = token.to_ascii_lowercase();
        let slot = match lower.as_str() {
            "normal" => None,
            "italic" | "oblique" => Some(0),
            "small-caps" => Some(1),
            "bold" | "bolder" | "lighter" | "100" | "200" | "300" | "400" | "500" | "600"
            | "700" | "800" | "900" => Some(2),
            "ultra-condensed" | "extra-condensed" | "condensed" | "semi-condensed"
            | "semi-expanded" | "expanded" | "extra-expanded" | "ultra-expanded" => Some(3),
            _ => break,
        };
        if let Some(slot) = slot {
            parts[slot].1 = lower;
        }
        rest = rest[end..].trim_start();
    }
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let size = rest[..end].split('/').next().unwrap_or_default();
    let family = rest[end..].trim();
    if family.is_empty() || !size.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return Ok(Vec::new());
    }
    let mut out: Vec<(String, String)> = parts
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    out.push((
        "font-size".to_owned(),
        sanitize::check_declaration("font-size", size)?,
    ));
    out.push((
        "font-family".to_owned(),
        sanitize::check_declaration("font-family", family)?,
    ));
    Ok(out)
}

impl StyleSheets {
    /// Parse every `<style>` element's text (comments removed), refusing
    /// what this does not resolve, and bound the matching against `elements`.
    pub(super) fn parse(css: &str, elements: usize) -> Result<Self, FailureCode> {
        let text = sanitize::strip_comments(css)?;
        if text.contains('@') || text.contains('\\') {
            return Err(FailureCode::RenderParse);
        }
        let mut rules = Vec::new();
        let mut rest = text.as_str();
        while let Some(open) = rest.find('{') {
            let close = rest[open..]
                .find('}')
                .map(|close| open + close)
                .ok_or(FailureCode::RenderParse)?;
            let body = &rest[open + 1..close];
            if body.contains('{') {
                return Err(FailureCode::RenderParse);
            }
            let declarations: Rc<[Applied]> = declarations(body)?
                .into_iter()
                .map(|(name, value)| (Rc::from(name), Rc::from(value)))
                .collect();
            for part in rest[..open].split(',') {
                rules.push(Rule {
                    selector: selector(part)?,
                    declarations: Rc::clone(&declarations),
                });
            }
            rest = &rest[close + 1..];
        }
        if !rest.trim().is_empty() {
            return Err(FailureCode::RenderParse);
        }
        // Each rule is matched against every element and may apply all its
        // declarations to each: bound that before any of it is done.
        let per_element = rules.iter().fold(0u64, |total, rule| {
            total.saturating_add(1 + rule.declarations.len() as u64)
        });
        if per_element.saturating_mul(elements as u64) > MAX_MATCHING {
            return Err(FailureCode::RenderResource);
        }
        // A stable sort: a later rule of equal specificity still wins.
        rules.sort_by_key(|rule| rule.selector.specificity());
        Ok(Self { rules })
    }

    /// The presentation properties CSS sets on `node`, its `style` attribute
    /// last, in the order they apply (a later one replacing an earlier).
    pub(super) fn applied(&self, node: Node<'_, '_>) -> Result<Vec<Applied>, FailureCode> {
        let mut out = Vec::new();
        for rule in &self.rules {
            if rule.selector.matches(node) {
                out.extend(rule.declarations.iter().cloned());
            }
        }
        if let Some(style) = sanitize::plain(node, "style") {
            let style = declarations(&sanitize::strip_comments(style)?)?;
            out.extend(
                style
                    .into_iter()
                    .map(|(name, value)| (Rc::from(name), Rc::from(value))),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_selectors_parse_and_others_are_refused() {
        assert_eq!(
            selector("rect.a.b#c").unwrap(),
            Selector {
                element: Some("rect".to_owned()),
                classes: vec!["a".to_owned(), "b".to_owned()],
                ids: vec!["c".to_owned()],
            }
        );
        assert!(selector("*").is_ok());
        assert!(selector(".x").is_ok());
        for refused in [
            "g rect", "g > rect", "a + b", "a ~ b", "a:hover", "[x]", "", ".", "é", "rect.é",
        ] {
            assert_eq!(
                selector(refused).unwrap_err(),
                FailureCode::RenderParse,
                "{refused}"
            );
        }
    }

    #[test]
    fn sheets_refuse_what_is_not_resolved_here() {
        for css in [
            "@media screen { rect { fill: red } }",
            "rect { fill: red !important }",
            "rect { fill: red ! important }",
            "rect { *fill: red }",
            "rect { fill: \\72 ed }",
            "rect { fill: red",
            "rect { a { fill: red } }",
            "stray",
        ] {
            assert!(StyleSheets::parse(css, 1).is_err(), "{css}");
        }
    }

    #[test]
    fn rules_and_declarations_apply_ten_million_times_at_most() {
        // Ten thousand rules of one declaration: 20,000 per element.
        let selectors: Vec<String> = (0..10_000).map(|i| format!(".c{i}")).collect();
        let css = format!("{} {{ fill: red }}", selectors.join(","));
        assert_eq!(
            StyleSheets::parse(&css, 501).err(),
            Some(FailureCode::RenderResource)
        );
        assert!(StyleSheets::parse(&css, 499).is_ok());
        // Many declarations count as much as many rules.
        let declarations = "fill: red;".repeat(20_000);
        assert_eq!(
            StyleSheets::parse(&format!(".a {{ {declarations} }}"), 501).err(),
            Some(FailureCode::RenderResource)
        );
    }

    #[test]
    fn shorthands_expand_and_only_presentation_properties_apply() {
        assert_eq!(
            declarations("marker: url(#m); width: 100; fill: red").unwrap(),
            [
                ("marker-start".to_owned(), "url(#m)".to_owned()),
                ("marker-mid".to_owned(), "url(#m)".to_owned()),
                ("marker-end".to_owned(), "url(#m)".to_owned()),
                ("fill".to_owned(), "red".to_owned()),
            ]
        );
        let parts = declarations("font: italic bold 14px/1.2 'Noto Sans', serif").unwrap();
        assert!(parts.contains(&("font-style".to_owned(), "italic".to_owned())));
        assert!(parts.contains(&("font-weight".to_owned(), "bold".to_owned())));
        assert!(parts.contains(&("font-size".to_owned(), "14px".to_owned())));
        assert!(parts.contains(&("font-family".to_owned(), "'Noto Sans', serif".to_owned())));
        assert_eq!(
            declarations("font: 2em serif").unwrap_err(),
            FailureCode::RenderParse
        );
    }
}
