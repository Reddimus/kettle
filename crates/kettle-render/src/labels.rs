#[cfg(test)]
pub(crate) fn overlay_label_cols(width: f32, cell_width: f32) -> usize {
    if !width.is_finite() || !cell_width.is_finite() || width <= 0.0 || cell_width <= 0.0 {
        return 0;
    }

    ((width / cell_width).floor() as usize).saturating_sub(1)
}

/// Fit a bottom-bar label without allowing Glyphon to create clipped rows.
pub(crate) fn fit_single_line_label(label: &str, max_cols: usize) -> String {
    if display_width(label) <= max_cols {
        return label.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "…".to_string();
    }

    let mut fitted = take_cols_front(label, max_cols - 1);
    fitted.push('…');
    fitted
}

/// Fit a key hint such as `(Enter run · Tab/↑↓ select · Esc cancel)` to
/// `max_cols`. Hints list keys in order of use and end with the way out, so
/// when the whole hint does not fit, segments are dropped from the middle,
/// then from the front, and the last one stays readable; only when it alone
/// does not fit is it cut with an ellipsis. Text in any other shape is fitted
/// as a plain label.
pub(crate) fn fit_key_hint(hint: &str, max_cols: usize) -> String {
    if display_width(hint) <= max_cols {
        return hint.to_string();
    }
    let segments: Vec<&str> = match hint.strip_prefix('(').and_then(|h| h.strip_suffix(')')) {
        Some(inner) => inner.split(" · ").collect(),
        None => return fit_single_line_label(hint, max_cols),
    };
    let Some((last, leading)) = segments.split_last() else {
        return fit_single_line_label(hint, max_cols);
    };
    for keep in (0..leading.len()).rev() {
        let mut kept = leading[..keep].to_vec();
        kept.push(last);
        let candidate = format!("({})", kept.join(" · "));
        if display_width(&candidate) <= max_cols {
            return candidate;
        }
    }
    fit_single_line_label(&format!("({last})"), max_cols)
}

/// Display width (terminal columns) of a string, via `unicode-width`: the
/// measure every overlay layout fits its text by. Callers that size a
/// control for text the renderer will draw measure with this too.
pub fn display_width(s: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    if s.is_ascii() {
        return codepoint_columns(s);
    }
    s.graphemes(true).map(grapheme_columns).sum()
}

fn codepoint_columns(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// A grapheme's columns: the sum of its codepoints' widths, and for one
/// presented as emoji at least its width as an emoji sequence. Chrome titles
/// draw those in the colour-emoji face about two cells wide, and a keycap or
/// a text symbol with U+FE0F sums to one, so measured by codepoint it pushed
/// the rest of a fitted title off its lane. The sequence width counts U+FE0F
/// only after a base it applies to: an ellipsis a fitter puts before a kept
/// U+FE0F joins it in one grapheme and stays one column.
fn grapheme_columns(grapheme: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let columns = codepoint_columns(grapheme);
    if !grapheme.is_ascii() && crate::fonts::emoji_text(grapheme) {
        columns.max(grapheme.width())
    } else {
        columns
    }
}

/// Take whole graphemes from the FRONT of `s` up to `cols` display columns.
///
/// A cut inside a grapheme draws something `s` never held: half a flag is a
/// lone regional indicator, and a ZWJ sequence cut after its joiner is a
/// different emoji. Each grapheme counts as [`display_width`] counts it.
pub(crate) fn take_cols_front(s: &str, cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut acc = 0usize;
    let mut end = 0;
    for (at, grapheme) in s.grapheme_indices(true) {
        let w = grapheme_columns(grapheme);
        if acc + w > cols {
            break;
        }
        acc += w;
        end = at + grapheme.len();
    }
    s[..end].to_string()
}

/// Take whole graphemes from the BACK of `s` up to `cols` display columns,
/// as [`take_cols_front`] takes them from the front. Cut by codepoint, the
/// back of a Japanese flag then a US one (regional indicators J P U S)
/// starts with P U, which draws as a third flag.
pub(crate) fn take_cols_back(s: &str, cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut acc = 0usize;
    let mut start = s.len();
    for (at, grapheme) in s.grapheme_indices(true).rev() {
        let w = grapheme_columns(grapheme);
        if acc + w > cols {
            break;
        }
        acc += w;
        start = at;
    }
    s[start..].to_string()
}

/// Shorten `s` to at most `n` display columns with a MIDDLE ellipsis, preserving
/// both ends — the right choice for paths and `user@host:dir` titles where the
/// tail (program / leaf dir) is the most identifying part. When `s` looks like a
/// path, the trailing segment after the last `/` or `\` is kept whole if it fits
/// (`C:\…\pwsh.exe`); otherwise it falls back to a symmetric split. Measured in
/// columns, not chars, so CJK/wide glyphs don't overflow.
pub(crate) fn middle_ellipsis(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if display_width(s) <= n {
        return s.to_string();
    }
    if n == 1 {
        return "…".to_string();
    }
    let budget = n - 1; // reserve 1 col for the `…`
    // Prefer keeping the trailing path segment (program / leaf) intact.
    let leaf_separator = s.rfind(['/', '\\']);
    let leaf_start = leaf_separator.map(|i| i + 1).unwrap_or(0);
    if let Some(separator_start) = leaf_separator {
        let leaf = &s[leaf_start..];
        let separator = &s[separator_start..leaf_start];
        let tail_w = display_width(separator) + display_width(leaf);
        if tail_w <= budget && !leaf.is_empty() {
            let head = take_cols_front(&s[..separator_start], budget - tail_w);
            return format!("{head}…{separator}{leaf}");
        }
    }
    // Fallback: symmetric middle split.
    let front_cols = budget.div_ceil(2);
    let back_cols = budget - front_cols;
    let head = take_cols_front(s, front_cols);
    let tail = take_cols_back(s, back_cols);
    format!("{head}…{tail}")
}

/// Build a tab title that fits `n` display columns. Unlike pane titles, tabs
/// favor the *right* side of path-like strings when they must truncate: in a
/// path, the tail is normally the project/leaf (`...flight-event-line-server-go`)
/// and is more useful than the home or drive prefix. Non-path titles use a
/// middle ellipsis.
pub(crate) fn fit_tab_title(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if display_width(s) <= n {
        return s.to_string();
    }
    if !s.contains(['/', '\\']) {
        return middle_ellipsis(s, n);
    }
    if n < 3 {
        return take_cols_back(s, n);
    }
    format!("...{}", take_cols_back(s, n - 3))
}

/// Fit a directory-derived tab label into `n` columns by progressively
/// shedding detail: tier 1 the full (home-abbreviated) path; tier 2 the leaf
/// directory name alone; tier 3 the tail of the leaf with a leading `…` once
/// even the name doesn't fit. The rightmost part (the project / current
/// directory) is the most identifying, so it is kept to the end. This mirrors
/// `fit_pane_titlebar_title`'s progressive-shed shape.
pub(crate) fn fit_tab_path(full: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    // Tier 1: the whole path.
    if display_width(full) <= n {
        return full.to_string();
    }
    // Tier 2: the leaf directory name (segment after the last separator).
    let leaf_start = full.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let leaf = &full[leaf_start..];
    if !leaf.is_empty() && display_width(leaf) <= n {
        return leaf.to_string();
    }
    // Tier 3: the tail of the leaf, with a leading `…` marking the cut.
    let tail_src = if leaf.is_empty() { full } else { leaf };
    if n == 1 {
        return "…".to_string();
    }
    format!("…{}", take_cols_back(tail_src, n - 1))
}

/// Fit only the `{title}` value for a tab segment, using the same budget math as
/// the renderer's tab text path. `title_w` is the title lane in pixels, `cell_w`
/// is the measured monospace cell width, and `tab_format` contributes the fixed
/// non-title prefix/suffix such as `{n}: `.
pub fn fit_tab_segment_title(
    title: &str,
    path: Option<&str>,
    idx: usize,
    tab_format: &str,
    title_w: f32,
    cell_w: f32,
) -> String {
    let n = (idx + 1).to_string();
    let avail = (title_w.max(0.0) / cell_w.max(1.0)) as usize;
    let fixed_label = kettle_config::template::fill(tab_format, &[("n", &n), ("title", "")]);
    let fixed_w = display_width(&fixed_label);
    let probe = kettle_config::template::fill(tab_format, &[("n", &n), ("title", "x")]);
    // By codepoint, which adds up across a join: `#` before the title or
    // U+FE0F after it joins the probe's `x` in one grapheme.
    let substitutions = codepoint_columns(&probe).saturating_sub(codepoint_columns(&fixed_label));
    let maxc = avail
        .saturating_sub(fixed_w)
        .checked_div(substitutions)
        .unwrap_or(0);
    match path {
        Some(p) => fit_tab_path(p, maxc),
        None => fit_tab_title(title, maxc),
    }
}

/// Build a per-pane titlebar label that fits `budget` display columns, shedding
/// the least-useful parts first: full → drop the `WxH` size text → drop the
/// `[group]` tag → shorten the title, keeping the program/leaf name (a middle
/// ellipsis, or `fit_tab_path`'s tiers when `title_path` is set). The bell
/// indicator (if any) is kept throughout because it is small and important.
/// The full label reads `"  [g]  title  WxH  🔔"`.
pub fn fit_pane_titlebar_title(
    group: Option<&str>,
    title_prefix: &str,
    title: &str,
    title_path: Option<&str>,
    size_text: Option<&str>,
    bell: Option<&str>,
    budget: usize,
) -> String {
    let bell_part = bell.map(|b| format!("  {b}")).unwrap_or_default();
    let display_title = title_path.unwrap_or(title);
    let assemble = |g: Option<&str>, t: &str, sz: Option<&str>| -> String {
        let mut s = String::new();
        if let Some(g) = g {
            s.push_str("  [");
            s.push_str(g);
            s.push(']');
        }
        s.push_str("  ");
        s.push_str(title_prefix);
        s.push_str(t);
        if let Some(sz) = sz {
            s.push_str("  ");
            s.push_str(sz);
        }
        s.push_str(&bell_part);
        s
    };
    // 1. Everything.
    let full = assemble(group, display_title, size_text);
    if display_width(&full) <= budget {
        return full;
    }
    // 2. Drop the size text.
    let no_size = assemble(group, display_title, None);
    if display_width(&no_size) <= budget {
        return no_size;
    }
    // 3. Drop the group tag.
    let no_group = assemble(None, display_title, None);
    if display_width(&no_group) <= budget {
        return no_group;
    }

    // 4. With metadata shed, prefer the authoritative cwd path when available:
    // full path if it fits, then leaf, then ellipsized leaf tail. This mirrors
    // tab fitting and avoids rendering a shell's already-truncated cwd suffix
    // when Kettle also has OSC 7 cwd truth.
    let fixed = 2 + display_width(title_prefix) + display_width(&bell_part);
    if fixed > budget {
        let bell = bell.unwrap_or("");
        let fitted_bell = fit_single_line_label(bell, budget);
        let bell_part = if !fitted_bell.is_empty() && budget > display_width(&fitted_bell) {
            format!(" {fitted_bell}")
        } else {
            fitted_bell
        };
        let remaining = budget.saturating_sub(display_width(&bell_part));
        let prefix = fit_single_line_label(title_prefix, remaining);
        let title_budget = remaining.saturating_sub(display_width(&prefix));
        let fitted_title = match title_path {
            Some(path) => fit_tab_path(path, title_budget),
            None => middle_ellipsis(title, title_budget),
        };
        return format!("{prefix}{fitted_title}{bell_part}");
    }
    let title_budget = budget.saturating_sub(fixed);
    let fitted_title = match title_path {
        Some(path) => fit_tab_path(path, title_budget),
        None => middle_ellipsis(title, title_budget),
    };
    format!("  {title_prefix}{fitted_title}{bell_part}")
}

/// Fit and format a tab label, including the native caption's recording notice.
pub fn tab_segment_label(seg: &crate::TabSeg, format: &str, cw: f32, notice: &str) -> String {
    let avail = (seg.title_rect.2.max(0.0) / cw.max(1.0)) as usize;
    let notice = if seg.active { notice } else { "" };
    let prefix = if notice.is_empty() {
        String::new()
    } else {
        format!("{notice} ")
    };
    let width = (seg.title_rect.2 - display_width(&prefix) as f32 * cw).max(0.0);
    let title = fit_tab_segment_title(&seg.title, seg.path.as_deref(), seg.idx, format, width, cw);
    let body = kettle_config::template::fill(
        format,
        &[("n", &(seg.idx + 1).to_string()), ("title", &title)],
    );
    fit_single_line_label(&format!("{prefix}{body}"), avail)
}
