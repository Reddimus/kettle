use super::production_source;

/// The places a literal would reach a person: a screen reader, a desktop
/// notification, a window title, a menu or picker row, a prompt, or
/// painted chrome.
const SINK_CALLS: &[&str] = &[
    "set_label(",
    "set_description(",
    "set_title(",
    "fire_notify(",
    "queue_desktop_notification(",
    "fit_single_line_label(",
];
const SINK_FIELDS: &[&str] = &["label:", "hint:", "title:", "prompt:"];
/// Literals that read the same in every language: key names in the
/// picker and editor hints.
const LANGUAGE_NEUTRAL: &[&str] = &["Enter", "Esc", "Tab", "Tab/↑↓", "↑↓"];

/// The string literals (plain or raw) in `src[from..]` up to the end of the
/// expression that starts there: a call's arguments when `from` is just
/// past `(`, or a field's value. Nested calls and macros are included.
fn literals_in(src: &str, from: usize, until_close: bool) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 && !until_close => break,
            b';' if depth == 0 => break,
            // Comments are not code: skip them whole.
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = src[i..].find('\n').map_or(bytes.len(), |end| i + end);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = src[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
                continue;
            }
            b'\'' => {
                // A char literal (`'"'`) or a lifetime; skip a char literal.
                if let Some(end) = src[i + 1..].find('\'')
                    && end <= 4
                {
                    i += end + 1;
                }
            }
            b'r' if bytes.get(i + 1).is_some_and(|b| matches!(b, b'"' | b'#'))
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) =>
            {
                let hashes = src[i + 1..].bytes().take_while(|b| *b == b'#').count();
                let open = i + 1 + hashes;
                if bytes.get(open) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    if let Some(end) = src[open + 1..].find(&close) {
                        found.push(src[open + 1..open + 1 + end].to_string());
                        i = open + 1 + end + close.len();
                        continue;
                    }
                }
            }
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                found.push(unescape(&src[i + 1..j.min(bytes.len())]));
                i = j + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    found
}

/// A plain literal's text with its escapes decoded (`\u{43}` is `C`).
fn unescape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    let mut chars = literal.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars
                    .by_ref()
                    .skip_while(|c| *c == '{')
                    .take_while(|c| *c != '}')
                    .collect();
                if let Some(decoded) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(decoded);
                }
            }
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    out.push(char::from(byte));
                }
            }
            Some('n' | 't' | 'r') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Whether a literal holds prose, outside `{…}` placeholders: two words, a
/// capitalised word, or a non-ASCII letter. Lowercase identifiers such as
/// a GPU debug label (`kettle-device`) or a config key are not prose.
fn is_text(literal: &str) -> bool {
    // `{{` and `}}` print braces; other braces delimit a placeholder.
    let unbraced = literal.replace("{{", "\u{0}").replace("}}", "\u{1}");
    let mut depth = 0;
    let visible: String = unbraced
        .chars()
        .filter(|c| {
            match c {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                _ => return depth == 0,
            }
            false
        })
        .collect();
    let words = visible
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if LANGUAGE_NEUTRAL.contains(&literal) {
        return false;
    }
    words.len() >= 2 && visible.contains(' ')
        || words
            .iter()
            .any(|word| word.chars().next().is_some_and(char::is_uppercase))
        || visible.chars().any(|c| c.is_alphabetic() && !c.is_ascii())
}

/// Every sink in `src` given literal text, as `file:line: sink`. Comment
/// lines are skipped; literals anywhere in a call's arguments count,
/// including inside `format!` and raw strings.
fn literal_sinks(file: &str, src: &str) -> Vec<String> {
    let line_of = |at: usize| src[..at].matches('\n').count() + 1;
    let is_comment = |at: usize| {
        let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
        src[start..at].trim_start().starts_with("//")
    };
    let mut found = Vec::new();
    for sink in SINK_CALLS {
        for (at, _) in src.match_indices(sink) {
            if !is_comment(at)
                && literals_in(src, at + sink.len(), true)
                    .iter()
                    .any(|literal| is_text(literal))
            {
                found.push(format!("{file}:{}: {sink}…", line_of(at)));
            }
        }
    }
    for field in SINK_FIELDS {
        for (at, _) in src.match_indices(field) {
            // A field, not a path segment (`kettle::label::`) or a word
            // ending in the name (`sublabel:`).
            let before = src[..at].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
                || src[at + field.len()..].starts_with(':')
                || is_comment(at)
            {
                continue;
            }
            if literals_in(src, at + field.len(), false)
                .iter()
                .any(|literal| is_text(literal))
            {
                found.push(format!("{file}:{}: {field}…", line_of(at)));
            }
        }
    }
    found
}

/// Kettle-owned text reaches people only through the catalogue, so no
/// UI sink in production code takes literal text. Terminal content, user
/// names, config values, logs and diagnostics are not routed through these
/// sinks with literals either; a new surface that needs one belongs in
/// `kettle-i18n`.
#[test]
fn ui_text_sinks_never_take_literal_text() {
    let sources = [
        ("kettle-ui/src/app.rs", production_source()),
        (
            "kettle-ui/src/settings.rs",
            kettle_test_support::production_source(include_str!("../settings.rs")),
        ),
        (
            "kettle-ui/src/macos_dock.rs",
            kettle_test_support::production_source(include_str!("../macos_dock.rs")),
        ),
        (
            "kettle-ui/src/notifications.rs",
            kettle_test_support::production_source(include_str!("../notifications.rs")),
        ),
        (
            "kettle-render/src/lib.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/lib.rs"
            )),
        ),
        (
            "kettle-render/src/terminal.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/terminal.rs"
            )),
        ),
        (
            "kettle-render/src/fonts.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/fonts.rs"
            )),
        ),
        (
            "kettle-render/src/overlays.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/overlays.rs"
            )),
        ),
        (
            "kettle-render/src/batching.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/batching.rs"
            )),
        ),
        (
            "kettle-render/src/theme.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/theme.rs"
            )),
        ),
        (
            "kettle-render/src/search.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/search.rs"
            )),
        ),
        (
            "kettle-render/src/completion.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/completion.rs"
            )),
        ),
        (
            "kettle-render/src/settings.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/settings.rs"
            )),
        ),
        (
            "kettle-render/src/geometry.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/geometry.rs"
            )),
        ),
        (
            "kettle-render/src/gpu.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/gpu.rs"
            )),
        ),
        (
            "kettle-render/src/background.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/background.rs"
            )),
        ),
        (
            "kettle-render/src/screenshot.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/screenshot.rs"
            )),
        ),
        (
            "kettle-render/src/labels.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/labels.rs"
            )),
        ),
        (
            "kettle-render/src/capture.rs",
            kettle_test_support::production_source(include_str!(
                "../../../kettle-render/src/capture.rs"
            )),
        ),
    ];
    let found: Vec<String> = sources
        .iter()
        .flat_map(|(file, src)| literal_sinks(file, src))
        .collect();
    assert!(
        found.is_empty(),
        "UI text must come from kettle-i18n, not a literal:\n{}",
        found.join("\n")
    );
}

/// The scanner finds each kind of sink, across a line break, inside a
/// later argument, `format!` and a raw string, and ignores comments,
/// placeholders and translated text.
#[test]
fn the_drift_scanner_finds_literal_sinks() {
    let src = [
        "node.set_label(\"Terminal menu\");",
        "fire_notify(",
        "    tr.text(Text::NotifyTitleThemeNotSaved),",
        "    \"Applied for this session\",",
        ");",
        "ContextMenuRow { label: \"Copy\".into(), hint: String::new() };",
        "// node.set_label(\"in a comment\");",
        "node.set_label(tr.text(Text::A11yWindow));",
        "let row = Row { label: \"\".into(), hint: format!(\"{count}/{total}\") };",
        "node.set_description(format!(\"Pane {id}\"));",
        "w.set_title(r\"kettle - lost\");",
        "Row {",
        "    label:",
        "        \"Split\".into(),",
        "};",
        "let text = fit_single_line_label(&format!(\"  ⬆ {}\", tr.update_banner(tag, url)), cols);",
        "kettle::label::make(\"x\");",
        "Row { hint: picker_hint(tr, &[(\"Enter\", T::PickerHintRun)]) };",
        "node.set_label(/* ) */ \"Copy\");",
        "node.set_label(/* \"Copy\" */ tr.text(Text::MenuCopy));",
        "node.set_label(format!(\"{{Copy}}\"));",
        "node.set_label(\"\\u{43}opy\");",
    ]
    .join("\n");
    let found = literal_sinks("probe.rs", &src);
    assert_eq!(
        found,
        [
            "probe.rs:1: set_label(…",
            "probe.rs:19: set_label(…",
            "probe.rs:21: set_label(…",
            "probe.rs:22: set_label(…",
            "probe.rs:10: set_description(…",
            "probe.rs:11: set_title(…",
            "probe.rs:2: fire_notify(…",
            "probe.rs:6: label:…",
            "probe.rs:13: label:…",
        ],
        "{found:#?}"
    );
}
