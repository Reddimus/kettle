use super::*;

/// One on-screen quick-select target: where its label sits and what it is.
#[derive(Clone)]
pub(crate) struct HintTarget {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) label: String,
    /// The pane the target was read from, whose directory a relative path
    /// resolves against, even if focus moved or that pane exited since.
    pub(super) pane: u64,
    pub(super) what: HintWhat,
}

/// What a quick-select target is.
#[derive(Clone)]
pub(super) enum HintWhat {
    /// Text the detector found: a URL, a path, or something to copy.
    Text {
        kind: kettle_core::hints::Kind,
        text: String,
        /// The text meets a boundary on both sides (see `HintSpan::bounded`).
        bounded: bool,
    },
    /// An inline card on screen: picking it opens its item in the viewer.
    Card(kettle_core::InlineNonce),
}

/// A card's place in a pane for quick select: its first and last visible
/// cells, as (row, column), and its nonce.
pub(super) type CardCells = ((usize, usize), (usize, usize), kettle_core::InlineNonce);

/// The quick-select targets in a pane, in reading order: each text span the
/// detector found outside every card, and each card at its first visible
/// cell. A card's rows hold its marks, not text a user would pick.
pub(super) fn hint_targets_with_cards(
    spans: Vec<kettle_core::hints::HintSpan>,
    cards: &[CardCells],
) -> Vec<(usize, usize, HintWhat)> {
    let inside_card = |row: usize, col: usize| {
        cards.iter().any(|((top, left), (bottom, right), _)| {
            (*top..=*bottom).contains(&row) && (*left..=*right).contains(&col)
        })
    };
    let mut found: Vec<(usize, usize, HintWhat)> = spans
        .into_iter()
        .filter(|span| !inside_card(span.row, span.start))
        .map(|span| {
            (
                span.row,
                span.start,
                HintWhat::Text {
                    kind: span.kind,
                    text: span.text,
                    bounded: span.bounded,
                },
            )
        })
        .collect();
    found.extend(
        cards
            .iter()
            .map(|((row, col), _, nonce)| (*row, *col, HintWhat::Card(*nonce))),
    );
    found.sort_by_key(|(row, col, _)| (*row, *col));
    found
}

/// What picking a quick-select hint does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HintAction {
    OpenUrl,
    OpenPath,
    Copy,
}

/// A URL opens and anything else is copied; a label typed with Shift
/// (`alternate`) copies a URL and opens a path. A path that may not name a
/// whole local file (`unsafe_path`: from a pane that may be remote, or cut
/// short by a character the pattern does not take) is copied, never opened.
pub(super) fn hint_action(
    kind: kettle_core::hints::Kind,
    alternate: bool,
    unsafe_path: bool,
) -> HintAction {
    use kettle_core::hints::Kind;
    match (kind, alternate) {
        (Kind::Url, false) => HintAction::OpenUrl,
        (Kind::Path, true) if !unsafe_path => HintAction::OpenPath,
        _ => HintAction::Copy,
    }
}

/// What quick select for previews offers: text naming a local file a lane
/// can preview, a path or a `file://` link, as `preview_link` labels it. A
/// path off Windows only, where `hint_path_url` resolves one.
pub(super) fn hint_previews(what: &HintWhat) -> bool {
    use kettle_core::hints::Kind;
    match what {
        HintWhat::Text {
            kind: Kind::Path,
            text,
            bounded: true,
        } => !cfg!(windows) && previewable_name(text),
        HintWhat::Text {
            kind: Kind::Url,
            text,
            ..
        } => kettle_core::links::local_file_path(text)
            .is_some_and(|path| previewable_name(&path.to_string_lossy())),
        _ => false,
    }
}
