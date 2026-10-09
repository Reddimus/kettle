//! Inline cards as a harness prints them: the block of placeholder cells
//! under a tool call and the caption beneath it, all built by Kettle. Nothing
//! here comes from the harness or the sender except the file's name, which is
//! sanitized before it can reach a caption.

use kettle_core::{InlineMarker, InlineNonce};
use kettle_media::MediaKind;

/// Where Claude Code starts a hook message's lines: five columns in.
pub(crate) const CLAUDE_INDENT: usize = 5;
/// Fewest and most rows a Claude Code card spans.
const CLAUDE_ROWS: (usize, usize) = (3, 8);
/// Longest caption kept, before it is fitted to the pane's width.
pub(crate) const MAX_CAPTION_BYTES: usize = 512;

/// A Claude Code card's size in cells for an image of `image` pixels in a
/// pane of `pane` (columns, lines) with cells of `cell` pixels: a quarter of
/// the pane's lines, from three to eight rows, and the columns that keep the
/// image's shape, within the width Claude Code prints without wrapping.
/// `None` when the pane cannot hold a card with its label and caption.
pub(crate) fn claude_card_size(
    pane: (usize, usize),
    cell: (f32, f32),
    image: (u32, u32),
) -> Option<(u8, u8)> {
    let (columns, lines) = pane;
    let (cell_width, cell_height) = cell;
    let (width, height) = image;
    let finite = |value: f32| value.is_finite() && value > 0.0;
    if !finite(cell_width) || !finite(cell_height) || width == 0 || height == 0 {
        return None;
    }
    // One column is left free: Claude Code wraps a line that reaches the edge.
    let room = columns
        .checked_sub(CLAUDE_INDENT + 1)?
        .min(usize::from(kettle_render::MAX_CARD_COLUMNS));
    let rows = (lines / 4).clamp(CLAUDE_ROWS.0, CLAUDE_ROWS.1);
    // The label above, which must print on one line, and the caption below
    // take a line each.
    let label = kettle_render::CardHarness::ClaudeHook.label_columns();
    if room == 0 || columns <= label || lines < rows + 2 {
        return None;
    }
    let shaped = rows as f32 * cell_height / cell_width * width as f32 / height as f32;
    let card_columns = (shaped.round() as usize).clamp(1, room);
    Some((u8::try_from(rows).ok()?, u8::try_from(card_columns).ok()?))
}

/// The message Claude Code prints under the call: the card's rows of
/// placeholder cells, then its caption, each on a line of its own after the
/// hook's label.
pub(crate) fn claude_card_message(
    nonce: InlineNonce,
    size: (u8, u8),
    caption: &str,
) -> Option<String> {
    let (rows, columns) = size;
    let mut message = String::with_capacity(
        usize::from(rows) * (usize::from(columns) * 20 + 1) + caption.len() + 1,
    );
    for row in 0..rows {
        message.push('\n');
        for column in 0..columns {
            message.push_str(&InlineMarker { row, column, nonce }.encode()?);
        }
    }
    message.push('\n');
    message.push_str(caption);
    Some(message)
}

/// What a card says under it: the file's name, when it came from a file, and
/// what Kettle made of it, on one line no wider than `columns`. Nothing else
/// the sender sent, such as a title, reaches a caption.
pub(crate) fn card_caption(
    name: Option<&str>,
    kind: MediaKind,
    size: (u32, u32),
    columns: usize,
) -> String {
    let (width, height) = size;
    let detail = format!("{} {width}x{height}", kind.as_str());
    let caption = match name.map(caption_name).filter(|name| !name.is_empty()) {
        Some(name) => {
            let detail = format!(" - {detail}");
            let room =
                columns.saturating_sub(unicode_width::UnicodeWidthStr::width(detail.as_str()));
            format!("{}{detail}", fit(&name, room))
        }
        None => detail,
    };
    if unicode_width::UnicodeWidthStr::width(caption.as_str()) > columns {
        fit(&caption, columns)
    } else {
        caption
    }
}

/// A file's name as a caption may hold it: no control, format, separator,
/// private-use or zero-width character, so it cannot break the caption's
/// line or forge a card's marks, and no longer than [`MAX_CAPTION_BYTES`].
pub(crate) fn caption_name(name: &str) -> String {
    let mut kept = String::new();
    for character in name.chars() {
        let code = u32::from(character);
        let private = (0xE000..=0xF8FF).contains(&code) || code >= 0xF_0000;
        let zero_width = unicode_width::UnicodeWidthChar::width(character).is_none_or(|w| w == 0);
        let format = crate::app::is_bidi_format_char(character)
            || matches!(character, '\u{2028}' | '\u{2029}' | '\u{061C}');
        if character.is_control() || private || zero_width || format {
            continue;
        }
        if kept.len() + character.len_utf8() > MAX_CAPTION_BYTES {
            break;
        }
        kept.push(character);
    }
    kept
}

/// `text` cut to `columns` display columns, ending with an ellipsis when
/// anything was cut.
fn fit(text: &str, columns: usize) -> String {
    use unicode_width::UnicodeWidthChar as _;
    if unicode_width::UnicodeWidthStr::width(text) <= columns {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut width = 0;
    for character in text.chars() {
        let next = character.width().unwrap_or(0);
        if width + next + 1 > columns {
            break;
        }
        kept.push(character);
        width += next;
    }
    if columns > 0 {
        kept.push('…');
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_takes_a_quarter_of_the_pane_and_keeps_the_images_shape() {
        // 80x40 pane, cells twice as tall as wide, a 4:3 image.
        let (rows, columns) = claude_card_size((80, 40), (8.0, 16.0), (640, 480)).unwrap();
        assert_eq!(rows, 8, "a quarter of 40 lines, at most eight");
        assert_eq!(columns, 21, "8 rows × 2 × 4/3");
        assert_eq!(
            claude_card_size((80, 12), (8.0, 16.0), (640, 480))
                .unwrap()
                .0,
            3
        );
        // A very wide image fills the room left of the edge, and no more.
        let (_, wide) = claude_card_size((80, 40), (8.0, 16.0), (10_000, 10)).unwrap();
        assert_eq!(usize::from(wide), 80 - CLAUDE_INDENT - 1);
        let (_, widest) = claude_card_size((400, 40), (8.0, 16.0), (10_000, 10)).unwrap();
        assert_eq!(widest, kettle_render::MAX_CARD_COLUMNS);
        // A pane that cannot hold the rows with a label and caption has none,
        // nor one too narrow to print the label on one line.
        assert_eq!(claude_card_size((80, 4), (8.0, 16.0), (640, 480)), None);
        let label = kettle_render::CardHarness::ClaudeHook.label_columns();
        assert_eq!(label, 61);
        assert_eq!(claude_card_size((label, 40), (8.0, 16.0), (640, 480)), None);
        assert!(claude_card_size((label + 1, 40), (8.0, 16.0), (640, 480)).is_some());
        assert_eq!(claude_card_size((6, 40), (8.0, 16.0), (640, 480)), None);
        assert_eq!(claude_card_size((80, 40), (0.0, 16.0), (640, 480)), None);
        assert_eq!(claude_card_size((80, 40), (8.0, 16.0), (0, 480)), None);
    }

    #[test]
    fn the_message_is_the_cards_rows_then_its_caption() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let message = claude_card_message(nonce, (3, 4), "plot.png - raster 64x48").unwrap();
        let lines: Vec<&str> = message.split('\n').collect();
        assert_eq!(
            lines.len(),
            5,
            "a leading break, three rows and the caption"
        );
        assert_eq!(lines[0], "", "the rows start on the line after the label");
        for (row, line) in lines[1..4].iter().enumerate() {
            assert_eq!(
                line.chars().filter(|&c| c == '\u{10eeee}').count(),
                4,
                "{row}"
            );
            let expected: String = (0..4)
                .map(|column| {
                    InlineMarker {
                        row: row as u8,
                        column,
                        nonce,
                    }
                    .encode()
                    .unwrap()
                })
                .collect();
            assert_eq!(*line, expected);
        }
        assert_eq!(lines[4], "plot.png - raster 64x48");
        // An eight-row, widest card stays within what Claude Code prints whole.
        let widest = claude_card_message(nonce, (8, kettle_render::MAX_CARD_COLUMNS), "x").unwrap();
        assert!(
            widest.encode_utf16().count() <= kettle_ctl::show::MAX_INLINE_MESSAGE_UTF16,
            "{}",
            widest.encode_utf16().count()
        );
    }

    #[test]
    fn a_caption_names_the_file_and_fits_one_line() {
        assert_eq!(
            card_caption(Some("plot.png"), MediaKind::Raster, (640, 480), 80),
            "plot.png - raster 640x480"
        );
        assert_eq!(
            card_caption(None, MediaKind::Svg, (64, 48), 80),
            "svg 64x48",
            "no file, no name"
        );
        let long = card_caption(Some(&"x".repeat(200)), MediaKind::Svg, (1, 2), 30);
        assert!(
            unicode_width::UnicodeWidthStr::width(long.as_str()) <= 30,
            "{long}"
        );
        assert!(long.ends_with("… - svg 1x2"), "{long}");
        let tight = card_caption(Some("plot.png"), MediaKind::Raster, (640, 480), 8);
        assert!(
            unicode_width::UnicodeWidthStr::width(tight.as_str()) <= 8,
            "{tight}"
        );
    }

    #[test]
    fn a_caption_name_cannot_forge_marks_or_break_its_line() {
        let forged = format!(
            "a{}\u{0305}b\nc\u{202e}d\u{2028}e\u{200b}f\u{e000}g",
            '\u{10eeee}'
        );
        assert_eq!(caption_name(&forged), "abcdefg");
        assert!(caption_name(&"é".repeat(400)).len() <= MAX_CAPTION_BYTES);
    }
}
