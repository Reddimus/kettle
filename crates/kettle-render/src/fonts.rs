use super::{
    Arc, Attrs, Config, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Renderer, Shaping,
    SwashCache, TextBuffer, fontdb,
};

/// Monochrome faces that carry text-presentation symbols, most specific first.
///
/// A colour-emoji face is deliberately absent: it is what these steer away
/// from. Only a family the system actually has is used, so an unusual install
/// keeps its existing fallback cascade rather than tofu-boxing anything.
#[cfg(target_os = "macos")]
const TEXT_SYMBOL_FAMILIES: &[&str] = &["STIX Two Math", "Apple Symbols"];

#[cfg(target_os = "windows")]
const TEXT_SYMBOL_FAMILIES: &[&str] = &["Segoe UI Symbol"];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const TEXT_SYMBOL_FAMILIES: &[&str] = &[
    "Noto Sans Symbols2",
    "Noto Sans Symbols 2",
    "Symbola",
    "DejaVu Sans",
];

/// The codepoint the candidate faces are probed with.
///
/// Being installed is not enough. A stock Ubuntu runner has DejaVu Sans and
/// DejaVu Sans does not have U+23FA, so requesting it there resolves nothing
/// and shaping falls straight back through the cascade to the colour face,
/// which is the state this whole change exists to leave. One probe is
/// deliberate rather than exhaustive: this is the codepoint the defect was
/// reported against, and a family that lacks it is not a useful text-symbol
/// face here. A host where no candidate passes keeps the cascade it had.
pub(crate) const TEXT_SYMBOL_PROBE: char = '\u{23FA}';

/// The first [`TEXT_SYMBOL_FAMILIES`] entry this system has AND that carries a
/// glyph for [`TEXT_SYMBOL_PROBE`].
pub(crate) fn resolve_text_symbol_family(font_system: &mut FontSystem) -> Option<&'static str> {
    let probe = TEXT_SYMBOL_PROBE;
    for wanted in TEXT_SYMBOL_FAMILIES.iter().copied() {
        let ids: Vec<fontdb::ID> = font_system
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(wanted))
            })
            .map(|face| face.id)
            .collect();
        for id in ids {
            // `Font::unicode_codepoints` is empty unless cosmic-text's
            // `monospace_fallback` feature is on, which it is not here, so ask
            // the face's own character map instead.
            if font_system
                .get_font(id, fontdb::Weight::NORMAL)
                .is_some_and(|font| font.as_swash().charmap().map(probe) != 0)
            {
                return Some(wanted);
            }
        }
    }
    None
}

/// VS15 may request a text glyph even for default emoji. Use a verified
/// monochrome face that covers the cluster's spacing scalars; otherwise the
/// caller can draw a replacement without changing the stored grid text.
pub(crate) fn grapheme_text_family(
    font_system: &mut FontSystem,
    primary: &str,
    base: char,
    marks: &[char],
) -> Option<String> {
    use unicode_width::UnicodeWidthChar;
    for family in std::iter::once(primary).chain(TEXT_SYMBOL_FAMILIES.iter().copied()) {
        let ids: Vec<_> = font_system
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
            })
            .map(|face| face.id)
            .collect();
        for id in ids {
            if font_system
                .get_font(id, fontdb::Weight::NORMAL)
                .is_some_and(|font| {
                    let face = font.as_swash();
                    if [b"COLR", b"CBDT", b"sbix", b"SVG "]
                        .iter()
                        .any(|tag| face.table(u32::from_be_bytes(**tag)).is_some())
                    {
                        return false;
                    }
                    let cmap = face.charmap();
                    std::iter::once(base)
                        .chain(marks.iter().copied())
                        .filter(|c| c.width().is_some_and(|width| width > 0))
                        .all(|c| cmap.map(c) != 0)
                })
            {
                return Some(family.to_owned());
            }
        }
    }
    None
}

/// Whether this codepoint is emoji-capable, text by default, and occupies one
/// column.
///
/// Derived from the width table rather than a vendored `Emoji_Presentation`
/// list. Every `Emoji_Presentation=Yes` codepoint is East Asian Wide, so
/// `unicode-width` reports two columns for it, and `unicode-width` also widens
/// an emoji-capable codepoint to two when U+FE0F follows. One column alone and
/// two with U+FE0F therefore means `Emoji=Yes` with `Emoji_Presentation=No`.
/// `⏺` U+23FA is one; `⏰` U+23F0 is not, because it is already two columns.
///
/// **One column is part of the rule, not an accident of the derivation.** A
/// handful of default-text emoji are East Asian Wide and stay at two columns
/// with or without U+FE0F, U+3030 and U+1F202 among them, and width cannot tell
/// those apart from `Emoji_Presentation=Yes`. They are out of scope here: a
/// colour glyph in a two-column cell occupies the space it was given, so it
/// neither overflows nor misaligns anything. This guards against a two-cell
/// colour bitmap painted into a one-cell slot.
///
/// Nothing here decides how a cell is drawn on its own. It only says which
/// cells must not be handed to a colour-emoji face, which is a font-selection
/// question the shaper answers.
pub(crate) fn single_column_text_presentation(ch: char) -> bool {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    // ASCII is the overwhelming majority of terminal output and none of it is
    // emoji-capable, so this is the early-out that keeps the row walk cheap.
    if ch.is_ascii() || ch.width() != Some(1) {
        return false;
    }
    // At most four bytes for the base plus three for U+FE0F.
    let mut buf = [0u8; 8];
    let base = ch.encode_utf8(&mut buf).len();
    let mut selector = [0u8; 4];
    let vs16 = '\u{FE0F}'.encode_utf8(&mut selector).len();
    buf[base..base + vs16].copy_from_slice(&selector[..vs16]);
    std::str::from_utf8(&buf[..base + vs16]).is_ok_and(|qualified| qualified.width() == 2)
}

pub(crate) fn load_bundled_font(font_system: &mut FontSystem, face: &'static [u8]) {
    font_system
        .db_mut()
        .load_font_source(fontdb::Source::Binary(Arc::new(face)));
}

impl Renderer {
    /// Line height used by chrome text buffers. Terminal rows may be shorter
    /// or taller via `cell-height`, so overlay geometry must not infer this
    /// value from `cell_h` when budgeting safety text.
    pub fn overlay_text_line_height(&self) -> f32 {
        self.metrics.line_height
    }

    /// Measured advance used by chrome text. Terminal columns may be narrowed
    /// or widened via `cell-width`, but overlay glyphs keep their natural
    /// advance, so safety-copy budgets must not divide by `cell_w` directly.
    pub fn overlay_text_cell_width(&self) -> f32 {
        self.cell_w / self.cell_scale_w.max(0.01)
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.last_scene = None;
        self.font_size = clamp_font_size(size);
        // Re-derive physical metrics at the current DPI scale so a
        // font-size change (zoom, reload) keeps HiDPI scaling applied.
        self.metrics = metrics_for(self.font_size, self.scale);
        self.remeasure_cell();
    }

    /// The current *logical* font size (the user-facing pt value, before the
    /// DPI multiply). Zoom keybinds step this rather than
    /// back-deriving it from the now-physical `cell_h`, which would otherwise
    /// double-apply the scale factor.
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// The face this renderer requests for codepoints Unicode renders as text,
    /// or `None` when the system has no candidate that carries them.
    ///
    /// Surfaced through `ui_geometry` because the answer depends entirely on
    /// which fonts the host has. A verification scenario that asserts the
    /// bullet is monochrome is asserting something the host may be unable to
    /// produce, and it should say so rather than fail.
    pub fn text_presentation_face(&self) -> Option<&'static str> {
        self.text_symbol_family
    }

    /// Update the device-pixel scale factor (DPI). Wired to winit's
    /// `ScaleFactorChanged`, fired at startup and whenever the window moves to
    /// a monitor with a different scale. Recomputes physical metrics from the
    /// unchanged *logical* `font_size` and re-measures the cell, so glyphs keep
    /// the same visual size across DPI changes. No-op when the scale is
    /// unchanged. The caller must re-grid afterward (cell_w/cell_h change),
    /// e.g. via `App::resize_all`.
    pub fn set_scale(&mut self, scale: f32) {
        self.last_scene = None;
        let s = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if (self.scale - s).abs() < f32::EPSILON {
            return;
        }
        self.scale = s;
        self.metrics = metrics_for(self.font_size, s);
        self.remeasure_cell();
    }

    /// Update the primary font family and re-measure the cell. Called by
    /// `reload_config` so a `font-family = …` change in the user's config
    /// takes effect at runtime, not only the `font-size` part of a reload.
    pub fn set_font_family(&mut self, family: String) {
        self.last_scene = None;
        if self.font_family.as_ref() == family.as_str() {
            return;
        }
        self.font_family = family.into();
        self.remeasure_cell();
    }

    /// Re-derive `cell_w`/`cell_h` from the current `font_family` + `metrics`
    /// using the same measurer the constructor used. Extracted so font-size
    /// and font-family updates share one implementation; otherwise the two
    /// setters would drift on which fields they touch.
    pub(crate) fn remeasure_cell(&mut self) {
        let family = self.font_family.clone();
        let m = self.metrics;
        let mut measure = TextBuffer::new(&mut self.font_system, m);
        let (cw, ch) = measure_cell(&mut self.font_system, &mut measure, &family, m);
        // Apply the configured cell_width/cell_height
        // multipliers AFTER measurement so a font-family or font-
        // size change preserves the user's chosen scale.
        self.cell_w = cw * self.cell_scale_w.max(0.01);
        self.cell_h = ch * self.cell_scale_h.max(0.01);
        // The cell-locked glyph cache keys bake in the physical font size; a
        // size / scale / family change makes every cached slot stale, so drop
        // them (the atlas textures keep their allocation; the packer resets).
        self.glyph_pipeline.clear();
        self.swash = SwashCache::new();
        self.grid_glyphs_dirty = true;
    }

    /// Terminator parity (`cell_width` / `cell_height`):
    /// update the cell-scale multipliers + re-measure. Called by the
    /// App's `reload_config` path when the user reloads with a new
    /// `cell-width` / `cell-height` value. No-op when the requested
    /// scale matches the current one.
    pub fn set_cell_scale(&mut self, w: f32, h: f32) {
        self.last_scene = None;
        let w = w.max(0.01);
        let h = h.max(0.01);
        if (self.cell_scale_w - w).abs() < f32::EPSILON
            && (self.cell_scale_h - h).abs() < f32::EPSILON
        {
            return;
        }
        self.cell_scale_w = w;
        self.cell_scale_h = h;
        self.remeasure_cell();
    }

    pub(crate) fn ensure_bundled_style_faces(&mut self) {
        if self.bundled_style_faces_loaded {
            return;
        }
        for face in [
            kettle_config::font::BOLD,
            kettle_config::font::ITALIC,
            kettle_config::font::BOLD_ITALIC,
        ] {
            load_bundled_font(&mut self.font_system, face);
        }
        self.bundled_style_faces_loaded = true;
        self.pane_style_keys.fill(0);
        self.pane_line_keys.iter_mut().for_each(Vec::clear);
        self.chrome_style_key = 0;
    }
}

/// OpenType features to shape pane text with: the coarse ligature toggle
/// expressed as `liga/clig/calt/dlig = 0` when off, then the user's explicit
/// `font-feature` overrides applied on top (so they can re-enable or tune
/// individual features). Cited: Ghostty `font-feature`, kitty `font_features`.
pub(crate) fn font_features(cfg: &Config) -> FontFeatures {
    let mut ff = FontFeatures::new();
    if !cfg.font_ligatures {
        for tag in [b"liga", b"clig", b"calt", b"dlig"] {
            ff.disable(FeatureTag::new(tag));
        }
    }
    for f in &cfg.font_features {
        ff.set(FeatureTag::new(&f.tag), f.value);
    }
    ff
}

/// Sanitize a font size against the renderer's safe range. 5.0 is the
/// floor below which cosmic-text's metrics become numerically unstable
/// (sub-pixel cell dims, antialiasing falls apart); 72.0 is the ceiling
/// above which a typical 1080p window's worth of cells exceeds the wgpu
/// 8192-px-per-side texture limit. Shared by `Renderer::new` and
/// `set_font_size` so the startup path and the runtime path can't
/// drift on which sizes they accept. Pure so the bounds are unit-tested
/// without standing up wgpu.
pub fn clamp_font_size(size: f32) -> f32 {
    // `f32::clamp` passes NaN through, and cosmic-text would silently turn it
    // into zero-sized cells. Route NaN to the floor instead.
    if size.is_nan() {
        return 5.0;
    }
    size.clamp(5.0, 72.0)
}

/// Build glyphon [`Metrics`] for a *logical* `font_size` at a given
/// device-pixel `scale` (the window's `scale_factor`). glyphon shapes and
/// rasterizes in the same coordinate space as the wgpu surface, which winit
/// sizes in **physical** pixels, so a logical `font_size` must be multiplied
/// by the scale factor or text renders at `1/scale` of its intended size on
/// HiDPI displays. The line height is 1.25 times the size. `scale` is
/// sanitized (NaN / ≤0 → 1.0) so a bogus value can't produce zero-sized cells.
pub fn metrics_for(font_size: f32, scale: f32) -> Metrics {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let px = font_size * s;
    Metrics::new(px, px * 1.25)
}

/// Metrics for a terminal PANE's text buffer. The glyph size stays
/// `font_size` (the DPI-scaled px from `metrics_for`), but the LINE HEIGHT is
/// the grid's actual row height `cell_h` — which already folds in the 1.25
/// line-height ratio AND any `cfg.cell_height` multiplier. The
/// cursor and selection/vi quads step by `cell_h` per row (`by = oy + line *
/// ch`), so the text must advance by the same `cell_h`; laying it out at the
/// unscaled `metrics.line_height` instead drifts a fraction of a row per line —
/// a full row off near the bottom of a tall window whenever `cell_height != 1`.
pub fn pane_metrics(font_size: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_size, cell_h)
}

/// The fonts and cell size a window needs before its GPU exists.
///
/// A pane's grid depends only on the cell size, so loading these first lets
/// the first pane start while the GPU initializes. A renderer built with them
/// takes them over instead of enumerating the system fonts a second time.
pub struct StartupFonts {
    pub(crate) font_system: FontSystem,
    pub(crate) text_symbol_family: Option<&'static str>,
    /// Physical-pixel cell size, with `cell-width` and `cell-height` applied.
    pub cell: (f32, f32),
    /// The inputs `cell` was measured from.
    key: StartupFontsKey,
}

#[derive(Clone, Debug, PartialEq)]
struct StartupFontsKey {
    family: String,
    font_size: f32,
    scale: f32,
    cell_width: f32,
    cell_height: f32,
}

impl StartupFontsKey {
    fn new(cfg: &Config, scale: f32) -> Self {
        Self {
            family: cfg.font_family.clone(),
            font_size: clamp_font_size(cfg.font_size),
            scale,
            cell_width: cfg.cell_width,
            cell_height: cfg.cell_height,
        }
    }
}

/// The system fonts, before the display's scale is known.
///
/// Nothing here depends on the scale, so these can be loaded on another
/// thread while the event loop starts. [`PreparedFonts::measure`] then
/// measures the cell once the scale is known.
pub struct PreparedFonts {
    font_system: FontSystem,
    text_symbol_family: Option<&'static str>,
    /// Families [`PreparedFonts::warm_family`] has already shaped.
    pub(crate) warm: Vec<String>,
}

impl PreparedFonts {
    /// Enumerate the system fonts, load the bundled face, and resolve the
    /// text-presentation face.
    pub fn enumerate() -> Self {
        let t_font_system = std::time::Instant::now();
        let mut font_system = FontSystem::new();
        let font_system_ms = t_font_system.elapsed().as_secs_f64() * 1000.0;
        let t_bundled = std::time::Instant::now();
        load_bundled_font(&mut font_system, kettle_config::font::REGULAR);
        // Resolved once: fontdb is already populated and the answer cannot
        // change while the renderer lives.
        let text_symbol_family = resolve_text_symbol_family(&mut font_system);
        log::info!("renderer init: text-presentation face {text_symbol_family:?}");
        // Split, because `FontSystem::new()` is the one people suspect (it
        // enumerates system fonts) and a combined figure cannot exonerate it.
        log::info!(
            "renderer init: FontSystem::new {font_system_ms:.1}ms, bundled font {:.1}ms",
            t_bundled.elapsed().as_secs_f64() * 1000.0
        );
        Self {
            font_system,
            text_symbol_family,
            warm: Vec::new(),
        }
    }

    /// Shape the cell probe in `family` once, so that measuring it later
    /// finds cosmic-text's font matches and faces already loaded: those for
    /// the default attributes every text buffer starts with, and those for
    /// `family`.
    ///
    /// Bold is not warmed. The first styled cell loads the bundled bold
    /// faces, and adding a face clears cosmic-text's match cache.
    pub fn warm_family(&mut self, family: &str) {
        if self.warm.iter().any(|warm| warm == family) {
            return;
        }
        // Matches and faces do not depend on the size, so any metrics do.
        let metrics = metrics_for(13.0, 1.0);
        let mut probe = TextBuffer::new(&mut self.font_system, metrics);
        measure_cell(&mut self.font_system, &mut probe, family, metrics);
        self.warm.push(family.to_owned());
    }

    /// Measure the cell for `cfg` at `scale`, the display's scale factor.
    pub fn measure(self, cfg: &Config, scale: f32) -> StartupFonts {
        let Self {
            mut font_system,
            text_symbol_family,
            ..
        } = self;
        let key = StartupFontsKey::new(cfg, scale);
        let metrics = metrics_for(key.font_size, scale);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cell_w, cell_h) = measure_cell(&mut font_system, &mut measure, &key.family, metrics);
        // `cell-width` and `cell-height` are clamped to [0.5, 3.0] at parse
        // time, so the cell cannot degenerate to 0 here.
        let cell = (
            cell_w * key.cell_width.max(0.01),
            cell_h * key.cell_height.max(0.01),
        );
        StartupFonts {
            font_system,
            text_symbol_family,
            cell,
            key,
        }
    }
}

// The font preload builds these on its own thread and hands them to the
// event-loop thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<PreparedFonts>();
    assert_send::<StartupFonts>();
};

impl StartupFonts {
    /// Enumerate the system fonts, load the bundled face, and measure the
    /// cell for `cfg` at `scale`, the display's scale factor.
    pub fn load(cfg: &Config, scale: f32) -> Self {
        PreparedFonts::enumerate().measure(cfg, scale)
    }

    /// These fonts, measured for `cfg` at `scale` instead. The system fonts
    /// are not enumerated again.
    pub fn remeasure(self, cfg: &Config, scale: f32) -> Self {
        let Self {
            font_system,
            text_symbol_family,
            ..
        } = self;
        PreparedFonts {
            font_system,
            text_symbol_family,
            warm: Vec::new(),
        }
        .measure(cfg, scale)
    }

    /// Whether these fonts were measured for `cfg` at `scale`.
    pub fn matches(&self, cfg: &Config, scale: f32) -> bool {
        self.key == StartupFontsKey::new(cfg, scale)
    }
}

pub(crate) fn measure_cell(
    fs: &mut FontSystem,
    buf: &mut TextBuffer,
    family: &str,
    metrics: Metrics,
) -> (f32, f32) {
    buf.set_metrics(metrics);
    // Size the measure box from the (physical) metrics so the probe never
    // wraps. At a large font on a high-DPI display the physical font size can
    // be ~200px, making the 10-glyph probe ~1300px wide. If it wrapped,
    // `line_w` would cover only the first line and `cell_w` would come out too
    // narrow, mis-gridding the terminal. A monospace `M` is ~0.6em, so 10 fit
    // in ~6em; 20em + slack never wraps at any size or scale.
    let box_w = metrics.font_size * 20.0 + 100.0;
    let box_h = metrics.line_height * 2.0 + 100.0;
    buf.set_size(Some(box_w), Some(box_h));
    buf.set_text(
        "MMMMMMMMMM",
        &Attrs::new().family(Family::Name(family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(fs, false);
    let mut w = metrics.font_size * 0.6;
    if let Some(run) = buf.layout_runs().next()
        && run.line_w > 0.0
    {
        w = run.line_w / 10.0;
    }
    (w, metrics.line_height)
}
