use super::{Config, GColor, PaneView, Renderer, Rgb, chrome, color};

/// Disabled and secondary menu text: 45% foreground over the panel
/// background, so it reads as about 55% transparent without alpha-blending
/// through to whatever lives under the panel.
pub(crate) fn dim_blend(fg: Rgb, bg: Rgb) -> Rgb {
    solid_blend(fg, bg, 45)
}

/// Opaque color mixture used for UI surfaces that must remain legible over a
/// translucent terminal window. `source_percent` is clamped to 0..=100; the
/// result is pre-blended rather than alpha-composited with terminal text.
pub(crate) fn solid_blend(source: Rgb, target: Rgb, source_percent: u16) -> Rgb {
    let source_percent = source_percent.min(100);
    let target_percent = 100 - source_percent;
    let channel = |source: u8, target: u8| {
        ((u16::from(source) * source_percent + u16::from(target) * target_percent + 50) / 100) as u8
    };
    Rgb::new(
        channel(source.r, target.r),
        channel(source.g, target.g),
        channel(source.b, target.b),
    )
}

/// Minimum separation between the selected row's surface and the panel body
/// before Kettle stops trusting the theme's own selection color.
const COMPLETION_SELECTION_SEPARATION: f64 = 1.35;

/// Every color the completion card paints. Buffer preparation shapes the
/// emphasis run with an explicit color, so both passes must derive them from
/// one place or a selected row would keep the unselected tint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CompletionPalette {
    pub(crate) panel_bg: Rgb,
    pub(crate) border: Rgb,
    pub(crate) divider: Rgb,
    pub(crate) selection_bg: Rgb,
    pub(crate) rail: Rgb,
    pub(crate) header: Rgb,
    pub(crate) label: Rgb,
    pub(crate) description: Rgb,
    pub(crate) emphasis: Rgb,
    pub(crate) selected_label: Rgb,
    pub(crate) selected_description: Rgb,
    pub(crate) selected_emphasis: Rgb,
    pub(crate) scroll_track: Rgb,
    pub(crate) scroll_thumb: Rgb,
}

#[cfg(test)]
pub(crate) fn completion_palette(theme: &kettle_config::Theme, accent: Rgb) -> CompletionPalette {
    chrome::Colors::default().completion(theme, accent)
}

pub(crate) fn completion_palette_with_colors(
    theme: &kettle_config::Theme,
    accent: Rgb,
    colors: &mut chrome::Colors,
) -> CompletionPalette {
    let panel_bg = solid_blend(theme.foreground, theme.background, 9);
    let selection_bg = completion_selection_surface(theme.selection_background, panel_bg, accent);
    // A blended surface is no longer the pair the theme designed its selection
    // foreground for, so fall back to the ordinary text color there.
    let selected_fg = if selection_bg == theme.selection_background {
        theme.selection_foreground
    } else {
        theme.foreground
    };
    CompletionPalette {
        panel_bg,
        border: solid_blend(theme.foreground, theme.background, 30),
        divider: solid_blend(theme.foreground, panel_bg, 16),
        selection_bg,
        rail: colors.icon(accent, selection_bg),
        header: colors.muted(theme.foreground, panel_bg),
        label: colors.text(theme.foreground, panel_bg),
        description: colors.muted(theme.foreground, panel_bg),
        emphasis: colors.text(accent, panel_bg),
        selected_label: colors.text(selected_fg, selection_bg),
        selected_description: colors.muted(selected_fg, selection_bg),
        selected_emphasis: colors.text(accent, selection_bg),
        scroll_track: solid_blend(theme.foreground, panel_bg, 12),
        scroll_thumb: colors.icon(
            solid_blend(theme.foreground, panel_bg, 45),
            solid_blend(theme.foreground, panel_bg, 12),
        ),
    }
}

/// Selected-row surface. The theme's own selection color is preferred; a theme
/// whose selection sits on top of the panel body would erase the active row, so
/// the window accent is blended in until the two surfaces separate.
pub(crate) fn completion_selection_surface(
    theme_selection: Rgb,
    panel_bg: Rgb,
    accent: Rgb,
) -> Rgb {
    if color::contrast_ratio(theme_selection, panel_bg) >= COMPLETION_SELECTION_SEPARATION {
        return theme_selection;
    }
    for percent in [30_u16, 45, 60, 80, 100] {
        let blended = solid_blend(accent, panel_bg, percent);
        if color::contrast_ratio(blended, panel_bg) >= COMPLETION_SELECTION_SEPARATION {
            return blended;
        }
    }
    // An accent that cannot separate on its own still has to yield a visible
    // row; drive it toward the reachable endpoint instead.
    color::with_min_contrast(accent, panel_bg, COMPLETION_SELECTION_SEPARATION)
}

impl Renderer {
    /// Multi-window (Peacock): set/clear this window's accent.
    pub fn set_accent_override(&mut self, accent: Option<Rgb>) {
        self.accent_override = accent;
    }

    pub fn set_live_background_opacity_floor(&mut self, floor: Option<f32>) {
        self.last_scene = None;
        self.live_background_opacity_floor = floor.map(|value| value.clamp(0.0, 1.0));
    }

    /// The accent every LIVE chrome element uses (focused-pane border,
    /// active-tab strip, drag ghost, context-menu + settings highlights,
    /// pane titlebars): the per-window override when the App resolved one,
    /// else the static config cascade.
    pub(crate) fn ui_accent(&self, cfg: &Config, theme: &kettle_config::Theme) -> Rgb {
        self.accent_override
            .unwrap_or_else(|| cfg.resolved_accent(theme))
    }
}

/// Pick the per-pane titlebar background color from the focus / broadcast state.
///
/// Explicit `title-*-bg-color` config wins; otherwise every state derives from
/// the theme. Focused falls back to `focused_split_color`, then the resolved
/// accent, the same cascade as the pane border and screenshot accent. Broadcast
/// falls back to the accent. Inactive falls back to the theme's surface
/// `palette[8]`.
///
/// Pure so the cascade is drift-guarded without standing up wgpu.
pub(crate) fn pick_titlebar_bg(
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    // Multi-window: the already-resolved per-window accent (the live
    // renderer passes its `ui_accent`; tests and any cfg-only caller pass
    // `cfg.resolved_accent(theme)`).
    accent: Rgb,
    focused: bool,
    broadcast: bool,
) -> Rgb {
    if focused {
        cfg.title_transmit_bg_color
            .or(cfg.focused_split_color)
            .unwrap_or(accent)
    } else if broadcast {
        cfg.title_receive_bg_color.unwrap_or(accent)
    } else {
        cfg.title_inactive_bg_color.unwrap_or(theme.palette[8])
    }
}

/// Resolve the opaque fill color for the window chrome strips (tab bar,
/// status bar, new-tab button). Without a wallpaper, or with
/// `chrome-background = theme`, this is the theme's chrome color (`palette[8]`).
/// With a wallpaper, the other modes let the chrome read deliberately against
/// the moving background:
///   - `black` / `white`: a fixed neutral panel.
///   - `auto`: the wallpaper's average color, nudged toward black/white only as
///     far as needed to keep the (theme-colored) tab text readable on it
///     (`with_min_contrast` against `theme.foreground`, 3:1). Falls back to the
///     theme color if no frame has been sampled yet.
///
/// `bg_avg` is `Some` only when a wallpaper frame was sampled this frame for
/// `auto`. Pure so the mapping is unit-tested without a GPU.
pub(crate) fn resolve_chrome_bg(
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    bg_avg: Option<Rgb>,
) -> Rgb {
    use kettle_config::{BackgroundType, ChromeBackground};
    // Only a wallpaper (image or starfield) changes the chrome color; otherwise
    // theme as before. (Over a pure-black starfield, `auto` keeps the chrome
    // black — black already clears the 3:1 contrast bar against a light fg.)
    if !matches!(
        cfg.background_type,
        BackgroundType::Image | BackgroundType::Starfield
    ) {
        return theme.palette[8];
    }
    match cfg.chrome_background {
        ChromeBackground::Theme => theme.palette[8],
        ChromeBackground::Black => Rgb::new(0, 0, 0),
        ChromeBackground::White => Rgb::new(255, 255, 255),
        ChromeBackground::Auto => {
            // The starfield has no decoded frame to sample; it's a black sky, so
            // treat its average as black (→ a seamless dark bar after the
            // contrast nudge) rather than falling back to the theme color.
            let avg = bg_avg.or_else(|| {
                matches!(cfg.background_type, BackgroundType::Starfield)
                    .then_some(Rgb::new(0, 0, 0))
            });
            match avg {
                Some(avg) => color::with_min_contrast(avg, theme.foreground, 3.0),
                None => theme.palette[8],
            }
        }
    }
}

/// Text on the opaque confirmation panel keeps WCAG AA contrast in custom themes.
pub fn confirm_dialog_text_color(theme: &kettle_config::Theme) -> Rgb {
    color::with_min_contrast(theme.foreground, theme.background, 4.5)
}

pub(crate) fn confirm_dialog_border_color(theme: &kettle_config::Theme) -> Rgb {
    color::with_min_contrast(
        color::dim(theme.foreground, theme.background),
        theme.background,
        1.5,
    )
}

/// Wash alpha for a visual-bell flash `ramp` (1.0 at the peak, 0.0 when it
/// has decayed) on this theme: the configured lightness step scaled by the
/// ramp, converted for linear-light compositing of the foreground over the
/// background. Shared by the live frame and the `DebugScene::BellFlash`
/// fixture so the offscreen pixel check measures the shipped math.
pub fn bell_flash_alpha(theme: &kettle_config::Theme, cfg: &Config, ramp: f32) -> f32 {
    let step = cfg.bell_flash_intensity.clamp(0.0, 1.0) * ramp.clamp(0.0, 1.0);
    color::perceptual_wash_alpha(theme.background, theme.foreground, step)
}

pub(crate) fn srgb(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb`]: encode a linear channel back to an 8-bit sRGB byte.
pub(crate) fn srgb_encode(linear: f64) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The clear colour for a pass whose pipelines composite as if the destination
/// were premultiplied.
///
/// Every draw layered over this clear treats the existing attachment contents
/// as premultiplied: `quad` and `imgpipe` use `PREMULTIPLIED_ALPHA_BLENDING`
/// (`src_factor: One`), and `glyphpipe`'s `ALPHA_BLENDING` pairs
/// `src_factor: SrcAlpha` with `dst_factor: OneMinusSrcAlpha`, which is the
/// premultiplied "over" operator with the source premultiplied on the fly. The
/// clear is the one write in the chain that does not pass through a blend, so
/// it is the only one that has to premultiply itself. Skipping that leaves a
/// translucent background too bright by exactly the skipped alpha factor.
///
/// The multiply belongs in linear space: the attachment is an sRGB format, so
/// the hardware decodes before blending and re-encodes on write, and the whole
/// pass is linear. [`srgb`] already returns linear, so scaling its output is
/// the correct place.
///
/// Only `PreMultiplied` gets scaled. The live `PostMultiplied` path never draws
/// the scene directly into the surface: it renders this premultiplied
/// representation offscreen and explicitly unpremultiplies in the final
/// presentation pass. The other modes receive a straight clear here.
pub(crate) fn surface_clear_color(
    bg: Rgb,
    alpha: f64,
    mode: wgpu::CompositeAlphaMode,
) -> wgpu::Color {
    let alpha = alpha.clamp(0.0, 1.0);
    let scale = if matches!(mode, wgpu::CompositeAlphaMode::PreMultiplied) {
        alpha
    } else {
        1.0
    };
    wgpu::Color {
        r: srgb(bg.r) * scale,
        g: srgb(bg.g) * scale,
        b: srgb(bg.b) * scale,
        a: alpha,
    }
}

pub(crate) fn live_underlay_clear_color(
    bg: Rgb,
    floor: Option<f32>,
    mode: wgpu::CompositeAlphaMode,
    live_window: bool,
) -> wgpu::Color {
    if !live_window {
        return wgpu::Color::TRANSPARENT;
    }
    floor.map_or(wgpu::Color::TRANSPARENT, |alpha| {
        surface_clear_color(bg, f64::from(alpha), mode)
    })
}

pub(crate) fn use_live_pane_bases(live_window: bool, floor: Option<f32>) -> bool {
    live_window && floor.is_some()
}

pub(crate) fn inactive_pane_amount(cfg: &Config) -> f32 {
    (1.0 - cfg.inactive_color_offset)
        .clamp(0.0, 0.95)
        .max((1.0 - cfg.inactive_bg_color_offset).clamp(0.0, 0.95))
        .max((1.0 - cfg.unfocused_split_opacity).clamp(0.0, 0.95))
}

pub(crate) fn pane_inactive_amount(pane: &PaneView<'_>, count: usize, cfg: &Config) -> f32 {
    if !pane.focused && count > 1 {
        inactive_pane_amount(cfg)
    } else {
        0.0
    }
}

/// Apply dim after palette brightening and RGB resolution. The caller applies
/// minimum contrast last, against the background actually painted under it.
fn transformed_foreground(fg: Rgb, bg: Rgb, dim: bool) -> Rgb {
    if dim { color::dim(fg, bg) } else { fg }
}

#[cfg(test)]
pub(crate) fn attributed_foreground(
    fg: alacritty_terminal::vte::ansi::Color,
    bg: Rgb,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    let fg = if bold && cfg.bold_is_bright {
        color::bright_for_bold(fg)
    } else {
        fg
    };
    let fg = color::resolve(fg, theme, &Default::default());
    let fg = transformed_foreground(fg, bg, dim);
    if cfg.minimum_contrast > 1.0 {
        color::with_min_contrast(fg, bg, cfg.minimum_contrast as f64)
    } else {
        fg
    }
}

#[derive(Default)]
pub(crate) struct MinimumContrastCache {
    pub(crate) entries: std::collections::HashMap<(u32, u32, u64), Rgb>,
    pub(crate) misses: usize,
}

impl MinimumContrastCache {
    pub(crate) fn color_key(color: Rgb) -> u32 {
        (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
    }

    pub(crate) fn resolve(&mut self, fg: Rgb, bg: Rgb, minimum: f64) -> Rgb {
        if minimum <= 1.0 {
            return fg;
        }
        let key = (Self::color_key(fg), Self::color_key(bg), minimum.to_bits());
        if let Some(resolved) = self.entries.get(&key) {
            return *resolved;
        }
        let resolved = color::with_min_contrast(fg, bg, minimum);
        self.entries.insert(key, resolved);
        self.misses += 1;
        resolved
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.misses = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CellHighlight {
    None,
    Selection,
    Search(bool),
}

#[cfg(test)]
pub(crate) fn resolved_cell_foreground(
    fg: Rgb,
    bg: Rgb,
    highlight: CellHighlight,
    dim: bool,
    bold: bool,
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
) -> Rgb {
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};

    let spec = |c: Rgb| {
        AnsiColor::Spec(AnsiRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        })
    };
    match highlight {
        CellHighlight::Search(true) => attributed_foreground(
            spec(cfg.search_foreground.unwrap_or(theme.background)),
            cfg.search_background.unwrap_or(theme.palette[3]),
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::Search(false) | CellHighlight::Selection => attributed_foreground(
            spec(theme.selection_foreground),
            theme.selection_background,
            false,
            false,
            cfg,
            theme,
        ),
        CellHighlight::None => attributed_foreground(spec(fg), bg, dim, bold, cfg, theme),
    }
}

pub(crate) fn resolved_cell_foreground_cached(
    fg: Rgb,
    bg: Rgb,
    highlight: CellHighlight,
    attributes: (bool, bool),
    cfg: &kettle_config::Config,
    theme: &kettle_config::Theme,
    cache: &mut MinimumContrastCache,
) -> Rgb {
    let (dim, _) = attributes;
    let (fg, bg, dim) = match highlight {
        CellHighlight::Search(true) => (
            cfg.search_foreground.unwrap_or(theme.background),
            cfg.search_background.unwrap_or(theme.palette[3]),
            false,
        ),
        CellHighlight::Search(false) | CellHighlight::Selection => (
            theme.selection_foreground,
            theme.selection_background,
            false,
        ),
        CellHighlight::None => (fg, bg, dim),
    };
    let fg = transformed_foreground(fg, bg, dim);
    cache.resolve(fg, bg, cfg.minimum_contrast as f64)
}

/// Terminator parity, terminatorlib/config.py:106 + 117: compose the kettle
/// background opacity with Terminator's `background_darkness` and
/// `background_type`:
///
///   bg-type = solid (default):  alpha = background_opacity
///   bg-type = transparent:      alpha = background_opacity * background_darkness
///   bg-type = image/starfield:  same as transparent; darkness lets the
///                               backdrop show through the terminal area, not
///                               only behind the chrome
///
/// `background_darkness` runs from see-through (`0.0`) to fully covered
/// (`1.0`), because Terminator assigns it straight to the background colour's
/// alpha and its users lower it for more transparency.
/// `darkness_scales_the_backdrop_toward_see_through` pins that direction.
///
/// Parsing already clamps every input.
pub(crate) fn composed_bg_alpha(cfg: &kettle_config::Config) -> f64 {
    use kettle_config::BackgroundType;
    match cfg.background_type {
        BackgroundType::Solid => cfg.background_opacity as f64,
        // Starfield is an opaque kettle-drawn wallpaper, so the cell backgrounds
        // dim the same way as an image (darkness lets the field show through the
        // terminal area, not just behind the chrome).
        BackgroundType::Transparent | BackgroundType::Image | BackgroundType::Starfield => {
            (cfg.background_opacity as f64) * (cfg.background_darkness as f64)
        }
    }
}

/// Whether the window itself must support non-opaque presentation for the
/// configured background. Kept shared with window creation so wgpu never
/// selects an alpha mode for pixels the OS window was created unable to show.
pub fn window_requires_alpha_surface(cfg: &kettle_config::Config) -> bool {
    use kettle_config::BackgroundType;

    match cfg.background_type {
        BackgroundType::Solid | BackgroundType::Transparent => composed_bg_alpha(cfg) < 1.0,
        // Image alpha is unknown until decode. Starfield is different: its
        // back-most shader writes alpha 1 across the full surface, regardless
        // of the tint darkness used for terminal cells above it.
        BackgroundType::Image => true,
        BackgroundType::Starfield => false,
    }
}

pub(crate) fn background_has_wallpaper(cfg: &kettle_config::Config) -> bool {
    matches!(
        cfg.background_type,
        kettle_config::BackgroundType::Image | kettle_config::BackgroundType::Starfield
    )
}

/// Whether the back-most content proves alpha 1 at every surface pixel.
/// Once an opaque base covers the surface, source-over compositing cannot make
/// any later pixel translucent, regardless of the alpha of panes, glyphs,
/// inline images, or overlays.
pub(crate) fn final_scene_is_uniformly_opaque(
    cfg: &kettle_config::Config,
    opaque_wallpaper_covers_surface: bool,
) -> bool {
    match cfg.background_type {
        // The procedural wallpaper is a fullscreen oversized triangle whose
        // fragment shader always writes alpha 1. Unlike an image, there is no
        // decoded source alpha or cover-mode geometry to prove at runtime.
        kettle_config::BackgroundType::Starfield => true,
        kettle_config::BackgroundType::Image => opaque_wallpaper_covers_surface,
        kettle_config::BackgroundType::Solid | kettle_config::BackgroundType::Transparent => {
            composed_bg_alpha(cfg) >= 1.0
        }
    }
}

/// A premultiplied scene needs the fullscreen unpremultiply pass only for a
/// PostMultiplied surface and only while alpha can differ from one. At alpha 1,
/// straight and premultiplied RGB are identical.
pub(crate) fn needs_postmultiplied_presentation(
    alpha_mode: wgpu::CompositeAlphaMode,
    scene_is_uniformly_opaque: bool,
) -> bool {
    matches!(alpha_mode, wgpu::CompositeAlphaMode::PostMultiplied) && !scene_is_uniformly_opaque
}

pub(crate) fn desired_alpha_mode(
    cfg: &kettle_config::Config,
    supported: &[wgpu::CompositeAlphaMode],
) -> wgpu::CompositeAlphaMode {
    let preferred = if window_requires_alpha_surface(cfg) {
        [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Inherit,
            wgpu::CompositeAlphaMode::Opaque,
        ]
    } else {
        [
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Inherit,
        ]
    };
    preferred
        .into_iter()
        .find(|mode| supported.contains(mode))
        .unwrap_or_else(|| {
            supported
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Opaque)
        })
}

pub(crate) fn gc(c: Rgb) -> GColor {
    GColor::rgb(c.r, c.g, c.b)
}
