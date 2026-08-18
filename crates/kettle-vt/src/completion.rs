//! Bounded parser for Kettle's shell completion side channel.

pub const PREFIX: &[u8] = b"777;kettle-completion;";
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_TOTAL_CANDIDATES: usize = 2048;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
/// Protocol v4 emphasis token. Deliberately far smaller than a label: it only
/// has to carry the word the user actually typed, and it is never used to
/// filter, rank, quote, or insert anything.
pub const MAX_TOKEN_BYTES: usize = 128;
/// Visible command text before the cursor on its current editor line. The
/// renderer uses this bounded hint only to align the card with the editable
/// command column; it is never interpreted or executed.
pub const MAX_INPUT_PREFIX_BYTES: usize = 1024;
pub const KEY_TAB: u8 = 1;
pub const KEY_BACKTAB: u8 = 2;
const KEY_MASK: u8 = KEY_TAB | KEY_BACKTAB;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    Completion,
    Prediction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionCandidate {
    pub label: String,
    pub description: String,
    /// Zero-based position in the shell's complete result, not merely this
    /// bounded wire page.
    pub position: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionList {
    /// Prompt session that produced this list. Protocol v1/v2 publishers do
    /// not carry one.
    pub session: Option<u64>,
    pub generation: u64,
    /// The Tab/Shift-Tab request that produced this list. Protocol v1/v2
    /// publishers do not carry one.
    pub request: Option<u64>,
    pub kind: CompletionKind,
    pub selected: Option<usize>,
    pub total: usize,
    pub source: String,
    /// Protocol v4 emphasis hint: the token the shell was completing when the
    /// user first pressed Tab. Presentation only. Protocol v1-v3 publishers do
    /// not carry one, and an empty field normalizes to `None`.
    pub token: Option<String>,
    /// Protocol v4 presentation hint used to recover the command's stable
    /// starting column from the cursor position captured at the first Tab.
    pub input_prefix: Option<String>,
    pub candidates: Vec<CompletionCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionUpdate {
    /// Start a request-numbered completion session at the current prompt.
    Sync {
        session: u64,
        keys: u8,
    },
    /// Change which raw completion keys the active shell keymap assigned to
    /// Kettle without resetting the prompt session or request counter.
    Keymap {
        session: u64,
        keys: u8,
    },
    Show(CompletionList),
    Update(CompletionList),
    Clear {
        session: Option<u64>,
        generation: u64,
        request: Option<u64>,
    },
}

pub fn parse(payload: &[u8]) -> Option<CompletionUpdate> {
    if payload.len() > MAX_MESSAGE_BYTES || !payload.starts_with(PREFIX) {
        return None;
    }
    let text = std::str::from_utf8(&payload[PREFIX.len()..]).ok()?;
    let mut fields = text.split(';');
    let version = fields.next()?;
    if !matches!(version, "1" | "2" | "3" | "4") {
        return None;
    }
    // v4 adds one presentation field to `show`/`update`. Its session, request,
    // keymap, and clear semantics are v3's, so both versions share this path.
    let sequenced = matches!(version, "3" | "4");
    let operation = fields.next()?;
    if sequenced && operation == "sync" {
        let session = fields.next()?.parse::<u64>().ok()?;
        let keys = fields.next()?.parse::<u8>().ok()?;
        return fields
            .next()
            .is_none()
            .then_some(CompletionUpdate::Sync { session, keys })
            .filter(|_| keys & !KEY_MASK == 0);
    }
    if sequenced && operation == "keymap" {
        let session = fields.next()?.parse::<u64>().ok()?;
        let keys = fields.next()?.parse::<u8>().ok()?;
        return fields
            .next()
            .is_none()
            .then_some(CompletionUpdate::Keymap { session, keys })
            .filter(|_| keys & !KEY_MASK == 0);
    }
    let session = if sequenced {
        Some(fields.next()?.parse::<u64>().ok()?)
    } else {
        None
    };
    let generation = fields.next()?.parse::<u64>().ok()?;
    if operation == "clear" {
        let request = if sequenced {
            Some(fields.next()?.parse::<u64>().ok()?)
        } else {
            None
        };
        return fields.next().is_none().then_some(CompletionUpdate::Clear {
            session,
            generation,
            request,
        });
    }
    if !matches!(operation, "show" | "update") {
        return None;
    }
    let request = if sequenced {
        Some(fields.next()?.parse::<u64>().ok()?)
    } else {
        None
    };
    let kind = match fields.next()? {
        "completion" => CompletionKind::Completion,
        "prediction" => CompletionKind::Prediction,
        _ => return None,
    };
    let selected = match fields.next()? {
        "" => None,
        value => Some(value.parse::<usize>().ok()?),
    };
    let source = decode_field(fields.next()?, 64)?;
    // These are presentation-only hints. Their fields remain structurally
    // required in v4, but unsafe, malformed, or oversized values degrade to no
    // emphasis/alignment rather than hiding an otherwise safe candidate page.
    let token = if version == "4" {
        decode_field(fields.next()?, MAX_TOKEN_BYTES).filter(|value| !value.is_empty())
    } else {
        None
    };
    let input_prefix = if version == "4" {
        decode_field(fields.next()?, MAX_INPUT_PREFIX_BYTES).filter(|value| !value.is_empty())
    } else {
        None
    };
    let (offset, total) = if matches!(version, "2" | "3" | "4") {
        (
            fields.next()?.parse::<usize>().ok()?,
            fields.next()?.parse::<usize>().ok()?,
        )
    } else {
        (0, usize::MAX)
    };
    let remaining: Vec<&str> = fields.collect();
    if !remaining.len().is_multiple_of(2) || remaining.len() / 2 > MAX_CANDIDATES {
        return None;
    }
    let raw_count = remaining.len() / 2;
    let total = if version == "1" { raw_count } else { total };
    if total == 0 || total > MAX_TOTAL_CANDIDATES || offset.checked_add(raw_count)? > total {
        return None;
    }
    let mut normalized_selected = None;
    let mut candidates = Vec::with_capacity(remaining.len() / 2);
    for (original_index, pair) in remaining.chunks_exact(2).enumerate() {
        // A single filesystem entry can legally contain a newline or bidi
        // control on Unix. It is not safe to put that label in terminal-owned
        // UI, but it must not suppress every other completion in the message.
        // Keep structural errors fail-closed above and skip only the unsafe
        // label here.
        let Some(label) = decode_field(pair[0], MAX_LABEL_BYTES).filter(|label| !label.is_empty())
        else {
            continue;
        };
        // Descriptions are optional presentation. PowerShell tooltips commonly
        // contain line breaks; dropping the whole candidate made the detached
        // card highlight a different row from the command PSReadLine inserted.
        // Keep the safe label and discard only an unsafe or malformed tooltip.
        let description = decode_field(pair[1], MAX_DESCRIPTION_BYTES).unwrap_or_default();
        if selected == Some(original_index) {
            normalized_selected = Some(candidates.len());
        }
        let position = if version == "1" {
            candidates.len()
        } else {
            offset.checked_add(original_index)?
        };
        candidates.push(CompletionCandidate {
            label,
            description,
            position,
        });
    }
    if candidates.is_empty() {
        return None;
    }
    let list = CompletionList {
        session,
        generation,
        request,
        kind,
        selected: normalized_selected,
        total: if version == "1" {
            candidates.len()
        } else {
            total
        },
        source,
        token,
        input_prefix,
        candidates,
    };
    Some(if operation == "show" {
        CompletionUpdate::Show(list)
    } else {
        CompletionUpdate::Update(list)
    })
}

fn decode_field(field: &str, max_bytes: usize) -> Option<String> {
    if field.len() > max_bytes.saturating_mul(3) {
        return None;
    }
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(field.len().min(max_bytes));
    let mut index = 0;
    while index < bytes.len() {
        let byte = if bytes[index] == b'%' {
            let hi = *bytes.get(index + 1)?;
            let lo = *bytes.get(index + 2)?;
            index += 3;
            hex(hi)?.checked_mul(16)?.checked_add(hex(lo)?)?
        } else {
            let value = bytes[index];
            index += 1;
            value
        };
        if byte < 0x20 || byte == 0x7f || decoded.len() == max_bytes {
            return None;
        }
        decoded.push(byte);
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.chars().any(unsafe_display_scalar)).then_some(decoded)
}

/// Keep terminal-owned UI from interpreting invisible direction controls as
/// part of a candidate. Joiners and variation selectors remain valid for emoji;
/// only Unicode controls and the bidi marks that can reorder neighboring text
/// are refused.
fn unsafe_display_scalar(value: char) -> bool {
    value.is_control()
        || matches!(
            value,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_show_update_and_clear() {
        let show = parse(
            b"777;kettle-completion;1;show;7;completion;1;fish;git;command;grep;search%20text",
        )
        .unwrap();
        let CompletionUpdate::Show(list) = show else {
            panic!("expected show");
        };
        assert_eq!(list.generation, 7);
        assert_eq!(list.selected, Some(1));
        assert_eq!(list.total, 2);
        assert_eq!(list.candidates[1].position, 1);
        assert_eq!(list.candidates[1].description, "search text");
        assert_eq!(
            parse(b"777;kettle-completion;1;clear;8"),
            Some(CompletionUpdate::Clear {
                session: None,
                generation: 8,
                request: None,
            })
        );
    }

    #[test]
    fn version_two_pages_keep_absolute_positions_and_total() {
        let CompletionUpdate::Update(list) = parse(
            b"777;kettle-completion;2;update;8;completion;1;fish;64;130;item64;first;item65;second",
        )
        .expect("version two page") else {
            panic!("expected update");
        };
        assert_eq!(list.total, 130);
        assert_eq!(list.selected, Some(1));
        assert_eq!(list.candidates[0].position, 64);
        assert_eq!(list.candidates[1].position, 65);

        assert!(
            parse(b"777;kettle-completion;2;show;1;completion;0;fish;64;64;item;row").is_none(),
            "a page cannot extend beyond the declared result"
        );
    }

    #[test]
    fn version_three_carries_the_tab_request_and_syncs_sessions() {
        assert_eq!(
            parse(b"777;kettle-completion;3;sync;12;3"),
            Some(CompletionUpdate::Sync {
                session: 12,
                keys: KEY_TAB | KEY_BACKTAB,
            })
        );
        assert_eq!(
            parse(b"777;kettle-completion;3;keymap;12;1"),
            Some(CompletionUpdate::Keymap {
                session: 12,
                keys: KEY_TAB,
            })
        );
        let CompletionUpdate::Show(list) =
            parse(b"777;kettle-completion;3;show;12;9;4;completion;0;fish;64;65;item;row")
                .expect("version three page")
        else {
            panic!("expected show");
        };
        assert_eq!(list.generation, 9);
        assert_eq!(list.session, Some(12));
        assert_eq!(list.request, Some(4));
        assert_eq!(list.total, 65);
        assert_eq!(list.candidates[0].position, 64);
        assert_eq!(
            parse(b"777;kettle-completion;3;clear;12;10;4"),
            Some(CompletionUpdate::Clear {
                session: Some(12),
                generation: 10,
                request: Some(4),
            })
        );
        assert!(
            parse(b"777;kettle-completion;3;sync").is_none(),
            "a sync without a prompt-session identity must fail closed"
        );
        assert!(
            parse(b"777;kettle-completion;3;sync;12;4").is_none(),
            "a sync with unknown key bits must fail closed"
        );
        assert!(
            parse(b"777;kettle-completion;3;clear;12;10").is_none(),
            "a sequenced clear without its request identity must fail closed"
        );
    }

    #[test]
    fn version_four_carries_bounded_presentation_hints() {
        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;4;show;12;9;4;completion;0;fish;ch;git%20ch;64;65;item;row",
        )
        .expect("version four page") else {
            panic!("expected show");
        };
        assert_eq!(list.session, Some(12));
        assert_eq!(list.generation, 9);
        assert_eq!(list.request, Some(4));
        assert_eq!(list.token.as_deref(), Some("ch"));
        assert_eq!(list.input_prefix.as_deref(), Some("git ch"));
        assert_eq!(list.total, 65);
        assert_eq!(list.candidates[0].position, 64);

        let CompletionUpdate::Update(list) =
            parse(b"777;kettle-completion;4;update;12;10;4;completion;;fish;;;0;1;item;row")
                .expect("empty presentation hints are valid absences")
        else {
            panic!("expected update");
        };
        assert_eq!(list.token, None);
        assert_eq!(list.input_prefix, None);

        // v4 reuses v3's sequencing envelope verbatim.
        assert_eq!(
            parse(b"777;kettle-completion;4;sync;12;3"),
            Some(CompletionUpdate::Sync {
                session: 12,
                keys: KEY_TAB | KEY_BACKTAB,
            })
        );
        assert_eq!(
            parse(b"777;kettle-completion;4;keymap;12;1"),
            Some(CompletionUpdate::Keymap {
                session: 12,
                keys: KEY_TAB,
            })
        );
        assert_eq!(
            parse(b"777;kettle-completion;4;clear;12;10;4"),
            Some(CompletionUpdate::Clear {
                session: Some(12),
                generation: 10,
                request: Some(4),
            })
        );
    }

    #[test]
    fn version_four_requires_hint_fields_but_degrades_unsafe_hint_values() {
        assert!(
            parse(b"777;kettle-completion;4;show;12;9;4;completion;0;fish;64;65;item;row")
                .is_none(),
            "a v4 page without its token field must fail closed"
        );
        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;4;show;12;9;4;completion;0;fish;a%E2%80%AEb;git%20a;64;65;item;row",
        )
        .expect("an unsafe emphasis hint must not hide safe candidates")
        else {
            panic!("expected show");
        };
        assert_eq!(list.token, None);
        assert_eq!(list.input_prefix.as_deref(), Some("git a"));

        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;4;show;12;9;4;completion;0;fish;a%09b;git%20a;64;65;item;row",
        )
        .expect("a control-bearing emphasis hint must degrade") else {
            panic!("expected show");
        };
        assert_eq!(list.token, None);
        let oversized = "a".repeat(MAX_TOKEN_BYTES + 1);
        let CompletionUpdate::Show(list) = parse(
            format!(
                "777;kettle-completion;4;show;12;9;4;completion;0;fish;{oversized};git;64;65;item;row"
            )
            .as_bytes(),
        )
        .expect("an oversized emphasis hint must degrade")
        else {
            panic!("expected show");
        };
        assert_eq!(list.token, None);
        let exact = "a".repeat(MAX_TOKEN_BYTES);
        let CompletionUpdate::Show(list) = parse(
            format!(
                "777;kettle-completion;4;show;12;9;4;completion;0;fish;{exact};git;64;65;item;row"
            )
            .as_bytes(),
        )
        .expect("the token cap is inclusive") else {
            panic!("expected show");
        };
        assert_eq!(list.token.as_deref(), Some(exact.as_str()));
        let oversized_prefix = "a".repeat(MAX_INPUT_PREFIX_BYTES + 1);
        let CompletionUpdate::Show(list) = parse(
            format!(
                "777;kettle-completion;4;show;12;9;4;completion;0;fish;g;{oversized_prefix};64;65;item;row"
            )
            .as_bytes(),
        )
        .expect("an oversized alignment hint must degrade")
        else {
            panic!("expected show");
        };
        assert_eq!(list.input_prefix, None);
        for unsafe_prefix in ["git%20a%E2%80%AEb", "git%09a"] {
            let payload = format!(
                "777;kettle-completion;4;show;12;9;4;completion;0;fish;g;{unsafe_prefix};64;65;item;row"
            );
            let CompletionUpdate::Show(list) =
                parse(payload.as_bytes()).expect("an unsafe alignment hint must degrade")
            else {
                panic!("expected show");
            };
            assert_eq!(list.input_prefix, None);
        }
        assert!(
            parse(b"777;kettle-completion;5;show;12;9;4;completion;0;fish;g;64;65;item;row")
                .is_none(),
            "an unknown protocol version stays unparsed"
        );
    }

    #[test]
    fn legacy_publishers_carry_no_emphasis_token() {
        for payload in [
            b"777;kettle-completion;1;show;7;completion;1;fish;git;command".as_slice(),
            b"777;kettle-completion;2;show;8;completion;0;fish;0;1;git;command".as_slice(),
            b"777;kettle-completion;3;show;12;9;4;completion;0;fish;0;1;git;command".as_slice(),
        ] {
            let (CompletionUpdate::Show(list) | CompletionUpdate::Update(list)) =
                parse(payload).expect("legacy page")
            else {
                panic!("expected show");
            };
            assert_eq!(list.token, None, "{payload:?}");
        }
    }

    /// The shell encoders truncate by characters against these byte caps, so
    /// the exact boundary is load-bearing: one byte over and the whole message
    /// is dropped, taking every candidate with it.
    #[test]
    fn field_length_caps_are_inclusive() {
        for (max, exact) in [
            (MAX_LABEL_BYTES, MAX_LABEL_BYTES),
            (MAX_DESCRIPTION_BYTES, MAX_DESCRIPTION_BYTES),
        ] {
            assert_eq!(
                decode_field(&"a".repeat(exact), max).map(|s| s.len()),
                Some(exact)
            );
            assert!(decode_field(&"a".repeat(exact + 1), max).is_none());
        }
        // Four-byte characters land exactly on the cap when the encoder counts
        // characters at a quarter of the byte budget.
        let label = "\u{10348}".repeat(MAX_LABEL_BYTES / 4);
        let encoded: String = label.bytes().map(|byte| format!("%{byte:02X}")).collect();
        assert_eq!(
            decode_field(&encoded, MAX_LABEL_BYTES).as_deref(),
            Some(label.as_str())
        );
    }

    #[test]
    fn rejects_structural_errors_and_unbounded_lists() {
        assert!(parse(b"777;kettle-completion;1;show;1;completion;two;fish;a;").is_none());
        let mut payload = b"777;kettle-completion;1;show;1;completion;;fish".to_vec();
        for index in 0..=MAX_CANDIDATES {
            payload.extend_from_slice(format!(";item{index};").as_bytes());
        }
        assert!(parse(&payload).is_none());
        assert!(
            parse(b"777;kettle-completion;3;show;1;1;1;completion;0;fish;0;2049;item;row")
                .is_none(),
            "the accessibility result size must stay inside the retained-state cap"
        );
    }

    #[test]
    fn skips_only_unsafe_rows_and_remaps_the_selection() {
        let update = parse(
            b"777;kettle-completion;1;show;1;completion;2;fish;safe;first;\
              a%0Aevil;newline;chosen;third;safe%E2%80%AEtxt;bidi",
        )
        .expect("safe rows around unsafe candidates must survive");
        let CompletionUpdate::Show(list) = update else {
            panic!("expected show");
        };
        assert_eq!(list.candidates.len(), 2);
        assert_eq!(list.candidates[0].label, "safe");
        assert_eq!(list.candidates[1].label, "chosen");
        assert_eq!(list.selected, Some(1));
        assert_eq!(list.total, 2);
        assert_eq!(list.candidates[1].position, 1);

        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;3;show;9;2;1;completion;0;powershell;0;2;Get-Acl;Get-Acl%0A%20%20%20%20%20%20%20%20%20%20;Get-AppPackage;safe",
        )
        .expect("an unsafe optional tooltip must not remove its safe candidate")
        else {
            panic!("expected show");
        };
        assert_eq!(list.candidates.len(), 2);
        assert_eq!(list.candidates[0].label, "Get-Acl");
        assert_eq!(list.candidates[0].description, "");
        assert_eq!(list.selected, Some(0));

        let CompletionUpdate::Show(list) =
            parse(b"777;kettle-completion;1;show;2;completion;8;fish;only;row")
                .expect("an out-of-range selection must not hide valid rows")
        else {
            panic!("expected show");
        };
        assert_eq!(list.selected, None);

        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;1;show;3;completion;;fish;safe;row;bad%E2%80%A8line;separator;bad%E2%80%A9paragraph;separator",
        )
        .expect("Unicode line separators must skip only their own rows")
        else {
            panic!("expected show");
        };
        assert_eq!(list.candidates.len(), 1);
        assert_eq!(list.candidates[0].label, "safe");

        let CompletionUpdate::Show(list) = parse(
            b"777;kettle-completion;2;show;4;completion;2;fish;64;130;safe;first;bad%0Arow;newline;chosen;third",
        )
        .expect("a version two page must retain its absolute coordinates")
        else {
            panic!("expected show");
        };
        assert_eq!(list.total, 130);
        assert_eq!(list.selected, Some(1));
        assert_eq!(list.candidates[0].position, 64);
        assert_eq!(list.candidates[1].position, 66);
    }
}
