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

use kettle_config::{
    BellMode, Config, CursorStyle, CursorUnfocusedStyle, FocusMode, ScrollbarMode, UpdatePolicy,
};
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
    /// A process-wide quick-terminal shortcut persisted as a config value.
    Hotkey,
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HotkeyCapture {
    Cancel,
    Value(Option<kettle_config::keybinds::Trigger>),
    Pending,
}

pub(crate) fn capture_hotkey(
    key: Option<kettle_config::keybinds::Key>,
    mods: kettle_config::keybinds::Mods,
) -> HotkeyCapture {
    use kettle_config::keybinds::{Key, Trigger};
    match key {
        Some(Key::Escape) if mods.is_empty() => HotkeyCapture::Cancel,
        Some(Key::Backspace | Key::Delete) if mods.is_empty() => HotkeyCapture::Value(None),
        Some(key) if !mods.is_empty() => HotkeyCapture::Value(Some(Trigger { mods, key })),
        _ => HotkeyCapture::Pending,
    }
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

/// Description keys stay independent of config tokens and translated labels.
pub fn description(field: &Field) -> Text {
    match field.label {
        Text::QuickTerminalHotkey => Text::QuickTerminalHotkeyDescription,
        Text::QuickTerminalPosition => Text::QuickTerminalPositionDescription,
        Text::QuickTerminalSize => Text::QuickTerminalSizeDescription,
        Text::QuickTerminalAnimation => Text::QuickTerminalAnimationDescription,
        Text::QuickTerminalAutohide => Text::QuickTerminalAutohideDescription,
        Text::SettingsFieldAgentPreviews => Text::SettingsDescriptionFieldAgentPreviews,
        Text::SettingsFieldAgentPreviewsClaudeCode => {
            Text::SettingsDescriptionFieldAgentPreviewsClaudeCode
        }
        Text::SettingsFieldAgentPreviewsCodex => Text::SettingsDescriptionFieldAgentPreviewsCodex,
        Text::SettingsFieldAgentPreviewsAgy => Text::SettingsDescriptionFieldAgentPreviewsAgy,
        Text::SettingsFieldAddKettleToPath => Text::SettingsDescriptionFieldAddKettleToPath,
        Text::SettingsFieldBackgroundAnimation => Text::SettingsDescriptionFieldBackgroundAnimation,
        Text::SettingsFieldBackgroundImage => Text::SettingsDescriptionFieldBackgroundImage,
        Text::SettingsFieldBackgroundOpacity => Text::SettingsDescriptionFieldBackgroundOpacity,
        Text::SettingsFieldBackgroundType => Text::SettingsDescriptionFieldBackgroundType,
        Text::SettingsFieldBell => Text::SettingsDescriptionFieldBell,
        Text::SettingsFieldChromeBackground => Text::SettingsDescriptionFieldChromeBackground,
        Text::SettingsFieldCloseButtonOnTab => Text::SettingsDescriptionFieldCloseButtonOnTab,
        Text::SettingsFieldCompletionOverlay => Text::SettingsDescriptionFieldCompletionOverlay,
        Text::SettingsFieldCopyTrimWhitespace => Text::SettingsDescriptionFieldCopyTrimWhitespace,
        Text::SettingsFieldCursorUnfocusedStyle => {
            Text::SettingsDescriptionFieldCursorUnfocusedStyle
        }
        Text::SettingsFieldCopyOnSelect => Text::SettingsDescriptionFieldCopyOnSelect,
        Text::SettingsFieldPasteProtection => Text::SettingsDescriptionFieldPasteProtection,
        Text::SettingsFieldOsc52 => Text::SettingsDescriptionFieldOsc52,
        Text::SettingsFieldCursorBlink => Text::SettingsDescriptionFieldCursorBlink,
        Text::SettingsFieldCursorBlinkTimeout => Text::SettingsDescriptionFieldCursorBlinkTimeout,
        Text::SettingsFieldCursorShape => Text::SettingsDescriptionFieldCursorShape,
        Text::SettingsFieldDetachableTabs => Text::SettingsDescriptionFieldDetachableTabs,
        Text::SettingsFieldFocus => Text::SettingsDescriptionFieldFocus,
        Text::SettingsFieldGraphemeClustering => Text::SettingsDescriptionFieldGraphemeClustering,
        Text::SettingsFieldBuiltinBoxDrawing => Text::SettingsDescriptionFieldBuiltinBoxDrawing,
        Text::SettingsFieldFontSize => Text::SettingsDescriptionFieldFontSize,
        Text::SettingsFieldGpuBackend => Text::SettingsDescriptionFieldGpuBackend,
        Text::SettingsFieldGpuDevice => Text::SettingsDescriptionFieldGpuDevice,
        Text::SettingsFieldGpuForceSoftware => Text::SettingsDescriptionFieldGpuForceSoftware,
        Text::SettingsFieldGpuPowerPreference => Text::SettingsDescriptionFieldGpuPowerPreference,
        Text::SettingsFieldInvertSearch => Text::SettingsDescriptionFieldInvertSearch,
        Text::SettingsFieldLanguage => Text::SettingsDescriptionFieldLanguage,
        Text::SettingsFieldMouseHideWhileTyping => {
            Text::SettingsDescriptionFieldMouseHideWhileTyping
        }
        Text::SettingsFieldScrollTabbar => Text::SettingsDescriptionFieldScrollTabbar,
        Text::SettingsFieldScrollbackLines => Text::SettingsDescriptionFieldScrollbackLines,
        Text::SettingsFieldScrollbackMemory => Text::SettingsDescriptionFieldScrollbackMemory,
        Text::SettingsFieldScrollbar => Text::SettingsDescriptionFieldScrollbar,
        Text::SettingsFieldScrollbarWidth => Text::SettingsDescriptionFieldScrollbarWidth,
        Text::SettingsFieldSearchCase => Text::SettingsDescriptionFieldSearchCase,
        Text::SettingsFieldSearchWrap => Text::SettingsDescriptionFieldSearchWrap,
        Text::SettingsFieldShowTitlebar => Text::SettingsDescriptionFieldShowTitlebar,
        Text::SettingsFieldTabBar => Text::SettingsDescriptionFieldTabBar,
        Text::SettingsFieldTabBarPosition => Text::SettingsDescriptionFieldTabBarPosition,
        Text::SettingsFieldTabMinWidth => Text::SettingsDescriptionFieldTabMinWidth,
        Text::SettingsFieldTheme => Text::SettingsDescriptionFieldTheme,
        Text::SettingsFieldAppearance => Text::SettingsDescriptionFieldAppearance,
        Text::SettingsFieldMinimumContrast => Text::SettingsDescriptionFieldMinimumContrast,
        Text::SettingsFieldUpdateCheckInterval => Text::SettingsDescriptionFieldUpdateCheckInterval,
        Text::SettingsFieldRestoreSession => Text::SettingsDescriptionFieldRestoreSession,
        Text::SettingsFieldRestoreScrollbackLines => {
            Text::SettingsDescriptionRestoreScrollbackLines
        }
        Text::SettingsFieldRestoreScrollbackRemote => {
            Text::SettingsDescriptionRestoreScrollbackRemote
        }
        Text::SettingsFieldUpdatePolicy => Text::SettingsDescriptionFieldUpdatePolicy,
        Text::SettingsFieldVimMenuNav => Text::SettingsDescriptionFieldVimMenuNav,
        Text::SettingsFieldWindowBlur => Text::SettingsDescriptionFieldWindowBlur,
        Text::SettingsFieldWindowPadding => Text::SettingsDescriptionFieldWindowPadding,
        Text::SettingsKeybindClosePane => Text::SettingsDescriptionKeybindClosePane,
        Text::SettingsKeybindCommandPalette => Text::SettingsDescriptionKeybindCommandPalette,
        Text::SettingsKeybindCopy => Text::SettingsDescriptionKeybindCopy,
        Text::SettingsKeybindNewTab => Text::SettingsDescriptionKeybindNewTab,
        Text::SettingsKeybindNextTab => Text::SettingsDescriptionKeybindNextTab,
        Text::SettingsKeybindOpenSettings => Text::SettingsDescriptionKeybindOpenSettings,
        Text::SettingsKeybindPaste => Text::SettingsDescriptionKeybindPaste,
        Text::SettingsKeybindPreviousTab => Text::SettingsDescriptionKeybindPreviousTab,
        Text::SettingsKeybindSplitDown => Text::SettingsDescriptionKeybindSplitDown,
        Text::SettingsKeybindSplitRight => Text::SettingsDescriptionKeybindSplitRight,
        Text::SettingsKeybindStartSearch => Text::SettingsDescriptionKeybindStartSearch,
        Text::SettingsKeybindToggleZoom => Text::SettingsDescriptionKeybindToggleZoom,
        _ => unreachable!("settings field lacks a description"),
    }
}

/// What this window can do with Window blur, as Settings shows it. Blur is
/// only asked for while it is on, so `Checking` and `Unavailable` always
/// describe a saved On.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlurState {
    /// Blur is off, shows, or this window system never says it cannot.
    #[default]
    Available,
    /// A Wayland compositor is being asked; the answer comes within half a
    /// second. Settings shows neither On nor Unavailable until it does.
    Checking,
    /// This session cannot blur windows.
    Unavailable,
}

/// What a row shows under its label. While the window system cannot blur
/// (`BlurState::Unavailable`), the Window blur row says so, and names the
/// saved setting, instead of describing an effect the window will not show.
/// Search still matches the field's own description.
pub fn row_description(field: &Field, blur: BlurState, cfg: &Config, tr: &Translator) -> String {
    match field.key {
        "window-blur" if blur == BlurState::Unavailable => {
            tr.settings_note_blur_unavailable(&read(cfg, field, tr))
        }
        _ => tr.text(description(field)).to_string(),
    }
}

/// What a row's control shows: its value, except that a Window blur switch
/// whose effect this session cannot show (or has not yet decided) says so
/// instead of On.
pub fn row_value(field: &Field, blur: BlurState, cfg: &Config, tr: &Translator) -> String {
    match (field.key, blur) {
        ("window-blur", BlurState::Unavailable) => {
            tr.text(Text::SettingsValueUnavailable).to_string()
        }
        ("window-blur", BlurState::Checking) => tr.text(Text::SettingsValueChecking).to_string(),
        _ => read(cfg, field, tr),
    }
}

/// Whether Reset covers `field` (`None` where it does not: the theme, the
/// GPU pin, shortcuts, free text, and a row that does not apply now), and if
/// so whether it would change anything.
pub fn reset_state(cfg: &Config, field: &Field) -> Option<bool> {
    if field_disabled(cfg, field.key) {
        return None;
    }
    default_value(field).map(|_| !holds_default(cfg, field))
}

/// The value a row's Reset restores, as the row shows values.
pub fn default_display(field: &Field, tr: &Translator) -> Option<String> {
    default_value(field).map(|_| read(&Config::default(), field, tr))
}

/// What a pointer event on a row's Reset chip does. Only a primary click
/// resets; a secondary click or a wheel notch acts on the row as it would
/// anywhere else on it, so the chip never takes a step meant for the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetPointer {
    Reset,
    Row,
}

pub fn reset_pointer(is_click: bool, dir: i32) -> ResetPointer {
    if is_click && dir > 0 {
        ResetPointer::Reset
    } else {
        ResetPointer::Row
    }
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
    pub(crate) query: crate::text_editor::TextEditor,
    pub search_focused: bool,
}

/// Fold Spanish precomposed and decomposed accents without changing stored text.
fn search_fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| {
            Some(match c {
                'á' | 'à' | 'ä' | 'â' => 'a',
                'é' | 'è' | 'ë' | 'ê' => 'e',
                'í' | 'ì' | 'ï' | 'î' => 'i',
                'ó' | 'ò' | 'ö' | 'ô' => 'o',
                'ú' | 'ù' | 'ü' | 'û' => 'u',
                'ñ' => 'n',
                '\u{0300}'..='\u{036f}' => return None,
                c => c,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchRoute {
    Pass,
    Handled,
    Edit,
}

impl SettingsNav {
    /// Focus the row that edits config `key`, in its own category, with no
    /// search in the way. An unknown key leaves the navigation where it is.
    pub(crate) fn focus_key(&mut self, cats: &[Category], key: &str) -> bool {
        let Some((category, field)) = cats.iter().enumerate().find_map(|(category, cat)| {
            let field = cat.fields.iter().position(|field| field.key == key)?;
            Some((category, field))
        }) else {
            return false;
        };
        self.query = Default::default();
        self.search_focused = false;
        self.capturing = false;
        self.category = category;
        self.field = field;
        true
    }

    pub(crate) fn apply_hit(
        &mut self,
        cats: &[Category],
        cfg: &Config,
        tr: &Translator,
        hit: kettle_render::SettingsHit,
    ) -> Option<(usize, usize)> {
        match hit {
            kettle_render::SettingsHit::Search => {
                self.search_focused = true;
                self.capturing = false;
            }
            kettle_render::SettingsHit::Category(index) if index < cats.len() => {
                self.query = Default::default();
                self.search_focused = false;
                self.category = index;
                self.field = self.field.min(cats[index].fields.len().saturating_sub(1));
                self.capturing = false;
            }
            kettle_render::SettingsHit::Field(index) | kettle_render::SettingsHit::Reset(index) => {
                return self.focus_result(cats, cfg, tr, index);
            }
            _ => {}
        }
        None
    }

    pub(crate) fn search_key(
        &mut self,
        cats: &[Category],
        cfg: &Config,
        tr: &Translator,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
        macos: bool,
    ) -> SearchRoute {
        use winit::keyboard::{Key, NamedKey};
        if self.capturing {
            return SearchRoute::Pass;
        }
        let shortcut = if macos {
            mods.super_key()
        } else {
            mods.control_key()
        };
        let find = matches!(key, Key::Character(s) if s.eq_ignore_ascii_case("f"))
            && shortcut
            && !mods.alt_key();
        let slash = cfg.vim_menu_nav
            && !self.search_focused
            && !mods.intersects(
                winit::keyboard::ModifiersState::CONTROL
                    | winit::keyboard::ModifiersState::ALT
                    | winit::keyboard::ModifiersState::SUPER,
            )
            && matches!(key, Key::Character(s) if s == "/");
        if find || slash {
            self.search_focused = true;
            return SearchRoute::Handled;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if !self.query.is_empty() {
                self.query = Default::default();
                self.reconcile(cats, cfg, tr);
                return SearchRoute::Handled;
            }
            return SearchRoute::Pass;
        }
        if self.search_focused {
            if matches!(
                key,
                Key::Named(NamedKey::Enter | NamedKey::ArrowDown | NamedKey::Tab)
            ) {
                self.reconcile(cats, cfg, tr);
                if !self.visible_fields(cats, tr).is_empty() {
                    self.search_focused = false;
                }
                return SearchRoute::Handled;
            }
            return SearchRoute::Edit;
        }
        if self.query.is_empty() {
            return SearchRoute::Pass;
        }
        let count = self.visible_fields(cats, tr).len();
        if count == 0 {
            return if matches!(key, Key::Named(NamedKey::Tab)) {
                SearchRoute::Pass
            } else {
                SearchRoute::Handled
            };
        }
        let delta = match key {
            Key::Named(NamedKey::ArrowDown) => Some(1),
            Key::Named(NamedKey::ArrowUp) => Some(-1),
            Key::Character(s) if cfg.vim_menu_nav && !mods.alt_key() && !mods.super_key() => {
                match (s.to_ascii_lowercase().as_str(), mods.control_key()) {
                    ("j", false) => Some(1),
                    ("k", false) => Some(-1),
                    ("d", true) => Some((count / 2).max(1) as isize),
                    ("u", true) => Some(-((count / 2).max(1) as isize)),
                    ("g", false) => {
                        let enabled: Vec<_> = self
                            .visible_fields(cats, tr)
                            .into_iter()
                            .filter(|&(c, f)| !field_disabled(cfg, cats[c].fields[f].key))
                            .collect();
                        if let Some(&(cat, field)) = if mods.shift_key() {
                            enabled.last()
                        } else {
                            enabled.first()
                        } {
                            (self.category, self.field) = (cat, field);
                        }
                        return SearchRoute::Handled;
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(delta) = delta {
            self.move_result(cats, cfg, tr, delta);
            SearchRoute::Handled
        } else {
            SearchRoute::Pass
        }
    }

    /// Stable catalogue identities, grouped in catalogue order during search.
    pub fn visible_fields(&self, cats: &[Category], tr: &Translator) -> Vec<(usize, usize)> {
        let query = search_fold(self.query.text());
        cats.iter()
            .enumerate()
            .flat_map(|(cat, category)| {
                let query = &query;
                category
                    .fields
                    .iter()
                    .enumerate()
                    .filter_map(move |(field, value)| {
                        let matched = if query.is_empty() {
                            cat == self.category
                        } else {
                            [
                                tr.text(value.label),
                                tr.text(description(value)),
                                value.key,
                                if value.key == "gpu" {
                                    "gpu-device-id gpu-vendor-id gpu-name"
                                } else {
                                    ""
                                },
                            ]
                            .iter()
                            .any(|text| search_fold(text).contains(query))
                        };
                        matched.then_some((cat, field))
                    })
            })
            .collect()
    }

    pub fn reconcile(&mut self, cats: &[Category], cfg: &Config, tr: &Translator) {
        let fields = self.visible_fields(cats, tr);
        if !fields.contains(&(self.category, self.field))
            && let Some(&(category, field)) = fields
                .iter()
                .find(|&&(c, f)| !field_disabled(cfg, cats[c].fields[f].key))
                .or(fields.first())
        {
            self.category = category;
            self.field = field;
        }
    }

    /// Pointer row indices always refer to the visible projection, including search.
    pub fn focus_result(
        &mut self,
        cats: &[Category],
        cfg: &Config,
        tr: &Translator,
        index: usize,
    ) -> Option<(usize, usize)> {
        let (category, field) = *self.visible_fields(cats, tr).get(index)?;
        if field_disabled(cfg, cats[category].fields[field].key) {
            return None;
        }
        self.category = category;
        self.field = field;
        self.capturing = false;
        self.search_focused = false;
        Some((category, field))
    }

    pub fn move_result(&mut self, cats: &[Category], cfg: &Config, tr: &Translator, delta: isize) {
        let fields: Vec<_> = self
            .visible_fields(cats, tr)
            .into_iter()
            .filter(|&(c, f)| !field_disabled(cfg, cats[c].fields[f].key))
            .collect();
        if fields.is_empty() {
            return;
        }
        let current = fields
            .iter()
            .position(|pair| *pair == (self.category, self.field))
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(fields.len() as isize) as usize;
        (self.category, self.field) = fields[next];
    }
}

/// State for the inline text prompt opened by a [`FieldKind::Text`] row (the
/// in-settings image-path entry). `key` is the config key to persist on submit;
/// `buf` is the shared grapheme-aware editor. Enter persists + reloads, Esc cancels.
#[derive(Debug, Clone)]
pub struct SettingsTextEdit {
    pub key: &'static str,
    pub(crate) buf: crate::text_editor::TextEditor,
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
                choice(
                    T::SettingsFieldAppearance,
                    "theme-mode",
                    &["auto", "light", "dark", "explicit"],
                    &[
                        Label::Text(T::SettingsValueFollowSystem),
                        Label::Text(T::SettingsValueLightAppearance),
                        Label::Text(T::SettingsValueDarkAppearance),
                        Label::Text(T::SettingsValueFixedTheme),
                    ],
                ),
                Field {
                    label: T::SettingsFieldTheme,
                    key: "theme",
                    kind: FieldKind::Choice {
                        values: kettle_config::Theme::POPULAR,
                        labels: None,
                    },
                },
                // The theme picker's Contrast button steps the same values.
                choice(
                    T::SettingsFieldMinimumContrast,
                    "minimum-contrast",
                    &["0", "3", "4.5", "7"],
                    &[
                        Label::Text(T::SettingsValueOff),
                        Label::Name(Cow::Borrowed("3")),
                        Label::Name(Cow::Borrowed("4.5")),
                        Label::Name(Cow::Borrowed("7")),
                    ],
                ),
                number(T::SettingsFieldFontSize, "font-size", 6, 72, 1, "px"),
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
                choice(
                    T::SettingsFieldCursorUnfocusedStyle,
                    "cursor-unfocused-style",
                    &["hollow", "hidden", "unchanged"],
                    &[
                        Label::Text(T::SettingsValueHollowCursor),
                        Label::Text(T::SettingsValueHiddenCursor),
                        Label::Text(T::SettingsValueUnchangedCursor),
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
        Category {
            name: T::SettingsCategoryTextRendering,
            fields: vec![
                toggle(T::SettingsFieldGraphemeClustering, "grapheme-clustering"),
                toggle(T::SettingsFieldBuiltinBoxDrawing, "builtin-box-drawing"),
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
                toggle(T::SettingsFieldCopyTrimWhitespace, "copy-trim-whitespace"),
                // When a text paste asks first. The Confirm paste dialog
                // links here.
                choice(
                    T::SettingsFieldPasteProtection,
                    "paste-protection",
                    &["auto", "always", "off"],
                    &[
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueAlways),
                        Label::Text(T::SettingsValueOff),
                    ],
                ),
                // What OSC 52 lets a program in a pane do with the clipboard.
                choice(
                    T::SettingsFieldOsc52,
                    "osc52",
                    &["off", "copy", "paste", "both"],
                    &[
                        Label::Text(T::SettingsValueOff),
                        Label::Text(T::SettingsValueCopyOnly),
                        Label::Text(T::SettingsValueReadOnly),
                        Label::Text(T::SettingsValueCopyAndRead),
                    ],
                ),
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
            name: T::SettingsCategoryAgents,
            // Display only. Agent control (`agent-server`) stays config-only:
            // it grants reading and typing, and applies at launch.
            fields: vec![
                toggle(T::SettingsFieldAgentPreviews, "agent-display"),
                toggle(
                    T::SettingsFieldAgentPreviewsClaudeCode,
                    "agent-display-claude-code",
                ),
                toggle(T::SettingsFieldAgentPreviewsCodex, "agent-display-codex"),
                toggle(T::SettingsFieldAgentPreviewsAgy, "agent-display-agy"),
                choice(
                    T::SettingsFieldAddKettleToPath,
                    "add-kettle-to-path",
                    &["auto", "off"],
                    &[
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueOff),
                    ],
                ),
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
        Category {
            name: T::SettingsCategorySessions,
            fields: vec![
                choice(
                    T::SettingsFieldRestoreSession,
                    "restore-session",
                    &["auto", "true", "false"],
                    &[
                        Label::Text(T::SettingsValueAutomatic),
                        Label::Text(T::SettingsValueOn),
                        Label::Text(T::SettingsValueOff),
                    ],
                ),
                number(
                    T::SettingsFieldRestoreScrollbackLines,
                    "restore-scrollback-lines",
                    0,
                    10_000,
                    100,
                    "",
                ),
                toggle(
                    T::SettingsFieldRestoreScrollbackRemote,
                    "restore-scrollback-remote",
                ),
            ],
        },
        Category {
            name: T::SettingsCategoryQuickTerminal,
            fields: vec![
                Field {
                    label: T::QuickTerminalHotkey,
                    key: "quick-terminal-hotkey",
                    kind: FieldKind::Hotkey,
                },
                choice(
                    T::QuickTerminalPosition,
                    "quick-terminal-position",
                    &["top", "bottom", "left", "right", "center"],
                    &[
                        Label::Text(T::SettingsValueTop),
                        Label::Text(T::SettingsValueBottom),
                        Label::Text(T::SettingsValueLeft),
                        Label::Text(T::SettingsValueRight),
                        Label::Text(T::QuickTerminalCenter),
                    ],
                ),
                number(T::QuickTerminalSize, "quick-terminal-size", 1, 100, 5, "%"),
                toggle(T::QuickTerminalAnimation, "quick-terminal-animation"),
                toggle(T::QuickTerminalAutohide, "quick-terminal-autohide"),
            ],
        },
    ]
}

fn owned_choice_index(cfg: &Config, key: &str, values: &[String]) -> Option<usize> {
    let current = read_choice(cfg, key);
    if key != "gpu" || current == "auto" {
        return values.iter().position(|value| *value == current);
    }
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let mut fields = value.splitn(3, ':');
            let vendor = u32::from_str_radix(fields.next()?, 16).ok()?;
            let device = u32::from_str_radix(fields.next()?, 16).ok()?;
            let rank = kettle_render::pin_match_rank_fields(vendor, device, fields.next()?, cfg);
            (rank != u8::MAX).then_some((rank, index))
        })
        .min()
        .map(|(_, index)| index)
}

/// Read a field's current value from `cfg`, formatted for display next to its
/// label (e.g. `"14 px"`, `"Automatic"`, `"On"`) in `tr`'s language. Returns a
/// best-effort string; an unknown key (catalogue/Config drift) yields a
/// fallback such as `"Off"`, `""`, or `"—"` rather than panicking.
pub fn read(cfg: &Config, field: &Field, tr: &Translator) -> String {
    use Text as T;
    if field.key == "theme" {
        return tr.theme_name(&cfg.theme_name).to_string();
    }
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
        FieldKind::ChoiceOwned { values, labels } => owned_choice_index(cfg, field.key, values)
            .and_then(|index| labels.get(index))
            .map(|label| label.show(tr).to_string())
            .unwrap_or_else(|| {
                if read_choice(cfg, field.key) == "auto" {
                    tr.text(T::SettingsValueAutomatic).to_string()
                } else {
                    let identity = if cfg.gpu_name.trim().is_empty() {
                        format!("{:04x}:{:04x}", cfg.gpu_vendor_id, cfg.gpu_device_id)
                    } else {
                        cfg.gpu_name.clone()
                    };
                    tr.settings_value_not_detected(&identity)
                }
            }),
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
        FieldKind::Hotkey => cfg
            .quick_terminal_hotkey
            .map(|key| key.label())
            .unwrap_or_else(|| tr.text(T::SettingsValueUnbound).to_string()),
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
    use kettle_render::display_width as width;
    let mut label_cols = 0;
    let mut value_cols = width(tr.text(Text::SettingsPressChord));
    for field in cats.iter().flat_map(|cat| &cat.fields) {
        label_cols = label_cols.max(width(tr.text(field.label)));
        value_cols = value_cols.max(field_value_cols(cfg, field, tr));
    }
    (label_cols, value_cols)
}

/// Columns of the widest value `field` can show in `tr`'s language: its
/// current value, every choice, a number's limits, both switch states. The
/// renderer sizes the row's control for it, so the control keeps one width
/// while the value changes; it measures with the renderer's own
/// `display_width`.
pub fn field_value_cols(cfg: &Config, field: &Field, tr: &Translator) -> usize {
    use kettle_render::display_width as width;
    let mut cols = width(&read(cfg, field, tr));
    match &field.kind {
        FieldKind::Toggle => {
            for value in [Text::SettingsValueOn, Text::SettingsValueOff] {
                cols = cols.max(width(tr.text(value)));
            }
            // The words `row_value` shows instead while this session cannot
            // blur, or is still asking whether it can.
            if field.key == "window-blur" {
                for value in [Text::SettingsValueChecking, Text::SettingsValueUnavailable] {
                    cols = cols.max(width(tr.text(value)));
                }
            }
        }
        FieldKind::Choice { values, labels } => match labels {
            Some(labels) => {
                for label in *labels {
                    cols = cols.max(width(label.show(tr)));
                }
            }
            None => {
                for value in *values {
                    cols = cols.max(width(value));
                }
            }
        },
        FieldKind::ChoiceOwned { labels, .. } => {
            for label in labels {
                cols = cols.max(width(label.show(tr)));
            }
        }
        FieldKind::Number {
            min, max, suffix, ..
        } => {
            cols = cols
                .max(width(&format_number(*min, suffix)))
                .max(width(&format_number(*max, suffix)));
            // The words `read` shows for a sentinel instead of a quantity.
            let words: &[&str] = match field.key {
                "scrollback" => &[tr.text(Text::SettingsValueInfinite)],
                "cursor-blink-timeout" => &[tr.text(Text::SettingsValueNever)],
                "scrollback-bytes" => &[tr.text(Text::SettingsValueNoCap), "<1 MB"],
                _ => &[],
            };
            for word in words {
                cols = cols.max(width(word));
            }
        }
        _ => {}
    }
    cols
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
        // A ratio set by hand between two steps (2.5) moves to its neighbour
        // rather than wrapping from the list's start.
        FieldKind::Choice { .. } if field.key == "minimum-contrast" => {
            kettle_render::minimum_contrast_token(kettle_render::step_minimum_contrast(
                cfg.minimum_contrast,
                dir >= 0,
            ))
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
            let idx = owned_choice_index(cfg, field.key, values).unwrap_or(0) as i32;
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
        FieldKind::Hotkey | FieldKind::Keybind { .. } => String::new(),
        // Text fields don't cycle: activating opens an inline prompt in the
        // overlay handler. No-op here.
        FieldKind::Text { .. } => String::new(),
    }
}

/// The value `field` has in a config that does not set it, in the token
/// [`next_value`] writes; `None` for rows a reset does not cover: the theme
/// (its picker and light/dark following own it), the GPU pin, shortcuts and
/// free text.
pub fn default_value(field: &Field) -> Option<String> {
    let defaults = Config::default();
    match &field.kind {
        FieldKind::Toggle => Some(read_bool(&defaults, field.key).to_string()),
        FieldKind::Choice { .. } if field.key == "theme" => None,
        FieldKind::Choice { .. } => Some(read_choice(&defaults, field.key)),
        FieldKind::Number { suffix, .. } => Some(write_number(
            field.key,
            read_number(&defaults, field.key),
            suffix,
        )),
        FieldKind::ChoiceOwned { .. }
        | FieldKind::Hotkey
        | FieldKind::Keybind { .. }
        | FieldKind::Text { .. } => None,
    }
}

/// Whether `field` already holds its [`default_value`] in `cfg`.
pub fn holds_default(cfg: &Config, field: &Field) -> bool {
    match &field.kind {
        FieldKind::Toggle => read_bool(cfg, field.key) == read_bool(&Config::default(), field.key),
        FieldKind::Choice { .. } => {
            read_choice(cfg, field.key) == read_choice(&Config::default(), field.key)
        }
        FieldKind::Number { .. } => {
            read_number(cfg, field.key) == read_number(&Config::default(), field.key)
        }
        _ => true,
    }
}

/// Is this field a rebindable keybinding? The overlay handler uses
/// this to route Enter/Space into chord-capture instead of the value-cycle path.
pub fn is_keybind(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Hotkey | FieldKind::Keybind { .. })
}

/// The canonical action token for a keybind field (for capture +
/// persistence). `None` for non-keybind fields.
pub fn keybind_action(field: &Field) -> Option<&'static str> {
    match field.kind {
        FieldKind::Keybind { action } => Some(action),
        _ => None,
    }
}

/// A safe localized label for confirmation copy. Payloads and raw action names
/// must never enter a prompt through the debug or CLI action formatter.
pub(crate) fn confirmation_action_label(action: &kettle_config::Action, tr: &Translator) -> String {
    use kettle_config::Action;
    let key = match action {
        Action::SplitRight => Text::SettingsKeybindSplitRight,
        Action::SplitDown => Text::SettingsKeybindSplitDown,
        Action::ClosePane => Text::SettingsKeybindClosePane,
        Action::NewTab => Text::SettingsKeybindNewTab,
        Action::NextTab => Text::SettingsKeybindNextTab,
        Action::PrevTab => Text::SettingsKeybindPreviousTab,
        Action::StartSearch => Text::SettingsKeybindStartSearch,
        Action::CommandPalette => Text::SettingsKeybindCommandPalette,
        Action::OpenSettings => Text::SettingsKeybindOpenSettings,
        Action::ToggleZoom => Text::SettingsKeybindToggleZoom,
        Action::Copy => Text::SettingsKeybindCopy,
        Action::Paste => Text::SettingsKeybindPaste,
        Action::CloseTab => Text::PaletteCloseTab,
        Action::CloseWindow => Text::PaletteCloseWindow,
        Action::NewWindow => Text::PaletteNewWindow,
        Action::SendText(_) => Text::ConfirmActionSendText,
        _ => Text::ConfirmActionCustom,
    };
    tr.text(key).to_string()
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

pub(crate) fn read_bool(cfg: &Config, key: &str) -> bool {
    match key {
        "quick-terminal-animation" => cfg.quick_terminal_animation,
        "quick-terminal-autohide" => cfg.quick_terminal_autohide,
        "grapheme-clustering" => cfg.grapheme_clustering,
        "builtin-box-drawing" => cfg.builtin_box_drawing,
        "cursor-blink" => cfg.cursor_blink,
        "show-titlebar" => cfg.show_titlebar,
        "window-blur" => cfg.window_blur,
        "restore-scrollback-remote" => cfg.restore_scrollback_remote,
        "copy-on-select" => cfg.copy_on_select,
        "copy-trim-whitespace" => cfg.copy_trim_whitespace,
        "mouse-hide-while-typing" => cfg.mouse_hide_while_typing,
        "vim-menu-nav" => cfg.vim_menu_nav,
        "gpu-force-software" => cfg.gpu_force_software,
        "close-button-on-tab" => cfg.close_button_on_tab,
        "scroll-tabbar" => cfg.scroll_tabbar,
        "detachable-tabs" => cfg.detachable_tabs,
        "search-wrap" => cfg.search_wrap,
        "invert-search" => cfg.invert_search,
        "agent-display" => cfg.agent_display,
        "agent-display-claude-code" => cfg.agent_display_claude_code,
        "agent-display-codex" => cfg.agent_display_codex,
        "agent-display-agy" => cfg.agent_display_agy,
        _ => false,
    }
}

fn read_choice(cfg: &Config, key: &str) -> String {
    match key {
        "quick-terminal-position" => cfg.quick_terminal_position.token().to_string(),
        "language" => cfg.language.token().to_string(),
        "scrollbar" => match cfg.scrollbar {
            ScrollbarMode::Never => "never",
            ScrollbarMode::Auto => "auto",
            ScrollbarMode::Always => "always",
        }
        .to_string(),
        "restore-session" => cfg.restore_session.token().to_string(),
        "paste-protection" => match cfg.paste_protection {
            kettle_config::PasteProtection::Auto => "auto",
            kettle_config::PasteProtection::Always => "always",
            kettle_config::PasteProtection::Off => "off",
        }
        .to_string(),
        "osc52" => match cfg.osc52 {
            kettle_config::Osc52::Off => "off",
            kettle_config::Osc52::Copy => "copy",
            kettle_config::Osc52::Paste => "paste",
            kettle_config::Osc52::Both => "both",
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
        "cursor-unfocused-style" => match cfg.cursor_unfocused_style {
            CursorUnfocusedStyle::Hollow => "hollow",
            CursorUnfocusedStyle::Hidden => "hidden",
            CursorUnfocusedStyle::Unchanged => "unchanged",
        }
        .to_string(),
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
        "add-kettle-to-path" => match cfg.add_kettle_to_path {
            kettle_config::AddKettleToPath::Auto => "auto",
            kettle_config::AddKettleToPath::Off => "off",
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
        "minimum-contrast" => kettle_render::minimum_contrast_token(cfg.minimum_contrast),
        "theme-mode" => match cfg.theme_mode {
            kettle_config::ThemeMode::Auto => "auto",
            kettle_config::ThemeMode::Light => "light",
            kettle_config::ThemeMode::Dark => "dark",
            kettle_config::ThemeMode::Explicit => "explicit",
        }
        .to_string(),
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
        "restore-scrollback-lines" => cfg.restore_scrollback_lines as i64,
        "quick-terminal-size" => i64::from(cfg.quick_terminal_size),
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
    let value = match key {
        "font-size" => value.clamp(5, 72),
        "background-opacity" => value.clamp(0, 100),
        "window-padding-x" => value.max(0),
        "restore-scrollback-lines" => value.clamp(0, 10_000),
        "scrollback" => value.clamp(0, kettle_config::INFINITE_SCROLLBACK as i64),
        "scrollback-bytes" => {
            value.clamp(0, (kettle_config::MAX_SCROLLBACK_BYTES / 1_000_000) as i64)
        }
        "tab-min-width" => value.clamp(40, 600),
        "scrollbar-width" => value.clamp(2, 40),
        "update-check-interval-hours" => value.clamp(1, u32::MAX as i64),
        "cursor-blink-timeout" => value.clamp(0, 3600),
        _ => value,
    };
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
    fn cursor_and_copy_settings_round_trip_and_explain_selection_gestures() {
        let cats = categories(&[]);
        for key in [
            "cursor-style",
            "cursor-blink",
            "cursor-unfocused-style",
            "copy-trim-whitespace",
        ] {
            let field = cats
                .iter()
                .flat_map(|c| &c.fields)
                .find(|f| f.key == key)
                .unwrap();
            let cfg = Config::default();
            let value = next_value(&cfg, field, 1);
            let edited = Config::parse_text(&format!("{key} = {value}"));
            assert!(Config::detect_malformed_values(&format!("{key} = {value}")).is_empty());
            assert_ne!(read(&cfg, field, &EN), read(&edited, field, &EN));
            assert_ne!(EN.text(field.label), ES.text(field.label));
            assert_ne!(EN.text(description(field)), ES.text(description(field)));
        }
        let field = cats
            .iter()
            .flat_map(|c| &c.fields)
            .find(|f| f.key == "copy-on-select")
            .unwrap();
        for tr in [EN, ES] {
            let help = tr.text(description(field));
            assert!(help.contains("Option/Alt") && help.contains("Shift"));
        }
    }

    #[test]
    fn paste_protection_and_clipboard_access_are_settings_rows() {
        let cats = categories(&[]);
        let behavior = cats
            .iter()
            .find(|cat| cat.name == Text::SettingsCategoryBehavior)
            .expect("a Behavior category");
        let row = |key: &str| {
            behavior
                .fields
                .iter()
                .position(|field| field.key == key)
                .unwrap_or_else(|| panic!("{key} is not a Behavior row"))
        };
        // Beside the copy rows, in the order a paste meets them.
        assert_eq!(row("paste-protection"), row("copy-trim-whitespace") + 1);
        assert_eq!(row("osc52"), row("paste-protection") + 1);
        for (key, label, values, shown) in [
            (
                "paste-protection",
                "Paste protection",
                &["auto", "always", "off"][..],
                &["Automatic", "Always", "Off"][..],
            ),
            (
                "osc52",
                "Clipboard access by programs",
                &["off", "copy", "paste", "both"][..],
                &["Off", "Copy only", "Read only", "Copy and read"][..],
            ),
        ] {
            let field = &behavior.fields[row(key)];
            assert_eq!(EN.text(field.label), label);
            let FieldKind::Choice {
                values: tokens,
                labels: Some(labels),
            } = &field.kind
            else {
                panic!("{key} is a labelled choice")
            };
            assert_eq!(*tokens, values);
            assert_eq!(
                labels.iter().map(|l| l.show(&EN)).collect::<Vec<_>>(),
                shown
            );
            // Every value is one the config accepts and reads back, and the
            // row cycles through all of them and round again.
            let mut cfg = Config::default();
            let start = read_choice(&cfg, key);
            assert_eq!(start, if key == "osc52" { "copy" } else { "auto" });
            let mut seen = vec![start.clone()];
            for _ in 0..values.len() {
                let next = next_value(&cfg, field, 1);
                let line = format!("{key} = {next}");
                assert!(Config::detect_malformed_values(&line).is_empty(), "{line}");
                cfg = Config::parse_text(&line);
                assert_eq!(read_choice(&cfg, key), next);
                assert_eq!(read(&cfg, field, &EN), {
                    let at = values.iter().position(|v| *v == next).unwrap();
                    shown[at]
                });
                seen.push(next);
            }
            assert_eq!(seen.first(), seen.last());
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), values.len());
            // A description in each language, and Spanish labels of its own.
            for tr in [EN, ES] {
                assert!(!tr.text(description(field)).is_empty());
            }
            assert_ne!(EN.text(field.label), ES.text(field.label));
            assert_ne!(EN.text(description(field)), ES.text(description(field)));
        }
        assert_eq!(
            ES.text(Text::SettingsFieldPasteProtection),
            "Protección al pegar"
        );
        assert!(
            ES.text(Text::SettingsFieldOsc52)
                .ends_with(" al portapapeles")
        );
        // The config values mean what the labels say.
        let osc52 = |value: &str| Config::parse_text(&format!("osc52 = {value}")).osc52;
        assert!(osc52("copy").can_copy() && !osc52("copy").can_paste());
        assert!(!osc52("paste").can_copy() && osc52("paste").can_paste());
        assert!(osc52("both").can_copy() && osc52("both").can_paste());
        assert!(!osc52("off").can_copy() && !osc52("off").can_paste());
    }

    #[test]
    fn restore_session_settings_has_three_choices() {
        let field = categories(&[])
            .into_iter()
            .flat_map(|c| c.fields)
            .find(|f| f.key == "restore-session")
            .expect("restore session in Settings");
        let FieldKind::Choice { values, .. } = &field.kind else {
            panic!("restore is a choice")
        };
        assert_eq!(*values, &["auto", "true", "false"]);
        for (value, next) in [("auto", "true"), ("true", "false"), ("false", "auto")] {
            let cfg = Config::parse_text(&format!("restore-session = {value}"));
            assert_eq!(read_choice(&cfg, field.key), value);
            assert_eq!(next_value(&cfg, &field, 1), next);
        }
        assert_ne!(EN.text(description(&field)), ES.text(description(&field)));
    }

    #[test]
    fn c08_grapheme_setting_round_trips_in_both_languages() {
        let cats = categories(&[]);
        let cat = cats
            .iter()
            .find(|cat| cat.fields.iter().any(|f| f.key == "grapheme-clustering"))
            .expect("Text/Rendering group must expose grapheme clustering");
        let field = cat
            .fields
            .iter()
            .find(|f| f.key == "grapheme-clustering")
            .unwrap();
        assert_eq!(EN.text(cat.name), "Text / Rendering");
        assert!(!ES.text(description(field)).is_empty());
        assert_ne!(EN.text(description(field)), ES.text(description(field)));
        let cfg = Config::default();
        assert!(read_bool(&cfg, field.key));
        assert_eq!(next_value(&cfg, field, 1), "false");
        let off = Config::parse_text("grapheme-clustering = false");
        assert!(!read_bool(&off, field.key));
        assert_eq!(next_value(&off, field, 1), "true");
    }

    #[test]
    fn builtin_box_drawing_setting_round_trips_in_both_languages() {
        let cats = categories(&[]);
        let cat = cats
            .iter()
            .find(|cat| cat.name == Text::SettingsCategoryTextRendering)
            .expect("Text / Rendering group");
        let field = cat
            .fields
            .iter()
            .find(|f| f.key == "builtin-box-drawing")
            .expect("Text / Rendering exposes built-in box drawing");
        assert!(matches!(field.kind, FieldKind::Toggle));
        assert_eq!(EN.text(field.label), "Built-in box drawing");
        assert_ne!(EN.text(field.label), ES.text(field.label));
        assert_ne!(EN.text(description(field)), ES.text(description(field)));
        let cfg = Config::default();
        assert!(read_bool(&cfg, field.key));
        assert!(holds_default(&cfg, field));
        assert_eq!(next_value(&cfg, field, 1), "false");
        let off = Config::parse_text("builtin-box-drawing = false");
        assert!(!read_bool(&off, field.key));
        assert!(!holds_default(&off, field));
        assert_eq!(next_value(&off, field, 1), "true");
    }

    /// Every row a reset covers writes a token the config accepts, and that
    /// token reads back as the default: after an edit the row is no longer at
    /// its default, and writing the default value brings it back.
    #[test]
    fn reset_writes_each_rows_default_back() {
        for field in categories(&[]).iter().flat_map(|c| &c.fields) {
            let Some(default) = default_value(field) else {
                continue;
            };
            let line = format!("{} = {default}", field.key);
            assert!(Config::detect_malformed_values(&line).is_empty(), "{line}");
            let restored = Config::parse_text(&line);
            assert!(holds_default(&restored, field), "{line}");
            assert_eq!(
                read(&restored, field, &EN),
                read(&Config::default(), field, &EN),
                "{line}"
            );
            let edited = Config::parse_text(&format!(
                "{} = {}",
                field.key,
                next_value(&Config::default(), field, 1)
            ));
            if read(&edited, field, &EN) != read(&Config::default(), field, &EN) {
                assert!(!holds_default(&edited, field), "{}", field.key);
            }
        }
        let theme = categories(&[])
            .into_iter()
            .flat_map(|c| c.fields)
            .find(|f| f.key == "theme")
            .unwrap();
        assert_eq!(default_value(&theme), None);
    }

    #[test]
    fn default_scrollback_memory_is_64_mb() {
        let cfg = Config::default();
        assert_eq!(cfg.scrollback, 10_000);
        assert_eq!(cfg.scrollback_bytes, 64_000_000);
        let cats = categories(&[]);
        let field = cats
            .iter()
            .flat_map(|cat| &cat.fields)
            .find(|field| field.key == "scrollback-bytes")
            .unwrap();
        assert_eq!(read(&cfg, field, &EN), "64 MB");
    }

    #[test]
    fn quick_terminal_settings_round_trip_and_capture_global_chords() {
        use kettle_config::keybinds::{Key, Mods};
        let cats = categories(&[]);
        let group = cats
            .iter()
            .find(|cat| cat.name == Text::SettingsCategoryQuickTerminal)
            .unwrap();
        assert_eq!(group.fields.len(), 5);
        assert!(is_keybind(&group.fields[0]));
        assert_eq!(
            read(&Config::default(), &group.fields[0], &EN),
            EN.text(Text::SettingsValueUnbound)
        );
        assert_eq!(read(&Config::default(), &group.fields[2], &EN), "40%");
        let cfg = Config::parse_text(
            "quick-terminal-hotkey = cmd+escape\nquick-terminal-position = center\nquick-terminal-size = 70",
        );
        assert_eq!(
            read(&cfg, &group.fields[0], &EN),
            cfg.quick_terminal_hotkey.unwrap().label()
        );
        assert_eq!(
            read(&cfg, &group.fields[1], &ES),
            ES.text(Text::QuickTerminalCenter)
        );
        assert_eq!(read(&cfg, &group.fields[2], &EN), "70%");
        assert_eq!(next_value(&cfg, &group.fields[3], 0), "false");
        assert_eq!(
            capture_hotkey(Some(Key::Escape), Mods::empty()),
            HotkeyCapture::Cancel
        );
        assert_eq!(
            capture_hotkey(Some(Key::Delete), Mods::empty()),
            HotkeyCapture::Value(None)
        );
        assert_eq!(
            capture_hotkey(Some(Key::Char('x')), Mods::empty()),
            HotkeyCapture::Pending
        );
        assert_eq!(capture_hotkey(None, Mods::CTRL), HotkeyCapture::Pending);
        assert_eq!(
            capture_hotkey(Some(Key::Escape), Mods::SUPER),
            HotkeyCapture::Value(cfg.quick_terminal_hotkey)
        );
        assert!(
            group
                .fields
                .iter()
                .all(|field| EN.text(description(field)) != ES.text(description(field)))
        );
    }

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
                        // A ratio such as 4.5 has no case to give.
                        assert!(
                            label
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit()),
                            "{label}"
                        );
                        if *token == "auto" && !matches!(field.key, "update-policy" | "theme-mode")
                        {
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

    /// Agents → Agent previews and its Claude Code and Codex rows edit
    /// `agent-display`, `agent-display-claude-code` and
    /// `agent-display-codex` as ordinary toggles; its last row chooses
    /// `add-kettle-to-path`, Automatic or Off.
    #[test]
    fn agent_previews_rows_round_trip() {
        let cats = categories(&[]);
        let agents = cats
            .iter()
            .find(|cat| cat.name == Text::SettingsCategoryAgents)
            .expect("Agents category");
        assert_eq!(agents.fields.len(), 5);
        let rows = [
            (
                "agent-display",
                Text::SettingsFieldAgentPreviews,
                (|cfg: &Config| cfg.agent_display) as fn(&Config) -> bool,
            ),
            (
                "agent-display-claude-code",
                Text::SettingsFieldAgentPreviewsClaudeCode,
                |cfg: &Config| cfg.agent_display_claude_code,
            ),
            (
                "agent-display-codex",
                Text::SettingsFieldAgentPreviewsCodex,
                |cfg: &Config| cfg.agent_display_codex,
            ),
            (
                "agent-display-agy",
                Text::SettingsFieldAgentPreviewsAgy,
                |cfg: &Config| cfg.agent_display_agy,
            ),
        ];
        for (field, (key, label, value)) in agents.fields.iter().zip(rows) {
            assert_eq!(field.key, key);
            assert_eq!(field.label, label);
            assert!(matches!(field.kind, FieldKind::Toggle));
            let off = Config::default();
            assert!(!value(&off), "{key} is off by default");
            assert_eq!(read(&off, field, &EN), "Off");
            assert_eq!(next_value(&off, field, 1), "true");
            let on = Config::parse_text(&format!("{} = {}", field.key, next_value(&off, field, 1)));
            assert!(value(&on), "{key}");
            assert_eq!(read(&on, field, &EN), "On");
            assert_eq!(next_value(&on, field, 1), "false");
        }
        let path = &agents.fields[4];
        assert_eq!(path.key, "add-kettle-to-path");
        assert_eq!(path.label, Text::SettingsFieldAddKettleToPath);
        let auto = Config::default();
        assert_eq!(read(&auto, path, &EN), "Automatic");
        assert_eq!(next_value(&auto, path, 1), "off");
        let off = Config::parse_text("add-kettle-to-path = off");
        assert_eq!(read(&off, path, &EN), "Off");
        assert_eq!(next_value(&off, path, 1), "auto");
        // Agent control stays out of Settings: it grants reading and typing.
        assert!(
            cats.iter()
                .flat_map(|cat| &cat.fields)
                .all(|field| field.key != "agent-server")
        );
    }

    #[test]
    fn display_units_have_spaces_and_keep_config_serialization() {
        for (suffix, expected) in [
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
        for tr in [&EN, &ES] {
            assert_eq!(read(&cfg, field("font-size"), tr), "13 px");
        }
        assert_eq!(
            EN.text(description(field("font-size"))),
            "Set the terminal font size in logical pixels."
        );
        assert_eq!(
            ES.text(description(field("font-size"))),
            "Ajustar el tamaño de la fuente de la terminal en píxeles lógicos."
        );
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

    /// The Appearance page's minimum-contrast row steps Off, 3, 4.5, 7 and
    /// back to Off, each value a config token that reads back the same; a
    /// ratio set by hand (2.5) shows as it is and steps to its neighbour.
    #[test]
    fn minimum_contrast_row_round_trips_and_keeps_a_hand_set_ratio() {
        let cats = categories(&[]);
        let (index, field) = cats[0]
            .fields
            .iter()
            .enumerate()
            .find(|(_, field)| field.key == "minimum-contrast")
            .expect("an Appearance row");
        assert_eq!(cats[0].fields[index - 1].key, "theme", "right after Theme");
        let mut cfg = Config::default();
        let mut shown = vec![read(&cfg, field, &EN)];
        for _ in 0..4 {
            let token = next_value(&cfg, field, 1);
            cfg = Config::parse_text(&format!("minimum-contrast = {token}"));
            shown.push(read(&cfg, field, &EN));
        }
        assert_eq!(shown, ["Off", "3", "4.5", "7", "Off"]);
        assert_eq!(next_value(&Config::default(), field, -1), "7");
        let hand = Config::parse_text("minimum-contrast = 2.5");
        assert_eq!(read(&hand, field, &EN), "2.5");
        assert_eq!(read(&hand, field, &ES), "2.5");
        assert_eq!(next_value(&hand, field, 1), "3");
        assert_eq!(next_value(&hand, field, -1), "0");
        assert_eq!(
            read(&Config::default(), field, &ES),
            ES.text(Text::SettingsValueOff)
        );
        assert_eq!(default_value(field).as_deref(), Some("0"));
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
    fn stepping_outside_catalogue_ranges_respects_config_domains() {
        let cats = categories(&[]);
        for (key, config, expected) in [
            ("font-size", "font-size = 5", "5"),
            ("background-opacity", "background-opacity = 0", "0.00"),
        ] {
            let cfg = Config::parse_text(config);
            let field = cats
                .iter()
                .flat_map(|cat| &cat.fields)
                .find(|field| field.key == key)
                .unwrap();
            let value = next_value(&cfg, field, -1);
            assert_eq!(
                value, expected,
                "Settings must respect the parser's lower bound for {key}"
            );
            assert!(Config::detect_malformed_values(&format!("{key} = {value}")).is_empty());
        }
    }

    #[test]
    fn gpu_labels_resolve_id_only_and_name_only_pins() {
        let gpus = [(
            "10de:2191:NVIDIA GeForce GTX 1660 Ti".into(),
            "NVIDIA GeForce GTX 1660 Ti (Discrete)".into(),
        )];
        let cats = categories(&gpus);
        let field = cats
            .iter()
            .flat_map(|cat| &cat.fields)
            .find(|field| field.key == "gpu")
            .unwrap();
        for cfg in [
            Config {
                gpu_vendor_id: 0x10de,
                gpu_device_id: 0x2191,
                ..Config::default()
            },
            Config {
                gpu_name: "geforce gtx".into(),
                ..Config::default()
            },
        ] {
            assert_eq!(
                read(&cfg, field, &EN),
                "NVIDIA GeForce GTX 1660 Ti (Discrete)",
                "a detected pin must display the resolved adapter"
            );
            assert_eq!(next_value(&cfg, field, 1), "auto");
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
        // The shortest label wins: macOS spells Super as Cmd.
        let stable = if cfg!(target_os = "macos") {
            "Shift+Cmd+D"
        } else {
            "Ctrl+Shift+W"
        };
        assert_eq!(shown.into_iter().collect::<Vec<_>>(), [stable]);
    }

    #[test]
    fn confirmation_action_labels_localize_every_settings_action() {
        for tr in [EN, Translator::new(Language::Es)] {
            for field in categories(&[]).iter().flat_map(|category| &category.fields) {
                if let Some(token) = keybind_action(field) {
                    let action = kettle_config::Action::from_name(token).unwrap();
                    assert_eq!(
                        confirmation_action_label(&action, &tr),
                        tr.text(field.label)
                    );
                }
            }
            assert_eq!(
                confirmation_action_label(&kettle_config::Action::CloseWindow, &tr),
                tr.text(Text::PaletteCloseWindow)
            );
        }
        assert_eq!(
            confirmation_action_label(&kettle_config::Action::Copy, &Translator::new(Language::Es)),
            "Copiar"
        );
    }

    #[test]
    fn confirmation_action_labels_never_reveal_send_text_payloads() {
        for payload in [
            "private-token-123".to_string(),
            "\x1b[31msecret\n\r\0".to_string(),
            "huge-private-payload".repeat(100_000),
        ] {
            let action = kettle_config::Action::SendText(payload);
            for tr in [EN, Translator::new(Language::Es)] {
                assert_eq!(
                    confirmation_action_label(&action, &tr),
                    tr.text(Text::ConfirmActionSendText)
                );
            }
        }
    }

    #[test]
    fn confirmation_action_labels_use_safe_localized_fallbacks() {
        for tr in [EN, Translator::new(Language::Es)] {
            for action in [
                kettle_config::Action::GotoTab(255),
                kettle_config::Action::NewTabShell(255),
            ] {
                assert_eq!(
                    confirmation_action_label(&action, &tr),
                    tr.text(Text::ConfirmActionCustom)
                );
            }
        }
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
                "px"
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

    /// Rows fit the default window; category paging keeps every active
    /// category visible and pointer-accessible in all supported languages.
    #[cfg(debug_assertions)]
    #[test]
    fn the_panel_fits_a_default_window_in_every_language() {
        let cats = categories(&[]);
        let cfg = Config::default();
        for tr in [
            Translator::new(Language::En),
            Translator::new(Language::Es),
            Translator::pseudo(),
        ] {
            let (label_cols, value_cols) = column_widths(&cfg, &cats, &tr);
            let columns = 2 + label_cols + 2 + value_cols;
            assert!(
                columns <= 144,
                "{:?}: rows need {columns} columns",
                tr.language()
            );
            for active in 0..cats.len() {
                let set = kettle_render::SettingsOverlay {
                    categories: cats.iter().map(|cat| tr.text(cat.name).into()).collect(),
                    active_category: active,
                    ..Default::default()
                };
                let layout =
                    kettle_render::settings_layout(&set, (8.0, 16.0), (1152.0, 720.0), 8.0);
                let rect = layout.category_rects[active].expect("selected category stays visible");
                assert_eq!(
                    layout.pointer_hit((rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0), None),
                    kettle_render::SettingsHit::Category(active)
                );
            }
        }
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
        let f = number(Text::SettingsFieldFontSize, "font-size", 6, 72, 1, "px");
        assert_eq!(next_value(&cfg, &f, 1), "15");
        assert_eq!(next_value(&cfg, &f, -1), "13");
        assert_eq!(read(&cfg, &f, &EN), "14 px");
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
    #[test]
    fn builtin_theme_settings_display_keeps_configuration_identifiers() {
        let field = categories(&[])
            .into_iter()
            .flat_map(|category| category.fields)
            .find(|field| field.key == "theme")
            .unwrap();
        for (identifier, label) in [
            ("Builtin Dark", "Oscuro integrado"),
            ("Builtin Light", "Claro integrado"),
        ] {
            let cfg = Config {
                theme_name: identifier.to_string(),
                ..Config::default()
            };
            assert_eq!(read(&cfg, &field, &ES), label);
            assert_eq!(read_choice(&cfg, "theme"), identifier);
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use kettle_i18n::Language;
    use winit::keyboard::{Key, ModifiersState as Mods, NamedKey};
    const EN: Translator = Translator::new(Language::En);
    const ES: Translator = Translator::new(Language::Es);

    fn matches(query: &str, tr: &Translator) -> Vec<(usize, &'static str)> {
        let cats = categories(&[]);
        let nav = SettingsNav {
            query: query.into(),
            ..Default::default()
        };
        nav.visible_fields(&cats, tr)
            .into_iter()
            .map(|(c, f)| (c, cats[c].fields[f].key))
            .collect()
    }

    #[test]
    fn settings_search_matches_labels_descriptions_keys_and_spanish_accents() {
        assert!(
            matches("FONT SIZE", &EN)
                .iter()
                .any(|(_, key)| *key == "font-size")
        );
        assert_eq!(matches("idle seconds", &EN), [(0, "cursor-blink-timeout")]);
        let graphics = categories(&[])
            .iter()
            .position(|cat| cat.name == Text::SettingsCategoryGraphics)
            .unwrap();
        assert_eq!(
            matches("gpu-force-software", &EN),
            [(graphics, "gpu-force-software")]
        );
        assert_eq!(matches("gpu-device-id", &EN), [(graphics, "gpu")]);
        assert_eq!(matches("OPACIDAD", &ES), [(0, "background-opacity")]);
        assert_eq!(matches("tamano de la fuente", &ES), [(0, "font-size")]);
        assert_eq!(
            matches("tamaño de la fuente", &ES),
            matches("taman\u{303}o de la fuente", &ES)
        );
        assert!(matches("no-setting-could-match-this", &EN).is_empty());
    }

    /// Search finds the paste and clipboard rows by what a user would type,
    /// in both languages, and by their config keys.
    #[test]
    fn settings_search_finds_paste_protection_and_clipboard_access() {
        for (tr, query, key) in [
            (EN, "paste", "paste-protection"),
            (ES, "pegar", "paste-protection"),
            (EN, "clipboard", "osc52"),
            (ES, "portapapeles", "osc52"),
            (EN, "osc 52", "osc52"),
            (EN, "paste-protection", "paste-protection"),
        ] {
            assert!(
                matches(query, &tr).iter().any(|(_, found)| *found == key),
                "searching {query:?} does not find {key}"
            );
        }
    }

    #[test]
    fn settings_search_groups_across_categories_in_catalogue_order() {
        let results = matches("terminal", &EN);
        assert!(results.iter().any(|(cat, _)| *cat == 0));
        assert!(results.iter().any(|(cat, _)| *cat == 3));
        assert!(results.windows(2).all(|w| w[0].0 <= w[1].0));
        let cats = categories(&[]);
        let nav = SettingsNav {
            category: 4,
            ..Default::default()
        };
        assert_eq!(nav.visible_fields(&cats, &EN).len(), cats[4].fields.len());
        assert!(
            nav.visible_fields(&cats, &EN)
                .iter()
                .all(|(cat, _)| *cat == 4)
        );
    }

    #[test]
    fn settings_search_keyboard_focus_results_and_two_escapes() {
        let cats = categories(&[]);
        let cfg = Config::default();
        for (macos, chord, other) in [
            (true, Mods::SUPER, Mods::CONTROL),
            (false, Mods::CONTROL, Mods::SUPER),
        ] {
            let mut nav = SettingsNav::default();
            assert_eq!(
                nav.search_key(&cats, &cfg, &EN, &Key::Character("f".into()), other, macos),
                SearchRoute::Pass
            );
            assert_eq!(
                nav.search_key(&cats, &cfg, &EN, &Key::Character("f".into()), chord, macos),
                SearchRoute::Handled
            );
            assert!(nav.search_focused);
            assert_eq!(
                nav.search_key(
                    &cats,
                    &cfg,
                    &EN,
                    &Key::Character("j".into()),
                    Mods::empty(),
                    macos
                ),
                SearchRoute::Edit
            );
            nav.query = "terminal".into();
            nav.reconcile(&cats, &cfg, &EN);
            let first = (nav.category, nav.field);
            assert_eq!(
                nav.search_key(
                    &cats,
                    &cfg,
                    &EN,
                    &Key::Named(NamedKey::ArrowDown),
                    Mods::empty(),
                    macos
                ),
                SearchRoute::Handled
            );
            assert!(!nav.search_focused);
            nav.search_key(
                &cats,
                &cfg,
                &EN,
                &Key::Named(NamedKey::ArrowDown),
                Mods::empty(),
                macos,
            );
            assert_ne!((nav.category, nav.field), first);
            assert_eq!(
                nav.search_key(
                    &cats,
                    &cfg,
                    &EN,
                    &Key::Named(NamedKey::Escape),
                    Mods::empty(),
                    macos
                ),
                SearchRoute::Handled
            );
            assert!(nav.query.is_empty());
            assert_eq!(
                nav.search_key(
                    &cats,
                    &cfg,
                    &EN,
                    &Key::Named(NamedKey::Escape),
                    Mods::empty(),
                    macos
                ),
                SearchRoute::Pass,
                "the normal Settings handler closes on the second Escape"
            );
        }
        let mut nav = SettingsNav::default();
        nav.search_key(
            &cats,
            &cfg,
            &EN,
            &Key::Character("/".into()),
            Mods::empty(),
            false,
        );
        assert!(nav.search_focused);
        nav.search_key(
            &cats,
            &cfg,
            &EN,
            &Key::Named(NamedKey::Enter),
            Mods::empty(),
            false,
        );
        assert!(!nav.search_focused);
        nav.search_focused = false;
        nav.search_key(
            &cats,
            &cfg,
            &EN,
            &Key::Character("/".into()),
            Mods::SHIFT,
            false,
        );
        assert!(
            nav.search_focused,
            "slash can require Shift on a keyboard layout"
        );
        nav.search_focused = false;
        let cfg = Config {
            vim_menu_nav: false,
            ..cfg
        };
        assert_eq!(
            nav.search_key(
                &cats,
                &cfg,
                &EN,
                &Key::Character("/".into()),
                Mods::empty(),
                false
            ),
            SearchRoute::Pass
        );
    }

    #[test]
    fn settings_hits_focus_search_categories_and_filtered_chips() {
        let cats = categories(&[]);
        let cfg = Config::default();
        let mut nav = SettingsNav::default();
        nav.apply_hit(&cats, &cfg, &EN, kettle_render::SettingsHit::Search);
        assert!(nav.search_focused);
        nav.query = "gpu".into();
        let fields = nav.visible_fields(&cats, &EN);
        let index = fields
            .iter()
            .position(|&(c, f)| cats[c].fields[f].key == "gpu-backend")
            .unwrap();
        let target = nav
            .apply_hit(&cats, &cfg, &EN, kettle_render::SettingsHit::Field(index))
            .unwrap();
        assert_eq!(cats[target.0].fields[target.1].key, "gpu-backend");
        assert_eq!((nav.category, nav.field), target);
        assert!(!nav.search_focused);
        let background = cats
            .iter()
            .position(|cat| cat.name == Text::SettingsCategoryBackground)
            .unwrap();
        nav.apply_hit(
            &cats,
            &cfg,
            &EN,
            kettle_render::SettingsHit::Category(background),
        );
        assert_eq!(nav.category, background);
        assert!(nav.query.is_empty());
        let image = cats[background]
            .fields
            .iter()
            .position(|f| f.key == "background-image")
            .unwrap();
        assert!(
            nav.apply_hit(&cats, &cfg, &EN, kettle_render::SettingsHit::Field(image))
                .is_none()
        );
        assert_ne!(nav.field, image, "disabled rows cannot take focus");
    }

    #[test]
    fn settings_pointer_paging_updates_navigation_until_both_ends_are_reachable() {
        let cats = categories(&[]);
        let cfg = Config::default();
        for wheel in [false, true] {
            let mut nav = SettingsNav::default();
            let mut set = kettle_render::SettingsOverlay {
                categories: cats.iter().map(|cat| EN.text(cat.name).into()).collect(),
                ..Default::default()
            };
            for forward in [true, false] {
                for _ in 0..cats.len() {
                    set.active_category = nav.category;
                    let layout =
                        kettle_render::settings_layout(&set, (8.0, 16.0), (800.0, 600.0), 8.0);
                    let arrow = if forward {
                        layout.category_next
                    } else {
                        layout.category_previous
                    };
                    let Some((target, rect)) = arrow else {
                        break;
                    };
                    let hit = layout.pointer_hit(
                        (rect.0 + 1.0, rect.1 + 1.0),
                        wheel.then_some(if forward { -1 } else { 1 }),
                    );
                    nav.apply_hit(&cats, &cfg, &EN, hit);
                    assert_eq!(nav.category, target);
                    assert!(nav.field < cats[target].fields.len());
                    set.active_category = nav.category;
                    assert!(
                        kettle_render::settings_layout(&set, (8.0, 16.0), (800.0, 600.0), 8.0)
                            .category_rects[target]
                            .is_some()
                    );
                }
                let layout = kettle_render::settings_layout(&set, (8.0, 16.0), (800.0, 600.0), 8.0);
                let end = if forward { cats.len() - 1 } else { 0 };
                assert!(layout.category_rects[end].is_some());
            }
        }
    }

    #[test]
    fn settings_every_field_has_short_english_and_spanish_descriptions() {
        for cat in categories(&[]) {
            for field in cat.fields {
                let key = description(&field);
                let en = EN.text(key);
                let es = ES.text(key);
                assert!(!en.is_empty() && !es.is_empty(), "{}", field.key);
                assert!(en.chars().count() <= 70, "{}: {en}", field.key);
                assert!(
                    kettle_render::display_width(es) <= 76,
                    "{}: {es}",
                    field.key
                );
                assert_ne!(en, es, "{}", field.key);
            }
        }
    }

    /// Some sessions cannot blur a window: GNOME, X11 (KDE Plasma's
    /// included) and Windows without its backdrop. The Window blur switch
    /// then reads Unavailable instead of On, and its description says why
    /// and names the saved setting, so neither the switch nor the screen
    /// reader claims an effect the window does not show. While a Wayland
    /// compositor is still being asked it reads Checking, never On first.
    /// Every other row keeps its own value and description.
    #[test]
    fn unavailable_blur_is_explained_beside_its_switch() {
        let cats = categories(&[]);
        let fields: Vec<&Field> = cats.iter().flat_map(|c| &c.fields).collect();
        let blur = fields.iter().find(|f| f.key == "window-blur").unwrap();
        let cfg = Config::default();
        assert!(
            cfg.window_blur,
            "Checking and Unavailable describe a saved On"
        );
        for tr in [EN, ES] {
            assert_eq!(
                row_description(blur, BlurState::Available, &cfg, &tr),
                tr.text(description(blur))
            );
            assert_eq!(
                row_description(blur, BlurState::Checking, &cfg, &tr),
                tr.text(description(blur))
            );
            assert_eq!(
                row_value(blur, BlurState::Available, &cfg, &tr),
                tr.text(Text::SettingsValueOn)
            );
        }
        assert_eq!(
            row_description(blur, BlurState::Unavailable, &cfg, &EN),
            "This session cannot blur; saved setting: On. Opacity still applies."
        );
        for (saved, expected) in [
            (
                true,
                "Esta sesión no desenfoca (guardado: Activado); la opacidad sí se aplica.",
            ),
            (
                false,
                "Esta sesión no desenfoca (guardado: Desactivado); la opacidad sí se aplica.",
            ),
        ] {
            let cfg = Config {
                window_blur: saved,
                ..Config::default()
            };
            let note = row_description(blur, BlurState::Unavailable, &cfg, &ES);
            assert!(kettle_render::display_width(&note) <= 76, "{saved}: {note}");
            assert_eq!(note, expected);
        }
        assert!(
            row_description(blur, BlurState::Unavailable, &cfg, &EN)
                .chars()
                .count()
                <= 75
        );
        assert_eq!(
            row_value(blur, BlurState::Unavailable, &cfg, &EN),
            "Unavailable"
        );
        assert_eq!(
            row_value(blur, BlurState::Unavailable, &cfg, &ES),
            "No disponible"
        );
        assert_eq!(row_value(blur, BlurState::Checking, &cfg, &EN), "Checking…");
        assert_eq!(
            row_value(blur, BlurState::Checking, &cfg, &ES),
            "Comprobando…"
        );
        for field in fields.iter().filter(|f| f.key != "window-blur") {
            for state in [BlurState::Checking, BlurState::Unavailable] {
                assert_eq!(
                    row_description(field, state, &cfg, &EN),
                    EN.text(description(field)),
                    "{}",
                    field.key
                );
                assert_eq!(
                    row_value(field, state, &cfg, &EN),
                    read(&cfg, field, &EN),
                    "{}",
                    field.key
                );
            }
        }
    }

    /// Reset covers a row only where `default_value` does and the row
    /// applies now; it can act only where the value differs from the
    /// default, and it names that default as the row shows values.
    #[test]
    fn reset_state_follows_the_rows_default_and_applicability() {
        let cats = categories(&[]);
        let field = |key: &str| {
            cats.iter()
                .flat_map(|c| &c.fields)
                .find(|f| f.key == key)
                .unwrap()
                .clone()
        };
        let font = field("font-size");
        let mut cfg = Config::default();
        assert_eq!(reset_state(&cfg, &font), Some(false));
        cfg.font_size += 2.0;
        assert_eq!(reset_state(&cfg, &font), Some(true));
        assert_eq!(
            default_display(&font, &EN).as_deref(),
            Some(read(&Config::default(), &font, &EN).as_str())
        );
        assert_eq!(reset_state(&cfg, &field("theme")), None);
        assert_eq!(default_display(&field("theme"), &EN), None);
        // The blink timeout does not apply while the cursor does not blink.
        let timeout = field("cursor-blink-timeout");
        cfg.cursor_blink_timeout += 5;
        assert_eq!(reset_state(&cfg, &timeout), Some(true));
        cfg.cursor_blink = false;
        assert_eq!(reset_state(&cfg, &timeout), None);
    }

    /// Only a primary click on Reset resets. A secondary click or a wheel
    /// notch over the chip acts on its row as anywhere else on it.
    #[test]
    fn only_a_primary_click_on_reset_resets() {
        assert_eq!(reset_pointer(true, 1), ResetPointer::Reset);
        assert_eq!(reset_pointer(true, -1), ResetPointer::Row);
        assert_eq!(reset_pointer(false, 1), ResetPointer::Row);
        assert_eq!(reset_pointer(false, -1), ResetPointer::Row);
    }

    /// A row's control is sized for the widest value it can show in the
    /// UI's language, so stepping it never resizes it.
    #[test]
    fn value_columns_cover_every_value_a_row_can_show() {
        let cats = categories(&[]);
        let cfg = Config::default();
        for tr in [EN, ES] {
            for field in cats.iter().flat_map(|c| &c.fields) {
                let cols = field_value_cols(&cfg, field, &tr);
                for dir in [1, -1] {
                    let mut seen = cfg.clone();
                    for _ in 0..40 {
                        let next = next_value(&seen, field, dir);
                        if next.is_empty() {
                            break;
                        }
                        seen = Config::parse_text(&format!("{} = {next}", field.key));
                        assert!(
                            kettle_render::display_width(&read(&seen, field, &tr)) <= cols,
                            "{:?} {}: {:?} wider than {cols}",
                            tr.language(),
                            field.key,
                            read(&seen, field, &tr)
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod control_size_tests {
    use super::*;
    use kettle_i18n::Language;

    #[test]
    fn description_budget_and_capture_prompts_fit_settings_layout() {
        let cfg = Config::default();
        let cats = categories(&[]);
        let (cw, line) = (8.4_f32, 17.5_f32);
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            let prompt = tr.text(Text::SettingsPressChord);
            assert!(prompt.contains("Esc"), "{language:?}: {prompt}");
            for surface in [(1280.0_f32, 820.0_f32), (640.0, 820.0)] {
                for field in cats
                    .iter()
                    .flat_map(|cat| &cat.fields)
                    .filter(|f| is_keybind(f))
                {
                    let set = kettle_render::SettingsOverlay {
                        rows: vec![kettle_render::SettingsRow {
                            label: tr.text(field.label).into(),
                            value: prompt.into(),
                            description: tr.text(description(field)).into(),
                            control: kettle_render::SettingsControl::Value,
                            reset: reset_state(&cfg, field),
                            ..Default::default()
                        }],
                        reset_label: tr.text(Text::SettingsResetButton).into(),
                        ..Default::default()
                    };
                    let layout = kettle_render::settings_layout(&set, (cw, line), surface, cw);
                    let context = format!("{language:?} {surface:?} {}", field.key);
                    if surface.0 == 1280.0 {
                        let cols = (layout.rows[0].description_rect.2 / cw).floor() as usize;
                        assert!(cols >= 76, "{context}: description has {cols} columns");
                    }
                    let value_rect = kettle_render::settings_value_rect(
                        &set, cw, line, surface.0, surface.1, cw,
                    );
                    let cols = (value_rect.2 / cw).floor() as usize;
                    assert!(
                        kettle_render::display_width(prompt) <= cols,
                        "{context}: {prompt} in {cols} columns"
                    );
                }
            }
        }
    }

    /// Each row as the projection builds it, showing its widest value.
    fn widest_rows(cfg: &Config, tr: &Translator) -> Vec<(String, kettle_render::SettingsRow)> {
        categories(&[])
            .iter()
            .flat_map(|c| c.fields.iter())
            .filter(|f| !is_keybind(f) && !is_text(f))
            .map(|f| {
                let cols = field_value_cols(cfg, f, tr);
                let control = match f.kind {
                    FieldKind::Toggle => kettle_render::SettingsControl::Toggle(true),
                    FieldKind::Choice { .. } | FieldKind::ChoiceOwned { .. } => {
                        kettle_render::SettingsControl::Choice
                    }
                    _ => kettle_render::SettingsControl::Value,
                };
                (
                    f.key.to_string(),
                    kettle_render::SettingsRow {
                        label: tr.text(f.label).into(),
                        value: "x".repeat(cols),
                        control,
                        value_cols: cols,
                        reset: Some(true),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    /// The Window blur switch keeps one width through every state it can
    /// show (On, Off, Checking…, Unavailable) in English and Spanish, so a
    /// Wayland probe or an unavailable session never moves its edge or the
    /// Reset chip beside it.
    #[test]
    fn window_blur_keeps_one_width_through_its_states() {
        let cats = categories(&[]);
        let blur = cats
            .iter()
            .flat_map(|c| &c.fields)
            .find(|f| f.key == "window-blur")
            .unwrap();
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            for surface in [(1280.0_f32, 820.0_f32), (640.0, 820.0)] {
                let mut chips = Vec::new();
                for saved in [true, false] {
                    let cfg = Config {
                        window_blur: saved,
                        ..Config::default()
                    };
                    for state in [
                        BlurState::Available,
                        BlurState::Checking,
                        BlurState::Unavailable,
                    ] {
                        let set = kettle_render::SettingsOverlay {
                            rows: vec![kettle_render::SettingsRow {
                                label: tr.text(blur.label).into(),
                                value: row_value(blur, state, &cfg, &tr),
                                control: kettle_render::SettingsControl::Toggle(saved),
                                unavailable: state != BlurState::Available,
                                value_cols: field_value_cols(&cfg, blur, &tr),
                                reset: Some(!saved),
                                ..Default::default()
                            }],
                            reset_label: tr.text(Text::SettingsResetButton).into(),
                            ..Default::default()
                        };
                        let layout =
                            kettle_render::settings_layout(&set, (8.4, 17.5), surface, 8.4);
                        chips.push((saved, state, layout.rows[0].chip_rect));
                    }
                }
                let first = chips[0].2;
                for (saved, state, chip) in &chips {
                    assert_eq!(
                        *chip, first,
                        "{language:?} {surface:?} saved {saved} {state:?}"
                    );
                }
            }
        }
    }

    /// Every row's widest value, in English and Spanish, shows whole in the
    /// default window (1280x820 at the 14 px font's 8.4x17.5 cell). In the
    /// narrow one (640x820) it does wherever it fits beside the whole label,
    /// and elsewhere keeps about half the row. Every changed row keeps a
    /// Reset chip in both, its word or its icon.
    #[test]
    fn longest_values_stay_whole_where_they_fit() {
        let cfg = Config::default();
        let (cw, line) = (8.4_f32, 17.5_f32);
        for language in [Language::En, Language::Es] {
            let tr = Translator::new(language);
            for (surface, all_fit) in [((1280.0, 820.0), true), ((640.0, 820.0), false)] {
                for (key, row) in widest_rows(&cfg, &tr) {
                    let set = kettle_render::SettingsOverlay {
                        rows: vec![row.clone()],
                        reset_label: tr.text(Text::SettingsResetButton).into(),
                        ..Default::default()
                    };
                    let layout = kettle_render::settings_layout(&set, (cw, line), surface, cw);
                    let geometry = &layout.rows[0];
                    let chip = geometry.chip_rect;
                    let knob = match row.control {
                        kettle_render::SettingsControl::Toggle(_) => {
                            kettle_render::settings_control_height(line) - 4.0
                        }
                        _ => 0.0,
                    };
                    let arrows = if row.control == kettle_render::SettingsControl::Choice {
                        4
                    } else {
                        0
                    };
                    let need = (row.value_cols + arrows) as f32 * cw;
                    let fits = need <= chip.2 - 8.0 - knob + 0.01;
                    let inner = geometry.rect.2;
                    let label = kettle_render::display_width(&row.label) as f32 * cw;
                    let context = format!("{language:?} {surface:?} {key}");
                    if all_fit || label + 18.0 + need + 20.0 + knob <= inner {
                        assert!(fits, "{context}: {need} in {chip:?}");
                    } else {
                        assert!(chip.2 >= inner * 0.48 - 0.01, "{context}");
                    }
                    assert!(geometry.reset.is_some(), "{context}: no Reset");
                }
            }
        }
    }
}

#[cfg(test)]
mod restore_scrollback_tests {
    use super::*;

    #[test]
    fn sessions_fields_read_and_edit_output_policy() {
        let tr = Translator::new(kettle_i18n::Language::Es);
        let categories = categories(&[]);
        let sessions = categories
            .iter()
            .find(|cat| cat.name == Text::SettingsCategorySessions)
            .unwrap();
        let cfg =
            Config::parse_text("restore-scrollback-lines = 4321\nrestore-scrollback-remote = true");
        let lines = sessions
            .fields
            .iter()
            .find(|f| f.key == "restore-scrollback-lines")
            .unwrap();
        assert_eq!(read(&cfg, lines, &tr), "4321");
        assert_eq!(write_number(lines.key, -1, ""), "0");
        assert_eq!(write_number(lines.key, 20_000, ""), "10000");
        let remote = sessions
            .fields
            .iter()
            .find(|f| f.key == "restore-scrollback-remote")
            .unwrap();
        assert_eq!(read(&cfg, remote, &tr), tr.text(Text::SettingsValueOn));
        assert_eq!(sessions.fields.len(), 3);
    }

    #[test]
    fn appearance_choices_round_trip_modes_and_localize() {
        let categories = categories(&[]);
        let field = categories
            .iter()
            .flat_map(|category| &category.fields)
            .find(|field| field.key == "theme-mode")
            .unwrap();
        for (mode, next, label) in [
            (kettle_config::ThemeMode::Auto, "light", "Follow system"),
            (kettle_config::ThemeMode::Light, "dark", "Light"),
            (kettle_config::ThemeMode::Dark, "explicit", "Dark"),
            (kettle_config::ThemeMode::Explicit, "auto", "Fixed theme"),
        ] {
            let cfg = Config {
                theme_mode: mode,
                ..Config::default()
            };
            assert_eq!(read(&cfg, field, &Translator::default()), label);
            assert_eq!(next_value(&cfg, field, 1), next);
            let spanish = Translator::new(kettle_i18n::Language::Es);
            assert_ne!(read(&cfg, field, &spanish), label);
        }
    }
}
