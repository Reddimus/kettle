use super::optional_u64_param;
use serde_json::json;

#[test]
fn explicit_targets_are_typed_instead_of_falling_back() {
    assert_eq!(optional_u64_param(&json!({}), "pane").unwrap(), None);
    assert_eq!(
        optional_u64_param(&json!({"pane": 42}), "pane").unwrap(),
        Some(42)
    );
    for value in [json!(null), json!("42"), json!(-1), json!(1.5)] {
        let params = json!({"pane": value});
        assert!(
            optional_u64_param(&params, "pane").is_err(),
            "explicit malformed target must not resolve as absent: {params}"
        );
    }
}
