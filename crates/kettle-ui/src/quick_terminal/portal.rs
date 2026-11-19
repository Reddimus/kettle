//! Desktop-owned Wayland shortcuts. The model and display policy are portable.
use kettle_config::keybinds::{Key, Mods, Trigger};
use kettle_i18n::{Text, Translator};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::Watch;

pub(crate) const SHORTCUT: &str = "toggle_quick_terminal";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Disabled,
    OtherOwner,
    Connecting,
    Pending,
    Registered {
        trigger: Option<String>,
        configurable: bool,
        focus_dependent: bool,
    },
    Suppressed,
    Unavailable,
    NoAppId,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Capture,
    Retry,
    Configure,
    Clear,
    None,
}

impl Status {
    pub(crate) fn action(&self, clear: bool) -> Action {
        if clear {
            return Action::Clear;
        }
        match self {
            Self::Disabled => Action::Capture,
            Self::Suppressed | Self::Failed | Self::NoAppId | Self::Unavailable => Action::Retry,
            Self::Registered {
                configurable: true, ..
            } => Action::Configure,
            _ => Action::None,
        }
    }

    pub(crate) fn note(&self, tr: &Translator, gnome: bool) -> String {
        tr.text(match self {
            Self::Disabled => Text::QuickPortalUnset,
            Self::OtherOwner => Text::QuickPortalOtherOwner,
            Self::Connecting => Text::QuickPortalConnecting,
            Self::Pending => Text::QuickPortalPending,
            Self::Registered {
                focus_dependent: true,
                ..
            } => Text::QuickPortalFocus,
            Self::Registered {
                configurable: true, ..
            } => Text::QuickPortalRegistered,
            Self::Registered { .. } if gnome => Text::QuickPortalGnome,
            Self::Registered { .. } => Text::QuickPortalDesktop,
            Self::Suppressed | Self::Failed => Text::QuickPortalRetry,
            Self::Unavailable => Text::QuickPortalUnavailable,
            Self::NoAppId => Text::QuickPortalIdentity,
        })
        .into()
    }

    pub(crate) fn value(&self, cfg: &kettle_config::Config, tr: &Translator) -> String {
        match self {
            Self::Registered { trigger, .. } => trigger
                .clone()
                .unwrap_or_else(|| tr.text(Text::QuickPortalManaged).into()),
            _ => cfg.quick_terminal_hotkey.map_or_else(
                || tr.text(Text::SettingsValueUnbound).into(),
                |trigger| trigger.label(),
            ),
        }
    }
}

pub(crate) fn settings_action(status: &Status, clear: bool, apply: impl FnOnce(Action)) -> bool {
    let action = status.action(clear);
    if action == Action::Capture {
        return false;
    }
    apply(action);
    true
}

// Shifted punctuation in Trigger may already have undergone US normalization.
// Without the original layout, even Shift+8 could have meant German Shift+plus.
pub(crate) fn preferred_trigger(trigger: Trigger) -> Option<String> {
    let key = match trigger.key {
        Key::Char(c) if trigger.mods.contains(Mods::SHIFT) && !c.is_ascii_alphabetic() => {
            return None;
        }
        Key::Char(c) if c.is_ascii_alphabetic() || c.is_ascii_digit() => {
            c.to_ascii_lowercase().to_string()
        }
        Key::Char(c) => match c {
            '`' => "grave",
            '-' => "minus",
            '=' => "equal",
            '[' => "bracketleft",
            ']' => "bracketright",
            '\\' => "backslash",
            ';' => "semicolon",
            '\'' => "apostrophe",
            ',' => "comma",
            '.' => "period",
            '/' => "slash",
            ' ' => "space",
            '+' => "plus",
            '~' => "asciitilde",
            '!' => "exclam",
            '@' => "at",
            '#' => "numbersign",
            '$' => "dollar",
            '%' => "percent",
            '^' => "asciicircum",
            '&' => "ampersand",
            '*' => "asterisk",
            '(' => "parenleft",
            ')' => "parenright",
            '_' => "underscore",
            '{' => "braceleft",
            '}' => "braceright",
            '|' => "bar",
            ':' => "colon",
            '"' => "quotedbl",
            '<' => "less",
            '>' => "greater",
            '?' => "question",
            _ => return None,
        }
        .into(),
        Key::Enter => "Return".into(),
        Key::Tab => "Tab".into(),
        Key::Escape => "Escape".into(),
        Key::Backspace => "BackSpace".into(),
        Key::Delete => "Delete".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "Page_Up".into(),
        Key::PageDown => "Page_Down".into(),
        Key::Up => "Up".into(),
        Key::Down => "Down".into(),
        Key::Left => "Left".into(),
        Key::Right => "Right".into(),
        Key::F(n @ 1..=35) => format!("F{n}"),
        Key::F(_) => return None,
        Key::Menu => "Menu".into(),
    };
    let mut parts = Vec::new();
    for (modifier, name) in [
        (Mods::CTRL, "CTRL"),
        (Mods::ALT, "ALT"),
        (Mods::SHIFT, "SHIFT"),
        (Mods::SUPER, "LOGO"),
    ] {
        if trigger.mods.contains(modifier) {
            parts.push(name.to_string());
        }
    }
    parts.push(key);
    Some(parts.join("+"))
}

pub(crate) fn sanitize(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut cols = 0;
    for c in value.chars().take(4096) {
        if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cols + width > 36 {
            result.push('\u{2026}');
            break;
        }
        cols += width;
        result.push(c);
    }
    (!result.trim().is_empty()).then_some(result)
}

pub(crate) fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control)
}

#[derive(Default)]
pub(crate) struct Machine {
    pub(crate) session: Option<String>,
    held: bool,
}

impl Machine {
    pub(crate) fn close(&mut self) {
        self.session = None;
        self.held = false;
    }

    pub(crate) fn activate(&mut self, session: &str, id: &str) -> bool {
        if self.session.as_deref() != Some(session) || id != SHORTCUT || self.held {
            return false;
        }
        self.held = true;
        true
    }

    pub(crate) fn deactivate(&mut self, session: &str, id: &str) {
        if self.session.as_deref() == Some(session) && id == SHORTCUT {
            self.held = false;
        }
    }
}

pub(crate) trait ActivationBridge {
    type Window: Clone;
    fn external(&mut self, token: &str, target: &Self::Window) -> bool;
    fn recent(&mut self, from: u64, source: Self::Window, to: u64, target: Self::Window) -> bool;
    fn without_gesture(&mut self, to: u64, target: Self::Window) -> bool;
}

pub(crate) fn activate_target<B: ActivationBridge>(
    bridge: &mut B,
    token: Option<&str>,
    previous: Option<(u64, B::Window)>,
    to: u64,
    target: B::Window,
) -> bool {
    if let Some(token) = token {
        return bridge.external(token, &target);
    }
    if let Some((from, source)) = previous
        && bridge.recent(from, source, to, target.clone())
    {
        return true;
    }
    bridge.without_gesture(to, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_preferences_preserve_layout_and_omit_ambiguous_shift() {
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::CTRL, Key::Char('+'))).as_deref(),
            Some("CTRL+plus")
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::CTRL, Key::Char('`'))).as_deref(),
            Some("CTRL+grave")
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::SUPER, Key::Escape)).as_deref(),
            Some("LOGO+Escape")
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('*'))),
            None
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('~'))),
            None
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::ALT, Key::F(12))).as_deref(),
            Some("ALT+F12")
        );
        assert_eq!(
            preferred_trigger(Trigger::new(Mods::CTRL, Key::Char('é'))),
            None
        );
    }

    #[test]
    fn portal_press_requires_release_even_after_a_long_repeat_delay() {
        let mut model = Machine {
            session: Some("current".into()),
            ..Default::default()
        };
        assert!(!model.activate("foreign", SHORTCUT));
        assert!(!model.activate("current", "foreign"));
        assert!(model.activate("current", SHORTCUT));
        // No wall-clock expiry: a repeat hours later still belongs to this hold.
        assert!(!model.activate("current", SHORTCUT));
        model.deactivate("foreign", SHORTCUT);
        assert!(!model.activate("current", SHORTCUT));
        model.deactivate("current", SHORTCUT);
        assert!(model.activate("current", SHORTCUT));
        model.close();
        assert!(!model.activate("current", SHORTCUT));
        model.session = Some("replacement".into());
        assert!(model.activate("replacement", SHORTCUT));
    }

    #[test]
    fn portal_tokenless_activation_uses_optional_serial_request_without_a_kettle_source() {
        struct Bridge(Vec<&'static str>);
        impl ActivationBridge for Bridge {
            type Window = u64;
            fn external(&mut self, _: &str, _: &u64) -> bool {
                self.0.push("external");
                true
            }
            fn recent(&mut self, _: u64, _: u64, _: u64, _: u64) -> bool {
                self.0.push("recent");
                false
            }
            fn without_gesture(&mut self, _: u64, _: u64) -> bool {
                self.0.push("optional serial");
                true
            }
        }
        let mut bridge = Bridge(Vec::new());
        assert!(activate_target(&mut bridge, None, None, 2, 2));
        assert_eq!(bridge.0, ["optional serial"]);
        bridge.0.clear();
        assert!(activate_target(&mut bridge, Some("capability"), None, 2, 2));
        assert_eq!(bridge.0, ["external"]);
        bridge.0.clear();
        assert!(activate_target(&mut bridge, None, Some((1, 1)), 2, 2));
        assert_eq!(bridge.0, ["recent", "optional serial"]);
    }

    #[test]
    fn portal_wayland_dimensions_ignore_position_and_unmaximize_below_100() {
        use crate::quick_terminal::{Rect, session_geometry, session_maximized};
        use kettle_config::QuickTerminalPosition as Position;
        let screen = Rect {
            x: -100,
            y: 0,
            width: 2000,
            height: 1000,
        };
        for position in [
            Position::Top,
            Position::Bottom,
            Position::Left,
            Position::Right,
            Position::Center,
        ] {
            let rect = session_geometry(screen, position, 40, true);
            assert_eq!((rect.width, rect.height), (800, 400));
        }
        assert_eq!(
            session_geometry(screen, Position::Top, 40, false).width,
            2000
        );
        assert_eq!(session_maximized(true, 100), Some(true));
        assert_eq!(session_maximized(true, 40), Some(false));
        assert_eq!(session_maximized(false, 40), None);
        assert!(!crate::quick_terminal::can_slide(true, || panic!(
            "Wayland must not start a slide or motion watcher"
        )));
        assert!(crate::quick_terminal::can_slide(false, || true));
    }

    #[test]
    fn portal_settings_shared_actions_and_localized_rows() {
        use crate::settings::{self, RowContext};
        let cats = settings::categories(&[]);
        let hotkey = cats
            .iter()
            .flat_map(|c| &c.fields)
            .find(|f| f.key == "quick-terminal-hotkey")
            .unwrap();
        let cfg = kettle_config::Config::default();
        let states = [
            Status::Disabled,
            Status::OtherOwner,
            Status::Connecting,
            Status::Pending,
            Status::Registered {
                trigger: None,
                configurable: false,
                focus_dependent: false,
            },
            Status::Registered {
                trigger: None,
                configurable: true,
                focus_dependent: false,
            },
            Status::Registered {
                trigger: None,
                configurable: true,
                focus_dependent: true,
            },
            Status::Suppressed,
            Status::Unavailable,
            Status::NoAppId,
            Status::Failed,
        ];
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = Translator::new(language);
            for status in &states {
                let context = RowContext {
                    blur: Default::default(),
                    quick: Some(status.clone()),
                    gnome: true,
                };
                let note = settings::row_description(hotkey, context.clone(), &cfg, &tr);
                assert!(
                    if language == kettle_i18n::Language::En {
                        note.chars().count() <= 70
                    } else {
                        kettle_render::display_width(&note) <= 76
                    },
                    "{note}"
                );
                assert_eq!(
                    settings::row_value(hotkey, context, &cfg, &tr),
                    status.value(&cfg, &tr)
                );
                assert_eq!(status.action(true), Action::Clear);
            }
            let identity = Status::NoAppId.note(&tr, false);
            assert!(!identity.contains("not installed") && !identity.contains("falta instalar"));
        }
        assert_eq!(Status::Disabled.action(false), Action::Capture);
        assert_eq!(Status::Pending.action(false), Action::None);
        assert_eq!(Status::Suppressed.action(false), Action::Retry);
        assert_eq!(
            Status::Registered {
                trigger: None,
                configurable: true,
                focus_dependent: false
            }
            .action(false),
            Action::Configure
        );
    }

    #[test]
    fn portal_tokens_are_bounded_without_logging_their_contents() {
        assert!(valid_token(&"x".repeat(4096)));
        for token in [String::new(), "x".repeat(4097), "control\n".into()] {
            assert!(!valid_token(&token));
        }
    }

    #[test]
    fn portal_display_treats_absent_or_unsafe_description_as_unknown() {
        assert_eq!(
            sanitize("\u{1b}\u{85}\u{061c}\u{200f}\u{202e}\u{2066}"),
            None
        );
        assert_eq!(
            sanitize("Press <Control>grave\n"),
            Some("Press <Control>grave".into())
        );
        assert!(sanitize(&"界".repeat(100)).unwrap().chars().count() <= 19);
        let cfg = kettle_config::Config::default();
        let tr = Translator::default();
        assert_eq!(
            Status::Registered {
                trigger: None,
                configurable: false,
                focus_dependent: false
            }
            .value(&cfg, &tr),
            tr.text(Text::QuickPortalManaged)
        );
    }
}
