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
//!   sheet may not import or refer outside the document at all.
//! - Event attributes, `xml:base`, comments and processing instructions are
//!   dropped.
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

/// What an element's `href` (plain or `xlink:`) names: a local fragment id,
/// or `None` when it has none or names anything else.
pub(super) fn href<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .filter(|attribute| {
            attribute.name() == "href" && matches!(attribute.namespace(), None | Some(XLINK_NS))
        })
        .find_map(|attribute| fragment(attribute.value()))
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

/// The declarations of a `style` attribute: split at semicolons outside
/// quotes and parentheses. Unbalanced text refuses the document.
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

/// A style sheet may hold rules referring to nothing outside the document:
/// no import, no `url(` other than a local fragment, and no escapes that
/// could spell one. Returns whether any rule sets a marker property.
pub(super) fn check_style_sheet(css: &str) -> Result<bool, FailureCode> {
    if css.contains('\\') || css.to_ascii_lowercase().contains("@import") {
        return Err(FailureCode::RenderParse);
    }
    if urls(css)? == Urls::External {
        return Err(FailureCode::RenderParse);
    }
    // Declarations are the text between `{` or `;` and `;` or `}`; whatever
    // precedes a `{` is a selector or an at-rule prelude.
    let mut marks = false;
    let mut start = 0;
    for (index, c) in css.char_indices() {
        if matches!(c, '{' | '}' | ';') {
            if c != '{'
                && let Some((name, _)) = css[start..index].split_once(':')
            {
                marks |= marker_property(name.trim());
            }
            start = index + 1;
        }
    }
    Ok(marks)
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

fn attribute_policy(attribute: &roxmltree::Attribute<'_, '_>) -> Result<Kept, FailureCode> {
    let local = attribute.name();
    let name = match attribute.namespace() {
        None => local.to_owned(),
        Some(XLINK_NS) => format!("xlink:{local}"),
        Some(XML_NS) if local == "space" => "xml:space".to_owned(),
        Some(_) => return Ok(Kept::Dropped),
    };
    let value = attribute.value();
    if local.len() > 2 && local[..2].eq_ignore_ascii_case("on") {
        return Ok(Kept::Dropped);
    }
    if local == "href" {
        return Ok(match fragment(value) {
            Some(id) => Kept::Rewritten(name, format!("#{id}")),
            None => Kept::Dropped,
        });
    }
    if local == "style" {
        if value.contains('\\') {
            return Err(FailureCode::RenderParse);
        }
        let mut kept = Vec::new();
        for declaration in declarations(value)? {
            if urls(declaration.value)? == Urls::External {
                continue;
            }
            kept.push(declaration.text.trim());
        }
        return Ok(Kept::Rewritten(name, kept.join(";")));
    }
    let lower = value.to_ascii_lowercase();
    if lower.contains("url(") {
        if value.contains('\\') {
            return Err(FailureCode::RenderParse);
        }
        if urls(value)? == Urls::External {
            return Ok(Kept::Dropped);
        }
    }
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
            if name == "style" {
                let css: String = node.children().filter_map(|child| child.text()).collect();
                check_style_sheet(&css)?;
            }
            writer.start_element(name);
            if node == root {
                writer.write_attribute("xmlns", SVG_NS);
                writer.write_attribute("xmlns:xlink", XLINK_NS);
            }
            for attribute in node.attributes() {
                let (name, value) = match attribute_policy(&attribute)? {
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
            stack.extend(node.children().rev().map(Some));
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
    fn style_sheets_report_marker_rules() {
        assert!(
            !check_style_sheet(".marker { fill: red } .marker:hover { opacity: 0.5 }").unwrap()
        );
        assert!(check_style_sheet("path { marker-mid: url(#m) }").unwrap());
        assert!(check_style_sheet("@media screen { g { marker: url(#m); } }").unwrap());
    }
}
