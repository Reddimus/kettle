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
/// `alacritty_terminal` raises `TermEvent::TextAreaSizeRequest(fmt)` with a
/// formatter that produces the standard xtwinops reply `CSI 4 ; h ; w t`.
/// Feed it a one-cell synthetic window whose cell is the exact total pixel
/// extent. This preserves a fractional-DPI text area (for example 100 columns
/// at 9.6 px = 960 px); multiplying a rounded per-cell metric would report
/// 1000 px instead. Sixel, kitty graphics and iTerm2-OSC-1337-aware apps use
/// this reply for pixel-perfect placement.
///
/// Same layering rationale as `reply_for_query`: keep the engine
/// `WindowSize` type contained here so kettle-ui can stay engine-internal-
/// free and just call this helper with the exact totals the PTY already knows.
pub fn reply_for_text_area_size(
    pixel_width: u16,
    pixel_height: u16,
    fmt: &(dyn Fn(alacritty_terminal::event::WindowSize) -> String + Send + Sync),
) -> String {
    fmt(alacritty_terminal::event::WindowSize {
        num_lines: 1,
        num_cols: 1,
        cell_width: pixel_width,
        cell_height: pixel_height,
    })
}

/// Terminator parity (terminatorlib/config.py:130
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
    // Pick the endpoint to push toward. Two things matter, in this order:
    //
    // 1. It has to be able to REACH `min_ratio`. Thresholding background
    //    luminance at 0.5 got this wrong, because WCAG's
    //    `(L+0.05)/(L'+0.05)` is not symmetric about the midpoint — the
    //    white/black crossover is at ~0.1791. On `#969696` that chose white at
    //    2.96:1 where black gives 7.10:1, so a requested 4.5 was unreachable
    //    from the chosen end and the function returned white anyway, quietly
    //    failing the guarantee it exists to provide.
    //
    // 2. Among endpoints that CAN reach it, take the one nearer the caller's
    //    own foreground, so we move the colour as little as possible. Simply
    //    maximizing contrast is wrong in the other direction: `#fdfdfd` on
    //    `#767676` is 4.465:1, a shortfall of 0.035, and both ends clear 4.5
    //    (white 4.542, black 4.623) — maximizing flips near-white text to
    //    near-black over nothing.
    //
    // If neither end can reach the target, take the better of the two and let
    // the caller have the closest thing available.
    let white = Rgb::new(255, 255, 255);
    let black = Rgb::new(0, 0, 0);
    let white_ratio = contrast_ratio(white, bg);
    let black_ratio = contrast_ratio(black, bg);
    let fg_l = relative_luminance(fg);
    // Distance in luminance is the honest measure of "how far we are moving
    // this colour", and it is what the bisection below actually travels.
    let nearer_white = (1.0 - fg_l) <= fg_l;
    let target = match (white_ratio >= min_ratio, black_ratio >= min_ratio) {
        (true, true) => {
            if nearer_white {
                white
            } else {
                black
            }
        }
        (true, false) => white,
        (false, true) => black,
        (false, false) => {
            if white_ratio >= black_ratio {
                white
            } else {
                black
            }
        }
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

/// Average (mean) RGB color of a tightly-packed RGBA8 buffer, used by
/// `chrome-background = auto` to tint the chrome strips "inspired by" the
/// wallpaper. Fully-transparent pixels (alpha 0) are skipped so a sprite-style
/// background with large transparent regions doesn't bias the mean toward
/// black. Samples at a stride so a 4K frame costs microseconds, not
/// milliseconds — the result is a broad-strokes tint, not a precise palette, so
/// sampling every Nth pixel is visually indistinguishable from a full average.
/// Returns mid-gray for an empty / all-transparent buffer (a safe neutral).
pub fn average_color(rgba: &[u8]) -> Rgb {
    let px = rgba.len() / 4;
    if px == 0 {
        return Rgb::new(128, 128, 128);
    }
    // Aim for ~4096 samples regardless of image size; never stride past the end.
    let stride = (px / 4096).max(1);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    let mut i = 0;
    while i < px {
        let o = i * 4;
        let a = rgba[o + 3];
        if a != 0 {
            r += rgba[o] as u64;
            g += rgba[o + 1] as u64;
            b += rgba[o + 2] as u64;
            n += 1;
        }
        i += stride;
    }
    if n == 0 {
        return Rgb::new(128, 128, 128);
    }
    Rgb::new((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lifting contrast must not invert the text.
    ///
    /// `#fdfdfd` on `#767676` is 4.465:1 — a shortfall of 0.035 against a
    /// requested 4.5. Both endpoints clear it (white 4.542, black 4.623), so
    /// choosing by *maximum* contrast flipped near-white text to near-black
    /// over nothing. Prefer the endpoint the foreground is already nearest, and
    /// cross over only when that side cannot reach the target.
    #[test]
    fn lifting_contrast_moves_the_colour_as_little_as_possible() {
        let bg = Rgb::parse("#767676").expect("hex");
        let fg = Rgb::parse("#fdfdfd").expect("hex");
        let got = with_min_contrast(fg, bg, 4.5);

        assert!(
            contrast_ratio(got, bg) >= 4.5 - 1e-6,
            "the requested ratio must still be met, got {:.3}",
            contrast_ratio(got, bg)
        );
        assert!(
            relative_luminance(got) > 0.5,
            "near-white text must stay light: #fdfdfd became rgb({},{},{})",
            got.r,
            got.g,
            got.b
        );
        // Symmetrically, near-black text on the same background stays dark.
        let dark = Rgb::parse("#040404").expect("hex");
        let got_dark = with_min_contrast(dark, bg, 4.5);
        assert!(
            contrast_ratio(got_dark, bg) >= 4.5 - 1e-6,
            "the requested ratio must be met from the dark side too"
        );
        assert!(
            relative_luminance(got_dark) < 0.5,
            "near-black text must stay dark: became rgb({},{},{})",
            got_dark.r,
            got_dark.g,
            got_dark.b
        );
    }

    /// When only ONE endpoint can reach the ratio, that one must be used even
    /// if the foreground started nearer the other. This is the original
    /// mid-tone bug: `#969696` on itself can only reach 4.5 through black.
    #[test]
    fn contrast_crosses_over_when_the_near_side_cannot_reach_the_target() {
        for (hex, want) in [("#969696", 4.5_f64), ("#8a8a8a", 4.5), ("#a0a0a0", 5.0)] {
            let bg = Rgb::parse(hex).expect("hex");
            // Foreground equal to the background: ratio 1.0, no side preference
            // beyond its own luminance.
            let got = with_min_contrast(bg, bg, want);
            let reached = contrast_ratio(got, bg);
            let best = contrast_ratio(Rgb::new(255, 255, 255), bg)
                .max(contrast_ratio(Rgb::new(0, 0, 0), bg));
            assert!(
                reached >= want - 1e-6 || (best < want && reached >= best - 1e-6),
                "{hex} at {want}: reached {reached:.3}, best possible {best:.3}"
            );
        }
    }

    /// Ratios inside the real 1..=21 range, not an unreachable sentinel. A
    /// previous version asked for 25 — above the 21:1 maximum — so it only
    /// ever exercised the "neither endpoint reaches it" branch and could not
    /// see a wrong choice among reachable ones.
    #[test]
    fn contrast_endpoint_choice_holds_across_attainable_ratios() {
        for hex in [
            "#000000", "#2e3436", "#767676", "#969696", "#c0c0c0", "#ffffff",
        ] {
            let bg = Rgb::parse(hex).expect("hex");
            let white = contrast_ratio(Rgb::new(255, 255, 255), bg);
            let black = contrast_ratio(Rgb::new(0, 0, 0), bg);
            for want in [3.0_f64, 4.5, 7.0, 21.0] {
                let got = with_min_contrast(bg, bg, want);
                let reached = contrast_ratio(got, bg);
                let best = white.max(black);
                assert!(
                    reached >= want - 1e-6 || (best < want && reached >= best - 1e-6),
                    "{hex} at {want}: reached {reached:.3}, best {best:.3}"
                );
            }
        }
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
