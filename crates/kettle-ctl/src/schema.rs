//! The control protocol's JSON Schema (draft 2020-12): every public method's
//! parameters and results, the request and reply envelopes, the error codes
//! and the event messages. `kettle ctl schema` prints it; its tests keep it in
//! step with the methods, the parsers and the replies it describes.

/// The schema of protocol v1, as `kettle ctl schema` prints it.
pub const CONTROL_V1: &str = include_str!("../schema/control-v1.schema.json");

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::CONTROL_V1;
    use crate::protocol::{Method, Response, Visibility, error_codes};
    use crate::show::{InlineRefusal, ShowFile, ShowPage, ShowRequest, ShowResult, show_failure};

    const ID: &str = "urn:kettle:control:v1";

    fn schema() -> Value {
        serde_json::from_str(CONTROL_V1).expect("the schema is JSON")
    }

    /// Whether `value` is valid against the schema's `location`: the whole
    /// document (`""`) or one of its definitions (`"/$defs/name"`).
    fn valid(location: &str, value: &Value) -> bool {
        let mut schemas = boon::Schemas::new();
        let mut compiler = boon::Compiler::new();
        compiler.add_resource(ID, schema()).unwrap();
        let index = compiler
            .compile(&format!("{ID}#{location}"), &mut schemas)
            .unwrap_or_else(|error| panic!("{location}: {error:#}"));
        schemas.validate(value, index).is_ok()
    }

    fn public_methods() -> BTreeSet<&'static str> {
        Method::ALL
            .into_iter()
            .filter(|method| method.visibility() == Visibility::Public)
            .map(Method::as_str)
            .collect()
    }

    /// The schema is a draft 2020-12 schema, every reference in it resolves,
    /// and it names itself.
    #[test]
    fn the_schema_compiles() {
        let document = schema();
        assert_eq!(
            document["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(document["$id"], ID);
        let definitions: Vec<String> = document["$defs"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for name in definitions {
            valid(&format!("/$defs/{name}"), &Value::Null);
        }
        valid("", &Value::Null);
    }

    /// The methods the schema describes are exactly the public ones, each
    /// with its params and result; the internal ones appear nowhere; and
    /// `docs/AGENT.md`'s table lists the same.
    #[test]
    fn the_schema_names_every_public_method_and_no_other() {
        let document = schema();
        let public = public_methods();
        assert_eq!(public.len(), 18);
        let request: BTreeSet<&str> = document["$defs"]["request"]["properties"]["method"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(request, public, "the request's methods");
        let index: BTreeSet<&str> = document["$defs"]["methods_index"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(index, public, "the methods index");
        for method in &public {
            for part in ["params", "result"] {
                assert!(
                    document["$defs"].get(format!("{method}_{part}")).is_some(),
                    "{method}_{part}"
                );
            }
        }
        for method in Method::ALL {
            if method.visibility() == Visibility::Internal {
                assert!(!CONTROL_V1.contains(method.as_str()), "{}", method.as_str());
            }
        }
        let agent = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/AGENT.md"),
        )
        .unwrap();
        let table = agent
            .split("### Methods (protocol v1)")
            .nth(1)
            .expect("the methods table")
            .split("\n### ")
            .next()
            .unwrap();
        let documented: BTreeSet<&str> = table
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|line| line.split('`').next())
            .collect();
        assert_eq!(documented, public, "docs/AGENT.md's methods");
    }

    /// The error codes are exactly the protocol's and the media failures'.
    #[test]
    fn the_schema_names_every_error_code() {
        let document = schema();
        let codes: BTreeSet<&str> = document["$defs"]["error_code"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|code| code.as_str().unwrap())
            .collect();
        let expected: BTreeSet<&str> = error_codes::ALL
            .into_iter()
            .chain(kettle_media::FailureCode::ALL.map(kettle_media::FailureCode::code))
            .collect();
        assert_eq!(codes, expected);
    }

    /// What the server answers validates: every kind of `show` result, every
    /// failure a `show` can end in, every protocol error, in their envelopes.
    #[test]
    fn real_replies_validate() {
        let route = (4, true, 2);
        let mut results = vec![
            ShowResult::new(
                route,
                9,
                kettle_media::MediaKind::Svg,
                (640, 480),
                &[kettle_media::Warning::FontFallback],
            ),
            ShowResult::file(route, 10, 2_400_000, "unsupported_media"),
        ];
        let mut page = ShowResult::new(route, 11, kettle_media::MediaKind::Pdf, (816, 1056), &[]);
        page.page = Some(ShowPage {
            index: 1,
            count: 12,
        });
        results.push(page);
        let mut refused =
            ShowResult::new(route, 12, kettle_media::MediaKind::Raster, (64, 48), &[]);
        refused.inline_refused = Some(InlineRefusal::NotTerminal);
        results.push(refused);
        for result in results {
            let value = serde_json::to_value(&result).unwrap();
            assert!(valid("/$defs/show_result", &value), "{value}");
            let reply = serde_json::to_value(Response::ok(7, value)).unwrap();
            assert!(valid("/$defs/reply_ok", &reply), "{reply}");
            assert!(valid("", &reply), "{reply}");
        }
        for failure in kettle_media::FailureCode::ALL {
            let reply = serde_json::to_value(show_failure(7, failure)).unwrap();
            assert!(valid("/$defs/reply_error", &reply), "{reply}");
        }
        for code in error_codes::ALL {
            let reply = serde_json::to_value(Response::err(7, code, "words")).unwrap();
            assert!(valid("/$defs/reply_error", &reply), "{reply}");
        }
        let file = ShowFile {
            bytes: 1,
            reason: "too_large".into(),
        };
        assert!(valid(
            "/$defs/show_result",
            &json!({
                "pane": 1, "verified": false, "window": 1, "item": 1, "kind": "file",
                "width": 0, "height": 0, "warnings": [],
                "file": serde_json::to_value(file).unwrap(),
            })
        ));
        assert!(
            !valid("/$defs/show_result", &json!({"pane": 1})),
            "a result is whole"
        );
    }

    /// An absolute path on this platform, as `show` takes one: a drive path
    /// on Windows, which refuses a leading slash there.
    fn absolute(name: &str) -> String {
        if cfg!(windows) {
            format!("C:\\media\\{name}")
        } else {
            format!("/media/{name}")
        }
    }

    /// `show`'s params: what its parser takes, the schema takes; what it
    /// refuses for its shape, the schema refuses. Size limits and indexes
    /// past a document are the parser's alone, and not asked here.
    #[test]
    fn show_params_agree_with_the_parser() {
        for params in [
            json!({"svg": "<svg/>"}),
            json!({"mermaid": "graph TD; A-->B", "title": "flow"}),
            json!({"image_b64": "iVBORw0KGgo="}),
            json!({"path": absolute("plot.png"), "dev": 1, "ino": 2}),
            json!({"path": absolute("flow.mmd"), "dev": 1, "ino": 2, "kind": "mermaid"}),
            json!({"path": absolute("plan.md"), "dev": 1, "ino": 2, "markdown_index": 3}),
            json!({"path": absolute("report.pdf"), "dev": 1, "ino": 2, "page_index": 7}),
            json!({"svg": "<svg/>", "key": "chart", "pane": 3}),
            json!({"svg": "<svg/>", "something_new": [1, 2]}),
            json!({}),
            json!({"svg": "<svg/>", "mermaid": "graph TD; A-->B"}),
            json!({"svg": ""}),
            json!({"svg": 5}),
            json!({"path": absolute("plot.png")}),
            json!({"path": absolute("plot.png"), "dev": -1, "ino": 2}),
            json!({"svg": "<svg/>", "kind": "mermaid"}),
            json!({"svg": "<svg/>", "page_index": 0}),
            json!({"path": absolute("r.pdf"), "dev": 1, "ino": 2, "page_index": 0, "markdown_index": 0}),
            json!({"path": absolute("r.pdf"), "dev": 1, "ino": 2, "page_index": -1}),
            json!({"path": absolute("flow.mmd"), "dev": 1, "ino": 2, "kind": "svg"}),
            json!(["svg"]),
        ] {
            let parsed = ShowRequest::parse(params.clone()).is_ok();
            assert_eq!(valid("/$defs/show_params", &params), parsed, "{params}");
        }
    }
}
