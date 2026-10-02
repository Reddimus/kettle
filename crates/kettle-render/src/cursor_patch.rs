//! The cursor patch: the pixels a blinking cursor changes, rendered once so a
//! Core Animation layer can blink them while Kettle submits no GPU work.
//!
//! Right after an off-phase frame is on screen,
//! [`Renderer::present_cursor_patch`] encodes the same scene twice more into
//! targets that hold only the patch rect, cursor on (`Pc`) and cursor off
//! (`Pd`). Both passes are [`Renderer::encode_scene_pass`] with a viewport
//! that maps the whole scene 1:1 onto the small target, so every pipeline
//! keeps its full-size screen uniform and nothing is uploaded. A combine pass
//! then writes `vec4(Pc.rgb, 1)` where the two differ and transparent black
//! elsewhere. Composited over the off frame that is the on frame: an opaque
//! pixel is the on frame's, and a clear one shows the off frame, which equals
//! the on frame wherever the cursor changed nothing. The scene targets hold
//! premultiplied colour, and `Pc.rgb` is what each window shows: an `Opaque`
//! surface displays the scene's RGB as stored, and on an alpha surface every
//! changed pixel has alpha 1 (the first condition below), where premultiplied
//! and straight colour are the same. The mask needs no prediction of what the
//! cursor rasterizes to.
//!
//! It is exact under three conditions, and a frame that may break one is
//! [`CursorPatchIneligible`]:
//!
//! - Every changed pixel is opaque in the on frame, so the patch's alpha of 1
//!   matches it on a translucent window. An inverted glyph whose ink leaves
//!   the block breaks this.
//! - `Pc` and `Pd` are exact crops of the full frames. Flat quads and glyphs
//!   placed on whole pixels crop exactly. Two things do not: a quad edge
//!   within 1/256 px of a pixel centre, because the rasterizer snaps to
//!   1/256 px and the offset viewport rounds the vertex differently before
//!   the snap; and interpolated shading (linearly filtered images, the
//!   outlines' SDF antialiasing, the starfield's fragment position).
//! - The combine's sRGB decode and re-encode returns every byte unchanged.
//!
//! The patch is presented only on macOS, where the UI owns the layer; other
//! platforms render it only in tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(target_os = "macos")]
use anyhow::anyhow;
use kettle_config::{BackgroundType, Config, Rgb};
use kettle_core::{GraphicsBudget, GraphicsReservation};

use crate::Renderer;
use crate::outline::OutlineInstance;
use crate::quad::QuadInstance;

/// The largest patch, in cells of the current font, in each direction.
const MAX_PATCH_CELLS: f32 = 4.0;
/// The largest patch side in pixels, whatever the font.
const MAX_PATCH_SIDE: u32 = 1024;
/// The rasterizer's vertex snap: 8 bits of subpixel precision.
const SNAP_STEP: f32 = 1.0 / 256.0;

const COMBINE_SHADER: &str = r#"
@group(0) @binding(0) var cursor_on: texture_2d<f32>;
@group(0) @binding(1) var cursor_off: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole target.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(corners[vertex], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(position.xy);
    let on = textureLoad(cursor_on, texel, 0);
    let off = textureLoad(cursor_off, texel, 0);
    if all(on == off) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(on.rgb, 1.0);
}
"#;

/// Where the patch sits in the window's surface, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatchRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What [`Renderer::present_cursor_patch`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPatchOutcome {
    /// The patch is on the cursor layer. Place the layer over this rect.
    Presented(PatchRect),
    /// The last frame cannot hand its blink to the layer; blink on the GPU.
    Ineligible(CursorPatchIneligible),
    /// The layer could not take a frame now; blink on the GPU. The UI must
    /// hide the layer, which may still show its last presented patch.
    Failed(CursorPatchFailure),
}

/// Why the last frame cannot hand its blink to the cursor layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPatchIneligible {
    /// No cursor layer is attached.
    NoLayer,
    /// The offset-viewport rules have not been measured for this GPU.
    UnmeasuredGpu,
    /// The surface's alpha convention is Auto or Inherit.
    AlphaMode,
    /// No presented frame is available, or renderer state changed afterward.
    NoFrame,
    /// The last frame drew the cursor. A hand-off starts in the off phase.
    PhaseOn,
    /// `cfg` is not the configuration the last frame was drawn with.
    ConfigChanged,
    /// No blinking cursor was drawn: unfocused, hidden, off screen, vi mode.
    NoCursor,
    /// The cursor is not a single block, beam or underline quad.
    Shape,
    /// A starfield or image wallpaper lies under every pixel, and neither
    /// renders exactly in a patch target.
    Background,
    /// The patch lies outside the surface.
    Empty,
    /// The patch is wider or taller than four cells or 1024 pixels.
    TooLarge,
    /// On a translucent window the inverted glyph's ink leaves the block, so
    /// a changed pixel is not opaque.
    TranslucentOverhang,
    /// An image or a pane outline overlaps the patch.
    Overlap,
    /// A quad edge in the patch lies within 1/256 px of a pixel centre.
    SnapBoundary,
}

impl CursorPatchIneligible {
    /// A stable name for logs and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLayer => "no_layer",
            Self::UnmeasuredGpu => "unmeasured_gpu",
            Self::AlphaMode => "alpha_mode",
            Self::NoFrame => "no_frame",
            Self::PhaseOn => "phase_on",
            Self::ConfigChanged => "config_changed",
            Self::NoCursor => "no_cursor",
            Self::Shape => "shape",
            Self::Background => "background",
            Self::Empty => "empty",
            Self::TooLarge => "too_large",
            Self::TranslucentOverhang => "translucent_overhang",
            Self::Overlap => "overlap",
            Self::SnapBoundary => "snap_boundary",
        }
    }
}

/// Why the cursor layer could not take the patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPatchFailure {
    /// The window is occluded, so Metal vends no drawable.
    Occluded,
    /// No drawable arrived in time.
    Timeout,
    /// The layer's surface needed reconfiguring; the next hand-off does it.
    Outdated,
    /// The layer's surface is gone and was dropped; attach the layer again.
    Lost,
    /// wgpu rejected the drawable or a patch pass.
    Validation,
    /// The graphics budget refused the patch targets.
    Budget,
}

impl CursorPatchFailure {
    /// A stable name for logs and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Occluded => "occluded",
            Self::Timeout => "timeout",
            Self::Outdated => "outdated",
            Self::Lost => "lost",
            Self::Validation => "validation",
            Self::Budget => "budget",
        }
    }
}

/// Only Apple Metal adapters may present patches. Intel/AMD Macs keep GPU
/// blink until their offset-viewport rasterization has been measured.
fn measured_layer_adapter(name: &str, backend: wgpu::Backend) -> bool {
    // wgpu 30 Metal leaves vendor/device IDs at zero and uses MTLDevice.name.
    name.starts_with("Apple ") && backend == wgpu::Backend::Metal
}

/// What a frame's scene pass drew, recorded once the frame is on screen so a
/// hand-off can encode the same scene again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SceneFacts {
    pub(crate) target_size: [u32; 2],
    /// The flag that pass ran with: the live pass, or the headless capture.
    pub(crate) live_window: bool,
    /// Whether that pass drew the cursor.
    pub(crate) cursor_on: bool,
    /// Whether the frame's pixels can be translucent on screen.
    pub(crate) translucent: bool,
    /// The surface convention the live frame used, also used by the combine.
    pub(crate) alpha_mode: wgpu::CompositeAlphaMode,
    /// The two `Config` values `encode_scene_pass` reads.
    pub(crate) background: Rgb,
    pub(crate) background_type: BackgroundType,
}

impl SceneFacts {
    fn drawn_with(&self, cfg: &Config) -> bool {
        cfg.theme.background == self.background && cfg.background_type == self.background_type
    }
}

/// A half-open box of whole pixels, `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelBox {
    pub(crate) x0: i64,
    pub(crate) y0: i64,
    pub(crate) x1: i64,
    pub(crate) y1: i64,
}

impl PixelBox {
    /// Every pixel a rect at `pos` of `size` can touch.
    pub(crate) fn bounding(pos: [f32; 2], size: [f32; 2]) -> Self {
        Self {
            x0: pos[0].floor() as i64,
            y0: pos[1].floor() as i64,
            x1: (pos[0] + size[0]).ceil() as i64,
            y1: (pos[1] + size[1]).ceil() as i64,
        }
    }

    /// The pixels whose centres a rect covers under the top-left rule, which
    /// are the pixels the rasterizer fills for a quad.
    pub(crate) fn covered(pos: [f32; 2], size: [f32; 2]) -> Self {
        Self {
            x0: (pos[0] - 0.5).ceil() as i64,
            y0: (pos[1] - 0.5).ceil() as i64,
            x1: ((pos[0] + size[0]) - 0.5).ceil() as i64,
            y1: ((pos[1] + size[1]) - 0.5).ceil() as i64,
        }
    }

    fn surface(size: [u32; 2]) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: i64::from(size[0]),
            y1: i64::from(size[1]),
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    fn width(self) -> i64 {
        self.x1 - self.x0
    }

    fn height(self) -> i64 {
        self.y1 - self.y0
    }

    fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn intersect(self, other: Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    fn intersects(self, other: Self) -> bool {
        !self.intersect(other).is_empty()
    }

    pub(crate) fn contains(self, other: Self) -> bool {
        other.is_empty()
            || (self.x0 <= other.x0
                && self.y0 <= other.y0
                && other.x1 <= self.x1
                && other.y1 <= self.y1)
    }

    fn grow(self, by: i64) -> Self {
        Self {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// Whether a quad edge at `edge` covers the same pixels in a patch target as
/// in the full frame. The offset viewport rounds the vertex differently
/// before the rasterizer snaps it to 1/256 px, so an edge within one snap step
/// of a pixel centre can land on either side of it. An edge exactly on a
/// centre snaps to it both times.
pub(crate) fn edge_is_snap_stable(edge: f32) -> bool {
    if !edge.is_finite() {
        return false;
    }
    let from_centre = (edge - 0.5) - (edge - 0.5).round();
    from_centre == 0.0 || from_centre.abs() > SNAP_STEP
}

/// Whether `quad` rasterizes the same inside `near` in a patch target as in
/// the full frame: every edge that crosses `near` is snap-stable.
fn quad_is_snap_stable_near(quad: &QuadInstance, near: PixelBox) -> bool {
    if !PixelBox::bounding(quad.pos, quad.size).intersects(near) {
        return true;
    }
    let x_edges = [quad.pos[0], quad.pos[0] + quad.size[0]];
    let y_edges = [quad.pos[1], quad.pos[1] + quad.size[1]];
    let crosses = |edge: f32, lo: i64, hi: i64| (lo as f32..=hi as f32).contains(&edge);
    x_edges
        .into_iter()
        .filter(|&edge| crosses(edge, near.x0, near.x1))
        .all(edge_is_snap_stable)
        && y_edges
            .into_iter()
            .filter(|&edge| crosses(edge, near.y0, near.y1))
            .all(edge_is_snap_stable)
}

/// Whether a pane outline can paint inside `near`. Its stroke and ramp stay
/// within the border width plus the corner radius of its edges; the interior
/// adds exactly nothing.
fn outline_touches(outline: &OutlineInstance, near: PixelBox) -> bool {
    if outline.border_width <= 0.0 {
        return false;
    }
    if !PixelBox::bounding(outline.pos, outline.size)
        .grow(2)
        .intersects(near)
    {
        return false;
    }
    // The stroke is a band of `border_width` inside every edge, with 2 px for
    // antialiasing.
    let [x, y] = outline.pos;
    let [width, height] = outline.size;
    let edge = outline.border_width + 2.0;
    let interior = PixelBox::covered(
        [x + edge, y + edge],
        [width - 2.0 * edge, height - 2.0 * edge],
    );
    if !interior.contains(near) {
        return true;
    }
    // A rounded corner bends the band inward, but only at the corners the
    // mask rounds (a pane's bottom window corners). Insetting every side by
    // the radius would refuse every cursor in the first row or column.
    let reach = outline.corner_radius.max(0.0) + edge;
    [
        (OUTLINE_CORNER_TOP_LEFT, x, y),
        (OUTLINE_CORNER_TOP_RIGHT, x + width - reach, y),
        (
            OUTLINE_CORNER_BOTTOM_RIGHT,
            x + width - reach,
            y + height - reach,
        ),
        (OUTLINE_CORNER_BOTTOM_LEFT, x, y + height - reach),
    ]
    .into_iter()
    .any(|(bit, corner_x, corner_y)| {
        outline.corner_mask & bit != 0
            && PixelBox::bounding([corner_x, corner_y], [reach, reach]).intersects(near)
    })
}

/// `OutlineInstance::corner_mask` bits: top-left, top-right, bottom-right,
/// bottom-left.
const OUTLINE_CORNER_TOP_LEFT: u32 = 1 << 0;
const OUTLINE_CORNER_TOP_RIGHT: u32 = 1 << 1;
const OUTLINE_CORNER_BOTTOM_RIGHT: u32 = 1 << 2;
const OUTLINE_CORNER_BOTTOM_LEFT: u32 = 1 << 3;
const _: () = assert!(
    OUTLINE_CORNER_BOTTOM_RIGHT == crate::OUTLINE_BOTTOM_RIGHT
        && OUTLINE_CORNER_BOTTOM_LEFT == crate::OUTLINE_BOTTOM_LEFT
);

/// The clip glyphon applies to a text area: its `TextBounds`, built from the
/// pane rect as the cursor-glyph prepare builds them, clamped to the surface.
fn glyphon_bounds(clip: (f32, f32, f32, f32), target_size: [u32; 2]) -> PixelBox {
    let x0 = (clip.0 as i32).max(0);
    let y0 = (clip.1 as i32).max(0);
    let x1 = ((clip.0 + clip.2) as i32)
        .min(target_size[0] as i32)
        .max(x0);
    let y1 = ((clip.1 + clip.3) as i32)
        .min(target_size[1] as i32)
        .max(y0);
    PixelBox {
        x0: x0.into(),
        y0: y0.into(),
        x1: x1.into(),
        y1: y1.into(),
    }
}

/// The patch's combine pipeline and its retained on and off targets.
pub(crate) struct PatchPipeline {
    shader: wgpu::ShaderModule,
    bind_group_layout: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    /// One combine pipeline per target format: the layer's, or the scene's in
    /// tests.
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    targets: Option<PatchTargets>,
}

struct PatchTargets {
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    on_view: wgpu::TextureView,
    off_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    _textures: [wgpu::Texture; 2],
    _gpu: GraphicsReservation,
}

impl PatchPipeline {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-cursor-patch"),
            source: wgpu::ShaderSource::Wgsl(COMBINE_SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kettle-cursor-patch-bgl"),
            entries: &[texture(0), texture(1)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kettle-cursor-patch-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        Self {
            shader,
            bind_group_layout,
            layout,
            pipelines: Vec::new(),
            targets: None,
        }
    }

    fn ensure_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipelines.iter().any(|(built, _)| *built == format) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kettle-cursor-patch-pipeline"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((format, pipeline));
    }

    fn bind_group(
        &self,
        device: &wgpu::Device,
        on: &wgpu::TextureView,
        off: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kettle-cursor-patch-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(on),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(off),
                },
            ],
        })
    }

    /// Keep on and off targets of `format` and this size, reusing the last
    /// pair when it matches. `false` when the graphics budget refuses them.
    fn ensure_targets(
        &mut self,
        device: &wgpu::Device,
        budget: &GraphicsBudget,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> bool {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.format == format && t.width == width && t.height == height)
        {
            return true;
        }
        self.targets = None;
        let Some(bytes) = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(8))
            .and_then(|bytes| usize::try_from(bytes).ok())
        else {
            return false;
        };
        let Some(gpu) = budget.reserve_gpu(bytes) else {
            return false;
        };
        let texture = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let on = texture("kettle-cursor-patch-on");
        let off = texture("kettle-cursor-patch-off");
        let on_view = on.create_view(&wgpu::TextureViewDescriptor::default());
        let off_view = off.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.bind_group(device, &on_view, &off_view);
        self.targets = Some(PatchTargets {
            format,
            width,
            height,
            on_view,
            off_view,
            bind_group,
            _textures: [on, off],
            _gpu: gpu,
        });
        true
    }

    /// Write the patch of `bind_group`'s on and off frames into `target`,
    /// which must be their size.
    fn encode_combine(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        bind_group: &wgpu::BindGroup,
    ) -> Result<(), CursorPatchFailure> {
        let Some((_, pipeline)) = self.pipelines.iter().find(|(built, _)| *built == format) else {
            return Err(CursorPatchFailure::Validation);
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-cursor-patch-combine"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// The surface on the Core Animation layer that shows the patch.
#[cfg(target_os = "macos")]
pub(crate) struct CursorLayerSurface {
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
    configured: Option<[u32; 2]>,
}

#[cfg(target_os = "macos")]
impl CursorLayerSurface {
    /// Size the layer's drawables to the patch, unless they already are.
    fn fit(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.configured == Some([width, height]) {
            return;
        }
        self.surface.configure(
            device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                color_space: self.color_space,
                width,
                height,
                present_mode: wgpu::PresentMode::Fifo,
                // Not opaque: the patch is clear wherever the cursor changes
                // nothing, and every other pixel has alpha 1.
                alpha_mode: wgpu::CompositeAlphaMode::PostMultiplied,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
        self.configured = Some([width, height]);
    }
}

impl Renderer {
    /// Show later cursor patches on `layer`, a Core Animation layer the UI
    /// placed over this window's Metal layer. Replaces an earlier layer.
    ///
    /// # Safety
    ///
    /// `layer` must point to a live `CAMetalLayer`. The surface retains it,
    /// and the renderer uses it only on the thread that renders this window.
    #[cfg(target_os = "macos")]
    pub unsafe fn attach_cursor_layer(
        &mut self,
        layer: std::ptr::NonNull<std::ffi::c_void>,
    ) -> anyhow::Result<()> {
        self.cursor_layer = None;
        let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer.as_ptr());
        // SAFETY: the caller guarantees `layer` is a live CAMetalLayer.
        let surface = unsafe { self.gpu.instance.create_surface_unsafe(target) }?;
        let caps = surface.get_capabilities(&self.gpu.adapter);
        // The main surface's format when the layer takes it, so both layers
        // hold the same pixel format; otherwise the layer's first sRGB one.
        let format = if caps.formats.contains(&self.config.format) {
            self.config.format
        } else {
            caps.formats
                .iter()
                .copied()
                .find(|format| format.is_srgb())
                .ok_or_else(|| anyhow!("the cursor layer offers no sRGB format"))?
        };
        if !caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
            || !caps.present_modes.contains(&wgpu::PresentMode::Fifo)
            || !caps.usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            return Err(anyhow!("the cursor layer cannot present a clear patch"));
        }
        self.cursor_layer = Some(CursorLayerSurface {
            surface,
            format,
            color_space: self.config.color_space,
            configured: None,
        });
        Ok(())
    }

    /// Stop presenting to the cursor layer and free the patch targets.
    /// The UI must hide the layer first: detaching does not clear its contents.
    pub fn detach_cursor_layer(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.cursor_layer = None;
        }
        self.cursor_patch = None;
    }

    fn has_cursor_layer(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.cursor_layer.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Render the cursor patch of the frame on screen and present it on the
    /// cursor layer.
    ///
    /// Call it right after a `render_frame` that returned
    /// [`FrameOutcome::Presented`](crate::FrameOutcome::Presented) in the
    /// cursor's off phase, with the same `cfg`. It uploads nothing and never
    /// changes what the window shows. The UI must keep the layer hidden unless
    /// the latest call returned `Presented`, including after a failure or detach.
    pub fn present_cursor_patch(&mut self, cfg: &Config) -> CursorPatchOutcome {
        if !self.has_cursor_layer() {
            return CursorPatchOutcome::Ineligible(CursorPatchIneligible::NoLayer);
        }
        let info = self.gpu.adapter.get_info();
        if !measured_layer_adapter(&info.name, info.backend) {
            return CursorPatchOutcome::Ineligible(CursorPatchIneligible::UnmeasuredGpu);
        }
        let (rect, facts) = match self.plan_cursor_patch(cfg) {
            Ok(plan) => plan,
            Err(why) => return CursorPatchOutcome::Ineligible(why),
        };
        match self.present_patch_to_layer(rect, &facts, cfg) {
            Ok(()) => CursorPatchOutcome::Presented(rect),
            Err(failure) => CursorPatchOutcome::Failed(failure),
        }
    }

    #[cfg(target_os = "macos")]
    fn present_patch_to_layer(
        &mut self,
        rect: PatchRect,
        facts: &SceneFacts,
        cfg: &Config,
    ) -> Result<(), CursorPatchFailure> {
        let Some(layer) = self.cursor_layer.as_mut() else {
            return Err(CursorPatchFailure::Lost);
        };
        let scope = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        layer.fit(&self.gpu.device, rect.width, rect.height);
        if let Some(error) = pollster::block_on(scope.pop()) {
            log::warn!("cursor layer configure failed: {error}");
            layer.configured = None;
            return Err(CursorPatchFailure::Validation);
        }
        let format = layer.format;
        let (frame, reconfigure) = match layer.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Occluded => return Err(CursorPatchFailure::Occluded),
            wgpu::CurrentSurfaceTexture::Timeout => return Err(CursorPatchFailure::Timeout),
            wgpu::CurrentSurfaceTexture::Outdated => {
                layer.configured = None;
                return Err(CursorPatchFailure::Outdated);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.cursor_layer = None;
                return Err(CursorPatchFailure::Lost);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                layer.configured = None;
                return Err(CursorPatchFailure::Validation);
            }
        };
        if reconfigure {
            layer.configured = None;
        }
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // A failed submission must discard the acquired drawable. The layer
        // keeps its old contents; C2 hides it until a new Presented result.
        self.submit_cursor_patch(rect, facts, cfg, &view, format)?;
        self.gpu.queue.present(frame);
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn present_patch_to_layer(
        &mut self,
        _rect: PatchRect,
        _facts: &SceneFacts,
        _cfg: &Config,
    ) -> Result<(), CursorPatchFailure> {
        Err(CursorPatchFailure::Lost)
    }

    /// Scope resource creation, encoding, finish and submit together. Native
    /// wgpu reports validation errors synchronously through this scope.
    fn submit_cursor_patch(
        &mut self,
        rect: PatchRect,
        facts: &SceneFacts,
        cfg: &Config,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) -> Result<(), CursorPatchFailure> {
        let scope = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let result = (|| {
            let mut encoder =
                self.gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("kettle-cursor-patch-encoder"),
                    });
            self.encode_cursor_patch(rect, facts, cfg, view, format, &mut encoder)?;
            self.gpu.queue.submit(std::iter::once(encoder.finish()));
            Ok(())
        })();
        let validation = pollster::block_on(scope.pop());
        if validation.is_some() || result == Err(CursorPatchFailure::Validation) {
            log::warn!("cursor patch validation failed: {validation:?}");
            #[cfg(target_os = "macos")]
            if let Some(layer) = self.cursor_layer.as_mut() {
                layer.configured = None;
            }
            // Invalid cached resources must not be reused on the next attempt.
            self.cursor_patch = None;
            return Err(CursorPatchFailure::Validation);
        }
        result
    }

    /// The patch rect for the frame on screen, or why it has none.
    pub(crate) fn plan_cursor_patch(
        &mut self,
        cfg: &Config,
    ) -> Result<(PatchRect, SceneFacts), CursorPatchIneligible> {
        use CursorPatchIneligible as Why;
        let facts = self.last_scene.ok_or(Why::NoFrame)?;
        if facts.cursor_on {
            return Err(Why::PhaseOn);
        }
        if !facts.drawn_with(cfg) || facts.alpha_mode != self.config.alpha_mode {
            return Err(Why::ConfigChanged);
        }
        if !matches!(
            facts.alpha_mode,
            wgpu::CompositeAlphaMode::Opaque
                | wgpu::CompositeAlphaMode::PreMultiplied
                | wgpu::CompositeAlphaMode::PostMultiplied
        ) {
            return Err(Why::AlphaMode);
        }
        if facts.background_type == BackgroundType::Starfield || self.bg_imgs.has_draws() {
            return Err(Why::Background);
        }
        let range = self.cursor_quad_range.clone().ok_or(Why::NoCursor)?;
        // A block, beam or underline is one quad; the hollow outline is four.
        if range.len() != 1 {
            return Err(Why::Shape);
        }
        let cursor = *self
            .quad_scratch
            .get(range.start as usize)
            .ok_or(Why::NoCursor)?;
        let ink = self.cursor_glyph_ink(facts.target_size);
        let block = PixelBox::bounding(cursor.pos, cursor.size);
        let patch = ink
            .map_or(block, |ink| block.union(ink))
            .intersect(PixelBox::surface(facts.target_size));
        if patch.is_empty() {
            return Err(Why::Empty);
        }
        let max_width = (MAX_PATCH_CELLS * self.cell_w)
            .ceil()
            .min(MAX_PATCH_SIDE as f32) as i64;
        let max_height = (MAX_PATCH_CELLS * self.cell_h)
            .ceil()
            .min(MAX_PATCH_SIDE as f32) as i64;
        if patch.width() > max_width || patch.height() > max_height {
            return Err(Why::TooLarge);
        }
        if facts.translucent
            && ink.is_some_and(|ink| !PixelBox::covered(cursor.pos, cursor.size).contains(ink))
        {
            return Err(Why::TranslucentOverhang);
        }
        let near = patch.grow(1);
        if self.inexact_draws_touch(near) {
            return Err(Why::Overlap);
        }
        if !self.quads_are_snap_stable_near(near, facts.live_window) {
            return Err(Why::SnapBoundary);
        }
        Ok((
            PatchRect {
                x: patch.x0 as u32,
                y: patch.y0 as u32,
                width: patch.width() as u32,
                height: patch.height() as u32,
            },
            facts,
        ))
    }

    /// The pixels the inverted cursor glyph inks, placed and clipped as
    /// glyphon's `prepare_glyph` places them, or `None` for no glyph.
    fn cursor_glyph_ink(&mut self, target_size: [u32; 2]) -> Option<PixelBox> {
        let (left, top, clip) = {
            let glyph = self.pending_cursor_glyph.as_ref()?;
            (glyph.x, glyph.y, glyph.clip)
        };
        let bounds = glyphon_bounds(clip, target_size);
        let mut ink: Option<PixelBox> = None;
        for run in self.cursor_glyph_buffer.layout_runs() {
            for glyph in run.glyphs {
                let physical = glyph.physical((left, top), 1.0);
                let Some(image) = self
                    .swash
                    .get_image(&mut self.font_system, physical.cache_key)
                    .as_ref()
                else {
                    continue;
                };
                let placement = image.placement;
                if placement.width == 0 || placement.height == 0 {
                    continue;
                }
                let x = i64::from(physical.x) + i64::from(placement.left);
                let y = i64::from(run.line_y.round() as i32) + i64::from(physical.y)
                    - i64::from(placement.top);
                let quad = PixelBox {
                    x0: x,
                    y0: y,
                    x1: x + i64::from(placement.width),
                    y1: y + i64::from(placement.height),
                }
                .intersect(bounds);
                if !quad.is_empty() {
                    ink = Some(ink.map_or(quad, |ink| ink.union(quad)));
                }
            }
        }
        ink
    }

    /// Whether an image or a pane outline can paint inside `near`. Their
    /// shading is interpolated, so it does not crop exactly.
    fn inexact_draws_touch(&self, near: PixelBox) -> bool {
        let any_rect_touches = |rects: Option<Vec<[f32; 4]>>| {
            rects.is_none_or(|rects| {
                rects.iter().any(|rect| {
                    PixelBox::bounding([rect[0], rect[1]], [rect[2], rect[3]]).intersects(near)
                })
            })
        };
        any_rect_touches(self.imgs.drawn_rects())
            || any_rect_touches(self.media_receipt_img.drawn_rects())
            || self
                .pane_outlines
                .uploaded()
                .is_none_or(|mut outlines| outlines.any(|outline| outline_touches(&outline, near)))
    }

    /// Whether every flat quad the scene pass draws rasterizes the same inside
    /// `near` in a patch target as in the full frame.
    fn quads_are_snap_stable_near(&self, near: PixelBox, live_window: bool) -> bool {
        let bases = if crate::use_live_pane_bases(live_window, self.live_background_opacity_floor) {
            &self.live_pane_bases
        } else {
            &self.pane_bases
        };
        self.quad_scratch
            .iter()
            .all(|quad| quad_is_snap_stable_near(quad, near))
            && [bases, &self.overlay_quads, &self.menu_quads]
                .into_iter()
                .all(|pipeline| {
                    pipeline.uploaded().is_some_and(|mut quads| {
                        quads.all(|quad| quad_is_snap_stable_near(&quad, near))
                    })
                })
    }

    /// Encode the on and off passes and the combine into `target`, a
    /// `rect`-sized view of `format`. Nothing is uploaded.
    fn encode_cursor_patch(
        &mut self,
        rect: PatchRect,
        facts: &SceneFacts,
        cfg: &Config,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), CursorPatchFailure> {
        let patch = self
            .cursor_patch
            .get_or_insert_with(|| PatchPipeline::new(&self.gpu.device));
        patch.ensure_pipeline(&self.gpu.device, format);
        if !patch.ensure_targets(
            &self.gpu.device,
            &self.graphics_budget,
            self.config.format,
            rect.width,
            rect.height,
        ) {
            return Err(CursorPatchFailure::Budget);
        }
        let Some(patch) = self.cursor_patch.as_ref() else {
            return Err(CursorPatchFailure::Budget);
        };
        let Some(targets) = patch.targets.as_ref() else {
            return Err(CursorPatchFailure::Budget);
        };
        for (view, cursor_on) in [(&targets.on_view, true), (&targets.off_view, false)] {
            self.encode_scene_pass(
                view,
                facts.target_size,
                cfg,
                facts.live_window,
                cursor_on,
                Some(rect),
                encoder,
            )
            .map_err(|error| {
                log::warn!("cursor patch pass failed: {error}");
                CursorPatchFailure::Validation
            })?;
        }
        patch.encode_combine(encoder, target, format, &targets.bind_group)
    }
}

/// The patch as the layer would show it, read back for tests.
#[cfg(test)]
pub(crate) struct PatchCapture {
    pub(crate) rect: PatchRect,
    pub(crate) pixels: image::RgbaImage,
}

#[cfg(test)]
impl Renderer {
    /// The patch `present_cursor_patch` would put on the layer, rendered into
    /// a target of the scene's format and read back.
    pub(crate) fn cursor_patch_for_tests(
        &mut self,
        cfg: &Config,
    ) -> Result<PatchCapture, CursorPatchIneligible> {
        let (rect, facts) = self.plan_cursor_patch(cfg)?;
        let format = self.config.format;
        let target = readable_target(&self.gpu.device, format, rect.width, rect.height);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.encode_cursor_patch(rect, &facts, cfg, &view, format, &mut encoder)
            .expect("the patch encodes");
        let bytes = read_texture(&self.gpu.device, &self.gpu.queue, &target, encoder);
        let pixels =
            image::RgbaImage::from_raw(rect.width, rect.height, bytes).expect("the patch is RGBA");
        Ok(PatchCapture { rect, pixels })
    }

    /// The last frame's scene rendered into `rect` alone, as a patch pass
    /// renders it.
    pub(crate) fn scene_window_for_tests(
        &mut self,
        cfg: &Config,
        rect: PatchRect,
        cursor_on: bool,
    ) -> image::RgbaImage {
        let (target, encoder) = self.scene_target_for_tests(cfg, rect, cursor_on, Some(rect));
        let bytes = read_texture(&self.gpu.device, &self.gpu.queue, &target, encoder);
        image::RgbaImage::from_raw(rect.width, rect.height, bytes).expect("the window is RGBA")
    }

    fn scene_target_for_tests(
        &self,
        cfg: &Config,
        rect: PatchRect,
        cursor_on: bool,
        window: Option<PatchRect>,
    ) -> (wgpu::Texture, wgpu::CommandEncoder) {
        let facts = self.last_scene.expect("a frame was rendered");
        let target = readable_target(
            &self.gpu.device,
            self.config.format,
            rect.width,
            rect.height,
        );
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.encode_scene_pass(
            &view,
            facts.target_size,
            cfg,
            facts.live_window,
            cursor_on,
            window,
            &mut encoder,
        )
        .expect("the window renders");
        (target, encoder)
    }

    /// The live surface in straight RGBA for layer compositing. Opaque surface
    /// alpha is ignored, PreMultiplied is normalized, and PostMultiplied runs
    /// the production presentation shader. This is not a screenshot.
    fn displayed_frame_for_tests(&mut self, cfg: &Config, cursor_on: bool) -> image::RgbaImage {
        let facts = self.last_scene.expect("a frame was rendered");
        let rect = PatchRect {
            x: 0,
            y: 0,
            width: facts.target_size[0],
            height: facts.target_size[1],
        };
        let (scene, mut encoder) = self.scene_target_for_tests(cfg, rect, cursor_on, None);
        let output =
            if facts.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied && facts.translucent {
                let mut presentation = crate::present::PresentationPipeline::new(
                    &self.gpu.device,
                    self.config.format,
                    self.graphics_budget.clone(),
                );
                assert!(presentation.ensure_target(&self.gpu.device, rect.width, rect.height));
                self.encode_scene_pass(
                    presentation.scene_view().expect("presentation target"),
                    facts.target_size,
                    cfg,
                    facts.live_window,
                    cursor_on,
                    None,
                    &mut encoder,
                )
                .expect("live scene renders");
                let target = readable_target(
                    &self.gpu.device,
                    self.config.format,
                    rect.width,
                    rect.height,
                );
                let view = target.create_view(&wgpu::TextureViewDescriptor::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("kettle-cursor-patch-test-present"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    presentation.draw(&mut pass);
                }
                target
            } else {
                scene
            };
        let mut bytes = read_texture(&self.gpu.device, &self.gpu.queue, &output, encoder);
        // What each surface shows: PreMultiplied and PostMultiplied present
        // these bytes as they are (the latter after its presentation pass
        // above), and an Opaque surface ignores their alpha.
        match facts.alpha_mode {
            wgpu::CompositeAlphaMode::Opaque => {
                for pixel in bytes.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
            wgpu::CompositeAlphaMode::PreMultiplied | wgpu::CompositeAlphaMode::PostMultiplied => {}
            _ => panic!("unsupported surface mode"),
        }
        image::RgbaImage::from_raw(rect.width, rect.height, bytes).expect("the surface is RGBA")
    }
}

#[cfg(test)]
fn readable_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-cursor-patch-test"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// Submit `encoder` with a copy of `texture` (4 bytes a texel) and return the
/// texel bytes, rows unpadded.
#[cfg(test)]
fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mut encoder: wgpu::CommandEncoder,
) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let unpadded = width * 4;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-cursor-patch-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv()
        .expect("the map callback ran")
        .expect("the readback maps");
    let data = slice.get_mapped_range().expect("mapped range");
    let mut bytes = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        bytes.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    buffer.unmap();
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
    use crate::headless_tests::{capture, focused, pane, snapshot_of};
    use crate::{PaneView, Rect4};

    fn production_source() -> String {
        kettle_test_support::production_source(include_str!("cursor_patch.rs"))
    }

    /// The only unstable edges are the ones the rasterizer's 1/256 px snap
    /// can move across a pixel centre: within one step of a centre, but not
    /// on it. The band comes from an Apple M5 Max, where edges 1/512 px from a
    /// centre moved and edges on a centre, or 1/1024 and 1/128 px from one,
    /// did not.
    #[test]
    fn only_edges_within_a_snap_step_of_a_pixel_centre_are_unstable() {
        for centre in [0.5f32, 17.5, 1919.5, 3839.5] {
            assert!(edge_is_snap_stable(centre), "on the centre {centre}");
            for off in [1.0 / 512.0, 1.0 / 1024.0, 1.0 / 256.0] {
                assert!(!edge_is_snap_stable(centre + off), "{centre} + {off}");
                assert!(!edge_is_snap_stable(centre - off), "{centre} - {off}");
            }
            for off in [1.0 / 128.0, 0.25, 0.5] {
                assert!(edge_is_snap_stable(centre + off), "{centre} + {off}");
                assert!(edge_is_snap_stable(centre - off), "{centre} - {off}");
            }
        }
        assert!(!edge_is_snap_stable(f32::NAN));
        assert!(!edge_is_snap_stable(f32::INFINITY));
    }

    /// `covered` is what the rasterizer fills under the top-left rule;
    /// `bounding` is every pixel the rect touches.
    #[test]
    fn pixel_boxes_follow_the_top_left_rule() {
        let pos = [10.25, 3.5];
        let size = [8.56, 17.0];
        assert_eq!(
            PixelBox::bounding(pos, size),
            PixelBox {
                x0: 10,
                y0: 3,
                x1: 19,
                y1: 21
            }
        );
        // Centres 10.5..=18.5 lie in [10.25, 18.81); the top edge sits on the
        // centre 3.5, which the top-left rule includes, and the bottom edge on
        // 20.5, which it excludes.
        assert_eq!(
            PixelBox::covered(pos, size),
            PixelBox {
                x0: 10,
                y0: 3,
                x1: 19,
                y1: 20
            }
        );
        assert!(PixelBox::covered([0.6, 0.0], [0.3, 1.0]).is_empty());
    }

    #[test]
    fn glyphon_clips_to_the_pane_and_the_surface() {
        assert_eq!(
            glyphon_bounds((-4.0, 10.7, 500.0, 90.0), [320, 120]),
            PixelBox {
                x0: 0,
                y0: 10,
                x1: 320,
                y1: 100
            }
        );
    }

    /// An outline paints only its stroke band: a patch deep inside the pane
    /// does not touch it, one at the edge does, and a zero width paints
    /// nothing.
    #[test]
    fn an_outline_touches_only_near_its_stroke() {
        let outline = OutlineInstance {
            pos: [0.0, 0.0],
            size: [400.0, 300.0],
            color: [1.0; 4],
            border_width: 1.0,
            corner_radius: 8.0,
            corner_mask: 0b1100,
            _pad: 0,
        };
        let inside = PixelBox {
            x0: 100,
            y0: 100,
            x1: 110,
            y1: 120,
        };
        let edge = PixelBox {
            x0: 2,
            y0: 100,
            x1: 12,
            y1: 120,
        };
        assert!(!outline_touches(&outline, inside));
        assert!(outline_touches(&outline, edge));
        // A cursor in the first row, a few pixels below an unrounded top
        // edge: clear of the stroke, so not touching. Only the rounded
        // bottom corners (mask 0b1100) reach further in.
        let first_row = PixelBox {
            x0: 22,
            y0: 7,
            x1: 33,
            y1: 26,
        };
        assert!(!outline_touches(&outline, first_row));
        let top_band = PixelBox {
            y0: 0,
            y1: 6,
            ..first_row
        };
        assert!(outline_touches(&outline, top_band));
        let bottom_left_corner = PixelBox {
            x0: 4,
            y0: 286,
            x1: 9,
            y1: 293,
        };
        assert!(outline_touches(&outline, bottom_left_corner));
        let top_left_corner = PixelBox {
            x0: 4,
            y0: 4,
            x1: 9,
            y1: 11,
        };
        assert!(!outline_touches(&outline, top_left_corner));
        assert!(outline_touches(
            &OutlineInstance {
                corner_mask: 0b0001,
                ..outline
            },
            top_left_corner
        ));
        assert!(!outline_touches(
            &OutlineInstance {
                border_width: 0.0,
                ..outline
            },
            edge
        ));
    }

    #[test]
    fn reasons_have_stable_names() {
        assert_eq!(
            CursorPatchIneligible::SnapBoundary.as_str(),
            "snap_boundary"
        );
        assert_eq!(
            CursorPatchIneligible::TranslucentOverhang.as_str(),
            "translucent_overhang"
        );
        assert_eq!(CursorPatchIneligible::AlphaMode.as_str(), "alpha_mode");
        assert_eq!(CursorPatchFailure::Occluded.as_str(), "occluded");
    }

    /// The patch passes are the scene pass itself, with a window, and the
    /// hand-off writes nothing to the GPU and has no Objective-C in it.
    #[test]
    fn the_patch_reuses_the_scene_pass_and_uploads_nothing() {
        let src = production_source();
        assert!(src.contains("self.encode_scene_pass("));
        assert!(src.contains("Some(rect),"));
        for draw in [
            ".draw_hiding(",
            "text_renderer",
            "self.quads.draw",
            "glyph_pipeline",
        ] {
            assert!(
                !src.contains(draw),
                "the patch must not copy the scene's pass list ({draw})"
            );
        }
        for forbidden in ["objc", "msg_send", ".write_buffer(", ".write_texture("] {
            assert!(!src.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn only_apple_metal_adapters_can_present_patches() {
        assert!(measured_layer_adapter("Apple M5 Max", wgpu::Backend::Metal));
        for (name, backend) in [
            ("Intel Iris", wgpu::Backend::Metal),
            ("AMD Radeon", wgpu::Backend::Metal),
            ("Microsoft Basic Render Driver", wgpu::Backend::Dx12),
            ("Apple M5 Max", wgpu::Backend::Vulkan),
            ("", wgpu::Backend::Metal),
        ] {
            assert!(!measured_layer_adapter(name, backend));
        }
        assert_eq!(
            CursorPatchIneligible::UnmeasuredGpu.as_str(),
            "unmeasured_gpu"
        );
        let src = production_source();
        let present = src
            .split("pub fn present_cursor_patch(")
            .nth(1)
            .expect("entry");
        assert!(
            present
                .find("!measured_layer_adapter(")
                .expect("adapter gate")
                < present.find("self.plan_cursor_patch(").expect("plan")
        );
        assert!(present.contains("CursorPatchIneligible::UnmeasuredGpu"));
    }

    #[test]
    fn only_a_presented_frame_grants_a_production_handoff() {
        let src = kettle_test_support::production_source(include_str!("lib.rs"));
        let frame = src
            .split("pub fn render_frame_with_status_and_pre_present<F>(")
            .nth(1)
            .expect("frame method")
            .split("pub fn render_uploads(")
            .next()
            .expect("frame body");
        let reset = frame.find("self.last_scene = None;").expect("frame reset");
        let upload = frame.find(".upload(").expect("first upload");
        assert!(reset < upload, "reset must precede uploads");
        assert_eq!(frame.matches("self.last_scene = Some(").count(), 2);
        assert_eq!(
            frame
                .matches("self.last_scene = Some(scene_facts);")
                .count(),
            1
        );
        let record = frame
            .find("self.last_scene = Some(scene_facts);")
            .expect("record");
        let present = frame
            .find("self.gpu.queue.present(frame);")
            .expect("present");
        let outcome = frame.find("Ok(FrameOutcome::Presented)").expect("outcome");
        assert!(present < record && record < outcome);
        let headless = frame
            .find("self.last_scene = Some(cursor_patch::SceneFacts {")
            .expect("headless record");
        let headless_branch = frame.find("let Some(acquired)").expect("acquire branch");
        let acquire_match = frame.find("match acquired").expect("acquire outcomes");
        assert!(headless_branch < headless && headless < acquire_match);
        let before_headless = &frame[headless_branch..headless];
        assert!(
            before_headless
                .contains("Test-only: a headless capture substitutes for a presented frame."),
            "document the headless-only hand-off exception"
        );
    }

    #[test]
    fn layer_validation_is_checked_before_acquire_and_present() {
        let src = production_source();
        let layer = src
            .split("fn present_patch_to_layer(")
            .nth(1)
            .expect("layer path")
            .split("#[cfg(not(target_os")
            .next()
            .expect("macOS path");
        let scope = layer.find("push_error_scope(").expect("configure scope");
        let fit = layer.find("layer.fit(").expect("configure");
        let check = layer
            .find("block_on(scope.pop())")
            .expect("configure check");
        let acquire = layer.find("get_current_texture()").expect("acquire");
        assert!(scope < fit && fit < check && check < acquire);
        assert!(layer[check..acquire].contains("layer.configured = None;"));
        let validation_arm = layer
            .split("wgpu::CurrentSurfaceTexture::Validation => {")
            .nth(1)
            .expect("validation arm")
            .split('}')
            .next()
            .expect("arm body");
        assert!(validation_arm.contains("layer.configured = None;"));
        let submit = layer.find("self.submit_cursor_patch(").expect("submit");
        let present = layer
            .find("self.gpu.queue.present(frame);")
            .expect("present");
        assert!(submit < present);
        assert!(
            layer[submit..present].contains("?;"),
            "failed submit must discard the frame"
        );
    }

    #[test]
    fn a_changed_config_and_missing_frame_are_ineligible() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            return;
        };
        assert_eq!(
            renderer.cursor_patch_for_tests(&cfg).err(),
            Some(CursorPatchIneligible::NoFrame)
        );
        let snap = snapshot_of(20, 4, b"hello");
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let mut changed = cfg.clone();
        changed.theme.background.r ^= 1;
        assert_eq!(
            renderer.cursor_patch_for_tests(&changed).err(),
            Some(CursorPatchIneligible::ConfigChanged)
        );
    }

    #[test]
    fn empty_and_oversized_patches_are_ineligible() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            return;
        };
        let snap = snapshot_of(20, 4, b"hello");
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let index = renderer.cursor_quad_range.as_ref().expect("cursor").start as usize;
        let original = renderer.quad_scratch[index];
        renderer.quad_scratch[index].pos = [400.0, 200.0];
        assert_eq!(
            renderer.plan_cursor_patch(&cfg).err(),
            Some(CursorPatchIneligible::Empty)
        );
        renderer.quad_scratch[index] = original;
        renderer.quad_scratch[index].size = [320.0, 120.0];
        assert_eq!(
            renderer.plan_cursor_patch(&cfg).err(),
            Some(CursorPatchIneligible::TooLarge)
        );
    }

    #[test]
    fn renderer_setters_invalidate_the_handoff() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            return;
        };
        let snap = snapshot_of(20, 4, b"hello");
        let state = renderer.recovery_state();
        for change in 0..8 {
            capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 320, 120)],
                &focused(false),
            );
            assert!(
                renderer.last_scene.is_some(),
                "headless capture grants a hand-off"
            );
            match change {
                0 => renderer.resize(321, 120),
                1 => renderer.set_scale(2.0),
                2 => renderer.set_font_size(15.0),
                3 => renderer.set_font_family("monospace".into()),
                4 => renderer.set_cell_scale(1.07, 1.0),
                5 => renderer.set_live_background_opacity_floor(Some(1.0)),
                6 => renderer.restore_recovery_state(&state),
                _ => {
                    // Force an alpha-mode transition even on an opaque-only fixture.
                    renderer.config.alpha_mode = wgpu::CompositeAlphaMode::PostMultiplied;
                    renderer.set_background_compositing(&cfg);
                }
            }
            assert_eq!(
                renderer.cursor_patch_for_tests(&cfg).err(),
                Some(CursorPatchIneligible::NoFrame),
                "setter {change} must invalidate the hand-off"
            );
        }
    }

    #[test]
    fn a_rejected_patch_submission_cannot_be_presented() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            return;
        };
        let snap = snapshot_of(20, 4, b"hello");
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let facts = renderer.last_scene.expect("capture");
        let rect = PatchRect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        let target = readable_target(&renderer.gpu.device, renderer.config.format, 16, 16);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        // Pipeline format mismatches the drawable. wgpu rejects the combine
        // pass at encoding/submit without returning a synchronous Result.
        assert_eq!(
            renderer.submit_cursor_patch(
                rect,
                &facts,
                &cfg,
                &view,
                wgpu::TextureFormat::Rgba8Unorm
            ),
            Err(CursorPatchFailure::Validation)
        );
        assert!(
            renderer.cursor_patch.is_none(),
            "drop invalid cached resources"
        );
    }

    #[test]
    fn a_missing_combine_pipeline_is_a_validation_failure() {
        let _serialized = gpu_test_guard();
        let Some((renderer, _cfg)) = crate::headless_tests::renderer(320, 120) else {
            return;
        };
        let patch = PatchPipeline::new(&renderer.gpu.device);
        let target = readable_target(&renderer.gpu.device, renderer.config.format, 1, 1);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = patch.bind_group(&renderer.gpu.device, &view, &view);
        let mut encoder = renderer
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        assert_eq!(
            patch.encode_combine(&mut encoder, &view, renderer.config.format, &bind_group),
            Err(CursorPatchFailure::Validation)
        );
    }

    fn composite(off: &image::RgbaImage, patch: &PatchCapture) -> image::RgbaImage {
        let mut out = off.clone();
        for (x, y, pixel) in patch.pixels.enumerate_pixels() {
            match pixel[3] {
                255 => out.put_pixel(patch.rect.x + x, patch.rect.y + y, *pixel),
                0 => assert_eq!(pixel.0, [0; 4], "a clear patch pixel is transparent black"),
                alpha => panic!("patch alpha {alpha}: every pixel is opaque or clear"),
            }
        }
        out
    }

    struct Case {
        name: &'static str,
        bytes: &'static str,
        configure: fn(&mut Config),
        scale: f32,
        translucent_surface: bool,
        surface_mode: Option<wgpu::CompositeAlphaMode>,
        opacity: Option<f32>,
        live_floor: Option<f32>,
        split: bool,
        /// Size the grid to the pane and park the cursor on its last column,
        /// under the scrollbar's gutter.
        last_column: bool,
    }

    // Parity exercises only the rasterizers with recorded crop probes. WARP
    // still runs portable eligibility and combine tests, but cannot present.
    fn measured_parity_adapter(renderer: &Renderer) -> bool {
        let info = renderer.gpu.adapter.get_info();
        measured_layer_adapter(&info.name, info.backend)
            || (info.backend == wgpu::Backend::Vulkan
                && info.device_type == wgpu::DeviceType::Cpu
                && info.name.to_ascii_lowercase().contains("llvmpipe"))
    }

    fn centre_distance(edge: f32) -> f32 {
        ((edge - 0.5) - (edge - 0.5).round()).abs()
    }

    // Keep both cell edges and beam edges out of the snap band without
    // depending on a particular version of the bundled font's metrics.
    fn safe_padding(base: f32, step: f32, cells: usize) -> f32 {
        (48..304)
            .map(|i| base + i as f32 / 256.0)
            .find(|&padding| {
                (cells.saturating_sub(1)..=cells + 2).all(|i| {
                    let edge = padding + i as f32 * step;
                    [
                        edge,
                        edge + 0.15 * step,
                        edge + 0.5 * step,
                        edge + 0.5 * step - 0.75,
                        edge + 0.5 * step + 0.75,
                    ]
                    .into_iter()
                    .all(|edge| centre_distance(edge) >= 1.0 / 64.0)
                })
            })
            .expect("font metrics must allow padding away from pixel centres")
    }

    fn stabilize_padding(renderer: &Renderer, cfg: &mut Config, snap: &crate::PaneSnapshot) {
        cfg.padding_x = safe_padding(cfg.padding_x, renderer.cell_w, snap.cursor.point.column.0);
        cfg.padding_y = safe_padding(
            cfg.padding_y,
            renderer.cell_h,
            snap.cursor.point.line.0.max(0) as usize,
        );
    }

    fn assert_cursor_edges_are_stable(renderer: &Renderer) {
        let range = renderer.cursor_quad_range.as_ref().expect("cursor range");
        let quad = renderer.quad_scratch[range.start as usize];
        for edge in [
            quad.pos[0],
            quad.pos[0] + quad.size[0],
            quad.pos[1],
            quad.pos[1] + quad.size[1],
        ] {
            assert!(
                centre_distance(edge) >= 1.0 / 64.0,
                "fixture cursor edge {edge} is too close to a pixel centre"
            );
        }
    }

    fn unchanged(_: &mut Config) {}

    fn cases() -> Vec<Case> {
        let case = |name, bytes| Case {
            name,
            bytes,
            configure: unchanged,
            scale: 1.0,
            translucent_surface: false,
            surface_mode: None,
            opacity: None,
            live_floor: None,
            split: false,
            last_column: false,
        };
        let mut cases = vec![
            case("block", "hello"),
            case("block on a glyph", "hello\x1b[2G"),
            case("beam", "\x1b[5 qhello"),
            case("underline", "\x1b[3 qhello"),
            case("wide block", "漢字\x1b[1G"),
            case("osc 12", "\x1b]12;#ff8800\x07hello"),
            Case {
                configure: |cfg| cfg.theme.cursor_text = cfg.theme.background,
                ..case("cursor text on the background", "hello\x1b[1G")
            },
            Case {
                configure: |cfg| cfg.cell_width = 1.07,
                ..case("fractional cells", "hello\x1b[2G")
            },
            Case {
                configure: |cfg| cfg.cell_width = 1.07,
                ..case("fractional beam", "\x1b[5 qhello")
            },
            Case {
                configure: |cfg| cfg.cell_width = 0.6,
                ..case("overhanging glyph, opaque", "WWWW\x1b[2G")
            },
            Case {
                scale: 2.0,
                ..case("scale 2", "hello\x1b[2G")
            },
            Case {
                configure: |cfg| {
                    cfg.background_opacity = 0.86;
                    cfg.background_blur = true;
                },
                translucent_surface: true,
                ..case("translucent block on a glyph", "hello\x1b[2G")
            },
            Case {
                configure: |cfg| cfg.padding_x = 7.0,
                split: true,
                ..case("right pane of a split", "hello")
            },
            Case {
                configure: |cfg| cfg.scrollbar = kettle_config::ScrollbarMode::Always,
                last_column: true,
                ..case("scrollbar by the last column", "")
            },
        ];
        for (name, bytes) in [
            ("matrix block", "hello\x1b[2G"),
            ("matrix beam", "\x1b[5 qhello\x1b[2G"),
            ("matrix underline", "\x1b[3 qhello\x1b[2G"),
        ] {
            for scale in [1.0, 2.0] {
                for fractional in [false, true] {
                    for translucent in [false, true] {
                        cases.push(Case {
                            configure: match (fractional, translucent) {
                                (false, false) => unchanged,
                                (true, false) => |cfg| cfg.cell_width = 1.07,
                                (false, true) => |cfg| {
                                    cfg.background_opacity = 0.86;
                                    cfg.background_blur = true;
                                },
                                (true, true) => |cfg| {
                                    cfg.cell_width = 1.07;
                                    cfg.background_opacity = 0.86;
                                    cfg.background_blur = true;
                                },
                            },
                            scale,
                            translucent_surface: translucent,
                            ..case(name, bytes)
                        });
                    }
                }
            }
        }
        let clamped = Config::parse_text("background-opacity = 2.5\n");
        assert_eq!(clamped.background_opacity, 1.0);
        for mode in [
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
        ] {
            for opacity in [0.0, 0.86, 1.0] {
                for live_floor in [None, Some(0.99)] {
                    cases.push(Case {
                        surface_mode: Some(mode),
                        opacity: Some(opacity),
                        live_floor,
                        ..case("live surface modes", "hello\x1b[2G")
                    });
                    if mode == wgpu::CompositeAlphaMode::Opaque || opacity == 1.0 {
                        cases.push(Case {
                            configure: |cfg| cfg.cell_width = 0.6,
                            surface_mode: Some(mode),
                            opacity: Some(opacity),
                            live_floor,
                            ..case("live overhang", "WWWW\x1b[2G")
                        });
                    }
                }
            }
        }
        cases
    }

    /// Compositing the patch over the off frame gives the on frame, byte for
    /// byte in straight RGBA for alpha surfaces, and RGB for Opaque surfaces,
    /// across cursor shapes, colours, cell widths, scales and layouts.
    #[test]
    fn the_patch_over_the_off_frame_is_the_on_frame() {
        let _serialized = gpu_test_guard();
        for case in cases() {
            let mut cfg = gpu_test_config();
            (case.configure)(&mut cfg);
            if let Some(opacity) = case.opacity {
                cfg.background_opacity = opacity;
            }
            let (width, height) = (320, 120);
            let Some(mut renderer) = Renderer::headless_for_tests_with(
                &cfg,
                width,
                height,
                case.scale,
                case.translucent_surface,
            )
            .expect("headless renderer builds") else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            if !measured_parity_adapter(&renderer) {
                eprintln!("unmeasured rasterizer; parity matrix not run");
                return;
            }
            if let Some(mode) = case.surface_mode {
                renderer.supported_alpha_modes = vec![mode];
            }
            renderer.set_live_background_opacity_floor(case.live_floor);
            let (cols, bytes) = if case.last_column {
                cfg.scrollbar_width = 40.0;
                let cols = ((width as f32 - 2.0 * cfg.padding_x - 40.0) / renderer.cell_w) as usize;
                (cols, format!("\x1b[{cols}Gx\x1b[{cols}G"))
            } else {
                (20, case.bytes.to_string())
            };
            let snap = snapshot_of(cols, 4, bytes.as_bytes());
            stabilize_padding(&renderer, &mut cfg, &snap);
            let other = snapshot_of(20, 4, b"left");
            let views: Vec<PaneView<'_>> = if case.split {
                let half = width as f32 / 2.0;
                vec![
                    PaneView {
                        id: 2,
                        rect: (0.0, 0.0, half, height as f32),
                        focused: false,
                        ..pane(&other, width, height)
                    },
                    PaneView {
                        rect: (half, 0.0, half, height as f32),
                        ..pane(&snap, width, height)
                    },
                ]
            } else if case.last_column {
                // Put the real scrollbar track through the last cell's centre.
                let pane_width = cfg.padding_x
                    + (cols as f32 - 0.5) * renderer.cell_w
                    + cfg.scrollbar_width / 2.0
                    + 2.0;
                vec![PaneView {
                    rect: (0.0, 0.0, pane_width, height as f32),
                    ..pane(&snap, width, height)
                }]
            } else {
                vec![pane(&snap, width, height)]
            };
            capture(&mut renderer, &cfg, &views, &focused(true));
            renderer.last_scene.as_mut().expect("on frame").live_window = true;
            let on = renderer.displayed_frame_for_tests(&cfg, true);
            capture(&mut renderer, &cfg, &views, &focused(false));
            renderer.last_scene.as_mut().expect("off frame").live_window = true;
            let off = renderer.displayed_frame_for_tests(&cfg, false);
            assert_cursor_edges_are_stable(&renderer);
            if matches!(
                case.name,
                "matrix block"
                    | "block on a glyph"
                    | "fractional cells"
                    | "scale 2"
                    | "translucent block on a glyph"
            ) {
                assert!(
                    renderer.cursor_glyph_ink([width, height]).is_some(),
                    "{}: the block fixture must have inverted glyph ink",
                    case.name
                );
            }
            assert_ne!(on, off, "{}: the cursor draws something", case.name);
            let patch = renderer
                .cursor_patch_for_tests(&cfg)
                .unwrap_or_else(|why| panic!("{}: ineligible ({})", case.name, why.as_str()));
            if case.last_column {
                let patch_box = PixelBox::bounding(
                    [patch.rect.x as f32, patch.rect.y as f32],
                    [patch.rect.width as f32, patch.rect.height as f32],
                );
                assert!(
                    renderer
                        .overlay_quads
                        .uploaded()
                        .expect("overlay uploads")
                        .any(|quad| PixelBox::bounding(quad.pos, quad.size).intersects(patch_box)),
                    "the scrollbar must overlap the cursor patch"
                );
            }
            assert!(
                patch.pixels.pixels().any(|pixel| pixel[3] == 255),
                "{}: the patch carries the cursor",
                case.name
            );
            assert!(
                composite(&off, &patch) == on,
                "{}: the patch over the displayed off frame is not the displayed on frame ({:?}, opacity {}, floor {:?})",
                case.name,
                renderer.config.alpha_mode,
                cfg.background_opacity,
                case.live_floor
            );
        }
    }

    /// A patch pass renders the crop of the full frame: the offset viewport
    /// and the moved glyph scissors put every pixel where the full pass does.
    #[test]
    fn a_window_renders_the_crop_of_the_full_frame() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, mut cfg)) = crate::headless_tests::renderer(320, 120) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        if !measured_parity_adapter(&renderer) {
            eprintln!("unmeasured rasterizer; crop comparison not run");
            return;
        }
        let snap = snapshot_of(20, 4, b"hello\r\nworld");
        stabilize_padding(&renderer, &mut cfg, &snap);
        let views = [pane(&snap, 320, 120)];
        let on = capture(&mut renderer, &cfg, &views, &focused(true));
        let off = capture(&mut renderer, &cfg, &views, &focused(false));
        assert_cursor_edges_are_stable(&renderer);
        let rect = PatchRect {
            x: 2,
            y: 1,
            width: 200,
            height: 90,
        };
        for (frame, cursor_on) in [(&on, true), (&off, false)] {
            let mut window = renderer.scene_window_for_tests(&cfg, rect, cursor_on);
            // capture() writes straight PNG pixels from a non-live scene.
            // This comparison includes alpha even when the fake surface is Opaque.
            crate::unpremultiply_rgba8(window.as_mut(), renderer.config.format.is_srgb());
            let crop = image::imageops::crop_imm(frame, rect.x, rect.y, rect.width, rect.height)
                .to_image();
            assert!(window == crop, "cursor_on {cursor_on}");
        }
    }

    /// Why an off-phase frame of `bytes` in a pane at `rect` has no patch
    /// (`Ok` when it has one), or `None` on a host with no GPU adapter.
    fn off_frame_reason(
        cfg: &Config,
        translucent_surface: bool,
        bytes: &[u8],
        rect: Rect4,
    ) -> Option<Result<(), CursorPatchIneligible>> {
        let (width, height) = (320, 120);
        let mut renderer =
            Renderer::headless_for_tests_with(cfg, width, height, 1.0, translucent_surface)
                .expect("headless renderer builds")?;
        let snap = snapshot_of(20, 4, bytes);
        let views = [PaneView {
            rect,
            ..pane(&snap, width, height)
        }];
        capture(&mut renderer, cfg, &views, &focused(false));
        Some(renderer.cursor_patch_for_tests(cfg).map(|_| ()))
    }

    const FULL: Rect4 = (0.0, 0.0, 320.0, 120.0);

    /// On a translucent window an inverted glyph whose ink leaves the block
    /// would put a translucent pixel under an opaque patch pixel.
    #[test]
    fn an_overhanging_glyph_on_a_translucent_window_is_ineligible() {
        let _serialized = gpu_test_guard();
        let mut cfg = gpu_test_config();
        cfg.cell_width = 0.6;
        cfg.background_opacity = 0.86;
        for mode in [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
        ] {
            let Some(mut renderer) = Renderer::headless_for_tests_with(&cfg, 320, 120, 1.0, true)
                .expect("headless renderer builds")
            else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            renderer.supported_alpha_modes = vec![mode];
            let snap = snapshot_of(20, 4, b"WWWW\x1b[2G");
            capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 320, 120)],
                &focused(false),
            );
            assert_eq!(
                renderer.cursor_patch_for_tests(&cfg).err(),
                Some(CursorPatchIneligible::TranslucentOverhang),
                "{mode:?} must reject a changed pixel outside the opaque block"
            );
        }
    }

    /// Auto and Inherit leave the surface's alpha convention to the platform,
    /// so the combine cannot know what the window shows.
    #[test]
    fn an_unknown_alpha_convention_is_ineligible() {
        let _serialized = gpu_test_guard();
        let mut cfg = gpu_test_config();
        cfg.background_opacity = 0.86;
        for mode in [
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Inherit,
        ] {
            let Some(mut renderer) = Renderer::headless_for_tests_with(&cfg, 320, 120, 1.0, true)
                .expect("headless renderer builds")
            else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            renderer.supported_alpha_modes = vec![mode];
            let snap = snapshot_of(20, 4, b"hello");
            capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 320, 120)],
                &focused(false),
            );
            assert_eq!(
                renderer.config.alpha_mode, mode,
                "the surface took the only mode offered"
            );
            assert_eq!(
                renderer.cursor_patch_for_tests(&cfg).err(),
                Some(CursorPatchIneligible::AlphaMode),
                "{mode:?} must be ineligible"
            );
        }
    }

    /// A frame drawn under one alpha convention is no hand-off for another,
    /// even if nothing dropped the frame's record when the mode changed.
    #[test]
    fn a_changed_alpha_convention_is_a_changed_config() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        let snap = snapshot_of(20, 4, b"hello");
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let drawn = renderer.config.alpha_mode;
        renderer.config.alpha_mode = if drawn == wgpu::CompositeAlphaMode::Opaque {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else {
            wgpu::CompositeAlphaMode::Opaque
        };
        assert_eq!(
            renderer.cursor_patch_for_tests(&cfg).err(),
            Some(CursorPatchIneligible::ConfigChanged)
        );
    }

    /// Vi mode's hollow cursor never blinks, and an on-phase frame is not a
    /// hand-off frame.
    #[test]
    fn vi_mode_and_the_on_phase_are_ineligible() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        let mut snap = snapshot_of(20, 4, b"hello");
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        assert_eq!(
            renderer.cursor_patch_for_tests(&cfg).err(),
            Some(CursorPatchIneligible::PhaseOn)
        );
        snap.vi_mode = true;
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        assert_eq!(
            renderer.cursor_patch_for_tests(&cfg).err(),
            Some(CursorPatchIneligible::NoCursor)
        );
    }

    /// A pane origin 1/512 px past a pixel centre puts the cursor's edges in
    /// the snap band; the same pane exactly on the centre does not.
    #[test]
    fn a_cursor_edge_in_the_snap_band_is_ineligible() {
        let _serialized = gpu_test_guard();
        let mut cfg = gpu_test_config();
        cfg.padding_x = 0.0;
        cfg.padding_y = 0.0;
        let band = (32.5 + 1.0 / 512.0, 0.0, 280.0, 120.0);
        let Some(result) = off_frame_reason(&cfg, false, b"\x1b[1Gx\x1b[1G", band) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        assert_eq!(result, Err(CursorPatchIneligible::SnapBoundary));
        // Exact-centre stability is covered by the pure edge test. For the
        // eligible control, pick padding from the current font metrics.
        let Some(mut renderer) = Renderer::headless_for_tests(&cfg, 320, 120).expect("renderer")
        else {
            return;
        };
        let snap = snapshot_of(20, 4, b"x\x1b[1G");
        stabilize_padding(&renderer, &mut cfg, &snap);
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        assert_cursor_edges_are_stable(&renderer);
        assert!(
            renderer.cursor_patch_for_tests(&cfg).is_ok(),
            "stable control is eligible"
        );
    }

    /// The starfield shades by fragment position, which a patch target moves.
    #[test]
    fn a_starfield_is_ineligible() {
        let _serialized = gpu_test_guard();
        let mut cfg = gpu_test_config();
        cfg.background_type = BackgroundType::Starfield;
        let Some(result) = off_frame_reason(&cfg, false, b"hello", FULL) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        assert_eq!(result, Err(CursorPatchIneligible::Background));
    }

    /// A linearly filtered image over the cursor does not crop exactly.
    #[test]
    fn an_image_over_the_cursor_is_ineligible() {
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        let snap = snapshot_of(20, 4, b"hello");
        let image = kettle_core::Placement {
            abs_line: crate::snapshot_viewport_top(&snap),
            col: 4,
            cell_cols: 2,
            cell_rows: 1,
            x_offset_cells: 0.0,
            y_offset_cells: 0.0,
            display_cols: 2.0,
            display_rows: 1.0,
            img: kettle_core::ImageData::new(3, 2, vec![200; 3 * 2 * 4]).expect("pixels"),
            source_rect: None,
            source_crop: None,
            id: Some(1),
            placement_id: 0,
            kitty_params: None,
            z: 0,
        };
        let images = [image];
        let views = [PaneView {
            images: &images,
            ..pane(&snap, 320, 120)
        }];
        capture(&mut renderer, &cfg, &views, &focused(false));
        assert_eq!(
            renderer.cursor_patch_for_tests(&cfg).err(),
            Some(CursorPatchIneligible::Overlap)
        );
    }

    /// The combine's sRGB decode and re-encode returns every byte, for the
    /// formats the scene and the layer use.
    #[test]
    fn the_combine_returns_every_byte_of_the_on_frame() {
        let _serialized = gpu_test_guard();
        let cfg = gpu_test_config();
        let Some((device, queue)) = pollster::block_on(async {
            let (_instance, adapter) = crate::resolve_headless_adapter(&cfg, "cursor-patch")
                .await
                .ok()?;
            adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .ok()
        }) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        use wgpu::TextureFormat::{Bgra8UnormSrgb, Rgba8UnormSrgb};
        for (scene, layer) in [
            (Rgba8UnormSrgb, Rgba8UnormSrgb),
            (Bgra8UnormSrgb, Bgra8UnormSrgb),
            (Rgba8UnormSrgb, Bgra8UnormSrgb),
        ] {
            let mut texels = Vec::new();
            for value in 0..=255u8 {
                texels.extend_from_slice(&[value, 255 - value, value.wrapping_mul(7), 255]);
            }
            let on = readable_target(&device, scene, 256, 1);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &on,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &texels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256 * 4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            // Never written, so transparent black: every texel differs.
            let off = readable_target(&device, scene, 256, 1);
            let out = readable_target(&device, layer, 256, 1);
            let mut patch = PatchPipeline::new(&device);
            patch.ensure_pipeline(&device, layer);
            let bind_group = patch.bind_group(
                &device,
                &on.create_view(&wgpu::TextureViewDescriptor::default()),
                &off.create_view(&wgpu::TextureViewDescriptor::default()),
            );
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            patch
                .encode_combine(
                    &mut encoder,
                    &out.create_view(&wgpu::TextureViewDescriptor::default()),
                    layer,
                    &bind_group,
                )
                .expect("the combine pipeline exists");
            let got = read_texture(&device, &queue, &out, encoder);
            let swapped = scene != layer;
            for (value, (want, got)) in texels.chunks(4).zip(got.chunks(4)).enumerate() {
                let got = if swapped {
                    [got[2], got[1], got[0], got[3]]
                } else {
                    [got[0], got[1], got[2], got[3]]
                };
                assert_eq!(
                    got,
                    [want[0], want[1], want[2], 255],
                    "{scene:?} -> {layer:?} texel {value}"
                );
            }
        }
    }

    /// On the macOS runner: a standalone CAMetalLayer takes one patch frame
    /// from a headless renderer, sized to the patch.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_metal_layer_takes_one_patch_frame() {
        const CHILD: &str = "KETTLE_CURSOR_PATCH_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "cursor_patch::tests::a_metal_layer_takes_one_patch_frame",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(CHILD, "1")
                    .spawn()
                    .expect("layer test child");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        assert!(status.success(), "layer test child failed");
                        return;
                    }
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(20))
                    }
                    result => {
                        // Signal only the child recorded by this spawn, then reap it.
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("layer test exceeded 30 seconds or wait failed: {result:?}");
                    }
                }
            }
        }
        let _serialized = gpu_test_guard();
        let Some((mut renderer, cfg)) = crate::headless_tests::renderer(320, 120) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        let info = renderer.gpu.adapter.get_info();
        if !measured_layer_adapter(&info.name, info.backend) {
            eprintln!("unmeasured Metal adapter; layer presentation not run");
            return;
        }
        let mut cfg = cfg;
        let layer = objc2_quartz_core::CAMetalLayer::new();
        let pointer = std::ptr::NonNull::from(&*layer).cast::<std::ffi::c_void>();
        // SAFETY: `layer` is a live CAMetalLayer that outlives the renderer's
        // use of it (the renderer is detached before `layer` drops).
        unsafe { renderer.attach_cursor_layer(pointer) }.expect("the layer attaches");
        let snap = snapshot_of(20, 4, b"hello");
        stabilize_padding(&renderer, &mut cfg, &snap);
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let CursorPatchOutcome::Presented(rect) = renderer.present_cursor_patch(&cfg) else {
            panic!("the patch presents");
        };
        let size = layer.drawableSize();
        assert_eq!(
            (size.width as u32, size.height as u32),
            (rect.width, rect.height),
            "the layer's drawables are patch-sized"
        );
        renderer.detach_cursor_layer();
        assert_eq!(
            renderer.present_cursor_patch(&cfg),
            CursorPatchOutcome::Ineligible(CursorPatchIneligible::NoLayer)
        );
    }
}
