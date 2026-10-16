//! The entry point for fuzzing what Kettle reads from a config file: the
//! file's bytes, its key binds, colors and themes, and the values with a
//! syntax of their own. [`config`] takes any bytes and must return without
//! panicking. A file reads the same with or without a UTF-8 byte-order mark
//! and with CRLF line ends as with LF, and a key bind or color that parses
//! parses again from how Kettle writes it. The fuzz target (feature
//! `fuzzing`) and the in-process sweep below call the same function.

use crate::color::Rgb;
use crate::keybinds::{apply_keybind, defaults, parse_trigger};
use crate::theme::Theme;
use crate::{
    Config, FontFeature, decode_config_text, parse_theme_schedule, parse_trigger_with_command,
};

/// Read `data` past its first byte as what that byte picks: a whole config
/// file, a key bind, a color or theme, or another value with its own syntax.
pub fn config(data: &[u8]) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let text = decode_config_text(bytes);
    match pick % 4 {
        0 => file(&text),
        1 => {
            if let Some(trigger) = parse_trigger(&text) {
                assert_eq!(parse_trigger(&trigger.label()), Some(trigger), "{text:?}");
            }
            let mut binds = defaults();
            let _ = apply_keybind(&mut binds, &text);
        }
        2 => {
            if let Some(color) = Rgb::parse(&text) {
                let written = format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
                assert_eq!(Rgb::parse(&written), Some(color));
            }
            let _ = Theme::parse(&text);
        }
        _ => {
            let _ = parse_theme_schedule(&text);
            let _ = FontFeature::parse(&text);
            let _ = parse_trigger_with_command(&text);
        }
    }
}

/// What a config file reads as: its entries, its unknown keys and its
/// malformed values.
fn read(text: &str) -> (String, Vec<String>, Vec<String>) {
    let entries = format!("{:?}", crate::parse::parse(text));
    let (_, unknown) = Config::parse_collect(text);
    (entries, unknown, Config::detect_malformed_values(text))
}

fn file(text: &str) {
    let read_plain = read(text);
    let with_bom = format!("\u{feff}{text}");
    if !text.starts_with('\u{feff}') {
        assert_eq!(
            read(&with_bom),
            read_plain,
            "a byte-order mark changed {text:?}"
        );
    }
    if !text.contains('\r') {
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(read(&crlf), read_plain, "CRLF line ends changed {text:?}");
    }
}

#[cfg(test)]
mod tests {
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    #[test]
    fn mutated_config_reads_the_same_however_its_lines_end() {
        let inputs: Vec<(u8, &[u8])> = vec![
            (0, b"# comment\nfont-family = JetBrains Mono\nfont-size = 14\ntheme = light:Latte,dark:Mocha\n\nkeybind = ctrl+shift+t=new_tab\npalette = 1=#ff0000\nscrollback = lots\nunknown-key = 1\nno equals here\n"),
            (0, b"background = #1e1e2e\nforeground=rgb:ff/ee/dd\ncursor-style = block\ntab-bar = auto\nconfig-file = ?other.conf\n"),
            (0, b"\xff\xfef\x00o\x00n\x00t\x00-\x00s\x00i\x00z\x00e\x00 \x00=\x00 \x001\x002\x00\n\x00"),
            (0, b"\xfe\xff\x00a\x00 \x00=\x00 \x00b"),
            (1, b"ctrl+shift+c=copy_to_clipboard"),
            (1, b"<Control><Shift>e"),
            (1, b"super+alt+F5"),
            (1, b"ctrl+shift+v=unbind"),
            (2, b"#abc"),
            (2, b"0xDEADBE"),
            (2, b"rgb:ff/80/00"),
            (2, b"palette = 0=#000000\nbackground = #101010\nforeground = cornflowerblue\n"),
            (3, b"light:Latte,dark:Mocha"),
            (3, b"calt=0"),
            (3, b"ERROR\\b.*=notify"),
        ];
        let seeds: Vec<Vec<u8>> = inputs
            .into_iter()
            .map(|(pick, bytes)| {
                let mut seed = vec![pick];
                seed.extend_from_slice(bytes);
                seed
            })
            .collect();
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("config", &seeds);
        for case in cases(&seeds, 800, 0x5eed_0501) {
            config(&case);
        }
    }
}
