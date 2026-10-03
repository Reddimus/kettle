#[path = "../build_support.rs"]
mod build_support;

use build_support::generate;

const SCHEMA: &str = r#"
[messages.title]
context = "Window title"
[messages.close_tab]
context = "Counted confirmation"
args = [{ name = "count", type = "u64" }]
plural = "count"
[messages.reattach]
context = "Container menu"
args = [{ name = "runtime", type = "str" }, { name = "container", type = "str" }]
"#;
const EN: &str = r#"
title = "Settings"
reattach = "Re-attach {runtime} {container}"
[close_tab]
one = "Close {count} pane?"
other = "Close {count} panes?"
"#;

#[test]
fn shipping_inputs_generate_every_api() {
    let code = generate(
        include_str!("../locales/schema.toml"),
        include_str!("../locales/en.toml"),
        include_str!("../locales/es.toml"),
    )
    .unwrap();
    assert!(code.contains("pub fn settings_heading(&self, category: &str)"));
    assert!(
        code.contains("pub fn settings_active_gpu(&self, name: &str, kind: &str, backend: &str)")
    );
    assert!(code.contains("pub fn settings_value_not_detected(&self, name: &str)"));
    assert!(code.contains("pub fn settings_a11y_dialog(&self, category: &str)"));
    // `text` plus one method per message with arguments.
    let schema: toml::Table = include_str!("../locales/schema.toml").parse().unwrap();
    let with_args = schema["messages"]
        .as_table()
        .unwrap()
        .values()
        .filter(|message| message.get("args").is_some())
        .count();
    assert_eq!(code.matches("pub fn ").count(), 1 + with_args);
}

#[test]
fn completeness_and_placeholder_parity_reject_bad_catalogues() {
    let cases = [
        (EN.replace("title = \"Settings\"", ""), "missing"),
        (format!("extra = \"Unexpected\"\n{EN}"), "extra"),
        (EN.replace("{container}", "{unknown}"), "placeholders"),
        (EN.replace("{runtime}", ""), "placeholders"),
        (EN.replace("other = \"Close {count} panes?\"", ""), "es:"),
        (EN.replace("one = \"Close {count} pane?\"", ""), "es:"),
        (
            EN.replace("[close_tab]", "[close_tab]\nzero = \"Zero\""),
            "es:",
        ),
        (EN.replace("title = \"Settings\"", "title = 3"), "es:"),
        (EN.replace("title = \"Settings\"", "title = \"\""), "empty"),
        (
            EN.replace("title = \"Settings\"", "title = \"{count}\""),
            "placeholders",
        ),
        (format!("{EN}\n[title]\nother = \"duplicate\""), "es:"),
        (
            EN.replace("one = \"Close {count} pane?\"", "one = \"Close pane?\""),
            "placeholders",
        ),
    ];
    for (bad, diagnostic) in cases {
        let error = generate(SCHEMA, EN, &bad).unwrap_err();
        assert!(error.contains(diagnostic), "{error}");
        assert!(
            generate(SCHEMA, &bad, EN).is_err(),
            "English must be complete too"
        );
    }
}

#[test]
fn syntax_and_schema_validation_reject_executable_fragments() {
    for text in [
        "{count:02}",
        "{count + 1}",
        "{}",
        "{0}",
        "{count",
        "count}",
        "{{count}",
        "{self}",
        "{r#count}",
    ] {
        let bad = EN.replace("{count}", text);
        assert!(generate(SCHEMA, EN, &bad).is_err(), "{text}");
    }
    for bad in [
        SCHEMA.replace("type = \"u64\"", "type = \"String\""),
        SCHEMA.replace("plural = \"count\"", "plural = \"unknown\""),
        SCHEMA.replace("type = \"u64\"", "type = \"str\""),
        SCHEMA.replace("messages.title", "messages.type"),
        SCHEMA.replace("name = \"runtime\"", "name = \"container\""),
        SCHEMA.replace("context = \"Window title\"", "context = \"\""),
        SCHEMA.replace("context = \"Window title\"", "unknown = true"),
        String::from("messages = {}"),
        String::from("not toml = ["),
    ] {
        assert!(generate(&bad, EN, EN).is_err(), "{bad}");
    }
    assert!(generate(SCHEMA, EN, "not valid toml").is_err());
    assert!(
        generate(
            SCHEMA,
            EN,
            &EN.replace("[close_tab]", "close_tab = \"static\"\n[unused]")
        )
        .is_err()
    );
}

#[test]
fn translator_text_is_a_rust_literal_with_braces_and_control_characters_escaped() {
    let schema = "[messages.title]\ncontext = 'Escaping test'";
    let text = "quote \" backslash \\ newline\n tab\t {{literal}} ; panic!(\"injection\")\nUnicode é\u{2028}";
    let catalogue = format!("title = {text:?}").replace("\\u{2028}", "\\u2028");
    let code = generate(schema, &catalogue, &catalogue).unwrap();
    let decoded = text.replace("{{literal}}", "{literal}");
    assert!(code.contains(&format!("Text::Title => {decoded:?}")));
    assert!(!code.contains("; panic!(\"injection\")"));
    let formatted = EN.replace(
        "Re-attach {runtime} {container}",
        "{{literal}} \\\"{container}\\\" \\\\ {runtime}",
    );
    assert!(
        generate(SCHEMA, EN, &formatted)
            .unwrap()
            .contains("{{literal}} \\\"{container}\\\" \\\\ {runtime}")
    );
}
