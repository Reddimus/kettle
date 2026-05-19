//! Resolve `alacritty_terminal` colors against the active theme + any
//! OSC-overridden palette.

use alacritty_terminal::term::color::Colors as TermColors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use kettle_config::{Rgb, Theme};

/// 256-color cube / grayscale ramp for indexed colors 16..=255.
fn indexed_256(i: u8) -> Rgb {
    match i {
        0..=15 => Rgb::new(0, 0, 0), // handled via palette elsewhere
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb::new(c(r), c(g), c(b))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgb::new(v, v, v)
        }
    }
}

fn named(n: NamedColor, theme: &Theme) -> Rgb {
    use NamedColor::*;
    match n {
        Black => theme.palette[0],
        Red => theme.palette[1],
        Green => theme.palette[2],
        Yellow => theme.palette[3],
        Blue => theme.palette[4],
        Magenta => theme.palette[5],
        Cyan => theme.palette[6],
        White => theme.palette[7],
        BrightBlack | DimBlack => theme.palette[8],
        BrightRed | DimRed => theme.palette[9],
        BrightGreen | DimGreen => theme.palette[10],
        BrightYellow | DimYellow => theme.palette[11],
        BrightBlue | DimBlue => theme.palette[12],
        BrightMagenta | DimMagenta => theme.palette[13],
        BrightCyan | DimCyan => theme.palette[14],
        BrightWhite | DimWhite => theme.palette[15],
        Foreground | BrightForeground | DimForeground => theme.foreground,
        Background => theme.background,
        Cursor => theme.cursor,
    }
}

/// Resolve a cell color. `term_colors` carries runtime OSC 4/10/11 overrides.
pub fn resolve(c: AnsiColor, theme: &Theme, term_colors: &TermColors) -> Rgb {
    match c {
        AnsiColor::Spec(rgb) => Rgb::new(rgb.r, rgb.g, rgb.b),
        AnsiColor::Named(n) => {
            if let Some(rgb) = term_colors[n] {
                Rgb::new(rgb.r, rgb.g, rgb.b)
            } else {
                named(n, theme)
            }
        }
        AnsiColor::Indexed(i) => {
            if let Some(rgb) = term_colors[i as usize] {
                Rgb::new(rgb.r, rgb.g, rgb.b)
            } else if i < 16 {
                theme.palette[i as usize]
            } else {
                indexed_256(i)
            }
        }
    }
}
