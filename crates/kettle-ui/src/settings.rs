//! Cycle 756: data model for the in-app **Settings overlay** — a
//! keyboard-navigable, non-technical-friendly settings panel (Terminator
//! parity, but native to kettle's overlay architecture). The overlay presents
//! the most-used config keys grouped into categories; changing a field writes
//! the value straight to the user's config file via `App::persist_pref` and
//! reloads it live (the same persist path the right-click Preferences submenu
//! uses), so edits take effect immediately without hand-editing the file.
//!
//! This module is the **pure** half: the category/field catalogue plus the
//! logic to read a field's current value from a [`Config`] and to compute the
//! next value when the user toggles / cycles / steps it. `app.rs` owns the
//! overlay state, input routing, and persistence; `kettle-render` owns drawing.
//! Keeping the catalogue here (free functions over `&Config`) makes it unit
//! testable without a window or renderer.

use kettle_config::{BellMode, Config, CursorStyle, FocusMode, ScrollbarMode};

/// How a setting is edited. The overlay maps ←/→ (or Space/Enter) onto these.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// A boolean. Left/Right/Space flip it. Persisted as `true` / `false`.
    Toggle,
    /// One of a fixed set of choices. Left/Right cycle. `values[i]` is what's
    /// written to config; `labels[i]` is shown to the user (often the same).
    Choice {
        values: &'static [&'static str],
        labels: &'static [&'static str],
    },
    /// An integer in `[min, max]` stepped by `step`. Optional `suffix` for
    /// display (e.g. "%", "px"). Some keys store a different on-disk form than
    /// the displayed integer (e.g. opacity is a 0.0–1.0 float shown as a
    /// percent) — that conversion lives in `read`/`write_value`.
    Number {
        min: i64,
        max: i64,
        step: i64,
        suffix: &'static str,
    },
}

/// One editable setting: a human label, the config key it persists to, and how
/// it's edited.
#[derive(Debug, Clone)]
pub struct Field {
    pub label: &'static str,
    pub key: &'static str,
    pub kind: FieldKind,
}

/// A named group of fields, shown as one tab/page in the overlay.
#[derive(Debug, Clone)]
pub struct Category {
    pub name: &'static str,
    pub fields: Vec<Field>,
}

/// Cycle 756: live navigation state while the settings overlay is open
/// (`App::settings_nav`). Indices into `categories()`; values are always read
/// fresh from `Config`, so the overlay reflects external config reloads too.
#[derive(Debug, Clone, Default)]
pub struct SettingsNav {
    pub category: usize,
    pub field: usize,
}

fn toggle(label: &'static str, key: &'static str) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Toggle,
    }
}

fn choice(
    label: &'static str,
    key: &'static str,
    values: &'static [&'static str],
    labels: &'static [&'static str],
) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Choice { values, labels },
    }
}

fn number(
    label: &'static str,
    key: &'static str,
    min: i64,
    max: i64,
    step: i64,
    suffix: &'static str,
) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Number {
            min,
            max,
            step,
            suffix,
        },
    }
}

/// The curated catalogue. Covers the settings a typical user actually reaches
/// for; the overlay also offers an "open config file" row for the long tail.
pub fn categories() -> Vec<Category> {
    vec![
        Category {
            name: "Appearance",
            fields: vec![
                number("Font size", "font-size", 6, 72, 1, "pt"),
                number("Background opacity", "background-opacity", 20, 100, 5, "%"),
                number("Window padding", "window-padding-x", 0, 40, 2, "px"),
                choice(
                    "Cursor shape",
                    "cursor-shape",
                    &["block", "beam", "underline"],
                    &["block", "beam (bar)", "underline"],
                ),
                toggle("Cursor blink", "cursor-blink"),
                toggle("Show pane titlebars", "show-titlebar"),
            ],
        },
        Category {
            name: "Behavior",
            fields: vec![
                choice(
                    "Scrollbar",
                    "scrollbar",
                    &["never", "auto", "always"],
                    &["hidden", "auto", "always"],
                ),
                choice(
                    "Bell",
                    "bell",
                    &["off", "visual", "attention", "both"],
                    &["off", "visual flash", "attention", "visual + attention"],
                ),
                number("Scrollback lines", "scrollback", 0, 100_000, 1_000, ""),
                toggle("Copy on select", "copy-on-select"),
                toggle("Hide mouse while typing", "mouse-hide-while-typing"),
                choice(
                    "Focus mode",
                    "focus",
                    &["click", "sloppy", "system"],
                    &["click to focus", "follows mouse", "system default"],
                ),
            ],
        },
    ]
}

/// Read a field's current value from `cfg`, formatted for display next to its
/// label (e.g. `"14pt"`, `"auto"`, `"on"`). Returns a best-effort string; an
/// unknown key (catalogue/Config drift) yields `"—"` rather than panicking.
pub fn read(cfg: &Config, field: &Field) -> String {
    match &field.kind {
        FieldKind::Toggle => {
            if read_bool(cfg, field.key) {
                "on".to_string()
            } else {
                "off".to_string()
            }
        }
        FieldKind::Choice { values, labels } => {
            let cur = read_choice(cfg, field.key);
            // Cycle 763: `labels.get(i)` (not `labels[i]`) so a catalogue entry
            // with mismatched values/labels lengths degrades to the raw value
            // instead of panicking on an out-of-bounds index.
            values
                .iter()
                .position(|v| *v == cur)
                .and_then(|i| labels.get(i))
                .map(|label| label.to_string())
                .unwrap_or_else(|| cur.clone())
        }
        FieldKind::Number { suffix, .. } => {
            format!("{}{}", read_number(cfg, field.key), suffix)
        }
    }
}

/// Compute the new on-disk value when the user nudges `field` by `dir`
/// (`+1` = right/increase, `-1` = left/decrease, `0` = toggle/activate).
/// Returns the string to hand to `persist_pref(field.key, _)`.
pub fn next_value(cfg: &Config, field: &Field, dir: i32) -> String {
    match &field.kind {
        FieldKind::Toggle => {
            // Any direction flips a toggle (Space, Left, Right all toggle).
            (!read_bool(cfg, field.key)).to_string()
        }
        FieldKind::Choice { values, .. } => {
            let cur = read_choice(cfg, field.key);
            let idx = values.iter().position(|v| *v == cur).unwrap_or(0) as i32;
            let n = values.len() as i32;
            // dir 0 (activate) advances forward, like a click.
            let step = if dir == 0 { 1 } else { dir };
            let next = (idx + step).rem_euclid(n) as usize;
            values[next].to_string()
        }
        FieldKind::Number {
            min,
            max,
            step,
            suffix,
        } => {
            let cur = read_number(cfg, field.key);
            let delta = if dir == 0 {
                *step
            } else {
                (dir as i64) * *step
            };
            let next = (cur + delta).clamp(*min, *max);
            write_number(field.key, next, suffix)
        }
    }
}

// ---- Config readers (string-keyed; the one place catalogue keys meet Config).

fn read_bool(cfg: &Config, key: &str) -> bool {
    match key {
        "cursor-blink" => cfg.cursor_blink,
        "show-titlebar" => cfg.show_titlebar,
        "copy-on-select" => cfg.copy_on_select,
        "mouse-hide-while-typing" => cfg.mouse_hide_while_typing,
        _ => false,
    }
}

fn read_choice(cfg: &Config, key: &str) -> String {
    match key {
        "scrollbar" => match cfg.scrollbar {
            ScrollbarMode::Never => "never",
            ScrollbarMode::Auto => "auto",
            ScrollbarMode::Always => "always",
        }
        .to_string(),
        "bell" => match cfg.bell {
            BellMode::Off => "off",
            BellMode::Visual => "visual",
            BellMode::Attention => "attention",
            BellMode::Both => "both",
        }
        .to_string(),
        "cursor-shape" => match cfg.cursor_style {
            CursorStyle::Block => "block",
            CursorStyle::Bar => "beam",
            CursorStyle::Underline => "underline",
        }
        .to_string(),
        "focus" => match cfg.focus {
            FocusMode::Click => "click",
            FocusMode::Sloppy => "sloppy",
            FocusMode::System => "system",
        }
        .to_string(),
        _ => String::new(),
    }
}

fn read_number(cfg: &Config, key: &str) -> i64 {
    match key {
        "font-size" => cfg.font_size.round() as i64,
        // opacity is stored 0.0–1.0; the overlay edits it as a percent.
        "background-opacity" => (cfg.background_opacity * 100.0).round() as i64,
        "window-padding-x" => cfg.padding_x.round() as i64,
        "scrollback" => cfg.scrollback as i64,
        _ => 0,
    }
}

/// Convert the stepped integer back into the on-disk string form for `key`.
/// Most keys are plain integers; opacity round-trips through a 0.0–1.0 float.
fn write_number(key: &str, value: i64, _suffix: &str) -> String {
    match key {
        "background-opacity" => format!("{:.2}", value as f64 / 100.0),
        _ => value.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::field_reassign_with_default,
    reason = "stepwise `let mut cfg = Config::default(); cfg.x = …` reads clearer \
              than a full struct literal for a one-field test tweak"
)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_keys_are_all_readable() {
        // Every catalogued field must resolve against a default Config (no
        // "—" / drift). This pins the catalogue ↔ Config mapping so renaming
        // a Config field without updating the catalogue fails the build's
        // tests rather than silently showing a blank value.
        let cfg = Config::default();
        for cat in categories() {
            for field in &cat.fields {
                let shown = read(&cfg, field);
                assert!(
                    !shown.is_empty() && shown != "—",
                    "field '{}' (key {}) read blank/unknown",
                    field.label,
                    field.key
                );
            }
        }
    }

    #[test]
    fn toggle_flips() {
        let cfg = Config::default();
        let f = toggle("Cursor blink", "cursor-blink");
        let before = read_bool(&cfg, "cursor-blink");
        let next = next_value(&cfg, &f, 0);
        assert_eq!(next, (!before).to_string());
    }

    #[test]
    fn choice_cycles_both_directions_and_wraps() {
        let cfg = Config::default(); // scrollbar default = auto
        let f = choice(
            "Scrollbar",
            "scrollbar",
            &["never", "auto", "always"],
            &["hidden", "auto", "always"],
        );
        // forward from auto -> always
        assert_eq!(next_value(&cfg, &f, 1), "always");
        // backward from auto -> never
        assert_eq!(next_value(&cfg, &f, -1), "never");
    }

    #[test]
    fn number_steps_and_clamps() {
        let mut cfg = Config::default();
        cfg.font_size = 14.0;
        let f = number("Font size", "font-size", 6, 72, 1, "pt");
        assert_eq!(next_value(&cfg, &f, 1), "15");
        assert_eq!(next_value(&cfg, &f, -1), "13");
        assert_eq!(read(&cfg, &f), "14pt");
        // clamp at ceiling
        cfg.font_size = 72.0;
        assert_eq!(next_value(&cfg, &f, 1), "72");
        // clamp at floor
        cfg.font_size = 6.0;
        assert_eq!(next_value(&cfg, &f, -1), "6");
    }

    #[test]
    fn opacity_round_trips_percent_to_float() {
        let mut cfg = Config::default();
        cfg.background_opacity = 0.85;
        let f = number("Background opacity", "background-opacity", 20, 100, 5, "%");
        assert_eq!(read(&cfg, &f), "85%");
        // 85% + 5% step -> 0.90 on disk
        assert_eq!(next_value(&cfg, &f, 1), "0.90");
    }
}
