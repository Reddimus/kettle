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

/// Resolve an OSC color **query** (OSC 4 `;i;?`, OSC 10/11/12 `;?`) index to
/// an `Rgb`. `alacritty_terminal` numbers these the same way `Colors` is
/// indexed: `0..=15` is the ANSI palette, `16..=255` is the xterm 256-color
/// cube + grayscale ramp, **256 = default foreground**, **257 = default
/// background**, **258 = cursor**. Anything else returns `None`.
///
/// Runtime overrides set via OSC 4 / 10 / 11 / 12 (stored in `term_colors`)
/// take precedence over the theme. Pure — no I/O, fully unit-tested — so the
/// app event loop just plugs the result into the engine-supplied formatter
/// and writes the bytes back to the PTY.
pub fn resolve_query(idx: usize, theme: &Theme, term_colors: &TermColors) -> Option<Rgb> {
    // Reject out-of-range up front — `Colors` is fixed-size and OSC queries
    // beyond the documented slots aren't meaningful for any app.
    let fallback = match idx {
        0..=15 => theme.palette[idx],
        16..=255 => indexed_256(idx as u8),
        256 => theme.foreground,
        257 => theme.background,
        258 => theme.cursor,
        _ => return None,
    };
    // Runtime override (set via OSC 4 / 10 / 11 / 12) wins over the theme.
    Some(
        term_colors[idx]
            .map(|rgb| Rgb::new(rgb.r, rgb.g, rgb.b))
            .unwrap_or(fallback),
    )
}

/// Resolve a color query and format the engine-supplied OSC reply.
///
/// The event loop receives `TermEvent::ColorRequest(idx, fmt)` where `fmt`
/// already knows the right OSC prefix (`10`, `11`, `12`, or `4;<idx>`) and
/// terminator (`ST`/`BEL`). This helper hides the `alacritty_terminal` `Rgb`
/// type from the UI crate: callers pass the formatter through and we hand
/// back the ready-to-write reply bytes, or `None` when the index is out of
/// range (the protocol allows no reply in that case).
pub fn reply_for_query(
    idx: usize,
    theme: &Theme,
    term_colors: &TermColors,
    fmt: &(dyn Fn(alacritty_terminal::vte::ansi::Rgb) -> String + Send + Sync),
) -> Option<String> {
    let rgb = resolve_query(idx, theme, term_colors)?;
    Some(fmt(alacritty_terminal::vte::ansi::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }))
}

/// Format the engine-supplied reply for `CSI 14 t` (text-area size in
/// pixels).
///
/// `alacritty_terminal` raises `TermEvent::TextAreaSizeRequest(fmt)` with
/// a formatter that needs a `WindowSize { num_lines, num_cols, cell_width,
/// cell_height }` and produces the standard xtwinops reply `CSI 4 ; h ; w t`
/// (height = rows × cell-height, width = cols × cell-width). Sixel, kitty
/// graphics and iTerm2-OSC-1337-aware apps use this to compute
/// pixel-perfect image placements; without it they fall back to guessed
/// 8×16 cells.
///
/// Same layering rationale as `reply_for_query`: keep the engine
/// `WindowSize` type contained here so kettle-ui can stay engine-internal-
/// free and just call this helper with the four numbers it already knows.
pub fn reply_for_text_area_size(
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
    fmt: &(dyn Fn(alacritty_terminal::event::WindowSize) -> String + Send + Sync),
) -> String {
    fmt(alacritty_terminal::event::WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: cell_w,
        cell_height: cell_h,
    })
}

/// Cycle 355 (Terminator parity, terminatorlib/config.py:130
/// `bold_is_bright`): when bold is set + the foreground is one of
/// the low palette indices (0..8), remap to the bright variant
/// (8..16). xterm convention; many programs (e.g. neovim's
/// `:Termguicolors` off, ls --color) depend on it.
///
/// Returns the original color if it doesn't match any low-palette
/// index — caller (the render loop) doesn't need to branch.
pub fn bright_for_bold(fg: Rgb, theme: &Theme) -> Rgb {
    for low in 0..8 {
        if theme.palette[low] == fg {
            return theme.palette[low + 8];
        }
    }
    fg
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

// ──────────────────────────────────────────────────────────────────────
// Minimum-contrast guard (WezTerm `minimum_contrast` parity).
//
// Pure WCAG 2.0 relative-luminance math + a binary-search adjuster that
// lightens or darkens the foreground until its contrast with the
// background reaches a target ratio. Used by the renderer to keep text
// readable on low-contrast themes; off (ratio ≤ 1.0) preserves colors.

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.0 relative luminance for an sRGB color (0.0 black .. 1.0 white).
pub fn relative_luminance(rgb: Rgb) -> f64 {
    0.2126 * srgb_to_linear(rgb.r) + 0.7152 * srgb_to_linear(rgb.g) + 0.0722 * srgb_to_linear(rgb.b)
}

/// WCAG 2.0 contrast ratio (1.0..=21.0). Symmetric in its arguments.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t.clamp(0.0, 1.0)).round() as u8
}

fn blend(fg: Rgb, target: Rgb, t: f64) -> Rgb {
    Rgb::new(
        lerp(fg.r, target.r, t),
        lerp(fg.g, target.g, t),
        lerp(fg.b, target.b, t),
    )
}

/// SGR 2 (`dim` / faint) attribute: blend `fg` halfway toward `bg`. xterm /
/// alacritty / iTerm2 all render dim as roughly 50 % intensity — close
/// enough that programs (fish prompt themers, `less` status lines, mc) can
/// rely on the visible-but-attenuated look. Pure so the SGR test suite can
/// pin the math without a GPU.
pub fn dim(fg: Rgb, bg: Rgb) -> Rgb {
    blend(fg, bg, 0.5)
}

/// Return `fg` adjusted toward the higher-contrast endpoint (white or
/// black, relative to `bg`) until the WCAG contrast ratio reaches
/// `min_ratio`. `min_ratio <= 1.0` is a no-op. Pure — binary-searches
/// the blend parameter to keep theme tint as much as possible.
pub fn with_min_contrast(fg: Rgb, bg: Rgb, min_ratio: f64) -> Rgb {
    if min_ratio <= 1.0 || contrast_ratio(fg, bg) >= min_ratio {
        return fg;
    }
    // Push the fg toward whichever extreme is *farther* from the bg's
    // luminance — that gains contrast fastest.
    let bg_l = relative_luminance(bg);
    let target = if bg_l < 0.5 {
        Rgb::new(255, 255, 255)
    } else {
        Rgb::new(0, 0, 0)
    };
    // If even the extreme can't reach min_ratio (clamped 21:1), return it.
    if contrast_ratio(target, bg) < min_ratio {
        return target;
    }
    // 14 iterations resolves t to ~1/16384 — pixel-imperceptible.
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(blend(fg, target, mid), bg) >= min_ratio {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    blend(fg, target, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_extremes_and_symmetry() {
        let w = Rgb::new(255, 255, 255);
        let k = Rgb::new(0, 0, 0);
        // White-on-black is 21:1; identical colors are 1:1; symmetric.
        assert!((contrast_ratio(w, k) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(k, w) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(w, w) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn with_min_contrast_is_a_noop_when_ratio_already_met() {
        let w = Rgb::new(255, 255, 255);
        let k = Rgb::new(0, 0, 0);
        // 21:1 already exceeds any sane threshold.
        assert_eq!(with_min_contrast(w, k, 7.0), w);
        // Disabled (≤ 1.0) returns the input unchanged.
        let gray = Rgb::new(128, 128, 128);
        assert_eq!(with_min_contrast(gray, gray, 1.0), gray);
        assert_eq!(with_min_contrast(gray, gray, 0.0), gray);
    }

    #[test]
    fn with_min_contrast_lifts_low_contrast_text() {
        // Mid-gray on dark bg is hard to read; ask for 4.5:1 (WCAG AA).
        let bg = Rgb::new(20, 20, 30);
        let fg = Rgb::new(80, 80, 90);
        let out = with_min_contrast(fg, bg, 4.5);
        assert!(
            contrast_ratio(out, bg) + 1e-6 >= 4.5,
            "got {} for {out:?}",
            contrast_ratio(out, bg)
        );
        // Direction: dark bg ⇒ lifted toward white (out is brighter).
        assert!(relative_luminance(out) > relative_luminance(fg));
    }

    #[test]
    fn resolve_query_covers_palette_named_cube_and_overrides() {
        use alacritty_terminal::term::color::Colors as TermColors;
        use alacritty_terminal::vte::ansi::Rgb as AnsiRgb;
        let theme = Theme::default();
        let mut colors = TermColors::default();

        // 0..=15 routes to the theme palette.
        assert_eq!(resolve_query(2, &theme, &colors), Some(theme.palette[2]));
        // 16..=255 uses the xterm 256-color cube. Index 196 is pure red.
        assert_eq!(
            resolve_query(196, &theme, &colors),
            Some(Rgb::new(255, 0, 0))
        );
        // 256 / 257 / 258 are default fg / bg / cursor.
        assert_eq!(resolve_query(256, &theme, &colors), Some(theme.foreground));
        assert_eq!(resolve_query(257, &theme, &colors), Some(theme.background));
        assert_eq!(resolve_query(258, &theme, &colors), Some(theme.cursor));
        // Out of range queries don't index past the fixed-size palette.
        assert_eq!(resolve_query(259, &theme, &colors), None);
        assert_eq!(resolve_query(99_999, &theme, &colors), None);

        // Runtime override (as set by OSC 4 / 10 / 11 / 12) wins over the theme.
        colors[257] = Some(AnsiRgb {
            r: 0xab,
            g: 0xcd,
            b: 0xef,
        });
        assert_eq!(
            resolve_query(257, &theme, &colors),
            Some(Rgb::new(0xab, 0xcd, 0xef))
        );
    }

    #[test]
    fn dim_blends_halfway_toward_bg() {
        // Pure white fg on pure black bg, dim → mid-gray.
        let w = Rgb::new(255, 255, 255);
        let k = Rgb::new(0, 0, 0);
        let mid = dim(w, k);
        assert!(
            mid.r >= 126 && mid.r <= 129,
            "dim(white, black).r ~= 128, got {}",
            mid.r
        );
        // Symmetric channels.
        assert_eq!(mid.r, mid.g);
        assert_eq!(mid.g, mid.b);
        // Dim onto the same color is a no-op (fg == bg → fg stays).
        assert_eq!(dim(k, k), k);
        // Dim a color onto itself: also unchanged.
        let red = Rgb::new(200, 50, 50);
        assert_eq!(dim(red, red), red);
    }

    #[test]
    fn with_min_contrast_darkens_on_light_bg() {
        let bg = Rgb::new(240, 240, 240);
        let fg = Rgb::new(180, 180, 180);
        let out = with_min_contrast(fg, bg, 4.5);
        assert!(contrast_ratio(out, bg) + 1e-6 >= 4.5);
        // Light bg ⇒ darkened toward black.
        assert!(relative_luminance(out) < relative_luminance(fg));
    }
}
