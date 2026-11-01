pub(super) const CELL_FLAG_UNDERLINE: u32 = 0b0000_0000_0000_1000;
pub(super) const CELL_FLAG_STRIKEOUT: u32 = 0b0000_0010_0000_0000;
pub(super) const CELL_FLAG_DOUBLE_UNDERLINE: u32 = 0b0000_1000_0000_0000;
pub(super) const CELL_FLAG_UNDERCURL: u32 = 0b0001_0000_0000_0000;
pub(super) const CELL_FLAG_DOTTED_UNDERLINE: u32 = 0b0010_0000_0000_0000;
pub(super) const CELL_FLAG_DASHED_UNDERLINE: u32 = 0b0100_0000_0000_0000;
pub(super) const CELL_FLAG_ALL_UNDERLINES: u32 = CELL_FLAG_UNDERLINE
    | CELL_FLAG_DOUBLE_UNDERLINE
    | CELL_FLAG_UNDERCURL
    | CELL_FLAG_DOTTED_UNDERLINE
    | CELL_FLAG_DASHED_UNDERLINE;
pub(super) const MAX_CTL_SELECTION_BYTES: usize = 128 * 1024;
pub(super) const MAX_CTL_TEXT_PAGE_BYTES: usize = 256 * 1024;
pub(super) const MAX_CTL_COMMAND_OUTPUT_BYTES: usize = 512 * 1024;
/// Bound live-state enumeration before building JSON values or terminal
/// snapshots. This matches the core search materialization ceiling and is far
/// above a plausible interactive terminal/window inventory.
pub(super) const MAX_CTL_COLLECTION_ITEMS: usize = 262_144;
/// `read_screen` pages are smaller than this, but the complete snapshot is
/// hashed for stable pagination. Reject before materializing a larger scrape.
pub(super) const MAX_CTL_SCREEN_SNAPSHOT_BYTES: usize = 512 * 1024;
pub(super) const MAX_CTL_SCROLLBACK_LINES: usize = 10_000;
pub(super) const MAX_CTL_SEND_KEYS: usize = 1024;
pub(super) const MAX_CTL_SEND_KEY_TOKEN_BYTES: usize = 64;
pub(super) const MAX_CTL_SEND_KEY_BYTES: usize = 64 * 1024;

pub(super) fn optional_u64_param(
    params: &serde_json::Value,
    name: &str,
) -> std::result::Result<Option<u64>, String> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("'{name}' must be an unsigned integer")),
    }
}

pub(super) fn ctl_capture_bounds(
    history_origin: u64,
    history_size: usize,
    start_line: u64,
    cursor_abs: u64,
    returned_lines: usize,
) -> (usize, usize, bool) {
    let screen_top = history_origin.saturating_add(history_size as u64);
    let take = screen_top
        .saturating_sub(start_line)
        .min(MAX_CTL_SCROLLBACK_LINES as u64)
        .max(1)
        .min(history_size as u64);
    let first_abs = screen_top.saturating_sub(take);
    let start_idx = start_line
        .saturating_sub(first_abs)
        .min(returned_lines as u64) as usize;
    // Inclusive of the cursor line; clamp to what screen_text actually returned.
    let end_idx = cursor_abs
        .saturating_sub(first_abs)
        .saturating_add(1)
        .min(returned_lines as u64) as usize;
    (start_idx, end_idx, start_line < first_abs)
}

pub(super) fn cap_ctl_selection(mut text: String) -> (String, bool) {
    if text.len() <= MAX_CTL_SELECTION_BYTES {
        return (text, false);
    }
    let mut end = MAX_CTL_SELECTION_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

pub(super) fn cap_ctl_command_response(
    mut response: kettle_ctl::protocol::Response,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{MAX_RESPONSE_LINE_BYTES, to_json_vec_bounded};
    let serde_json::Value::String(mut output) = response.result["output"].take() else {
        return response;
    };
    response.result["output"] = serde_json::json!("");
    let overhead = to_json_vec_bounded(&response, MAX_RESPONSE_LINE_BYTES)
        .map_or(MAX_RESPONSE_LINE_BYTES, |bytes| bytes.len());
    let budget = MAX_RESPONSE_LINE_BYTES.saturating_sub(overhead);
    let mut encoded = 0;
    let mut end = 0;
    for (at, ch) in output.char_indices() {
        let cost = match ch {
            '"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
            '\0'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if at + ch.len_utf8() > MAX_CTL_COMMAND_OUTPUT_BYTES || encoded + cost > budget {
            break;
        }
        encoded += cost;
        end = at + ch.len_utf8();
    }
    if end < output.len() {
        output.truncate(end);
        response.result["output_truncated"] = serde_json::json!(true);
    }
    response.result["output"] = serde_json::Value::String(output);
    response
}

/// Merge independently-owned placement classes without letting a saturated
/// class hide every item from the others. This preserves all three bounded
/// registries for visibility culling; the renderer applies the smaller shared
/// frame quota after removing offscreen placements.
pub(super) fn round_robin_bounded<T, const N: usize>(groups: [Vec<T>; N], limit: usize) -> Vec<T> {
    let mut groups = groups.map(Vec::into_iter);
    let mut out = Vec::with_capacity(
        groups
            .iter()
            .map(|group| group.len())
            .sum::<usize>()
            .min(limit),
    );
    while out.len() < limit {
        let before = out.len();
        for group in &mut groups {
            if let Some(item) = group.next() {
                out.push(item);
                if out.len() == limit {
                    break;
                }
            }
        }
        if out.len() == before {
            break;
        }
    }
    out
}

pub(super) fn ctl_snapshot(kind: &str, value: &(impl serde::Serialize + ?Sized)) -> String {
    // Stable FNV-1a is sufficient here: the token detects live-state drift; it
    // is not an authentication primitive.
    struct Hasher(u64);
    impl std::io::Write for Hasher {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for &byte in bytes {
                self.0 ^= u64::from(byte);
                self.0 = self.0.wrapping_mul(0x100000001b3);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = Hasher(0xcbf29ce484222325);
    let _ = std::io::Write::write_all(&mut hasher, kind.as_bytes());
    let _ = serde_json::to_writer(&mut hasher, value);
    format!("{:016x}", hasher.0)
}

/// Sum a lazily-produced inventory while stopping as soon as the budget is
/// crossed. Keeping this lazy is important: the limit exists to bound the
/// enumeration itself, not merely the result allocation that follows it.
pub(super) fn ctl_bounded_sum(
    counts: impl IntoIterator<Item = usize>,
    limit: usize,
) -> Option<usize> {
    let mut total = 0usize;
    for count in counts {
        total = total.checked_add(count)?;
        if total > limit {
            return None;
        }
    }
    Some(total)
}

#[cfg(test)]
pub(super) fn ctl_page_values(
    req: &kettle_ctl::protocol::Request,
    key: &str,
    values: Vec<serde_json::Value>,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};

    let page = match PageRequest::from_params(&req.params) {
        Ok(page) => page,
        Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
    };
    ctl_page_values_with_page(req, key, values, page)
}

pub(super) fn ctl_page_values_with_page(
    req: &kettle_ctl::protocol::Request,
    key: &str,
    values: Vec<serde_json::Value>,
    page: kettle_ctl::protocol::PageRequest,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{Response, error_codes as ec};

    if values.len() > MAX_CTL_COLLECTION_ITEMS {
        return Response::err(
            req.id,
            ec::RESPONSE_TOO_LARGE,
            "live-state collection exceeds the control enumeration budget",
        );
    }
    let snapshot = ctl_snapshot(key, &values);
    if page
        .snapshot
        .as_deref()
        .is_some_and(|want| want != snapshot)
    {
        return Response::err(
            req.id,
            ec::STALE_SNAPSHOT,
            "live state changed between pages; restart without cursor/snapshot",
        );
    }
    let total = values.len();
    let (start, end, next_cursor, truncated) = page.bounds(total);
    let mut result = serde_json::Map::new();
    result.insert(
        key.into(),
        serde_json::Value::Array(values[start..end].to_vec()),
    );
    result.insert("snapshot".into(), serde_json::Value::String(snapshot));
    result.insert("next_cursor".into(), next_cursor.into());
    result.insert("truncated".into(), truncated.into());
    result.insert("total".into(), total.into());
    Response::ok(req.id, serde_json::Value::Object(result))
}

pub(super) fn rect_json(rect: kettle_render::Rect4) -> serde_json::Value {
    serde_json::json!({
        "x": rect.0,
        "y": rect.1,
        "width": rect.2,
        "height": rect.3,
    })
}
