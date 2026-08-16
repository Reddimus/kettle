//! Data model for the keyboard-accessible Settings overlay. It groups common
//! options and writes changes through `App::persist_pref`, so they reload
//! immediately.
//!
//! This module is the **pure** half: the category/field catalogue plus the
//! logic to read a field's current value from a [`Config`] and to compute the
//! next value when the user toggles / cycles / steps it. `app.rs` owns the
//! overlay state, input routing, and persistence; `kettle-render` owns drawing.
//! Keeping the catalogue here (free functions over `&Config`) makes it unit
//! testable without a window or renderer.

use kettle_config::{BellMode, Config, CursorStyle, FocusMode, ScrollbarMode, UpdatePolicy};

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
    /// A rebindable keybinding. `action` is the canonical
    /// `Action::from_name` token (e.g. `"split_right"`). The displayed value is
    /// the chord currently bound to that action (reverse-looked-up from
    /// `cfg.keybinds`); activating it enters capture mode and the next chord is
    /// bound to the action live + persisted. `key` is unused for these (they
    /// don't persist via the key=value path; the editor appends a `keybind`
    /// line), so it carries the action name too.
    Keybind { action: &'static str },
    /// v2.24.0: a free-text string value (e.g. the `background-image` path).
    /// ←/→ don't cycle it; activating (Enter / Space / click) opens an inline
    /// text prompt pre-filled with the current value, and the typed string is
    /// persisted on submit. The displayed value is the current string (or a
    /// placeholder when empty).
    Text { placeholder: &'static str },
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

/// Live navigation state while the settings overlay is open
/// (`App::settings_nav`). Indices into `categories()`; values are always read
/// fresh from `Config`, so the overlay reflects external config reloads too.
#[derive(Debug, Clone, Default)]
pub struct SettingsNav {
    pub category: usize,
    pub field: usize,
    /// `true` while the focused Keybind field is waiting for the
    /// user to press a chord to bind. The next non-modifier key press is
    /// captured as the new binding; Esc cancels.
    pub capturing: bool,
}

/// v2.24.0: state for the inline text prompt opened by a [`FieldKind::Text`]
/// row (the in-settings image-path entry). `key` is the config key to persist
/// on submit; `buf` is the editable string (append / backspace only — cursor is
/// always at the end, plenty for a path). Enter persists + reloads, Esc cancels.
#[derive(Debug, Clone)]
pub struct SettingsTextEdit {
    pub key: &'static str,
    pub buf: String,
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

/// A rebindable-keybinding field. `action` is the canonical action
/// token; `label` is the human row label.
fn keybind(label: &'static str, action: &'static str) -> Field {
    Field {
        label,
        key: action,
        kind: FieldKind::Keybind { action },
    }
}

/// v2.24.0: a free-text field (the in-settings image-path entry).
fn text(label: &'static str, key: &'static str, placeholder: &'static str) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Text { placeholder },
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
                // The most popular themes as a cyclable list of
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
                toggle("Window blur", "window-blur"),
                number("Window padding", "window-padding-x", 0, 40, 2, "px"),
                choice(
                    // Use the canonical `cursor-style`
                    // key (CONFIG.md's authoritative spelling) rather than the
                    // `cursor-shape` back-compat alias, so the overlay persists
                    // the canonical line and SETTINGS.md ↔ CONFIG.md ↔ catalogue
                    // agree. `beam` stays as the user-facing value (accepted as
                    // the legacy alias for `bar`).
                    "Cursor shape",
                    "cursor-style",
                    &["block", "beam", "underline"],
                    &["block", "beam (bar)", "underline"],
                ),
                toggle("Cursor blink", "cursor-blink"),
                toggle("Show pane titlebars", "show-titlebar"),
            ],
        },
        // v2.24.0: a dedicated Background page. `starfield` is a zero-config
        // animated background; `image` takes a file path (edited inline here).
        // Sub-options below the type are gated (dimmed + skipped) when they
        // don't apply to the selected type — see `field_disabled`.
        Category {
            name: "Background",
            fields: vec![
                choice(
                    "Background",
                    "background-type",
                    &["solid", "image", "starfield", "transparent"],
                    &[
                        "solid color",
                        "image",
                        "starfield (animated)",
                        "transparent",
                    ],
                ),
                text(
                    "Image file",
                    "background-image",
                    "(set a path — e.g. ~/wall.png)",
                ),
                choice(
                    // Always-first to match the v2.24.0 default.
                    "Animation",
                    "background-animation",
                    &["always", "when-focused", "off"],
                    &["always", "when focused", "off"],
                ),
                choice(
                    "Chrome bar color",
                    "chrome-background",
                    &["theme", "auto", "black", "white"],
                    &["theme", "auto (from wallpaper)", "black", "white"],
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
                    "Completion overlay",
                    "completion-overlay",
                    &["auto", "off"],
                    &["automatic", "off"],
                ),
                // v2.28.0: width of the pronounced overlay scrollbar.
                number("Scrollbar width", "scrollbar-width", 2, 40, 2, "px"),
                choice(
                    "Bell",
                    "bell",
                    &["off", "visual", "attention", "both"],
                    &["off", "visual flash", "attention", "visual + attention"],
                ),
                number("Scrollback lines", "scrollback", 0, 100_000, 1_000, ""),
                number("Scrollback MB", "scrollback-bytes", 0, 1024, 10, "MB"),
                toggle("Copy on select", "copy-on-select"),
                toggle("Hide mouse while typing", "mouse-hide-while-typing"),
                choice(
                    "Updates",
                    "update-policy",
                    &["off", "notify", "auto"],
                    &["off", "notify", "install automatically"],
                ),
                number(
                    "Update check (hours)",
                    "update-check-interval-hours",
                    1,
                    720,
                    1,
                    "h",
                ),
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
            name: "Search",
            fields: vec![
                toggle("Wrap at boundaries", "search-wrap"),
                choice(
                    "Case mode",
                    "search-case-sensitive",
                    &["smart", "always", "never"],
                    &["Smart", "Match", "Ignore"],
                ),
                toggle("Invert default direction", "invert-search"),
            ],
        },
        // v2.28.0 (audit): a dedicated Tabs page surfacing the tab-bar keys that
        // were previously config-file-only. `tab-bar-position` offers only
        // top/bottom — left/right (vertical bars) parse but don't render yet, so
        // we don't let a non-technical user pick a silently-inert option.
        Category {
            name: "Tabs",
            fields: vec![
                choice(
                    "Tab bar",
                    "tab-bar",
                    &["off", "auto", "always"],
                    &["off", "auto (>1 tab)", "always"],
                ),
                choice(
                    "Tab bar position",
                    "tab-bar-position",
                    &["top", "bottom"],
                    &["top", "bottom"],
                ),
                number("Min tab width", "tab-min-width", 40, 600, 10, "px"),
                toggle("Scrollable tab bar", "scroll-tabbar"),
                toggle("Close button on tabs", "close-button-on-tab"),
                toggle("Detachable tabs", "detachable-tabs"),
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
                    &["auto", "low", "high"],
                    &["automatic", "low power / integrated", "high performance"],
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
            // `labels.get(i)` (not `labels[i]`) so a catalogue entry
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
            let value = read_number(cfg, field.key);
            // Both scrollback rows have a zero sentinel. Rendering it as a
            // quantity invited users to impose a finite cap without realising
            // what they were leaving.
            if field.key == "scrollback" && value == 0 {
                "infinite".to_string()
            } else if field.key == "scrollback-bytes" && cfg.scrollback_bytes == 0 {
                "no cap".to_string()
            } else if field.key == "scrollback-bytes" && cfg.scrollback_bytes < 1_000_000 {
                "<1MB".to_string()
            } else {
                format!("{value}{suffix}")
            }
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
        FieldKind::Text { placeholder } => {
            let v = read_string(cfg, field.key);
            if v.trim().is_empty() {
                placeholder.to_string()
            } else {
                v
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
                (dir as i64).saturating_mul(*step)
            };
            let stepped = cur.saturating_add(delta);
            // The Settings catalogue intentionally exposes a convenient range,
            // while the config grammar accepts wider values. Preserve those
            // values and step in the requested direction; clamping an already
            // out-of-range value snapped it to the catalogue boundary and could
            // destructively shrink live scrollback on the first keypress.
            let next = if (*min..=*max).contains(&cur) {
                stepped.clamp(*min, *max)
            } else {
                stepped
            };
            write_number(field.key, next, suffix)
        }
        // Keybinds don't change via ←/→: activating one enters capture mode in
        // the overlay handler, which binds the next chord directly. No-op here.
        FieldKind::Keybind { .. } => String::new(),
        // Text fields don't cycle: activating opens an inline prompt in the
        // overlay handler. No-op here.
        FieldKind::Text { .. } => String::new(),
    }
}

/// Is this field a rebindable keybinding? The overlay handler uses
/// this to route Enter/Space into chord-capture instead of the value-cycle path.
pub fn is_keybind(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Keybind { .. })
}

/// The canonical action token for a keybind field (for capture +
/// persistence). `None` for non-keybind fields.
pub fn keybind_action(field: &Field) -> Option<&'static str> {
    match field.kind {
        FieldKind::Keybind { action } => Some(action),
        _ => None,
    }
}

/// v2.24.0: is this a free-text field (the inline-prompt path entry)? The
/// overlay handler routes Enter / Space / click into a text prompt for these.
pub fn is_text(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Text { .. })
}

/// v2.24.0: is `key`'s row inapplicable to the current `background-type`, so the
/// overlay should DIM it and skip it during nav/click? Keeps the Background page
/// honest — e.g. the image path only matters for `image`, and animation / chrome
/// color only matter when there's a wallpaper (image or starfield).
pub fn field_disabled(cfg: &Config, key: &str) -> bool {
    use kettle_config::BackgroundType as BT;
    let t = cfg.background_type;
    match key {
        "background-image" => !matches!(t, BT::Image),
        "background-animation" | "chrome-background" => !matches!(t, BT::Image | BT::Starfield),
        _ => false,
    }
}

/// v2.24.0: the next field index from `start` stepping by `step` (`+1`/`-1`) that
/// is NOT [`field_disabled`], wrapping around. Lets keyboard nav skip dimmed
/// (inapplicable) rows. Returns `start` if every field is disabled (degenerate).
pub fn next_enabled_field(cfg: &Config, fields: &[Field], start: usize, step: i32) -> usize {
    let n = fields.len();
    if n == 0 {
        return 0;
    }
    let mut idx = start as i32;
    for _ in 0..n {
        idx = (idx + step).rem_euclid(n as i32);
        if !field_disabled(cfg, fields[idx as usize].key) {
            return idx as usize;
        }
    }
    start.min(n - 1)
}

// ---- Config readers (string-keyed; the one place catalogue keys meet Config).

fn read_bool(cfg: &Config, key: &str) -> bool {
    match key {
        "cursor-blink" => cfg.cursor_blink,
        "show-titlebar" => cfg.show_titlebar,
        "window-blur" => cfg.window_blur,
        "copy-on-select" => cfg.copy_on_select,
        "mouse-hide-while-typing" => cfg.mouse_hide_while_typing,
        "vim-menu-nav" => cfg.vim_menu_nav,
        "gpu-force-software" => cfg.gpu_force_software,
        "close-button-on-tab" => cfg.close_button_on_tab,
        "scroll-tabbar" => cfg.scroll_tabbar,
        "detachable-tabs" => cfg.detachable_tabs,
        "search-wrap" => cfg.search_wrap,
        "invert-search" => cfg.invert_search,
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
        "update-policy" => match cfg.update_policy {
            UpdatePolicy::Off => "off",
            UpdatePolicy::Notify => "notify",
            UpdatePolicy::Auto => "auto",
        }
        .to_string(),
        "bell" => match cfg.bell {
            BellMode::Off => "off",
            BellMode::Visual => "visual",
            BellMode::Attention => "attention",
            BellMode::Both => "both",
        }
        .to_string(),
        // Keyed on the canonical `cursor-style` to match
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
        "search-case-sensitive" => match cfg.search_case_sensitive {
            kettle_config::SearchCaseSensitivity::Smart => "smart",
            kettle_config::SearchCaseSensitivity::Always => "always",
            kettle_config::SearchCaseSensitivity::Never => "never",
        }
        .to_string(),
        "completion-overlay" => match cfg.completion_overlay {
            kettle_config::CompletionOverlayMode::Auto => "auto",
            kettle_config::CompletionOverlayMode::Off => "off",
        }
        .to_string(),
        // v2.23.0 background controls.
        "background-type" => match cfg.background_type {
            kettle_config::BackgroundType::Solid => "solid",
            kettle_config::BackgroundType::Image => "image",
            kettle_config::BackgroundType::Starfield => "starfield",
            kettle_config::BackgroundType::Transparent => "transparent",
        }
        .to_string(),
        "background-animation" => match cfg.background_animation {
            kettle_config::BackgroundAnimation::WhenFocused => "when-focused",
            kettle_config::BackgroundAnimation::Always => "always",
            kettle_config::BackgroundAnimation::Off => "off",
        }
        .to_string(),
        "chrome-background" => match cfg.chrome_background {
            kettle_config::ChromeBackground::Theme => "theme",
            kettle_config::ChromeBackground::Auto => "auto",
            kettle_config::ChromeBackground::Black => "black",
            kettle_config::ChromeBackground::White => "white",
        }
        .to_string(),
        // The live theme name (canonical bundled casing). When the
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
            if (cfg.gpu_vendor_id != 0 && cfg.gpu_device_id != 0) || !cfg.gpu_name.trim().is_empty()
            {
                format!(
                    "{:x}:{:x}:{}",
                    cfg.gpu_vendor_id, cfg.gpu_device_id, cfg.gpu_name
                )
            } else {
                "auto".to_string()
            }
        }
        // v2.28.0: Tabs category.
        "tab-bar" => match cfg.tab_bar {
            kettle_config::TabBarMode::Off => "off",
            kettle_config::TabBarMode::Auto => "auto",
            kettle_config::TabBarMode::Always => "always",
        }
        .to_string(),
        "tab-bar-position" => match cfg.tab_bar_pos {
            kettle_config::TabBarPos::Top => "top",
            kettle_config::TabBarPos::Bottom => "bottom",
            kettle_config::TabBarPos::Left => "left",
            kettle_config::TabBarPos::Right => "right",
        }
        .to_string(),
        _ => String::new(),
    }
}

fn read_string(cfg: &Config, key: &str) -> String {
    match key {
        "background-image" => cfg.background_image.clone(),
        _ => String::new(),
    }
}

fn read_number(cfg: &Config, key: &str) -> i64 {
    match key {
        "font-size" => cfg.font_size.round() as i64,
        // opacity is stored 0.0–1.0; the overlay edits it as a percent.
        "background-opacity" => (cfg.background_opacity * 100.0).round() as i64,
        "window-padding-x" => cfg.padding_x.round() as i64,
        // Report the SENTINEL, not the resolved line count. `scrollback` is
        // stored resolved, so infinite reads back as `INFINITE_SCROLLBACK`
        // (10 M) — far above this row's 100 000 ceiling. The step then computed
        // `(10_000_000 ± 1_000).clamp(0, 100_000)` = 100 000 and PERSISTED it,
        // so a single arrow press on a user with infinite scrollback silently
        // discarded their history limit. `0` is the config grammar's own
        // spelling of infinite, so round-tripping through it is lossless.
        "scrollback" => {
            if cfg.scrollback >= kettle_config::INFINITE_SCROLLBACK {
                0
            } else {
                cfg.scrollback as i64
            }
        }
        "scrollback-bytes" => (cfg.scrollback_bytes / 1_000_000) as i64,
        "tab-min-width" => cfg.tab_min_width.round() as i64,
        "scrollbar-width" => cfg.scrollbar_width.round() as i64,
        "update-check-interval-hours" => cfg.update_check_interval_hours as i64,
        _ => 0,
    }
}

/// Convert the stepped integer back into the on-disk string form for `key`.
/// Most keys are plain integers; opacity round-trips through a 0.0–1.0 float.
fn write_number(key: &str, value: i64, _suffix: &str) -> String {
    match key {
        "background-opacity" => format!("{:.2}", value as f64 / 100.0),
        "scrollback-bytes" => format!("{}MB", value.max(0)),
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

    /// The Scrollback row edits a value stored RESOLVED, so infinite read back
    /// as `INFINITE_SCROLLBACK` (10 M) against a row whose ceiling is 100 000.
    /// A single ←/→ press computed `(10_000_000 ± 1_000).clamp(0, 100_000)` and
    /// persisted 100 000 — silently discarding an unlimited history with no
    /// prompt and no way to tell it had happened.
    #[test]
    fn stepping_the_scrollback_row_cannot_silently_discard_infinite_history() {
        let mut cfg = Config::default();
        cfg.scrollback = kettle_config::INFINITE_SCROLLBACK;
        let field = number("Scrollback lines", "scrollback", 0, 100_000, 1_000, "");

        assert_eq!(
            read_number(&cfg, "scrollback"),
            0,
            "infinite must read back as the grammar's own sentinel, not the \
             resolved line count"
        );
        assert_eq!(
            read(&cfg, &field),
            "infinite",
            "a bare 0 would invite stepping off infinite without realising it"
        );
        // Stepping DOWN from infinite must stay infinite rather than land on
        // the ceiling.
        assert_eq!(next_value(&cfg, &field, -1), "0");
        // Stepping UP is a deliberate move to a finite limit, which is fine —
        // it just must not be the 100 000 the clamp used to force.
        assert_eq!(next_value(&cfg, &field, 1), "1000");

        // A finite value is unaffected.
        cfg.scrollback = 5_000;
        assert_eq!(read_number(&cfg, "scrollback"), 5_000);
        assert_eq!(read(&cfg, &field), "5000");
        assert_eq!(next_value(&cfg, &field, 1), "6000");

        let bytes = number("Scrollback MB", "scrollback-bytes", 0, 1024, 10, "MB");
        cfg.scrollback_bytes = 0;
        assert_eq!(read(&cfg, &bytes), "no cap");
        assert_eq!(next_value(&cfg, &bytes, -1), "0MB");
        assert_eq!(next_value(&cfg, &bytes, 1), "10MB");

        cfg.scrollback_bytes = 500_000;
        assert_eq!(read(&cfg, &bytes), "<1MB");
    }

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
            (
                "0:0:llvmpipe (LLVM 19.1.7)".to_string(),
                "llvmpipe (LLVM 19.1.7) (Software)".to_string(),
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

        // Software adapters commonly expose zero PCI ids. Their name remains
        // the pin identity, so Settings must not relabel an active software pin
        // as Automatic after persistence/reload.
        cfg.gpu_vendor_id = 0;
        cfg.gpu_device_id = 0;
        cfg.gpu_name = "llvmpipe (LLVM 19.1.7)".to_string();
        assert_eq!(read(&cfg, gpu_field), "llvmpipe (LLVM 19.1.7) (Software)");
        assert_eq!(next_value(&cfg, gpu_field, 1), "auto");
    }

    #[test]
    fn gpu_power_preference_defaults_to_automatic_in_settings() {
        let cats = categories(&[]);
        let graphics = cats
            .iter()
            .find(|c| c.name == "Graphics")
            .expect("Graphics");
        let pref = graphics
            .fields
            .iter()
            .find(|f| f.key == "gpu-power-preference")
            .expect("gpu-power-preference field");

        assert_eq!(read(&Config::default(), pref), "automatic");
        match &pref.kind {
            FieldKind::Choice { values, labels, .. } => {
                assert_eq!(values.first().copied(), Some("auto"));
                assert_eq!(labels.first().copied(), Some("automatic"));
            }
            other => panic!("expected GPU preference choice field, got {other:?}"),
        }
    }

    #[test]
    fn background_category_has_starfield_path_and_gating() {
        use kettle_config::BackgroundType as BT;
        let cats = categories(&[]);
        let bg = cats
            .iter()
            .find(|c| c.name == "Background")
            .expect("Background category exists");
        // The type choice offers the new zero-config starfield.
        let typef = bg
            .fields
            .iter()
            .find(|f| f.key == "background-type")
            .unwrap();
        match &typef.kind {
            FieldKind::Choice { values, .. } => {
                assert!(values.contains(&"starfield"), "type must offer starfield")
            }
            _ => panic!("background-type must be a Choice"),
        }
        // The image path is an inline-editable Text field.
        let img = bg
            .fields
            .iter()
            .find(|f| f.key == "background-image")
            .unwrap();
        assert!(is_text(img), "image path must be a Text field");

        // Gating by background-type.
        let mut cfg = Config::default();
        cfg.background_type = BT::Solid;
        assert!(field_disabled(&cfg, "background-image"));
        assert!(field_disabled(&cfg, "background-animation"));
        assert!(field_disabled(&cfg, "chrome-background"));
        assert!(!field_disabled(&cfg, "background-type")); // the type row is never gated
        cfg.background_type = BT::Image;
        assert!(!field_disabled(&cfg, "background-image"));
        assert!(!field_disabled(&cfg, "background-animation"));
        assert!(!field_disabled(&cfg, "chrome-background"));
        cfg.background_type = BT::Starfield;
        assert!(field_disabled(&cfg, "background-image")); // no file for a procedural bg
        assert!(!field_disabled(&cfg, "background-animation"));
        assert!(!field_disabled(&cfg, "chrome-background"));
    }

    #[test]
    fn next_enabled_field_skips_gated_rows() {
        use kettle_config::BackgroundType as BT;
        let cats = categories(&[]);
        let bg = cats.iter().find(|c| c.name == "Background").unwrap();
        let n = bg.fields.len();
        let mut cfg = Config::default();
        // Starfield: fields = [type(0), image(1 DISABLED), animation(2), chrome(3)].
        cfg.background_type = BT::Starfield;
        assert_eq!(
            next_enabled_field(&cfg, &bg.fields, 0, 1),
            2,
            "forward from type skips the disabled image path → animation"
        );
        assert_eq!(
            next_enabled_field(&cfg, &bg.fields, 0, -1),
            n - 1,
            "backward from type wraps to chrome"
        );
        // Solid: only the type row is enabled → nav stays put.
        cfg.background_type = BT::Solid;
        assert_eq!(next_enabled_field(&cfg, &bg.fields, 0, 1), 0);
    }

    #[test]
    fn chrome_background_round_trips_through_read_choice() {
        let mut cfg = Config::default();
        cfg.background_type = kettle_config::BackgroundType::Image;
        cfg.chrome_background = kettle_config::ChromeBackground::Auto;
        let f = choice(
            "Chrome bar color",
            "chrome-background",
            &["theme", "auto", "black", "white"],
            &["theme", "auto", "black", "white"],
        );
        assert_eq!(read(&cfg, &f), "auto");
    }

    /// Drift guard: `keybind_action` extracts the
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

        // Config-valid values outside the catalogue's convenient range step
        // normally; neither arrow may snap them to the nearest boundary.
        cfg.background_opacity = 0.10;
        let opacity = number("Background opacity", "background-opacity", 20, 100, 5, "%");
        assert_eq!(next_value(&cfg, &opacity, -1), "0.05");
        assert_eq!(next_value(&cfg, &opacity, 1), "0.15");

        cfg.scrollback = 500_000;
        let scrollback = number("Scrollback lines", "scrollback", 0, 100_000, 1_000, "");
        assert_eq!(next_value(&cfg, &scrollback, -1), "499000");
        assert_eq!(next_value(&cfg, &scrollback, 1), "501000");

        cfg.scrollback_bytes = 4_000_000_000;
        let bytes = number("Scrollback MB", "scrollback-bytes", 0, 1024, 10, "MB");
        assert_eq!(next_value(&cfg, &bytes, -1), "3990MB");
        assert_eq!(next_value(&cfg, &bytes, 1), "4010MB");

        cfg.update_check_interval_hours = 8_760;
        let updates = number(
            "Update check (hours)",
            "update-check-interval-hours",
            1,
            720,
            1,
            "h",
        );
        assert_eq!(next_value(&cfg, &updates, -1), "8759");
        assert_eq!(next_value(&cfg, &updates, 1), "8761");
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
