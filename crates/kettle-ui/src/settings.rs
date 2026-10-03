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
//!
//! Every word a user reads is a [`Text`] catalogue key, shown through the
//! caller's [`Translator`]; only proper names ([`Label::Name`]) and config
//! values stay verbatim.

use std::borrow::Cow;

use kettle_config::{BellMode, Config, CursorStyle, FocusMode, ScrollbarMode, UpdatePolicy};
use kettle_i18n::{Text, Translator};

/// What a choice shows: catalogue text, or a proper name (a theme, a GPU, a
/// graphics API) that reads the same in every language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Text(Text),
    Name(Cow<'static, str>),
}

impl Label {
    pub fn show<'a>(&'a self, tr: &Translator) -> &'a str {
        match self {
            Label::Text(text) => tr.text(*text),
            Label::Name(name) => name,
        }
    }
}

/// A verbatim proper name, for the static choice tables.
const fn name(name: &'static str) -> Label {
    Label::Name(Cow::Borrowed(name))
}

/// Language names are shown in their own language, so a reader can find
/// theirs whatever the UI speaks.
const LANGUAGE_LABELS: &[Label] = &[
    Label::Text(Text::SettingsValueAutomatic),
    name("English"),
    name("Español"),
];

/// Graphics API names are proper names.
const GPU_BACKEND_LABELS: &[Label] = &[
    Label::Text(Text::SettingsValueAutomatic),
    name("DirectX 12"),
    name("Vulkan"),
    name("Metal"),
    name("OpenGL"),
];

/// How a setting is edited. The overlay maps ←/→ (or Space/Enter) onto these.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// A boolean. Left/Right/Space flip it. Persisted as `true` / `false`.
    Toggle,
    /// One of a fixed set of choices. Left/Right cycle. `values[i]` is what's
    /// written to config; `labels[i]` is shown to the user. `None` labels mean
    /// the values are proper names (the theme list) and show verbatim.
    Choice {
        values: &'static [&'static str],
        labels: Option<&'static [Label]>,
    },
    /// Like [`FieldKind::Choice`] but the options are computed at runtime
    /// (owned `Vec<String>`) rather than `&'static`. Used by the GPU picker,
    /// whose options are the GPUs actually detected on this machine.
    /// `values[i]` is the token persisted (and round-tripped through `read`);
    /// `labels[i]` is shown. The rest of the catalogue stays `&'static`.
    ChoiceOwned {
        values: Vec<String>,
        labels: Vec<Label>,
    },
    /// An integer in `[min, max]` stepped by `step`. Optional `suffix` for
    /// display (e.g. "%", "px"): a unit symbol, the same in every language.
    /// Some keys store a different on-disk form than
    /// the displayed integer (e.g. opacity is a 0.0–1.0 float shown as a
    /// percent); `read_number` and `write_number` convert it.
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
    /// A free-text string value (e.g. the `background-image` path).
    /// ←/→ don't cycle it; activating (Enter / Space / click) opens an inline
    /// text prompt pre-filled with the current value, and the typed string is
    /// persisted on submit. The displayed value is the current string (or a
    /// placeholder when empty).
    Text { placeholder: Text },
}

/// One editable setting: a human label, the config key it persists to, and how
/// it's edited.
#[derive(Debug, Clone)]
pub struct Field {
    pub label: Text,
    pub key: &'static str,
    pub kind: FieldKind,
}

/// A named group of fields, shown as one tab/page in the overlay.
#[derive(Debug, Clone)]
pub struct Category {
    pub name: Text,
    pub fields: Vec<Field>,
}

/// Live navigation state while the settings overlay is open
/// (`WindowState::settings_nav`). Indices into `categories()`; values are
/// always read fresh from `Config`, so the overlay reflects external config
/// reloads too.
#[derive(Debug, Clone, Default)]
pub struct SettingsNav {
    pub category: usize,
    pub field: usize,
    /// `true` while the focused Keybind field is waiting for the
    /// user to press a chord to bind. The next non-modifier key press is
    /// captured as the new binding; Esc cancels.
    pub capturing: bool,
}

/// State for the inline text prompt opened by a [`FieldKind::Text`] row (the
/// in-settings image-path entry). `key` is the config key to persist on submit;
/// `buf` is the editable string (append / backspace only; the cursor is always
/// at the end, plenty for a path). Enter persists + reloads, Esc cancels.
#[derive(Debug, Clone)]
pub struct SettingsTextEdit {
    pub key: &'static str,
    pub buf: String,
}

fn toggle(label: Text, key: &'static str) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Toggle,
    }
}

fn choice(
    label: Text,
    key: &'static str,
    values: &'static [&'static str],
    labels: &'static [Label],
) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Choice {
            values,
            labels: Some(labels),
        },
    }
}

/// A runtime-options Choice (see [`FieldKind::ChoiceOwned`]).
fn choice_owned(label: Text, key: &'static str, values: Vec<String>, labels: Vec<Label>) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::ChoiceOwned { values, labels },
    }
}

fn number(
    label: Text,
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
fn keybind(label: Text, action: &'static str) -> Field {
    Field {
        label,
        key: action,
        kind: FieldKind::Keybind { action },
    }
}

/// A free-text field (the in-settings image-path entry).
fn text(label: Text, key: &'static str, placeholder: Text) -> Field {
    Field {
        label,
        key,
        kind: FieldKind::Text { placeholder },
    }
}

/// The curated catalogue. Covers the settings a typical user actually reaches
/// for; the long tail lives in the config file (right-click Preferences >
/// Advanced...).
///
/// `gpus` are the GPUs detected on this machine as `(token, label)` pairs (the
/// token is what [`read_choice`] round-trips for the `gpu` key — `"auto"` or
/// `"<vendor-hex>:<device-hex>:<name>"`). They populate the Graphics category's
/// device picker; pass `&[]` (e.g. in tests) for just the "Automatic" option.
pub fn categories(gpus: &[(String, String)]) -> Vec<Category> {
    use Text as T;
    // GPU device options: Automatic first, then each detected GPU by name.
    let mut gpu_values = vec!["auto".to_string()];
    let mut gpu_labels = vec![Label::Text(T::SettingsValueAutomatic)];
    for (val, label) in gpus {
        gpu_values.push(val.clone());
        gpu_labels.push(Label::Name(Cow::Owned(label.clone())));
    }
    vec![
        Category {
            name: T::SettingsCategoryAppearance,
            fields: vec![
                // The most popular themes as a cyclable list of
                // options; ←/→ live-previews each (the settings handler persists
                // + reloads on every step, so the theme applies instantly). The
                // full 500+ bundle stays reachable via the right-click Theme
                // submenu / NextTheme / the `theme =` config line. Theme names
                // are proper names, shown verbatim.
                Field {
                    label: T::SettingsFieldTheme,
                    key: "theme",
                    kind: FieldKind::Choice {
                        values: kettle_config::Theme::POPULAR,
                        labels: None,
                    },
                },
                number(T::SettingsFieldFontSize, "font-size", 6, 72, 1, "pt"),
                number(
                    T::SettingsFieldBackgroundOpacity,
                    "background-opacity",
                    20,
                    100,
                    1,
                    "%",
                ),
                toggle(T::SettingsFieldWindowBlur, "window-blur"),
                number(
                    T::SettingsFieldWindowPadding,
                    "window-padding-x",
                    0,
                    40,
                    2,
                    "px",
                ),
                choice(
                    // Use the canonical `cursor-style`
                    // key (CONFIG.md's authoritative spelling) rather than the
                    // `cursor-shape` back-compat alias, so the overlay persists
                    // the canonical line and SETTINGS.md ↔ CONFIG.md ↔ catalogue
                    // agree. Persist `beam`, the legacy alias for `bar`, while
                    // the display label is `Bar`.
                    T::SettingsFieldCursorShape,
                    "cursor-style",
                    &["block", "beam", "underline"],
                    &[
                        Label::Text(T::SettingsValueBlock),
                        Label::Text(T::SettingsValueBar),
                        Label::Text(T::SettingsValueUnderline),
                    ],
                ),
                toggle(T::SettingsFieldCursorBlink, "cursor-blink"),
                // Seconds without typing before the blink rests on a visible
                // cursor; 0 blinks indefinitely.
                number(
                    T::SettingsFieldCursorBlinkTimeout,
                    "cursor-blink-timeout",
                    0,
                    3600,
                    5,
                    "s",
                ),
                toggle(T::SettingsFieldShowTitlebar, "show-titlebar"),
            ],
        },
        // The Background page. `starfield` is a zero-config animated
        // background; `image` takes a file path (edited inline here).
        // Sub-options below the type are dimmed and skipped when they don't
        // apply to the selected type (see `field_disabled`).
        Category {
            name: T::SettingsCategoryBackground,
            fields: vec![
                choice(
                    T::SettingsFieldBackgroundType,
                    "background-type",
                    &["solid", "image", "starfield", "transparent"],
                    &[
                        Label::Text(T::SettingsValueSolidColor),
                        Label::Text(T::SettingsValueImage),
                        Label::Text(T::SettingsValueStarfield),
                        Label::Text(T::SettingsValueTransparent),
                    ],
                ),
                text(
                    T::SettingsFieldBackgroundImage,
                    "background-image",
                    T::SettingsValueImagePlaceholder,
                ),
                choice(
                    // `always` is listed first because it is the default.
                    T::SettingsFieldBackgroundAnimation,
                    "background-animation",
                    &["always", "when-focused", "off"],
                    &[
                        Label::Text(T::SettingsValueAlways),
                        Label::Text(T::SettingsValueWhenFocused),
                        Label::Text(T::SettingsValueOff),
                    ],
                ),
                choice(
                    T::SettingsFieldChromeBackground,
                    "chrome-background",
                    &["theme", "auto", "black", "white"],
                    &[
                        Label::Text(T::SettingsValueTheme),
                        Label::Text(T::SettingsValueAutomaticFromWallpaper),
                        Label::Text(T::SettingsValueBlack),
                        Label::Text(T::SettingsValueWhite),
                    ],
                ),
            ],
        },
        Category {
            name: T::SettingsCategoryBehavior,
            fields: vec![
                choice(
                    T::SettingsFieldLanguage,
                    "language",
                    &["auto", "en", "es"],
                    LANGUAGE_LABELS,
                ),
                choice(
                    T::SettingsFieldScrollbar,
                    "scrollbar",
                    &["never", "auto", "always"],
                    &[
                        Label::Text(T::SettingsValueHidden),
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueAlways),
                    ],
                ),
                choice(
                    T::SettingsFieldCompletionOverlay,
                    "completion-overlay",
                    &["auto", "off"],
                    &[
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueOff),
                    ],
                ),
                // Width of the pronounced overlay scrollbar.
                number(
                    T::SettingsFieldScrollbarWidth,
                    "scrollbar-width",
                    2,
                    40,
                    2,
                    "px",
                ),
                choice(
                    T::SettingsFieldBell,
                    "bell",
                    &["off", "visual", "attention", "both"],
                    &[
                        Label::Text(T::SettingsValueOff),
                        Label::Text(T::SettingsValueVisualFlash),
                        Label::Text(T::SettingsValueAttention),
                        Label::Text(T::SettingsValueVisualFlashAndAttention),
                    ],
                ),
                number(
                    T::SettingsFieldScrollbackLines,
                    "scrollback",
                    0,
                    100_000,
                    1_000,
                    "",
                ),
                number(
                    T::SettingsFieldScrollbackMemory,
                    "scrollback-bytes",
                    0,
                    1024,
                    10,
                    "MB",
                ),
                toggle(T::SettingsFieldCopyOnSelect, "copy-on-select"),
                toggle(
                    T::SettingsFieldMouseHideWhileTyping,
                    "mouse-hide-while-typing",
                ),
                choice(
                    T::SettingsFieldUpdatePolicy,
                    "update-policy",
                    &["off", "notify", "auto"],
                    &[
                        Label::Text(T::SettingsValueOff),
                        Label::Text(T::SettingsValueNotify),
                        Label::Text(T::SettingsValueInstallAutomatically),
                    ],
                ),
                number(
                    T::SettingsFieldUpdateCheckInterval,
                    "update-check-interval-hours",
                    1,
                    720,
                    1,
                    "h",
                ),
                // hjkl navigation in menus/overlays (default ON).
                toggle(T::SettingsFieldVimMenuNav, "vim-menu-nav"),
                choice(
                    T::SettingsFieldFocus,
                    "focus",
                    &["click", "sloppy", "system"],
                    &[
                        Label::Text(T::SettingsValueClickToFocus),
                        Label::Text(T::SettingsValueFollowsMouse),
                        Label::Text(T::SettingsValueSystemDefault),
                    ],
                ),
            ],
        },
        Category {
            name: T::SettingsCategorySearch,
            fields: vec![
                toggle(T::SettingsFieldSearchWrap, "search-wrap"),
                choice(
                    T::SettingsFieldSearchCase,
                    "search-case-sensitive",
                    &["smart", "always", "never"],
                    &[
                        Label::Text(T::SettingsValueSmart),
                        Label::Text(T::SettingsValueMatchCase),
                        Label::Text(T::SettingsValueIgnoreCase),
                    ],
                ),
                toggle(T::SettingsFieldInvertSearch, "invert-search"),
            ],
        },
        // `tab-bar-position` offers only top/bottom; left/right (vertical bars)
        // stay config-only, as docs/SETTINGS.md says.
        Category {
            name: T::SettingsCategoryTabs,
            fields: vec![
                choice(
                    T::SettingsFieldTabBar,
                    "tab-bar",
                    &["off", "auto", "always"],
                    &[
                        Label::Text(T::SettingsValueHidden),
                        Label::Text(T::SettingsValueAutomaticMultipleTabs),
                        Label::Text(T::SettingsValueAlways),
                    ],
                ),
                choice(
                    T::SettingsFieldTabBarPosition,
                    "tab-bar-position",
                    &["top", "bottom"],
                    &[
                        Label::Text(T::SettingsValueTop),
                        Label::Text(T::SettingsValueBottom),
                    ],
                ),
                number(
                    T::SettingsFieldTabMinWidth,
                    "tab-min-width",
                    40,
                    600,
                    10,
                    "px",
                ),
                toggle(T::SettingsFieldScrollTabbar, "scroll-tabbar"),
                toggle(T::SettingsFieldCloseButtonOnTab, "close-button-on-tab"),
                toggle(T::SettingsFieldDetachableTabs, "detachable-tabs"),
            ],
        },
        Category {
            name: T::SettingsCategoryGraphics,
            fields: vec![
                // Tier A: the power-preference policy (integrated vs discrete).
                // Applies on restart — the renderer/device graph can't hot-swap.
                choice(
                    T::SettingsFieldGpuPowerPreference,
                    "gpu-power-preference",
                    &["auto", "low", "high"],
                    &[
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueLowPower),
                        Label::Text(T::SettingsValueHighPerformance),
                    ],
                ),
                // Tier B: pin a specific detected GPU (or Automatic).
                choice_owned(T::SettingsFieldGpuDevice, "gpu", gpu_values, gpu_labels),
                // Advanced: backend + software fallback.
                choice(
                    T::SettingsFieldGpuBackend,
                    "gpu-backend",
                    &["auto", "dx12", "vulkan", "metal", "gl"],
                    GPU_BACKEND_LABELS,
                ),
                toggle(T::SettingsFieldGpuForceSoftware, "gpu-force-software"),
            ],
        },
        Category {
            name: T::SettingsCategoryKeybinds,
            fields: vec![
                keybind(T::SettingsKeybindSplitRight, "split_right"),
                keybind(T::SettingsKeybindSplitDown, "split_down"),
                keybind(T::SettingsKeybindClosePane, "close_pane"),
                keybind(T::SettingsKeybindNewTab, "new_tab"),
                keybind(T::SettingsKeybindNextTab, "next_tab"),
                keybind(T::SettingsKeybindPreviousTab, "previous_tab"),
                keybind(T::SettingsKeybindStartSearch, "start_search"),
                keybind(T::SettingsKeybindCommandPalette, "command_palette"),
                keybind(T::SettingsKeybindOpenSettings, "open_settings"),
                keybind(T::SettingsKeybindToggleZoom, "toggle_zoom"),
                keybind(T::SettingsKeybindCopy, "copy"),
                keybind(T::SettingsKeybindPaste, "paste"),
            ],
        },
    ]
}

/// Read a field's current value from `cfg`, formatted for display next to its
/// label (e.g. `"14 pt"`, `"Automatic"`, `"On"`) in `tr`'s language. Returns a
/// best-effort string; an unknown key (catalogue/Config drift) yields a
/// fallback such as `"Off"`, `""`, or `"—"` rather than panicking.
pub fn read(cfg: &Config, field: &Field, tr: &Translator) -> String {
    use Text as T;
    match &field.kind {
        FieldKind::Toggle => {
            let state = if read_bool(cfg, field.key) {
                T::SettingsValueOn
            } else {
                T::SettingsValueOff
            };
            tr.text(state).to_string()
        }
        FieldKind::Choice { values, labels } => {
            let cur = read_choice(cfg, field.key);
            // `labels.get(i)` (not `labels[i]`) so a catalogue entry
            // with mismatched values/labels lengths degrades to the raw value
            // instead of panicking on an out-of-bounds index.
            let shown = values
                .iter()
                .position(|v| *v == cur)
                .and_then(|i| match labels {
                    Some(labels) => labels.get(i).map(|label| label.show(tr)),
                    None => Some(values[i]),
                });
            match shown {
                Some(label) => label.to_string(),
                None => match (field.key, cur.as_str()) {
                    ("tab-bar-position", "left") => tr.text(T::SettingsValueLeft).to_string(),
                    ("tab-bar-position", "right") => tr.text(T::SettingsValueRight).to_string(),
                    _ => cur.clone(),
                },
            }
        }
        FieldKind::ChoiceOwned { values, labels } => {
            let cur = read_choice(cfg, field.key);
            values
                .iter()
                .position(|v| *v == cur)
                .and_then(|i| labels.get(i))
                .map(|label| label.show(tr).to_string())
                // No match (e.g. a pinned GPU that no longer enumerates) →
                // show the saved name if any, else "Automatic".
                .unwrap_or_else(|| {
                    if cfg.gpu_name.trim().is_empty() {
                        tr.text(T::SettingsValueAutomatic).to_string()
                    } else {
                        tr.settings_value_not_detected(&cfg.gpu_name)
                    }
                })
        }
        FieldKind::Number { suffix, .. } => {
            let value = read_number(cfg, field.key);
            // Both scrollback rows have a zero sentinel. Showing it as a
            // quantity invites users to impose a finite cap without realising
            // what they give up.
            if field.key == "scrollback" && value == 0 {
                tr.text(T::SettingsValueInfinite).to_string()
            } else if field.key == "cursor-blink-timeout" && value == 0 {
                tr.text(T::SettingsValueNever).to_string()
            } else if field.key == "scrollback-bytes" && cfg.scrollback_bytes == 0 {
                tr.text(T::SettingsValueNoCap).to_string()
            } else if field.key == "scrollback-bytes" && cfg.scrollback_bytes < 1_000_000 {
                // A comparison and a unit symbol: the same in every language.
                "<1 MB".to_string()
            } else {
                format_number(value, suffix)
            }
        }
        FieldKind::Keybind { action } => {
            // The chord bound to this action in the effective keymap, chosen
            // as the palette and menus choose it; "Unbound" if none. The
            // keymap's order changes between launches, so a first match would
            // not be stable for an action with several chords.
            match kettle_config::Action::from_name(action) {
                Some(a) => kettle_config::keybinds::hint_label(&cfg.keybinds, &a)
                    .unwrap_or_else(|| tr.text(T::SettingsValueUnbound).to_string()),
                None => "—".to_string(),
            }
        }
        FieldKind::Text { placeholder } => {
            let v = read_string(cfg, field.key);
            if v.trim().is_empty() {
                tr.text(*placeholder).to_string()
            } else {
                v
            }
        }
    }
}

/// A number and its unit symbol. Symbols are not translated, and both
/// supported languages separate them the same way.
fn format_number(value: i64, suffix: &str) -> String {
    if suffix.is_empty() || suffix == "%" {
        format!("{value}{suffix}")
    } else {
        format!("{value} {suffix}")
    }
}

/// Stable column widths from every category, including unselected choices,
/// measured in `tr`'s language.
pub fn column_widths(cfg: &Config, cats: &[Category], tr: &Translator) -> (usize, usize) {
    use unicode_width::UnicodeWidthChar;
    let width = |text: &str| text.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    let mut label_cols = 0;
    let mut value_cols = width(tr.text(Text::SettingsPressChord));
    for field in cats.iter().flat_map(|cat| &cat.fields) {
        label_cols = label_cols.max(width(tr.text(field.label)));
        value_cols = value_cols.max(width(&read(cfg, field, tr)));
        match &field.kind {
            FieldKind::Choice { values, labels } => match labels {
                Some(labels) => {
                    for label in *labels {
                        value_cols = value_cols.max(width(label.show(tr)));
                    }
                }
                None => {
                    for value in *values {
                        value_cols = value_cols.max(width(value));
                    }
                }
            },
            FieldKind::ChoiceOwned { labels, .. } => {
                for label in labels {
                    value_cols = value_cols.max(width(label.show(tr)));
                }
            }
            FieldKind::Number { max, suffix, .. } => {
                value_cols = value_cols.max(width(&format_number(*max, suffix)));
            }
            _ => {}
        }
    }
    (label_cols, value_cols)
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
        // The theme row steps through the popular themes of the current
        // theme's appearance, in look order, so ←/→ never flips between a
        // light and a dark palette.
        FieldKind::Choice { .. } if field.key == "theme" => {
            kettle_config::Theme::cycle_popular_by_look(&cfg.theme_name, dir >= 0).to_string()
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
            // The catalogue exposes a convenient range, but the config grammar
            // accepts wider values. An out-of-range value steps freely, because
            // clamping would snap it to the catalogue boundary and could
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

/// Is this a free-text field (the inline-prompt path entry)? The overlay
/// handler routes Enter / Space / click into a text prompt for these.
pub fn is_text(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Text { .. })
}

/// Does this row open the theme picker when activated (Enter, Space or a
/// click)? ←/→ and the wheel still step it through looks of one appearance.
pub fn opens_theme_picker(field: &Field) -> bool {
    field.key == "theme"
}

/// Is `key`'s row inapplicable to the current config, so the overlay should DIM
/// it and skip it during nav/click? The image path only matters for `image`,
/// animation and chrome color only matter with a wallpaper (image or
/// starfield), and the blink timeout only matters while the cursor blinks.
pub fn field_disabled(cfg: &Config, key: &str) -> bool {
    use kettle_config::BackgroundType as BT;
    let t = cfg.background_type;
    match key {
        "background-image" => !matches!(t, BT::Image),
        "background-animation" | "chrome-background" => !matches!(t, BT::Image | BT::Starfield),
        "cursor-blink-timeout" => !cfg.cursor_blink,
        _ => false,
    }
}

/// The next field index from `start` stepping by `step` (`+1`/`-1`) that is
/// NOT [`field_disabled`], wrapping around. Lets keyboard nav skip dimmed
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
        "language" => cfg.language.token().to_string(),
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
        // Keyed on the canonical `cursor-style` (not the `cursor-shape` alias)
        // to match the catalogue field + CONFIG.md.
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
        // Background controls.
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
        // Graphics.
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
        // Tabs category.
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
        // (10 M), far above this row's 100 000 ceiling, and one arrow press
        // would persist a finite limit. `0` is the config grammar's own
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
        "cursor-blink-timeout" => cfg.cursor_blink_timeout as i64,
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
    use kettle_i18n::Language;

    const EN: Translator = Translator::new(Language::En);
    const ES: Translator = Translator::new(Language::Es);

    #[test]
    fn display_choices_use_sentence_case_without_changing_tokens() {
        let mut cfg = Config::default();
        cfg.window_blur = false;
        for cat in categories(&[]) {
            for field in &cat.fields {
                if let FieldKind::Choice { values, labels } = &field.kind {
                    // Only the theme list shows its values verbatim.
                    let Some(labels) = labels else {
                        assert_eq!(field.key, "theme");
                        continue;
                    };
                    assert_eq!(values.len(), labels.len());
                    for (token, label) in values.iter().zip(*labels) {
                        let label = label.show(&EN);
                        assert!(
                            label.chars().next().is_some_and(char::is_uppercase),
                            "{label}"
                        );
                        if *token == "auto" && field.key != "update-policy" {
                            assert!(label.starts_with("Automatic"), "{}: {label}", field.key);
                        }
                    }
                }
            }
        }
        assert_eq!(
            read(
                &cfg,
                &toggle(Text::SettingsFieldWindowBlur, "window-blur"),
                &EN
            ),
            "Off"
        );
        assert_eq!(
            next_value(
                &cfg,
                &toggle(Text::SettingsFieldWindowBlur, "window-blur"),
                0
            ),
            "true"
        );
    }

    #[test]
    fn display_units_have_spaces_and_keep_config_serialization() {
        for (suffix, expected) in [
            ("pt", "13 pt"),
            ("px", "13 px"),
            ("MB", "13 MB"),
            ("h", "13 h"),
            ("s", "13 s"),
            ("%", "13%"),
            ("", "13"),
        ] {
            assert_eq!(format_number(13, suffix), expected);
        }
        assert_eq!(write_number("scrollback-bytes", 120, "MB"), "120MB");
        assert_eq!(write_number("background-opacity", 99, "%"), "0.99");
    }

    #[test]
    fn column_widths_include_every_category_choice_and_gpu_name() {
        let gpus = [(
            "8086:9a49:GPU".into(),
            "An unusually long detected GPU name (Integrated)".into(),
        )];
        let cats = categories(&gpus);
        use unicode_width::UnicodeWidthStr;
        // Columns are measured in the language the panel is drawn in.
        for tr in [EN, ES] {
            let (labels, values) = column_widths(&Config::default(), &cats, &tr);
            assert_eq!(
                labels,
                cats.iter()
                    .flat_map(|c| &c.fields)
                    .map(|f| tr.text(f.label).width())
                    .max()
                    .unwrap()
            );
            assert!(values >= gpus[0].1.width());
            assert!(values >= tr.text(Text::SettingsPressChord).width());
        }
        assert_ne!(
            column_widths(&Config::default(), &cats, &EN).0,
            column_widths(&Config::default(), &cats, &ES).0
        );
    }

    #[test]
    fn formatted_values_keep_sentinels_and_saved_gpu_names() {
        let mut cfg = Config::default();
        let cats = categories(&[]);
        let field = |key| {
            cats.iter()
                .flat_map(|cat| &cat.fields)
                .find(|f| f.key == key)
                .unwrap()
        };
        cfg.font_size = 13.0;
        cfg.background_opacity = 0.99;
        cfg.scrollback_bytes = 120_000_000;
        cfg.cursor_blink_timeout = 10;
        cfg.update_check_interval_hours = 24;
        assert_eq!(read(&cfg, field("font-size"), &EN), "13 pt");
        assert_eq!(read(&cfg, field("background-opacity"), &EN), "99%");
        assert_eq!(read(&cfg, field("scrollback-bytes"), &EN), "120 MB");
        assert_eq!(read(&cfg, field("cursor-blink-timeout"), &EN), "10 s");
        assert_eq!(
            read(&cfg, field("update-check-interval-hours"), &EN),
            "24 h"
        );
        cfg.scrollback = kettle_config::INFINITE_SCROLLBACK;
        cfg.scrollback_bytes = 0;
        cfg.cursor_blink_timeout = 0;
        assert_eq!(read(&cfg, field("scrollback"), &EN), "Infinite");
        assert_eq!(read(&cfg, field("scrollback-bytes"), &EN), "No cap");
        assert_eq!(read(&cfg, field("cursor-blink-timeout"), &EN), "Never");
        cfg.scrollback_bytes = 500_000;
        assert_eq!(read(&cfg, field("scrollback-bytes"), &EN), "<1 MB");
        cfg.gpu_name = "llvmpipe (LLVM 19.1.7)".into();
        assert_eq!(
            read(&cfg, field("gpu"), &EN),
            "llvmpipe (LLVM 19.1.7) (not detected)"
        );
        cfg.tab_bar = kettle_config::TabBarMode::Off;
        cfg.scrollbar = ScrollbarMode::Never;
        assert_eq!(read(&cfg, field("tab-bar"), &EN), "Hidden");
        assert_eq!(read(&cfg, field("scrollbar"), &EN), "Hidden");
        assert_eq!(next_value(&cfg, field("tab-bar"), 1), "auto");
        cfg.tab_bar_pos = kettle_config::TabBarPos::Left;
        assert_eq!(read(&cfg, field("tab-bar-position"), &EN), "Left");
    }

    /// The Scrollback row edits a value stored RESOLVED, so infinite reads back
    /// as `INFINITE_SCROLLBACK` (10 M) against a row whose ceiling is 100 000.
    /// A single ←/→ press must not clamp that to 100 000 and persist it,
    /// silently discarding an unlimited history.
    #[test]
    fn stepping_the_scrollback_row_cannot_silently_discard_infinite_history() {
        let mut cfg = Config::default();
        cfg.scrollback = kettle_config::INFINITE_SCROLLBACK;
        let field = number(
            Text::SettingsFieldScrollbackLines,
            "scrollback",
            0,
            100_000,
            1_000,
            "",
        );

        assert_eq!(
            read_number(&cfg, "scrollback"),
            0,
            "infinite must read back as the grammar's own sentinel, not the \
             resolved line count"
        );
        assert_eq!(
            read(&cfg, &field, &EN),
            "Infinite",
            "a bare 0 would invite stepping off infinite without realising it"
        );
        // Stepping DOWN from infinite must stay infinite rather than land on
        // the ceiling.
        assert_eq!(next_value(&cfg, &field, -1), "0");
        // Stepping UP is a deliberate move to a finite limit, which is fine,
        // but it must not jump to the 100 000 ceiling.
        assert_eq!(next_value(&cfg, &field, 1), "1000");

        // A finite value is unaffected.
        cfg.scrollback = 5_000;
        assert_eq!(read_number(&cfg, "scrollback"), 5_000);
        assert_eq!(read(&cfg, &field, &EN), "5000");
        assert_eq!(next_value(&cfg, &field, 1), "6000");

        let bytes = number(
            Text::SettingsFieldScrollbackMemory,
            "scrollback-bytes",
            0,
            1024,
            10,
            "MB",
        );
        cfg.scrollback_bytes = 0;
        assert_eq!(read(&cfg, &bytes, &EN), "No cap");
        assert_eq!(next_value(&cfg, &bytes, -1), "0MB");
        assert_eq!(next_value(&cfg, &bytes, 1), "10MB");

        cfg.scrollback_bytes = 500_000;
        assert_eq!(read(&cfg, &bytes, &EN), "<1 MB");
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
                let shown = read(&cfg, field, &EN);
                assert!(
                    !shown.is_empty() && shown != "—",
                    "field '{}' (key {}) read blank/unknown",
                    EN.text(field.label),
                    field.key
                );
            }
        }
    }

    #[test]
    fn gpu_device_choice_round_trips_and_cycles() {
        // The GPU picker's token round-trips through read_choice and the
        // ChoiceOwned cycle, and a pinned GPU shows its label.
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
            .find(|c| c.name == Text::SettingsCategoryGraphics)
            .expect("Graphics");
        let gpu_field = graphics
            .fields
            .iter()
            .find(|f| f.key == "gpu")
            .expect("gpu field");

        // Default cfg → "auto" token → shows "Automatic".
        let mut cfg = Config::default();
        assert_eq!(read(&cfg, gpu_field, &EN), "Automatic");
        // Cycling forward from auto lands on the first detected GPU's token.
        let next = next_value(&cfg, gpu_field, 1);
        assert_eq!(next, "10de:2191:NVIDIA GeForce GTX 1660 Ti");

        // A pinned NVIDIA cfg reads back the matching label.
        cfg.gpu_vendor_id = 0x10de;
        cfg.gpu_device_id = 0x2191;
        cfg.gpu_name = "NVIDIA GeForce GTX 1660 Ti".to_string();
        assert_eq!(
            read(&cfg, gpu_field, &EN),
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
        assert_eq!(
            read(&cfg, gpu_field, &EN),
            "Phantom GPU 9000 (not detected)"
        );

        // Software adapters commonly expose zero PCI ids. Their name remains
        // the pin identity, so Settings must not relabel an active software pin
        // as Automatic after persistence/reload.
        cfg.gpu_vendor_id = 0;
        cfg.gpu_device_id = 0;
        cfg.gpu_name = "llvmpipe (LLVM 19.1.7)".to_string();
        assert_eq!(
            read(&cfg, gpu_field, &EN),
            "llvmpipe (LLVM 19.1.7) (Software)"
        );
        assert_eq!(next_value(&cfg, gpu_field, 1), "auto");
    }

    #[test]
    fn gpu_power_preference_defaults_to_automatic_in_settings() {
        let cats = categories(&[]);
        let graphics = cats
            .iter()
            .find(|c| c.name == Text::SettingsCategoryGraphics)
            .expect("Graphics");
        let pref = graphics
            .fields
            .iter()
            .find(|f| f.key == "gpu-power-preference")
            .expect("gpu-power-preference field");

        assert_eq!(read(&Config::default(), pref, &EN), "Automatic");
        match &pref.kind {
            FieldKind::Choice { values, labels, .. } => {
                assert_eq!(values.first().copied(), Some("auto"));
                let first = labels.and_then(|labels| labels.first());
                assert_eq!(first.map(|label| label.show(&EN)), Some("Automatic"));
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
            .find(|c| c.name == Text::SettingsCategoryBackground)
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
    fn blink_timeout_row_reads_its_seconds_and_dims_without_blink() {
        let appearance = categories(&[])
            .into_iter()
            .find(|c| c.name == Text::SettingsCategoryAppearance)
            .expect("Appearance category");
        assert!(
            appearance
                .fields
                .iter()
                .any(|f| f.key == "cursor-blink-timeout")
        );
        let mut cfg = Config::parse_text("cursor-blink-timeout = 45");
        assert_eq!(read_number(&cfg, "cursor-blink-timeout"), 45);
        assert!(!field_disabled(&cfg, "cursor-blink-timeout"));
        cfg.cursor_blink = false;
        assert!(field_disabled(&cfg, "cursor-blink-timeout"));
        let row = appearance
            .fields
            .iter()
            .find(|f| f.key == "cursor-blink-timeout")
            .expect("timeout row");
        assert_eq!(
            read(&Config::parse_text("cursor-blink-timeout = 10"), row, &EN),
            "10 s"
        );
        assert_eq!(
            read(&Config::parse_text("cursor-blink-timeout = 0"), row, &EN),
            "Never"
        );
    }

    #[test]
    fn next_enabled_field_skips_gated_rows() {
        use kettle_config::BackgroundType as BT;
        let cats = categories(&[]);
        let bg = cats
            .iter()
            .find(|c| c.name == Text::SettingsCategoryBackground)
            .unwrap();
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
        let f = Field {
            label: Text::SettingsFieldChromeBackground,
            key: "chrome-background",
            kind: FieldKind::Choice {
                values: &["theme", "auto", "black", "white"],
                labels: None,
            },
        };
        assert_eq!(read(&cfg, &f, &EN), "auto");
    }

    /// Drift guard: `keybind_action` extracts the canonical action token the
    /// settings overlay routes into chord-capture (Enter on a keybind row).
    /// Returning the token for the wrong variant would silently break keybind
    /// editing. Pin Some(action) for keybind fields and None for every other
    /// field kind.
    /// An action with several chords shows the same one on every launch, the
    /// one the palette and menus show. The keymap is a `HashMap`, whose order
    /// differs between instances, so a first-match lookup changed from run to
    /// run.
    #[test]
    fn a_keybind_row_shows_one_stable_chord() {
        let row = keybind(Text::SettingsKeybindClosePane, "close_pane");
        let close = kettle_config::Action::from_name("close_pane").unwrap();
        let mut shown = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let mut cfg = Config::default();
            cfg.keybinds.retain(|_, action| *action != close);
            for chord in ["ctrl+shift+w", "super+shift+d", "ctrl+alt+shift+q"] {
                let trigger = kettle_config::keybinds::parse_trigger(chord).unwrap();
                cfg.keybinds.insert(trigger, close.clone());
            }
            let label = read(&cfg, &row, &EN);
            assert_eq!(
                Some(&label),
                kettle_config::keybinds::hint_label(&cfg.keybinds, &close).as_ref()
            );
            shown.insert(label);
        }
        assert_eq!(shown.into_iter().collect::<Vec<_>>(), ["Ctrl+Shift+W"]);
    }

    #[test]
    fn keybind_action_extracts_token_for_keybind_fields_only() {
        assert_eq!(
            keybind_action(&keybind(Text::SettingsKeybindSplitRight, "split_right")),
            Some("split_right")
        );
        // Empty-token boundary: still Some, just empty (the catalogue never
        // ships one, but the accessor must not special-case it to None).
        assert_eq!(
            keybind_action(&keybind(Text::SettingsKeybindCopy, "")),
            Some("")
        );
        // Non-keybind kinds yield None.
        assert_eq!(
            keybind_action(&toggle(Text::SettingsFieldCursorBlink, "cursor-blink")),
            None
        );
        assert_eq!(
            keybind_action(&choice(
                Text::SettingsFieldScrollbar,
                "scrollbar",
                &["auto"],
                &[Label::Text(Text::SettingsValueAutomatic)],
            )),
            None
        );
        assert_eq!(
            keybind_action(&number(
                Text::SettingsFieldFontSize,
                "font-size",
                6,
                72,
                1,
                "pt"
            )),
            None
        );
        // is_keybind agrees with keybind_action on the discriminant.
        assert!(is_keybind(&keybind(Text::SettingsKeybindCopy, "copy")));
        assert!(!is_keybind(&toggle(
            Text::SettingsFieldCursorBlink,
            "cursor-blink"
        )));
    }

    #[test]
    fn toggle_flips() {
        let cfg = Config::default();
        let f = toggle(Text::SettingsFieldCursorBlink, "cursor-blink");
        let before = read_bool(&cfg, "cursor-blink");
        let next = next_value(&cfg, &f, 0);
        assert_eq!(next, (!before).to_string());
    }

    /// The `catalogue_keys_are_all_readable` guard can't catch a missing
    /// `read_bool` arm for a default-ON toggle (the `_ => false` fallback shows
    /// a plausible "off"). Pin the row to its real default so the arm can't
    /// silently go missing.
    #[test]
    fn vim_menu_nav_row_reads_its_real_default() {
        let cfg = Config::default();
        assert!(
            read_bool(&cfg, "vim-menu-nav"),
            "vim-menu-nav defaults ON; the settings row must show it"
        );
        let f = toggle(Text::SettingsFieldVimMenuNav, "vim-menu-nav");
        assert_eq!(read(&cfg, &f, &EN), "On");
        assert_eq!(next_value(&cfg, &f, 0), "false");
    }

    /// ←/→ on the theme row step through popular themes of the current
    /// theme's appearance. The list's own order used to jump from
    /// TokyoNight Moon to the light TokyoNight Day.
    /// Enter, Space or a click on the Theme row opens the theme picker; no
    /// other row does.
    #[test]
    fn only_the_theme_row_opens_the_theme_picker() {
        let openers: Vec<&str> = categories(&[])
            .iter()
            .flat_map(|category| category.fields.iter())
            .filter(|field| opens_theme_picker(field))
            .map(|field| field.key)
            .collect();
        assert_eq!(openers, ["theme"]);
    }

    /// The Language row saves `auto`, `en` or `es`, and names each language
    /// in that language whatever the UI speaks.
    #[test]
    fn the_language_row_saves_a_token_and_names_languages_natively() {
        let field = categories(&[])
            .into_iter()
            .flat_map(|category| category.fields)
            .find(|field| field.key == "language")
            .expect("a Language row");
        let mut cfg = Config::default();
        assert_eq!(read_choice(&cfg, "language"), "auto");
        assert_eq!(next_value(&cfg, &field, 1), "en");
        cfg.language = kettle_config::LanguagePreference::Spanish;
        assert_eq!(read_choice(&cfg, "language"), "es");
        assert_eq!(next_value(&cfg, &field, 1), "auto");
        let FieldKind::Choice {
            labels: Some(labels),
            values,
        } = &field.kind
        else {
            panic!("the Language row is a choice");
        };
        for value in values.iter() {
            assert!(kettle_config::LanguagePreference::parse(value).is_some());
        }
        let shown = |tr: Translator| {
            labels
                .iter()
                .map(|label| label.show(&tr).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shown(Translator::new(Language::En)),
            ["Automatic", "English", "Español"]
        );
        assert_eq!(
            shown(Translator::new(Language::Es)),
            ["Automático", "English", "Español"]
        );
        assert_eq!(Translator::new(Language::Es).text(field.label), "Idioma");
    }

    #[test]
    fn the_theme_row_never_flips_between_light_and_dark() {
        let row = categories(&[])
            .into_iter()
            .flat_map(|c| c.fields)
            .find(|f| f.key == "theme")
            .unwrap();
        for name in kettle_config::Theme::POPULAR {
            let mut cfg = Config::default();
            cfg.theme_name = name.to_string();
            let dark = kettle_config::Theme::by_name(name).is_dark();
            for dir in [1, -1] {
                let next = next_value(&cfg, &row, dir);
                assert!(
                    kettle_config::Theme::POPULAR.contains(&next.as_str()),
                    "{next}"
                );
                assert_eq!(
                    kettle_config::Theme::by_name(&next).is_dark(),
                    dark,
                    "{name} -> {next}"
                );
            }
        }
    }

    #[test]
    fn choice_cycles_both_directions_and_wraps() {
        let cfg = Config::default(); // scrollbar default = auto
        let f = choice(
            Text::SettingsFieldScrollbar,
            "scrollbar",
            &["never", "auto", "always"],
            &[
                Label::Text(Text::SettingsValueHidden),
                Label::Text(Text::SettingsValueAutomatic),
                Label::Text(Text::SettingsValueAlways),
            ],
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
        let f = number(Text::SettingsFieldFontSize, "font-size", 6, 72, 1, "pt");
        assert_eq!(next_value(&cfg, &f, 1), "15");
        assert_eq!(next_value(&cfg, &f, -1), "13");
        assert_eq!(read(&cfg, &f, &EN), "14 pt");
        // clamp at ceiling
        cfg.font_size = 72.0;
        assert_eq!(next_value(&cfg, &f, 1), "72");
        // clamp at floor
        cfg.font_size = 6.0;
        assert_eq!(next_value(&cfg, &f, -1), "6");

        // Config-valid values outside the catalogue's convenient range step
        // normally; neither arrow may snap them to the nearest boundary.
        cfg.background_opacity = 0.10;
        let opacity = number(
            Text::SettingsFieldBackgroundOpacity,
            "background-opacity",
            20,
            100,
            1,
            "%",
        );
        assert_eq!(next_value(&cfg, &opacity, -1), "0.09");
        assert_eq!(next_value(&cfg, &opacity, 1), "0.11");

        cfg.scrollback = 500_000;
        let scrollback = number(
            Text::SettingsFieldScrollbackLines,
            "scrollback",
            0,
            100_000,
            1_000,
            "",
        );
        assert_eq!(next_value(&cfg, &scrollback, -1), "499000");
        assert_eq!(next_value(&cfg, &scrollback, 1), "501000");

        cfg.scrollback_bytes = 4_000_000_000;
        let bytes = number(
            Text::SettingsFieldScrollbackMemory,
            "scrollback-bytes",
            0,
            1024,
            10,
            "MB",
        );
        assert_eq!(next_value(&cfg, &bytes, -1), "3990MB");
        assert_eq!(next_value(&cfg, &bytes, 1), "4010MB");

        cfg.update_check_interval_hours = 8_760;
        let updates = number(
            Text::SettingsFieldUpdateCheckInterval,
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
        cfg.background_opacity = 0.99;
        let f = number(
            Text::SettingsFieldBackgroundOpacity,
            "background-opacity",
            20,
            100,
            1,
            "%",
        );
        assert_eq!(read(&cfg, &f, &EN), "99%");
        assert_eq!(next_value(&cfg, &f, -1), "0.98");
        assert_eq!(next_value(&cfg, &f, 1), "1.00");

        cfg.background_opacity = 1.0;
        assert_eq!(next_value(&cfg, &f, -1), "0.99");
    }
}
