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
    /// v2.23.0: like [`FieldKind::Choice`] but the options are computed at
    /// runtime (owned `Vec<String>`) rather than `&'static`. Used by the GPU
    /// picker, whose options are the GPUs actually detected on this machine.
    /// `values[i]` is the token persisted (and round-tripped through `read`);
    /// `labels[i]` is shown. A scoped relaxation — the rest of the catalogue
    /// stays `&'static` — so the dynamic-list need doesn't refactor everything.
    ChoiceOwned {
        values: Vec<String>,
        labels: Vec<String>,
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
    /// Cycle 766: a rebindable keybinding. `action` is the canonical
    /// `Action::from_name` token (e.g. `"split_right"`). The displayed value is
    /// the chord currently bound to that action (reverse-looked-up from
    /// `cfg.keybinds`); activating it enters capture mode and the next chord is
    /// bound to the action live + persisted. `key` is unused for these (they
    /// don't persist via the key=value path; the editor appends a `keybind`
    /// line), so it carries the action name too.
    Keybind { action: &'static str },
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
    /// Cycle 766: `true` while the focused Keybind field is waiting for the
    /// user to press a chord to bind. The next non-modifier key press is
    /// captured as the new binding; Esc cancels.
    pub capturing: bool,
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

/// v2.23.0: a runtime-options Choice (see [`FieldKind::ChoiceOwned`]).
fn choice_owned(
    label: &'static str,
    key: &'static str,
    values: Vec<String>,
    labels: Vec<String>,
) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::ChoiceOwned { values, labels },
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

/// Cycle 766: a rebindable-keybinding field. `action` is the canonical action
/// token; `label` is the human row label.
fn keybind(label: &'static str, action: &'static str) -> Field {
    Field {
        label,
        key: action,
        kind: FieldKind::Keybind { action },
    }
}

/// The curated catalogue. Covers the settings a typical user actually reaches
/// for; the overlay also offers an "open config file" row for the long tail.
///
/// `gpus` are the GPUs detected on this machine as `(token, label)` pairs (the
/// token is what [`read_choice`] round-trips for the `gpu` key — `"auto"` or
/// `"<vendor-hex>:<device-hex>:<name>"`). They populate the Graphics category's
/// device picker; pass `&[]` (e.g. in tests) for just the "Automatic" option.
pub fn categories(gpus: &[(String, String)]) -> Vec<Category> {
    // GPU device options: Automatic first, then each detected GPU.
    let mut gpu_values = vec!["auto".to_string()];
    let mut gpu_labels = vec!["Automatic".to_string()];
    for (val, label) in gpus {
        gpu_values.push(val.clone());
        gpu_labels.push(label.clone());
    }
    vec![
        Category {
            name: "Appearance",
            fields: vec![
                // Cycle 872: the most popular themes as a cyclable list of
                // options; ←/→ live-previews each (the settings handler persists
                // + reloads on every step, so the theme applies instantly). The
                // full 500+ bundle stays reachable via the right-click Theme
                // submenu / NextTheme / the `theme =` config line.
                choice(
                    "Theme",
                    "theme",
                    kettle_config::Theme::POPULAR,
                    kettle_config::Theme::POPULAR,
                ),
                number("Font size", "font-size", 6, 72, 1, "pt"),
                number("Background opacity", "background-opacity", 20, 100, 5, "%"),
                number("Window padding", "window-padding-x", 0, 40, 2, "px"),
                choice(
                    // Cycle 790 (audit E3): use the canonical `cursor-style`
                    // key (CONFIG.md's authoritative spelling) rather than the
                    // `cursor-shape` back-compat alias, so the overlay persists
                    // the canonical line and SETTINGS.md ↔ CONFIG.md ↔ catalogue
                    // agree. `beam` stays as the user-facing value (accepted as
                    // the Alacritty alias for `bar`).
                    "Cursor shape",
                    "cursor-style",
                    &["block", "beam", "underline"],
                    &["block", "beam (bar)", "underline"],
                ),
                toggle("Cursor blink", "cursor-blink"),
                toggle("Show pane titlebars", "show-titlebar"),
                // v2.23.0: background style + (for an animated image) how it
                // plays. The image *path* stays a config-file key (it needs a
                // file, not a cycle); these two cover the discoverable choices.
                choice(
                    "Background",
                    "background-type",
                    &["solid", "image", "transparent"],
                    &["solid color", "image", "transparent"],
                ),
                choice(
                    "Background animation",
                    "background-animation",
                    &["when-focused", "always", "off"],
                    &["when focused", "always", "off"],
                ),
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
                // Cycle 794: opt out of the in-app update checker.
                toggle("Check for updates", "update-check"),
                // v2.20.0: hjkl navigation in menus/overlays (default ON).
                toggle("Vim menu navigation", "vim-menu-nav"),
                choice(
                    "Focus mode",
                    "focus",
                    &["click", "sloppy", "system"],
                    &["click to focus", "follows mouse", "system default"],
                ),
            ],
        },
        Category {
            name: "Graphics",
            fields: vec![
                // Tier A: the power-preference policy (integrated vs discrete).
                // Applies on restart — the renderer/device graph can't hot-swap.
                choice(
                    "GPU preference",
                    "gpu-power-preference",
                    &["low", "high", "auto"],
                    &[
                        "integrated (power-saving)",
                        "discrete (performance)",
                        "automatic",
                    ],
                ),
                // Tier B: pin a specific detected GPU (or Automatic).
                choice_owned("GPU device", "gpu", gpu_values, gpu_labels),
                // Advanced: backend + software fallback.
                choice(
                    "GPU backend",
                    "gpu-backend",
                    &["auto", "dx12", "vulkan", "metal", "gl"],
                    &["automatic", "DirectX 12", "Vulkan", "Metal", "OpenGL"],
                ),
                toggle("Force software rendering", "gpu-force-software"),
            ],
        },
        Category {
            name: "Keybinds",
            fields: vec![
                keybind("Split right", "split_right"),
                keybind("Split down", "split_down"),
                keybind("Close pane", "close_pane"),
                keybind("New tab", "new_tab"),
                keybind("Next tab", "next_tab"),
                keybind("Previous tab", "previous_tab"),
                keybind("Search", "start_search"),
                keybind("Command palette", "command_palette"),
                keybind("Open settings", "open_settings"),
                keybind("Zoom pane", "toggle_zoom"),
                keybind("Copy", "copy"),
                keybind("Paste", "paste"),
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
        FieldKind::ChoiceOwned { values, labels } => {
            let cur = read_choice(cfg, field.key);
            values
                .iter()
                .position(|v| *v == cur)
                .and_then(|i| labels.get(i))
                .map(|label| label.to_string())
                // No match (e.g. a pinned GPU that no longer enumerates) →
                // show the saved name if any, else "Automatic".
                .unwrap_or_else(|| {
                    if cfg.gpu_name.trim().is_empty() {
                        "Automatic".to_string()
                    } else {
                        format!("{} (not detected)", cfg.gpu_name)
                    }
                })
        }
        FieldKind::Number { suffix, .. } => {
            format!("{}{}", read_number(cfg, field.key), suffix)
        }
        FieldKind::Keybind { action } => {
            // Reverse-look-up the chord currently bound to this action in the
            // effective keymap. Shows the first match (an action may have
            // several bindings); "unbound" if none.
            match kettle_config::Action::from_name(action) {
                Some(a) => cfg
                    .keybinds
                    .iter()
                    .find(|(_, v)| **v == a)
                    .map(|(t, _)| t.label())
                    .unwrap_or_else(|| "unbound".to_string()),
                None => "—".to_string(),
            }
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
        FieldKind::ChoiceOwned { values, .. } => {
            if values.is_empty() {
                return String::new();
            }
            let cur = read_choice(cfg, field.key);
            let idx = values.iter().position(|v| *v == cur).unwrap_or(0) as i32;
            let n = values.len() as i32;
            let step = if dir == 0 { 1 } else { dir };
            let next = (idx + step).rem_euclid(n) as usize;
            values[next].clone()
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
        // Keybinds don't change via ←/→: activating one enters capture mode in
        // the overlay handler, which binds the next chord directly. No-op here.
        FieldKind::Keybind { .. } => String::new(),
    }
}

/// Cycle 766: is this field a rebindable keybinding? The overlay handler uses
/// this to route Enter/Space into chord-capture instead of the value-cycle path.
pub fn is_keybind(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Keybind { .. })
}

/// Cycle 766: the canonical action token for a keybind field (for capture +
/// persistence). `None` for non-keybind fields.
pub fn keybind_action(field: &Field) -> Option<&'static str> {
    match field.kind {
        FieldKind::Keybind { action } => Some(action),
        _ => None,
    }
}

// ---- Config readers (string-keyed; the one place catalogue keys meet Config).

fn read_bool(cfg: &Config, key: &str) -> bool {
    match key {
        "cursor-blink" => cfg.cursor_blink,
        "show-titlebar" => cfg.show_titlebar,
        "copy-on-select" => cfg.copy_on_select,
        "update-check" => cfg.update_check,
        "mouse-hide-while-typing" => cfg.mouse_hide_while_typing,
        "vim-menu-nav" => cfg.vim_menu_nav,
        "gpu-force-software" => cfg.gpu_force_software,
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
        // Cycle 790 (audit E3): keyed on the canonical `cursor-style` to match
        // the catalogue field + CONFIG.md (was the `cursor-shape` alias).
        "cursor-style" => match cfg.cursor_style {
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
        // v2.23.0 background controls.
        "background-type" => match cfg.background_type {
            kettle_config::BackgroundType::Solid => "solid",
            kettle_config::BackgroundType::Image => "image",
            kettle_config::BackgroundType::Transparent => "transparent",
        }
        .to_string(),
        "background-animation" => match cfg.background_animation {
            kettle_config::BackgroundAnimation::WhenFocused => "when-focused",
            kettle_config::BackgroundAnimation::Always => "always",
            kettle_config::BackgroundAnimation::Off => "off",
        }
        .to_string(),
        // Cycle 872: the live theme name (canonical bundled casing). When the
        // current theme isn't in the curated POPULAR list, `read`'s Choice arm
        // falls back to showing this raw name, and ←/→ cycles into the list.
        "theme" => cfg.theme_name.clone(),
        // v2.23.0 Graphics.
        "gpu-power-preference" => match cfg.gpu_power_preference {
            kettle_config::GpuPowerPreference::Low => "low",
            kettle_config::GpuPowerPreference::High => "high",
            kettle_config::GpuPowerPreference::Auto => "auto",
        }
        .to_string(),
        "gpu-backend" => match cfg.gpu_backend {
            kettle_config::GpuBackend::Auto => "auto",
            kettle_config::GpuBackend::Dx12 => "dx12",
            kettle_config::GpuBackend::Vulkan => "vulkan",
            kettle_config::GpuBackend::Metal => "metal",
            kettle_config::GpuBackend::Gl => "gl",
        }
        .to_string(),
        // The pinned-GPU token, round-tripped against the picker's option
        // values: "<vendor-hex>:<device-hex>:<name>" when pinned, else "auto".
        // Must match the token app.rs builds from a detected GpuAdapterInfo.
        "gpu" => {
            if cfg.gpu_vendor_id != 0 && cfg.gpu_device_id != 0 {
                format!(
                    "{:x}:{:x}:{}",
                    cfg.gpu_vendor_id, cfg.gpu_device_id, cfg.gpu_name
                )
            } else {
                "auto".to_string()
            }
        }
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
        // Pass a representative detected-GPU list so the dynamic GPU field is
        // exercised too (not just the empty "Automatic"-only fallback).
        let gpus = [(
            "10de:2191:NVIDIA GeForce GTX 1660 Ti".to_string(),
            "NVIDIA GeForce GTX 1660 Ti (Discrete)".to_string(),
        )];
        for cat in categories(&gpus) {
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
    fn gpu_device_choice_round_trips_and_cycles() {
        // v2.23.0. The GPU picker's token round-trips through read_choice and
        // the ChoiceOwned cycle, and a pinned GPU shows its label.
        let gpus = [
            (
                "10de:2191:NVIDIA GeForce GTX 1660 Ti".to_string(),
                "NVIDIA GeForce GTX 1660 Ti (Discrete)".to_string(),
            ),
            (
                "8086:9a49:Intel(R) Iris(R) Plus Graphics".to_string(),
                "Intel(R) Iris(R) Plus Graphics (Integrated)".to_string(),
            ),
        ];
        let cats = categories(&gpus);
        let graphics = cats
            .iter()
            .find(|c| c.name == "Graphics")
            .expect("Graphics");
        let gpu_field = graphics
            .fields
            .iter()
            .find(|f| f.key == "gpu")
            .expect("gpu field");

        // Default cfg → "auto" token → shows "Automatic".
        let mut cfg = Config::default();
        assert_eq!(read(&cfg, gpu_field), "Automatic");
        // Cycling forward from auto lands on the first detected GPU's token.
        let next = next_value(&cfg, gpu_field, 1);
        assert_eq!(next, "10de:2191:NVIDIA GeForce GTX 1660 Ti");

        // A pinned NVIDIA cfg reads back the matching label.
        cfg.gpu_vendor_id = 0x10de;
        cfg.gpu_device_id = 0x2191;
        cfg.gpu_name = "NVIDIA GeForce GTX 1660 Ti".to_string();
        assert_eq!(
            read(&cfg, gpu_field),
            "NVIDIA GeForce GTX 1660 Ti (Discrete)"
        );
        // Cycling forward from NVIDIA → Intel's token.
        assert_eq!(
            next_value(&cfg, gpu_field, 1),
            "8086:9a49:Intel(R) Iris(R) Plus Graphics"
        );

        // A pinned GPU that's no longer detected degrades gracefully.
        cfg.gpu_name = "Phantom GPU 9000".to_string();
        cfg.gpu_vendor_id = 0xdead;
        cfg.gpu_device_id = 0xbeef;
        assert_eq!(read(&cfg, gpu_field), "Phantom GPU 9000 (not detected)");
    }

    /// Cycle 789 drift guard (audit D3). `keybind_action` extracts the
    /// canonical action token the settings overlay routes into chord-capture
    /// (Enter on a keybind row). A refactor that renamed `FieldKind::Keybind`'s
    /// `action` field — or returned the token for the wrong variant — would
    /// silently break keybind editing; only the interactive overlay exercised
    /// it before. Pin Some(action) for keybind fields and None for every other
    /// field kind.
    #[test]
    fn keybind_action_extracts_token_for_keybind_fields_only() {
        assert_eq!(
            keybind_action(&keybind("Split right", "split_right")),
            Some("split_right")
        );
        // Empty-token boundary: still Some, just empty (the catalogue never
        // ships one, but the accessor must not special-case it to None).
        assert_eq!(keybind_action(&keybind("Weird", "")), Some(""));
        // Non-keybind kinds yield None.
        assert_eq!(
            keybind_action(&toggle("Cursor blink", "cursor-blink")),
            None
        );
        assert_eq!(
            keybind_action(&choice("Scrollbar", "scrollbar", &["auto"], &["auto"])),
            None
        );
        assert_eq!(
            keybind_action(&number("Font size", "font-size", 6, 72, 1, "pt")),
            None
        );
        // is_keybind agrees with keybind_action on the discriminant.
        assert!(is_keybind(&keybind("X", "copy")));
        assert!(!is_keybind(&toggle("Y", "cursor-blink")));
    }

    #[test]
    fn toggle_flips() {
        let cfg = Config::default();
        let f = toggle("Cursor blink", "cursor-blink");
        let before = read_bool(&cfg, "cursor-blink");
        let next = next_value(&cfg, &f, 0);
        assert_eq!(next, (!before).to_string());
    }

    /// v2.20.0: the `catalogue_keys_are_all_readable` guard can't catch a
    /// missing `read_bool` arm for a default-ON toggle (the `_ => false`
    /// fallback shows a plausible "off"). Pin the row to its real default so
    /// the arm can't silently go missing.
    #[test]
    fn vim_menu_nav_row_reads_its_real_default() {
        let cfg = Config::default();
        assert!(
            read_bool(&cfg, "vim-menu-nav"),
            "vim-menu-nav defaults ON; the settings row must show it"
        );
        let f = toggle("Vim menu navigation", "vim-menu-nav");
        assert_eq!(read(&cfg, &f), "on");
        assert_eq!(next_value(&cfg, &f, 0), "false");
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
