//! Bounded parser for Kettle's shell completion side channel.

pub const PREFIX: &[u8] = b"777;kettle-completion;";
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    Completion,
    Prediction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionCandidate {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionList {
    pub generation: u64,
    pub kind: CompletionKind,
    pub selected: Option<usize>,
    pub source: String,
    pub candidates: Vec<CompletionCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionUpdate {
    Show(CompletionList),
    Update(CompletionList),
    Clear { generation: u64 },
}

pub fn parse(payload: &[u8]) -> Option<CompletionUpdate> {
    if payload.len() > MAX_MESSAGE_BYTES || !payload.starts_with(PREFIX) {
        return None;
    }
    let text = std::str::from_utf8(&payload[PREFIX.len()..]).ok()?;
    let mut fields = text.split(';');
    if fields.next()? != "1" {
        return None;
    }
    let operation = fields.next()?;
    let generation = fields.next()?.parse::<u64>().ok()?;
    if operation == "clear" {
        return fields
            .next()
            .is_none()
            .then_some(CompletionUpdate::Clear { generation });
    }
    if !matches!(operation, "show" | "update") {
        return None;
    }
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
    let remaining: Vec<&str> = fields.collect();
    if !remaining.len().is_multiple_of(2) || remaining.len() / 2 > MAX_CANDIDATES {
        return None;
    }
    let mut candidates = Vec::with_capacity(remaining.len() / 2);
    for pair in remaining.chunks_exact(2) {
        let label = decode_field(pair[0], MAX_LABEL_BYTES)?;
        if label.is_empty() {
            return None;
        }
        candidates.push(CompletionCandidate {
            label,
            description: decode_field(pair[1], MAX_DESCRIPTION_BYTES)?,
        });
    }
    if candidates.is_empty() || selected.is_some_and(|index| index >= candidates.len()) {
        return None;
    }
    let list = CompletionList {
        generation,
        kind,
        selected,
        source,
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
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
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
        assert_eq!(list.candidates[1].description, "search text");
        assert_eq!(
            parse(b"777;kettle-completion;1;clear;8"),
            Some(CompletionUpdate::Clear { generation: 8 })
        );
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
    fn rejects_controls_bad_selection_and_unbounded_lists() {
        assert!(parse(b"777;kettle-completion;1;show;1;completion;2;fish;a;").is_none());
        assert!(parse(b"777;kettle-completion;1;show;1;completion;;fish;a%0Aevil;").is_none());
        assert!(
            parse(b"777;kettle-completion;1;show;1;completion;;fish;safe%E2%80%AEtxt;").is_none(),
            "bidi overrides must not reorder terminal-owned UI"
        );
        let mut payload = b"777;kettle-completion;1;show;1;completion;;fish".to_vec();
        for index in 0..=MAX_CANDIDATES {
            payload.extend_from_slice(format!(";item{index};").as_bytes());
        }
        assert!(parse(&payload).is_none());
    }
}
