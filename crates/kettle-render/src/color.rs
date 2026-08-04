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
    let white = Rgb::new(255, 255, 255);
    let black = Rgb::new(0, 0, 0);

    // Blend `fg` toward `target` just far enough to reach `min_ratio`.
    // 14 iterations resolves t to ~1/16384 — pixel-imperceptible.
    let approach = |target: Rgb| -> Rgb {
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
    };

    // Which endpoint to head for. Two rules, in order:
    //
    // 1. It must be able to REACH `min_ratio`. Thresholding background
    //    luminance at 0.5 got this wrong, because WCAG's
    //    `(L+0.05)/(L'+0.05)` is not symmetric about the midpoint — white and
    //    black cross over at ~0.1791. On `#969696` that chose white at 2.96:1
    //    where black gives 7.10:1, so a requested 4.5 was unreachable from the
    //    chosen end and the function returned white anyway, silently failing
    //    the guarantee it exists to provide.
    //
    // 2. Among endpoints that can reach it, take the one that gets there with
    //    the SMALLEST change to the caller's colour. Neither "maximum
    //    contrast" nor "nearest endpoint" is that: maximizing flips `#fdfdfd`
    //    on `#767676` from near-white to near-black over a 0.035 shortfall,
    //    and nearest-endpoint sends `#969696` on `#5a5a5a` to `#020202` when
    //    white reaches the same target at `#ababab`. Measuring the actual
    //    journey is the only rule that gets both right, and it costs one extra
    //    bisection.
    //
    // If neither end can reach the target, return the better of the two and
    // let the caller have the closest thing available.
    let white_ok = contrast_ratio(white, bg) >= min_ratio;
    let black_ok = contrast_ratio(black, bg) >= min_ratio;
    match (white_ok, black_ok) {
        (true, true) => {
            let toward_white = approach(white);
            let toward_black = approach(black);
            let fg_l = relative_luminance(fg);
            let d_white = (relative_luminance(toward_white) - fg_l).abs();
            let d_black = (relative_luminance(toward_black) - fg_l).abs();
            if d_white <= d_black {
                toward_white
            } else {
                toward_black
            }
        }
        (true, false) => approach(white),
        (false, true) => approach(black),
        (false, false) => {
            if contrast_ratio(white, bg) >= contrast_ratio(black, bg) {
                white
            } else {
                black
            }
        }
    }
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

/// The colour to draw the glyph sitting under a focused block cursor.
///
/// `cell_bg` is that cell's own background; `colors` is the runtime palette.
///
/// An OSC 12 runtime cursor colour moves the block out from under the theme's
/// `cursor`/`cursor_text` pair, so the recoloured glyph has to follow
/// reverse-video — its own cell background — instead of `theme.cursor_text`,
/// which was tuned against `theme.cursor`. With no OSC 12 in force,
/// `cursor_text` is exactly right, and it is what `cursor-fg-color` sets.
///
/// The runtime override is `colors[258]`, not `resolve_query(258, ..)`.
/// `resolve_query` falls back to the theme and so always answers `Some`, which
/// made this branch unconditional and left `cursor-fg-color` unreachable:
/// setting a conspicuous cursor foreground did nothing unless an application
/// happened to send OSC 12. Keeping the decision in one named function is what
/// makes that testable — inline, the renderer's copy of it was not.
pub fn cursor_glyph_color(theme: &Theme, colors: &TermColors, cell_bg: Rgb) -> Rgb {
    if colors[258].is_some() {
        cell_bg
    } else {
        theme.cursor_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The renderer must ask whether a runtime cursor colour EXISTS, and the
    /// answer must reach the glyph.
    ///
    /// Driven through `cursor_glyph_color`, the function the frame builder
    /// calls — an earlier version of this test asserted the distinction on
    /// `resolve_query` and `colors[258]` directly, so restoring the bug at the
    /// call site left it green.
    #[test]
    fn the_cursor_glyph_follows_reverse_video_only_under_a_runtime_override() {
        let theme = Theme::default();
        let mut colors = TermColors::default();
        let cell_bg = Rgb::new(9, 8, 7);

        assert_eq!(
            cursor_glyph_color(&theme, &colors, cell_bg),
            theme.cursor_text,
            "with no OSC 12 in force the glyph takes cursor-fg-color"
        );
        assert_ne!(
            theme.cursor_text, cell_bg,
            "the fixture must distinguish the two answers"
        );

        colors[258] = Some(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            cursor_glyph_color(&theme, &colors, cell_bg),
            cell_bg,
            "an OSC 12 cursor colour moves the block, so the glyph reverses \
             against its own cell instead"
        );
    }

    /// `resolve_query` answers "what colour is this slot", NOT "did an
    /// application override it".
    ///
    /// The renderer used `resolve_query(258, ..).is_some()` to decide whether
    /// a runtime OSC 12 cursor colour was in force. It falls back to the theme
    /// and so always returns `Some`, which made that branch unconditional and
    /// left `theme.cursor_text` — the field `cursor-fg-color` sets —
    /// unreachable. The setting did nothing unless an application happened to
    /// send OSC 12.
    ///
    /// The distinction lives in `term_colors[258]`, and this pins it so a
    /// future caller cannot make the same substitution.
    #[test]
    fn resolving_a_slot_is_not_the_same_as_it_being_overridden() {
        let theme = Theme::default();
        let mut colors = TermColors::default();

        // No override: the slot still resolves (to the theme), but nothing has
        // overridden it.
        assert!(
            resolve_query(258, &theme, &colors).is_some(),
            "the slot always resolves, which is exactly why it cannot be used \
             as an override test"
        );
        assert!(
            colors[258].is_none(),
            "with no OSC 12 seen, there is no runtime override"
        );
        assert_eq!(
            resolve_query(258, &theme, &colors),
            Some(theme.cursor),
            "and it resolves to the theme's cursor colour"
        );

        // After an OSC 12, the override exists and wins.
        colors[258] = Some(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        assert!(colors[258].is_some(), "now there is a runtime override");
        assert_eq!(
            resolve_query(258, &theme, &colors),
            Some(Rgb::new(1, 2, 3)),
            "and it takes precedence over the theme"
        );
    }

    /// The endpoint must be chosen by how far the colour has to TRAVEL, not by
    /// which extreme it started nearer.
    ///
    /// `#969696` on `#5a5a5a` asking for 3.0 is the case that separates them:
    /// by luminance the foreground sits nearer black (0.305 from it, 0.695
    /// from white), so a nearest-endpoint rule heads for black and lands at
    /// roughly `#020202` — while white reaches the very same 3.0 at about
    /// `#ababab`. Both rules I tried before this one got a case wrong;
    /// measuring the actual journey gets all of them right.
    #[test]
    fn the_endpoint_is_chosen_by_distance_travelled_not_by_starting_side() {
        let bg = Rgb::parse("#5a5a5a").expect("hex");
        let fg = Rgb::parse("#969696").expect("hex");
        let got = with_min_contrast(fg, bg, 3.0);

        assert!(
            contrast_ratio(got, bg) >= 3.0 - 1e-6,
            "the requested ratio must be met, got {:.3}",
            contrast_ratio(got, bg)
        );
        // The short move is upward; the long one inverts the text.
        assert!(
            relative_luminance(got) > relative_luminance(fg),
            "must lighten (a small move) rather than invert to near-black: \
             got rgb({},{},{})",
            got.r,
            got.g,
            got.b
        );
        // And it must be a SMALL move — closer to the original than to the
        // opposite extreme.
        let travelled = (relative_luminance(got) - relative_luminance(fg)).abs();
        let inverting = (relative_luminance(fg) - relative_luminance(Rgb::new(0, 0, 0))).abs();
        assert!(
            travelled < inverting,
            "the chosen adjustment ({travelled:.4}) must be shorter than \
             inverting the text ({inverting:.4})"
        );
    }

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

    #[test]
    fn text_area_reply_preserves_exact_fractional_dpi_totals() {
        let formatter = |size: alacritty_terminal::event::WindowSize| {
            format!(
                "\u{1b}[4;{};{}t",
                u32::from(size.num_lines) * u32::from(size.cell_height),
                u32::from(size.num_cols) * u32::from(size.cell_width)
            )
        };
        assert_eq!(
            reply_for_text_area_size(960, 768, &formatter),
            "\u{1b}[4;768;960t"
        );
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
    fn average_color_means_and_skips_transparent() {
        // Solid red (opaque) → red.
        let red = [255u8, 0, 0, 255].repeat(100);
        assert_eq!(average_color(&red), Rgb::new(255, 0, 0));
        // Half black, half white (all opaque) → mid-gray.
        let mut bw = Vec::new();
        bw.extend(std::iter::repeat_n([0u8, 0, 0, 255], 2048).flatten());
        bw.extend(std::iter::repeat_n([255u8, 255, 255, 255], 2048).flatten());
        let avg = average_color(&bw);
        assert!(
            (120..=135).contains(&avg.r) && avg.r == avg.g && avg.g == avg.b,
            "got {avg:?}"
        );
        // Fully-transparent pixels are skipped — a green opaque pixel among
        // transparent ones yields green, not a darkened/black-biased mean.
        let mut mixed = [0u8, 0, 0, 0].repeat(50); // transparent black
        mixed.extend([0u8, 200, 0, 255]); // one opaque green
        assert_eq!(average_color(&mixed), Rgb::new(0, 200, 0));
        // Empty / all-transparent → neutral mid-gray, never a panic.
        assert_eq!(average_color(&[]), Rgb::new(128, 128, 128));
        assert_eq!(
            average_color(&[0, 0, 0, 0, 0, 0, 0, 0]),
            Rgb::new(128, 128, 128)
        );
    }

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
}
