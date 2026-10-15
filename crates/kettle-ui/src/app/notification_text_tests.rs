use super::{PaneInputResult, input_rejection_message, input_rejection_text};
use kettle_i18n::{Language, Translator};

/// A rejected input notifies in the UI's language while the log keeps the
/// English text; accepted input says nothing in either.
#[test]
fn rejected_input_notifies_in_the_ui_language() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    for result in [
        PaneInputResult::Backpressured,
        PaneInputResult::Oversize,
        PaneInputResult::Failed,
    ] {
        assert_eq!(
            input_rejection_text(result, &en),
            input_rejection_message(result)
        );
        let spanish = input_rejection_text(result, &es).unwrap();
        assert_ne!(Some(spanish), input_rejection_message(result));
    }
    // The broadcast safety warning survives translation.
    assert!(
        input_rejection_text(PaneInputResult::Backpressured, &es)
            .unwrap()
            .contains("algunos ya la hayan recibido")
    );
    for result in [PaneInputResult::Queued, PaneInputResult::ReadOnly] {
        assert_eq!(input_rejection_text(result, &es), None);
    }
}
