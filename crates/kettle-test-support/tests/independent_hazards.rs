//! Hazard probes for `production_source`, written against the CONTRACT rather
//! than the implementation, and deliberately kept in a separate integration
//! test from the helper's own unit tests.
//!
//! The reason for the separation is the history. Three hand-rolled versions of
//! this stripper preceded the shared one and two were unsound — one halted at a
//! `}` inside a multiline string, another missed an indented `#[cfg(test)]` —
//! and both passed the self-check written alongside them, because a check
//! authored with an implementation tends to share its blind spots. These cases
//! were written from the list of ways a source lexer can be fooled, then run
//! against whatever implementation exists.
//!
//! Each case asserts in BOTH directions: production text survives, and test
//! text does not. The first direction is the one that matters most — a guard of
//! the form `!src.contains(...)` fails OPEN when the slice loses production
//! code, so an over-strip reads as a pass while protecting nothing.

use kettle_test_support::production_source;

fn assert_kept(label: &str, src: &str, must_keep: &[&str], must_drop: &[&str]) {
    let out = production_source(src);
    for k in must_keep {
        assert!(
            out.contains(k),
            "{label}: production text {k:?} was LOST (negative guards fail open)\n--- slice ---\n{out}"
        );
    }
    for d in must_drop {
        assert!(
            !out.contains(d),
            "{label}: test text {d:?} SURVIVED\n--- slice ---\n{out}"
        );
    }
}

#[test]
fn brace_inside_hashed_raw_string_does_not_end_the_item_early() {
    assert_kept(
        "hashed raw string",
        "fn keep() {}\n#[cfg(test)]\nmod t {\n    const S: &str = r##\"}\"##;\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()", "#[cfg(test)]"],
    );
}

#[test]
fn brace_inside_char_literal_does_not_end_the_item_early() {
    assert_kept(
        "char literal brace",
        "fn keep() {}\n#[cfg(test)]\nmod t {\n    let c = '}';\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn a_lifetime_tick_is_not_mistaken_for_a_char_literal() {
    // `&'a str` opens a tick that never closes. A naive char-literal scanner
    // swallows the rest of the file, dropping all production code after it.
    assert_kept(
        "lifetime vs char",
        "fn keep<'a>(s: &'a str) -> &'a str { s }\n#[cfg(test)]\nmod t {\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep<'a>", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn nested_block_comments_are_tracked() {
    assert_kept(
        "nested block comment",
        "fn keep() {}\n#[cfg(test)]\nmod t {\n    /* /* } */ */\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn a_cfg_whose_test_is_part_of_a_longer_word_is_not_stripped() {
    assert_kept(
        "fastest substring",
        "fn keep() {}\n#[cfg(feature = \"fastest\")]\nfn must_survive() {}\nfn also_keep() {}\n",
        &["fn keep()", "fn must_survive()", "fn also_keep()"],
        &[],
    );
}

#[test]
fn an_attribute_between_the_cfg_and_its_item_still_strips_the_item() {
    assert_kept(
        "attr between cfg and item",
        "fn keep() {}\n#[cfg(test)]\n#[allow(dead_code)]\nmod t {\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn a_doc_comment_between_the_cfg_and_its_item_still_strips_the_item() {
    assert_kept(
        "doc comment between cfg and item",
        "fn keep() {}\n#[cfg(test)]\n/// docs\nmod t {\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn a_string_containing_a_block_comment_opener_is_not_treated_as_a_comment() {
    assert_kept(
        "string containing /*",
        "fn keep() { let _ = \"/*\"; }\nfn also_keep() {}\n#[cfg(test)]\nmod t {\n    fn hidden() {}\n}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn an_escaped_quote_does_not_end_the_string_early() {
    assert_kept(
        "escaped quote",
        "fn keep() { let _ = \"a\\\"}\"; }\nfn also_keep() {}\n#[cfg(test)]\nmod t {\n    fn hidden() {}\n}\n",
        &["fn keep()", "fn also_keep()"],
        &["fn hidden()"],
    );
}

#[test]
fn a_semicolon_terminated_cfg_item_is_stripped_without_eating_the_next_item() {
    assert_kept(
        "semicolon item",
        "fn keep() {}\n#[cfg(test)]\nuse std::collections::HashMap;\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["use std::collections::HashMap;"],
    );
}

#[test]
fn cfg_not_test_marks_production_only_code_and_must_survive() {
    // `#[cfg(not(test))]` is PRODUCTION-ONLY code. Stripping it is backwards.
    assert_kept(
        "cfg(not(test))",
        "fn keep() {}\n#[cfg(not(test))]\nfn production_only() {}\nfn also_keep() {}\n",
        &["fn keep()", "fn production_only()", "fn also_keep()"],
        &[],
    );
}

#[test]
fn cfg_any_unix_test_still_compiles_in_production_and_must_survive() {
    assert_kept(
        "cfg(any(unix, test))",
        "fn keep() {}\n#[cfg(any(unix, test))]\nfn unix_production() {}\nfn also_keep() {}\n",
        &["fn keep()", "fn unix_production()", "fn also_keep()"],
        &[],
    );
}

#[test]
fn a_block_doc_comment_on_a_test_item_is_removed_with_it() {
    // The prose is searchable text: a needle quoted in a test item's docs
    // would survive the item's removal and satisfy a guard by itself.
    assert_kept(
        "block doc comment",
        "fn keep() {}\n/** requires required_call() to remain */\n#[cfg(test)]\nmod t {\n    fn hidden() {}\n}\nfn also_keep() {}\n",
        &["fn keep()", "fn also_keep()"],
        &["required_call()", "fn hidden()"],
    );
}

#[test]
fn a_plain_block_comment_before_a_test_item_is_not_swallowed() {
    // An ordinary `/* … */` may document the PRECEDING production item.
    // Removing it would delete production text and make negative guards pass
    // vacuously, so it must survive.
    let src = "fn keep() {}\n/* belongs to keep(): calls required_call() */\n#[cfg(test)]\nmod t {\n    fn hidden() {}\n}\n";
    assert_kept(
        "plain block comment",
        src,
        &["fn keep()", "belongs to keep()", "required_call()"],
        &["fn hidden()"],
    );
}
