//! The SVG a job holds, rewritten so usvg can reach nothing outside it.
//!
//! The document is parsed once with a node limit and no DTD (so no entity
//! can be declared or expanded), then written back element by element:
//!
//! - Elements outside the SVG namespace (editor metadata) are dropped, and so
//!   are `script` and `foreignObject` with everything inside them: usvg renders
//!   neither, and an embedded HTML or script subtree is never kept.
//! - An `href` survives only as a local fragment (`#id`); any other value
//!   (`file:`, `http:`, `data:`, a relative or absolute path) is removed, so
//!   an image, `use` or `feImage` can name nothing but this document.
//! - A property value may refer only to local fragments (`url(#id)`); a
//!   declaration or attribute naming anything else is removed. A value whose
//!   references cannot be read plainly (a backslash escape, an unclosed
//!   `url(`) refuses the document rather than be guessed at, and a style
//!   sheet may not import or name anything at all, even in the document:
//!   which elements a rule reaches is not something the structural count can
//!   know.
//! - Event attributes, `xml:base`, comments and processing instructions are
//!   dropped, and of namespaced attributes only `xlink:href` and `xml:space`
//!   survive (usvg reads others by their local name). Of an element's two
//!   `href` spellings only the one usvg uses is kept: the plain one when it
//!   names a fragment.
//! - Comments are removed from style sheets and `style` attributes before
//!   either is read, and the stripped text is what usvg sees.
//! - `!important` is refused, and so is a font size relative to an inherited
//!   one (`em`, `ex`, `%`, `larger`, `smaller`), whose chains multiply past
//!   any bound; an `feConvolveMatrix` order may not pass `MAX_KERNEL_ORDER`.
//! - Every number in a property value or style declaration must lie within
//!   `MAX_NUMBER` and, unless zero, at least `MIN_NUMBER`, so the products
//!   usvg forms (a marker's size times a stroke width, a radius times a
//!   scale) stay finite and non-zero where it unwraps them. Hex colors and
//!   fragment names are not numbers; identifiers (`id`, `class`, filter
//!   result names) are not read.
//!
//! Values are escaped here (`&`, `<`, `>`, quotes, and in attributes tabs and
//! line breaks), never by substring replacement on the output, because
//! `xmlwriter` escapes only `<` in text and only the quote in attributes.
//! usvg's own resolvers are also set to load nothing, so a reference this
//! misses still reads no file and no network.

use kettle_media::FailureCode;
use roxmltree::{Document, Node, ParsingOptions};
use xmlwriter::{Indent, Options, XmlWriter};

pub(super) const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub(super) const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Nodes of every kind (elements, text, comments) the parser will build.
const MAX_NODES: u32 = 500_000;
/// The rewritten document's size.
pub(super) const MAX_SANITIZED_BYTES: usize = 8 * 1024 * 1024;
/// The largest convolution kernel side: usvg multiplies the two in 32 bits.
const MAX_KERNEL_ORDER: f64 = 64.0;
/// The largest number a property may hold: ten million user units, far past
/// any drawing, and five such factors stay finite in `f32`.
const MAX_NUMBER: f64 = 1e7;
/// The smallest non-zero magnitude: below the rounding noise editors write
/// (about 6e-17), while two such factors stay above `f32`'s smallest value.
const MIN_NUMBER: f64 = 1e-20;

/// Parse `text` as XML with no DTD and bounded nodes.
pub(super) fn parse(text: &str) -> Result<Document<'_>, FailureCode> {
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_NODES,
        ..ParsingOptions::default()
    };
    Document::parse_with_options(text, options).map_err(|error| match error {
        roxmltree::Error::NodesLimitReached => FailureCode::RenderResource,
        _ => FailureCode::RenderParse,
    })
}

/// An element that is written back: in the SVG namespace, and not one whose
/// whole subtree is dropped.
pub(super) fn kept(node: Node<'_, '_>) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some(SVG_NS)
        && !matches!(node.tag_name().name(), "script" | "foreignObject")
}

/// What an element's `href` names, as usvg reads it: the plain attribute when
/// it is a local fragment, else `xlink:href` when that is one (a plain value
/// naming anything else is removed, leaving the `xlink:` one).
pub(super) fn href<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    let named = |namespace: Option<&str>| {
        node.attributes()
            .find(|attribute| attribute.name() == "href" && attribute.namespace() == namespace)
            .and_then(|attribute| fragment(attribute.value()))
    };
    named(None).or_else(|| named(Some(XLINK_NS)))
}

/// A plain (unnamespaced) attribute: roxmltree's lookup by name alone also
/// matches namespaced attributes the writer drops.
pub(super) fn plain<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.namespace().is_none() && attribute.name() == name)
        .map(|attribute| attribute.value())
}

/// `#id`, with surrounding whitespace, as the id; anything else is `None`.
pub(super) fn fragment(value: &str) -> Option<&str> {
    let id = value.trim().strip_prefix('#')?;
    (!id.is_empty() && id.chars().all(id_char)).then_some(id)
}

fn id_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '#' | '"' | '\'' | '(' | ')' | '\\' | ';' | ',')
}

/// The `url(...)` references in a property value.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Urls<'a> {
    /// Only local fragments (possibly none), by id.
    Local(Vec<&'a str>),
    /// At least one reference outside the document.
    External,
}

/// Read every `url(` in `value` (in any case, quoted or not). A reference that
/// cannot be read plainly refuses the document.
pub(super) fn urls(value: &str) -> Result<Urls<'_>, FailureCode> {
    let bytes = value.as_bytes();
    let mut ids = Vec::new();
    let mut external = false;
    let mut at = 0;
    while let Some(offset) = bytes[at..]
        .windows(4)
        .position(|window| window.eq_ignore_ascii_case(b"url("))
    {
        let mut index = at + offset + 4;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let quote = match bytes.get(index) {
            Some(&quote @ (b'"' | b'\'')) => {
                index += 1;
                Some(quote)
            }
            _ => None,
        };
        let start = index;
        let end = loop {
            match (bytes.get(index), quote) {
                (None, _) => return Err(FailureCode::RenderParse),
                (Some(&byte), Some(quote)) if byte == quote => break index,
                (Some(b')'), None) => break index,
                (Some(b'"' | b'\'' | b'(' | b'\\'), None) => return Err(FailureCode::RenderParse),
                (Some(byte), None) if byte.is_ascii_whitespace() => break index,
                _ => index += 1,
            }
        };
        let argument = &value[start..end];
        index = end + usize::from(quote.is_some());
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b')') {
            return Err(FailureCode::RenderParse);
        }
        match fragment(argument) {
            Some(id) if argument.trim() == argument => ids.push(id),
            _ => external = true,
        }
        at = index + 1;
    }
    Ok(if external {
        Urls::External
    } else {
        Urls::Local(ids)
    })
}

/// The properties that place markers on a shape.
pub(super) fn marker_property(name: &str) -> bool {
    matches!(
        name,
        "marker" | "marker-start" | "marker-mid" | "marker-end"
    )
}

/// One CSS declaration: its property name and value.
pub(super) struct Declaration<'a> {
    pub(super) name: &'a str,
    pub(super) value: &'a str,
    /// The declaration's text, to write back.
    pub(super) text: &'a str,
}

/// `css` without its comments (each becomes a space). An unclosed comment
/// refuses the document.
pub(super) fn strip_comments(css: &str) -> Result<String, FailureCode> {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        out.push(' ');
        let end = rest[start + 2..]
            .find("*/")
            .ok_or(FailureCode::RenderParse)?;
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Call `each` with every number in `text`, split the way a parser splits
/// them (`1-2.5.5e-1` is three). Not necessarily a valid number: a lone sign
/// is returned as it is.
pub(super) fn numbers(text: &str, mut each: impl FnMut(&str)) {
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let mut dot = false;
    let mut exponent = false;
    let mut previous = b' ';
    for (index, &byte) in bytes.iter().enumerate() {
        let begins = match byte {
            b'0'..=b'9' => start.is_none(),
            b'.' => start.is_none() || dot || exponent,
            b'+' | b'-' => !(start.is_some() && matches!(previous, b'e' | b'E')),
            b'e' | b'E' if start.is_some() => {
                exponent = true;
                false
            }
            _ => {
                if let Some(begun) = start.take() {
                    each(&text[begun..index]);
                }
                previous = byte;
                continue;
            }
        };
        if begins {
            if let Some(begun) = start {
                each(&text[begun..index]);
            }
            start = Some(index);
            dot = false;
            exponent = false;
        }
        if byte == b'.' {
            dot = true;
        }
        previous = byte;
    }
    if let Some(begun) = start {
        each(&text[begun..]);
    }
}

/// Refuse a value holding a number past `MAX_NUMBER` or, unless zero, below
/// `MIN_NUMBER`. `url(...)` references and `#` names (hex colors, fragments)
/// are skipped first.
pub(super) fn check_numbers(value: &str) -> Result<(), FailureCode> {
    let mut plain = String::with_capacity(value.len());
    let mut rest = value;
    loop {
        let lower = rest.to_ascii_lowercase();
        match (lower.find("url("), rest.find('#')) {
            (Some(url), hash) if hash.is_none_or(|hash| url < hash) => {
                plain.push_str(&rest[..url]);
                plain.push(' ');
                let end = rest[url..]
                    .find(')')
                    .map_or(rest.len(), |end| url + end + 1);
                rest = &rest[end..];
            }
            (_, Some(hash)) => {
                plain.push_str(&rest[..hash]);
                plain.push(' ');
                let name = rest[hash + 1..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                    .map_or(rest.len(), |end| hash + 1 + end);
                rest = &rest[name..];
            }
            _ => {
                plain.push_str(rest);
                break;
            }
        }
    }
    let mut refused = false;
    numbers(&plain, |token| {
        if let Ok(number) = token.parse::<f64>() {
            let magnitude = number.abs();
            refused |= !magnitude.is_finite()
                || magnitude > MAX_NUMBER
                || (magnitude != 0.0 && magnitude < MIN_NUMBER);
        }
    });
    if refused {
        return Err(FailureCode::RenderParse);
    }
    Ok(())
}

/// Attributes that name things rather than measure them.
fn identifier(name: &str) -> bool {
    matches!(
        name,
        "id" | "class"
            | "lang"
            | "result"
            | "in"
            | "in2"
            | "requiredFeatures"
            | "requiredExtensions"
            | "systemLanguage"
    ) || name.starts_with("data-")
        || name.starts_with("aria-")
}

/// The declarations of a `style` attribute (comments already removed): split
/// at semicolons outside quotes and parentheses. Unbalanced text refuses the
/// document.
pub(super) fn declarations(style: &str) -> Result<Vec<Declaration<'_>>, FailureCode> {
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let mut start = 0;
    for (index, c) in style.char_indices().chain([(style.len(), ';')]) {
        match (c, quote) {
            (c, Some(open)) if c == open => quote = None,
            (_, Some(_)) => {}
            ('"' | '\'', None) => quote = Some(c),
            ('(', None) => depth += 1,
            (')', None) => depth = depth.checked_sub(1).ok_or(FailureCode::RenderParse)?,
            (';', None) if depth == 0 => {
                let text = &style[start..index];
                start = index + 1;
                if text.trim().is_empty() {
                    continue;
                }
                let (name, value) = text.split_once(':').ok_or(FailureCode::RenderParse)?;
                found.push(Declaration {
                    name: name.trim(),
                    value,
                    text,
                });
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(FailureCode::RenderParse);
    }
    Ok(found)
}

/// What a style sheet holds, once checked.
#[derive(Debug, Default)]
pub(super) struct StyleSheet {
    /// The sheet without comments: what usvg is given.
    pub(super) text: String,
    /// Selectors, counting each in a comma-separated list: the rules usvg
    /// matches against every element.
    pub(super) selectors: u64,
}

/// A style sheet may style but never name: no `url(` (so no paint server,
/// clip, mask, filter or marker can come from a rule, whose reach the
/// structural count cannot know), no import, no escapes that could spell
/// either, no `!important`, no relative font size and no number past the
/// bounds.
pub(super) fn check_style_sheet(css: &str) -> Result<StyleSheet, FailureCode> {
    let text = strip_comments(css)?;
    let lower = text.to_ascii_lowercase();
    if text.contains('\\') || lower.contains("@import") || lower.contains("url(") {
        return Err(FailureCode::RenderParse);
    }
    // A segment ending at `{` is a selector list or an at-rule prelude; one
    // ending at `;` or `}` is a declaration.
    let mut sheet = StyleSheet::default();
    let mut start = 0;
    for (index, c) in text.char_indices() {
        if !matches!(c, '{' | '}' | ';') {
            continue;
        }
        let segment = &text[start..index];
        start = index + 1;
        if c == '{' {
            if !segment.trim_start().starts_with('@') {
                sheet.selectors += 1 + segment.matches(',').count() as u64;
            }
            continue;
        }
        if let Some((name, value)) = segment.split_once(':') {
            check_declaration(name.trim(), value)?;
        }
    }
    sheet.text = text;
    Ok(sheet)
}

/// A declaration, in a style sheet or a `style` attribute, or a presentation
/// attribute: no `!important` (which would let a lower declaration win, past
/// what the structural count follows), no font size relative to an
/// inherited one (`em`, `ex`, `%`, `larger`, `smaller`: a chain of them
/// multiplies past any bound), and no number past the bounds.
fn check_declaration(name: &str, value: &str) -> Result<(), FailureCode> {
    if value.to_ascii_lowercase().contains("!important") {
        return Err(FailureCode::RenderParse);
    }
    if matches!(name, "font-size" | "font") {
        let relative = value.split_ascii_whitespace().any(|token| {
            let token = token.to_ascii_lowercase();
            token.ends_with("em")
                || token.ends_with("ex")
                || token.ends_with('%')
                || matches!(token.as_str(), "larger" | "smaller")
        });
        if relative {
            return Err(FailureCode::RenderParse);
        }
    }
    check_numbers(value)
}

/// What an attribute becomes when written back.
enum Kept {
    /// Written under this name with its value as it is.
    As(String),
    /// Written under this name with this value.
    Rewritten(String, String),
    /// Removed.
    Dropped,
}

/// What to write for `attribute`; `plain_href` is whether its element has a
/// plain `href` naming a fragment, which usvg prefers to `xlink:href`.
fn attribute_policy(
    attribute: &roxmltree::Attribute<'_, '_>,
    plain_href: bool,
) -> Result<Kept, FailureCode> {
    let local = attribute.name();
    let name = match attribute.namespace() {
        None => local.to_owned(),
        Some(XLINK_NS) if local == "href" && !plain_href => "xlink:href".to_owned(),
        Some(XML_NS) if local == "space" => "xml:space".to_owned(),
        Some(_) => return Ok(Kept::Dropped),
    };
    let value = attribute.value();
    if local.len() > 2 && local.as_bytes()[..2].eq_ignore_ascii_case(b"on") {
        return Ok(Kept::Dropped);
    }
    if local == "href" {
        return Ok(match fragment(value) {
            Some(id) => Kept::Rewritten(name, format!("#{id}")),
            None => Kept::Dropped,
        });
    }
    if identifier(local) || attribute.namespace().is_some() {
        return Ok(Kept::As(name));
    }
    if local == "style" {
        let style = strip_comments(value)?;
        if style.contains('\\') {
            return Err(FailureCode::RenderParse);
        }
        let mut kept = Vec::new();
        for declaration in declarations(&style)? {
            if urls(declaration.value)? == Urls::External {
                continue;
            }
            check_declaration(declaration.name, declaration.value)?;
            kept.push(declaration.text.trim().to_owned());
        }
        return Ok(Kept::Rewritten(name, kept.join(";")));
    }
    if value.to_ascii_lowercase().contains("url(") {
        if value.contains('\\') {
            return Err(FailureCode::RenderParse);
        }
        if urls(value)? == Urls::External {
            return Ok(Kept::Dropped);
        }
    }
    check_declaration(local, value)?;
    Ok(Kept::As(name))
}

/// Escape `text` for an attribute value or a text node. Attributes also
/// escape tabs and line breaks, which a parser would otherwise normalize.
fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&apos;"),
            '\t' if attribute => out.push_str("&#9;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// Write the kept parts of `document` back as SVG text.
pub(super) fn write(document: &Document<'_>) -> Result<String, FailureCode> {
    let root = document.root_element();
    if !kept(root) || root.tag_name().name() != "svg" {
        return Err(FailureCode::RenderParse);
    }
    let mut writer = XmlWriter::new(Options {
        use_single_quote: false,
        indent: Indent::None,
        attributes_indent: Indent::None,
    });
    writer.set_preserve_whitespaces(true);
    // Each entry is a node to enter, or (None) the end of an open element.
    let mut stack: Vec<Option<Node<'_, '_>>> = vec![Some(root)];
    let mut written = 0usize;
    while let Some(entry) = stack.pop() {
        let Some(node) = entry else {
            writer.end_element();
            continue;
        };
        if node.is_text() {
            let text = escape(node.text().unwrap_or_default(), false);
            written += text.len();
            writer.write_text(&text);
        } else if kept(node) {
            let name = node.tag_name().name();
            writer.start_element(name);
            if node == root {
                writer.write_attribute("xmlns", SVG_NS);
                writer.write_attribute("xmlns:xlink", XLINK_NS);
            }
            if name == "feConvolveMatrix" {
                let mut refused = false;
                numbers(plain(node, "order").unwrap_or_default(), |token| {
                    refused |= token
                        .parse::<f64>()
                        .map_or(true, |order| order > MAX_KERNEL_ORDER);
                });
                if refused {
                    return Err(FailureCode::RenderParse);
                }
            }
            let plain_href = plain(node, "href").and_then(fragment).is_some();
            for attribute in node.attributes() {
                let (name, value) = match attribute_policy(&attribute, plain_href)? {
                    Kept::As(name) => (name, escape(attribute.value(), true)),
                    Kept::Rewritten(name, value) => (name, escape(&value, true)),
                    Kept::Dropped => continue,
                };
                written += name.len() + value.len() + 4;
                writer.write_attribute_raw(&name, |buffer| {
                    buffer.extend_from_slice(value.as_bytes())
                });
            }
            stack.push(None);
            if name == "style" {
                // The sheet as checked, comments removed, in place of its text.
                let css: String = node.children().filter_map(|child| child.text()).collect();
                let sheet = escape(&check_style_sheet(&css)?.text, false);
                written += sheet.len();
                writer.write_text(&sheet);
            } else {
                stack.extend(node.children().rev().map(Some));
            }
        }
        if written > MAX_SANITIZED_BYTES {
            return Err(FailureCode::RenderResource);
        }
    }
    let out = writer.end_document();
    if out.len() > MAX_SANITIZED_BYTES {
        return Err(FailureCode::RenderResource);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(svg: &str) -> Result<String, FailureCode> {
        write(&parse(svg)?)
    }

    const OPEN: &str =
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">"#;

    #[test]
    fn local_references_survive_and_others_are_removed() {
        let out = clean(&format!(
            r##"{OPEN}<use href="#a"/><use xlink:href="http://x/y.svg#a"/><image href="file:///etc/passwd"/><image href="data:image/png;base64,AA=="/><rect fill="url(#g)" stroke="url(other.svg#g)"/></svg>"##
        ))
        .unwrap();
        assert!(out.contains(r##"<use href="#a"/>"##), "{out}");
        assert!(!out.contains("http://x"), "{out}");
        assert!(!out.contains("file:"), "{out}");
        assert!(!out.contains("data:"), "{out}");
        assert!(out.contains(r##"fill="url(#g)""##), "{out}");
        assert!(!out.contains("other.svg"), "{out}");
    }

    #[test]
    fn style_declarations_with_outside_references_are_removed() {
        let out = clean(&format!(
            r##"{OPEN}<rect style="fill:url(#g); stroke: url('https://x/p.png') ; opacity:0.5"/></svg>"##
        ))
        .unwrap();
        assert!(
            out.contains(r##"style="fill:url(#g);opacity:0.5""##),
            "{out}"
        );
    }

    #[test]
    fn unreadable_references_refuse_the_document() {
        for svg in [
            format!(r#"{OPEN}<rect style="fill:u\72l(http://x)"/></svg>"#),
            format!(r#"{OPEN}<rect fill="url(#g"/></svg>"#),
            format!(r#"{OPEN}<rect style="fill:url('#g)"/></svg>"#),
            format!(r#"{OPEN}<style>@import "x.css";</style></svg>"#),
            format!(r#"{OPEN}<style>rect {{ fill: url(http://x/p.png) }}</style></svg>"#),
            format!(r#"{OPEN}<style>rect {{ fill: \75 rl(x) }}</style></svg>"#),
        ] {
            assert_eq!(clean(&svg).unwrap_err(), FailureCode::RenderParse, "{svg}");
        }
    }

    #[test]
    fn escapes_survive_a_round_trip() {
        let out = clean(&format!(
            r#"{OPEN}<text data-x="a&amp;b&lt;&quot;&#10;c">&amp;lt; &lt;tag&gt; ]]&gt;</text></svg>"#
        ))
        .unwrap();
        let document = parse(&out).unwrap();
        let text = document
            .descendants()
            .find(|node| node.has_tag_name("text"))
            .unwrap();
        assert_eq!(text.attribute("data-x"), Some("a&b<\"\nc"));
        assert_eq!(text.text(), Some("&lt; <tag> ]]>"));
    }

    #[test]
    fn scripts_foreign_content_and_events_are_dropped() {
        let out = clean(&format!(
            r#"{OPEN}<script>alert(1)</script><switch><foreignObject><p xmlns="http://www.w3.org/1999/xhtml">html</p></foreignObject><text>fallback</text></switch><rect onload="x()" onclick="y()"/><metadata><x:rdf xmlns:x="urn:x"/></metadata></svg>"#
        ))
        .unwrap();
        for absent in [
            "script",
            "alert",
            "foreignObject",
            "html",
            "onload",
            "onclick",
            "rdf",
        ] {
            assert!(!out.contains(absent), "{absent} in {out}");
        }
        assert!(out.contains("<text>fallback</text>"), "{out}");
    }

    #[test]
    fn a_dtd_is_refused() {
        let svg = r#"<!DOCTYPE svg [<!ENTITY a "aaaa">]><svg xmlns="http://www.w3.org/2000/svg"><text>&a;</text></svg>"#;
        assert_eq!(clean(svg).unwrap_err(), FailureCode::RenderParse);
    }

    #[test]
    fn style_sheets_style_but_never_name() {
        // Styling is fine, and every selector of a list counts.
        let sheet = check_style_sheet(
            ".marker { fill: red } .marker:hover, .a,.b { opacity: 0.5; font-size: 12px }",
        )
        .unwrap();
        assert_eq!(sheet.selectors, 4);
        // A reference, an `!important`, a relative font size or a number
        // past the bounds is refused, a comment hiding it or not.
        for css in [
            "path { marker-mid: url(#m) }",
            "@media screen { g { marker: url(#m); } }",
            "rect { fill: url(#g) }",
            "rect { fill: red !important }",
            "text { font-size: 2em }",
            "text { /* x */ font-size: 150% }",
            "text { font: bold larger serif }",
            "rect { stroke-width: 1e10 }",
            "rect { fill: red } /* unclosed",
        ] {
            assert_eq!(
                check_style_sheet(css).unwrap_err(),
                FailureCode::RenderParse,
                "{css}"
            );
        }
        // Comments are gone from what usvg is given.
        assert_eq!(
            check_style_sheet("a{/*b*/fill:red}").unwrap().text,
            "a{ fill:red}"
        );
    }

    #[test]
    fn declarations_refuse_important_and_relative_font_sizes() {
        for svg in [
            format!(r##"{OPEN}<rect style="fill:url(#a)!important;fill:none"/></svg>"##),
            format!(r#"{OPEN}<g font-size="10000000em"/></svg>"#),
            format!(r#"{OPEN}<text style="font-size:larger"/></svg>"#),
        ] {
            assert_eq!(clean(&svg).unwrap_err(), FailureCode::RenderParse, "{svg}");
        }
        assert!(
            clean(&format!(
                r#"{OPEN}<text font-size="12" style="font-size:14px"/></svg>"#
            ))
            .is_ok()
        );
    }

    #[test]
    fn convolution_orders_are_bounded() {
        assert_eq!(
            clean(&format!(
                r#"{OPEN}<filter><feConvolveMatrix order="65536 65536"/></filter></svg>"#
            ))
            .unwrap_err(),
            FailureCode::RenderParse
        );
        assert!(
            clean(&format!(r#"{OPEN}<filter><feConvolveMatrix order="3" kernelMatrix="0 0 0 0 1 0 0 0 0"/></filter></svg>"#))
                .is_ok()
        );
    }

    #[test]
    fn a_non_ascii_attribute_name_is_written_back() {
        let out = clean(&format!(r#"{OPEN}<rect 名="x" o="y"/></svg>"#)).unwrap();
        assert!(out.contains(r#"名="x""#), "{out}");
    }

    #[test]
    fn of_namespaced_attributes_only_the_used_href_and_space_survive() {
        let out = clean(&format!(
            r##"{OPEN}<use xlink:href="#small" href="#large"/><use xlink:href="#only"/><text xml:space="preserve" xml:base="file:///x/">a</text><polyline xlink:marker-mid="url(#m)" xlink:title="t"/></svg>"##
        ))
        .unwrap();
        assert!(out.contains(r##"<use href="#large"/>"##), "{out}");
        assert!(out.contains(r##"<use xlink:href="#only"/>"##), "{out}");
        assert!(out.contains(r#"xml:space="preserve""#), "{out}");
        for absent in ["#small", "xml:base", "marker-mid", "xlink:title"] {
            assert!(!out.contains(absent), "{absent} in {out}");
        }
    }

    #[test]
    fn numbers_past_the_bounds_refuse_the_document() {
        for svg in [
            format!(r#"{OPEN}<rect width="1e30" height="1"/></svg>"#),
            format!(r#"{OPEN}<marker markerWidth="1e-30"/></svg>"#),
            format!(r#"{OPEN}<path d="M0 0L99999999 0"/></svg>"#),
            format!(r#"{OPEN}<rect style="stroke-width: 1e10"/></svg>"#),
            format!(r#"{OPEN}<style>rect {{ stroke-width: 1e10 }}</style></svg>"#),
        ] {
            assert_eq!(clean(&svg).unwrap_err(), FailureCode::RenderParse, "{svg}");
        }
        // Editor rounding noise, hex colors and names with digits are fine.
        assert!(
            clean(&format!(
                r##"{OPEN}<g id="a1e99" class="c1e99" transform="matrix(1 1.2246468e-16 -1.2246468e-16 1 0 0)"><rect fill="#1e9999" stroke="url(#g1e400)"/></g></svg>"##
            ))
            .is_ok()
        );
    }
}
