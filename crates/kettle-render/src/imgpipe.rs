//! Textured-quad pipeline for compositing decoded images (Sixel / kitty /
//! iTerm2) onto the grid. Textures are cached by `ImageData` identity so a
//! static image uploads once.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use bytemuck::{Pod, Zeroable};
use kettle_core::{
    GraphicsBudget, GraphicsReservation, ImageData, ImageSourceCrop, ImageSourceRect, PixelBuffer,
};

use crate::upload::{
    RetainedBytes, UploadCounters, UploadCounts, write_buffer_if_changed, write_texture_counted,
};

pub(crate) struct ImageItem {
    rect: [f32; 4],
    image: ImageData,
    source_rect: Option<ImageSourceRect>,
    source_crop: Option<ImageSourceCrop>,
    /// Optional destination clip in physical surface pixels. Inline terminal
    /// images use the owning pane's grid viewport; wallpapers stay unclipped.
    clip_rect: Option<[f32; 4]>,
    uv_override: Option<([f32; 2], [f32; 2])>,
    repeat: bool,
}

#[cfg(test)]
mod cache_lifetime_tests {
    use super::{ImageItem, ImagePipeline};
    use kettle_core::ImageData;
    use std::sync::Arc;

    #[test]
    fn composition_after_placement_drop_refreshes_cached_pixels_without_copying() {
        let _serialized = crate::gpu_tests::gpu_test_guard();
        pollster::block_on(async {
            let Ok((_, adapter)) = crate::resolve_headless_adapter(
                &crate::gpu_tests::gpu_test_config(),
                "image-cache-compose",
            )
            .await
            else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            eprintln!("image-cache-compose adapter: {:?}", adapter.get_info());
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("image-cache-compose"),
                    required_limits: crate::live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .expect("GPU device");
            let mut pipeline = ImagePipeline::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb)
                .expect("image pipeline");
            let mut image = ImageData::new(1, 1, vec![255, 0, 0, 255]).unwrap();
            let later = ImageData::new(1, 1, vec![0, 0, 255, 255]).unwrap();
            let previous_key = image.allocation_key();
            let later_key = later.allocation_key();
            let data_ptr = image.rgba.as_ptr();
            pipeline.upload(
                &device,
                &queue,
                [2.0, 1.0],
                &[
                    ImageItem::full(0.0, 0.0, 1.0, 1.0, image.clone()),
                    ImageItem::full(1.0, 0.0, 1.0, 1.0, later.clone()),
                ],
            );
            assert_eq!(
                read_two_pixels(&device, &queue, &pipeline),
                [255, 0, 0, 255, 0, 0, 255, 255]
            );
            assert_eq!(Arc::strong_count(&image.rgba), 1);
            assert_eq!(pipeline.cache[&previous_key]._pixels.strong_count(), 1);
            let previous_texture = pipeline.cache[&previous_key].texture.clone();
            let later_texture = pipeline.cache[&later_key].texture.clone();
            let patch = ImageData::new(1, 1, vec![0, 255, 0, 255]).unwrap();
            assert!(image.compose(&patch, 0, 0, true));
            assert_eq!(image.rgba.as_ptr(), data_ptr);
            let next_key = image.allocation_key();
            assert_ne!(next_key, previous_key);
            assert!(pipeline.cache[&previous_key]._pixels.upgrade().is_none());
            let items = [
                ImageItem::full(0.0, 0.0, 1.0, 1.0, image.clone()),
                ImageItem::full(1.0, 0.0, 1.0, 1.0, later),
            ];
            pipeline.prepare_frame(&device, &items);
            pipeline.upload(&device, &queue, [2.0, 1.0], &items);
            assert_eq!(pipeline.cache[&next_key].texture, previous_texture);
            assert_eq!(pipeline.cache[&later_key].texture, later_texture);
            assert_eq!(pipeline.upload_counts().texture_writes, 3);
            assert_eq!(
                read_two_pixels(&device, &queue, &pipeline),
                [0, 255, 0, 255, 0, 0, 255, 255]
            );
        });
    }

    #[test]
    fn cached_texture_does_not_retain_cpu_pixels() {
        let _serialized = crate::gpu_tests::gpu_test_guard();
        pollster::block_on(async {
            let Ok((_, adapter)) = crate::resolve_headless_adapter(
                &crate::gpu_tests::gpu_test_config(),
                "image-cache-lifetime",
            )
            .await
            else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("image-cache-lifetime"),
                    required_limits: crate::live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .expect("GPU device");
            let mut pipeline = ImagePipeline::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb)
                .expect("image pipeline");
            let image = ImageData::new(1, 1, vec![255; 4]).unwrap();
            let key = image.allocation_key();
            let pixels = Arc::downgrade(&image.rgba);
            pipeline.upload(
                &device,
                &queue,
                [1.0, 1.0],
                &[ImageItem::full(0.0, 0.0, 1.0, 1.0, image)],
            );

            assert!(pipeline.cache.contains_key(&key));
            assert!(pipeline.has_draws());
            assert_eq!(pixels.strong_count(), 0, "GPU cache retained CPU pixels");
            assert_eq!(pipeline.cache[&key]._pixels.as_ptr() as usize, key);
            drop(pixels);
            let next = ImageData::new(1, 1, vec![0; 4]).unwrap();
            assert_ne!(next.allocation_key(), key);
        });
    }

    #[test]
    fn replacement_reuses_texture_and_preserves_later_draws_at_quota() {
        let _serialized = crate::gpu_tests::gpu_test_guard();
        pollster::block_on(async {
            let Ok((_, adapter)) = crate::resolve_headless_adapter(
                &crate::gpu_tests::gpu_test_config(),
                "image-cache-reuse",
            )
            .await
            else {
                eprintln!("no GPU adapter on this host; skipped");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("image-cache-reuse"),
                    required_limits: crate::live_device_limits(adapter.limits()),
                    ..Default::default()
                })
                .await
                .expect("GPU device");
            let budget = kettle_core::GraphicsBudget::default();
            let mut pipeline = ImagePipeline::new_with_budget_and_instance_limit(
                &device,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                budget.clone(),
                2,
            )
            .unwrap();
            let first = kettle_core::ImageData::new(1, 1, vec![255; 4]).unwrap();
            let first_key = first.allocation_key();
            let later = kettle_core::ImageData::new(1, 1, vec![0, 255, 0, 255]).unwrap();
            let later_key = later.allocation_key();
            pipeline.upload(
                &device,
                &queue,
                [2.0, 1.0],
                &[
                    ImageItem::full(0.0, 0.0, 1.0, 1.0, first),
                    ImageItem::full(1.0, 0.0, 1.0, 1.0, later.clone()),
                ],
            );
            let first_texture = pipeline.cache[&first_key].texture.clone();
            let later_texture = pipeline.cache[&later_key].texture.clone();
            let charged = pipeline._screen_gpu.bytes()
                + pipeline.instance_gpu.bytes()
                + pipeline
                    .cache
                    .values()
                    .map(|c| c._gpu.bytes())
                    .sum::<usize>();
            // Fill accounting only, not VRAM. Leave exactly one extra padded row.
            let mut remaining = budget.limits().retained_bytes - charged - 256;
            let mut other_resources = Vec::new();
            while remaining > 0 {
                let bytes = remaining.min(budget.limits().image_bytes);
                other_resources.push(budget.reserve_gpu(bytes).unwrap());
                remaining -= bytes;
            }
            let extra_row = budget.reserve_gpu(256).unwrap();
            assert!(budget.reserve_gpu(1).is_none());
            let replacement = ImageData::new(1, 1, vec![255, 0, 0, 128]).unwrap();
            let replacement_key = replacement.allocation_key();
            let replacement_items = [
                ImageItem::full(0.0, 0.0, 1.0, 1.0, replacement),
                ImageItem::full(1.0, 0.0, 1.0, 1.0, later.clone()),
            ];
            pipeline.prepare_frame(&device, &replacement_items);
            pipeline.upload(&device, &queue, [2.0, 1.0], &replacement_items);
            assert_eq!(
                pipeline.cache[&replacement_key].texture, first_texture,
                "same-size replacement allocated a new texture"
            );
            assert_eq!(pipeline.cache[&later_key].texture, later_texture);
            assert_eq!(pipeline.upload_counts().texture_writes, 3);
            let pixels = read_two_pixels(&device, &queue, &pipeline);
            assert!((i16::from(pixels[0]) - 188).abs() <= 2, "{pixels:?}");
            assert_eq!(&pixels[1..], &[0, 0, 255, 0, 255, 0, 255]);

            drop(replacement_items);
            assert_eq!(pipeline.cache[&replacement_key]._pixels.strong_count(), 0);

            // A larger replacement must release the old reservation first.
            drop(extra_row);
            let larger = ImageData::new(1, 2, vec![0, 0, 255, 255, 0, 0, 255, 255]).unwrap();
            let larger_key = larger.allocation_key();
            let larger_items = [
                ImageItem::full(0.0, 0.0, 1.0, 1.0, larger),
                ImageItem::full(1.0, 0.0, 1.0, 1.0, later.clone()),
            ];
            pipeline.prepare_frame(&device, &larger_items);
            pipeline.upload(&device, &queue, [2.0, 1.0], &larger_items);
            assert_eq!(pipeline.cache[&larger_key].texture.height(), 2);
            assert_ne!(pipeline.cache[&larger_key].texture, first_texture);
            assert_eq!(
                read_two_pixels(&device, &queue, &pipeline),
                [0, 0, 255, 255, 0, 255, 0, 255]
            );
            drop(larger_items);
            pipeline.prepare_frame(&device, &[]);
            pipeline.upload(&device, &queue, [2.0, 1.0], &[]);
            assert!(pipeline.cache.is_empty());
            assert!(!pipeline.has_draws());
            assert!(budget.reserve_gpu(768).is_some());
            drop(other_resources);
            drop(pipeline);

            assert_handoff_released_on_buffer_growth_failure(&device, &queue);
        });
    }

    fn assert_handoff_released_on_buffer_growth_failure(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let budget = kettle_core::GraphicsBudget::default();
        let mut pipeline = ImagePipeline::new_with_budget_and_instance_limit(
            device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            budget.clone(),
            128,
        )
        .unwrap();
        pipeline.upload(
            device,
            queue,
            [1.0; 2],
            &[ImageItem::full(
                0.0,
                0.0,
                1.0,
                1.0,
                ImageData::new(1, 1, vec![255; 4]).unwrap(),
            )],
        );
        let charged = pipeline._screen_gpu.bytes() + pipeline.instance_gpu.bytes() + 256;
        let mut remaining = budget.limits().retained_bytes - charged;
        let mut other_resources = Vec::new();
        while remaining > 0 {
            let bytes = remaining.min(budget.limits().image_bytes);
            other_resources.push(budget.reserve_gpu(bytes).unwrap());
            remaining -= bytes;
        }
        let replacement = ImageData::new(1, 1, vec![0; 4]).unwrap();
        let items = (0..65)
            .map(|_| ImageItem::full(0.0, 0.0, 1.0, 1.0, replacement.clone()))
            .collect::<Vec<_>>();
        pipeline.prepare_frame(device, &items);
        assert_eq!(pipeline.reusable.values().map(Vec::len).sum::<usize>(), 1);
        assert!(pipeline.cache.is_empty());
        assert!(!pipeline.upload_retained(device, queue, [1.0; 2], &items));
        assert!(pipeline.reusable.is_empty());
        assert!(pipeline.cache.is_empty());
        assert!(!pipeline.has_draws());
        assert!(
            budget.reserve_gpu(256).is_some(),
            "unused handoff reservation leaked"
        );
    }

    fn read_two_pixels(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &ImagePipeline,
    ) -> [u8; 8] {
        let size = wgpu::Extent3d {
            width: 2,
            height: 1,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image-cache-reuse-target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image-cache-reuse-readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("image-cache-reuse-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pipeline.draw(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range().unwrap();
        let pixels = data[..8].try_into().unwrap();
        drop(data);
        readback.unmap();
        pixels
    }
}

impl ImageItem {
    fn instance(&self) -> Option<Inst> {
        let (origin, size) = self
            .uv_override
            .or_else(|| source_uv(&self.image, self.source_rect, self.source_crop))?;
        clipped_instance(self.rect, origin, size, self.clip_rect)
    }

    pub(crate) fn full(x: f32, y: f32, width: f32, height: f32, image: ImageData) -> Self {
        Self {
            rect: [x, y, width, height],
            image,
            source_rect: None,
            source_crop: None,
            clip_rect: None,
            uv_override: None,
            repeat: false,
        }
    }

    pub(crate) fn tiled(width: f32, height: f32, image: ImageData) -> Self {
        let uv_size = [width / image.width as f32, height / image.height as f32];
        Self {
            rect: [0.0, 0.0, width, height],
            image,
            source_rect: None,
            source_crop: None,
            clip_rect: None,
            uv_override: Some(([0.0, 0.0], uv_size)),
            repeat: true,
        }
    }

    pub(crate) fn placement(
        rect: [f32; 4],
        image: ImageData,
        source_rect: Option<ImageSourceRect>,
        source_crop: Option<ImageSourceCrop>,
        clip_rect: [f32; 4],
    ) -> Self {
        Self {
            rect,
            image,
            source_rect,
            source_crop,
            clip_rect: Some(clip_rect),
            uv_override: None,
            repeat: false,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Inst {
    pos: [f32; 2],
    size: [f32; 2],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Screen {
    size: [f32; 2],
    _pad: [f32; 2],
}

const SHADER: &str = r#"
struct Screen { size: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32,
      @location(0) pos: vec2<f32>,
      @location(1) size: vec2<f32>,
      @location(2) uv_origin: vec2<f32>,
      @location(3) uv_size: vec2<f32>) -> VsOut {
    var c = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0));
    let corner = c[vi];
    let px = pos + corner * size;
    let ndc = vec2<f32>(px.x / screen.size.x * 2.0 - 1.0,
                         1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = uv_origin + corner * uv_size;
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, smp, in.uv);
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

/// A cached GPU texture plus a weak pin of the pixel allocation's cache key.
///
/// The cache key is `Arc::as_ptr(&img.rgba)`, the heap address of the pixel
/// buffer. A weak reference keeps its control block and that address allocated
/// while allowing the pixels and their CPU reservation to be released. Without
/// the pin, image A could cache at
/// address `P` and be dropped, and a different image B could reallocate at
/// `P` before [`ImagePipeline::prepare_frame`] evicts A. `ensure_texture(B)` would then
/// hit A's stale entry and draw A's pixels.
struct CachedTexture {
    texture: wgpu::Texture,
    _pixels: Weak<PixelBuffer>,
    /// Accounts the retained GPU allocation until cache eviction.
    _gpu: GraphicsReservation,
    clamp_bind_group: wgpu::BindGroup,
    repeat_bind_group: wgpu::BindGroup,
}

type ReusableTextures = HashMap<[u32; 2], Vec<CachedTexture>>;

fn rgba_texture_bytes(width: u32, height: u32) -> Option<usize> {
    let row = u64::from(width).checked_mul(4)?;
    let aligned_row = row.checked_add(255)? & !255;
    aligned_row.checked_mul(u64::from(height))?.try_into().ok()
}

fn rgba_pixel_bytes(width: u32, height: u32) -> Option<usize> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?
        .try_into()
        .ok()
}

fn valid_texture_bytes(img: &ImageData, max_dimension: u32, image_limit: usize) -> Option<usize> {
    if img.width == 0 || img.height == 0 || img.width > max_dimension || img.height > max_dimension
    {
        return None;
    }
    let expected = rgba_pixel_bytes(img.width, img.height)?;
    if expected != img.byte_len() || expected > image_limit {
        return None;
    }
    let bytes = rgba_texture_bytes(img.width, img.height)?;
    (bytes <= image_limit).then_some(bytes)
}

fn source_uv(
    image: &ImageData,
    source_rect: Option<ImageSourceRect>,
    source_crop: Option<ImageSourceCrop>,
) -> Option<([f32; 2], [f32; 2])> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let (uv_origin, uv_size) = if let Some(source) = source_rect {
        let x1 = source.x.checked_add(source.width)?;
        let y1 = source.y.checked_add(source.height)?;
        if source.width == 0 || source.height == 0 || x1 > image.width || y1 > image.height {
            return None;
        }

        // Sample sub-rect edges at pixel centers. A cropped texture would clamp
        // there; doing the same in the shared parent texture prevents linear
        // filtering from bleeding adjacent placeholder tiles into one another.
        let image_w = image.width as f32;
        let image_h = image.height as f32;
        let u0 = (source.x as f32 + 0.5) / image_w;
        let v0 = (source.y as f32 + 0.5) / image_h;
        let u1 = (x1 as f32 - 0.5) / image_w;
        let v1 = (y1 as f32 - 0.5) / image_h;
        ([u0, v0], [u1 - u0, v1 - v0])
    } else {
        ([0.0, 0.0], [1.0, 1.0])
    };
    let Some(crop) = source_crop else {
        return Some((uv_origin, uv_size));
    };
    if !crop.top.is_finite()
        || !crop.bottom.is_finite()
        || crop.top < 0.0
        || crop.bottom > 1.0
        || crop.top >= crop.bottom
    {
        return None;
    }
    Some((
        [uv_origin[0], uv_origin[1] + uv_size[1] * crop.top],
        [uv_size[0], uv_size[1] * (crop.bottom - crop.top)],
    ))
}

/// Build one image instance, clipping its destination and UVs together.
///
/// Clipping on the CPU keeps each pane's images in the existing globally
/// batched draw list without relying on mutable render-pass scissor state.
/// Adjusting the UVs by the same normalized fractions is essential: clamping
/// only the destination would squash the entire source into the visible slice.
fn clipped_instance(
    rect: [f32; 4],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
    clip_rect: Option<[f32; 4]>,
) -> Option<Inst> {
    if !rect
        .into_iter()
        .chain(uv_origin)
        .chain(uv_size)
        .all(f32::is_finite)
        || rect[2] <= 0.0
        || rect[3] <= 0.0
    {
        return None;
    }

    let Some(clip) = clip_rect else {
        return Some(Inst {
            pos: [rect[0], rect[1]],
            size: [rect[2], rect[3]],
            uv_origin,
            uv_size,
        });
    };
    if !clip.into_iter().all(f32::is_finite) || clip[2] <= 0.0 || clip[3] <= 0.0 {
        return None;
    }

    let rect_end = [rect[0] + rect[2], rect[1] + rect[3]];
    let clip_end = [clip[0] + clip[2], clip[1] + clip[3]];
    if !rect_end.into_iter().chain(clip_end).all(f32::is_finite) {
        return None;
    }
    let x0 = rect[0].max(clip[0]);
    let y0 = rect[1].max(clip[1]);
    let x1 = rect_end[0].min(clip_end[0]);
    let y1 = rect_end[1].min(clip_end[1]);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let u0 = (x0 - rect[0]) / rect[2];
    let v0 = (y0 - rect[1]) / rect[3];
    let u1 = (x1 - rect[0]) / rect[2];
    let v1 = (y1 - rect[1]) / rect[3];
    let instance = Inst {
        pos: [x0, y0],
        size: [x1 - x0, y1 - y0],
        uv_origin: [
            uv_origin[0] + uv_size[0] * u0,
            uv_origin[1] + uv_size[1] * v0,
        ],
        uv_size: [uv_size[0] * (u1 - u0), uv_size[1] * (v1 - v0)],
    };
    instance
        .pos
        .into_iter()
        .chain(instance.size)
        .chain(instance.uv_origin)
        .chain(instance.uv_size)
        .all(f32::is_finite)
        .then_some(instance)
}

fn capped_instance_count(requested: usize, max_instances: usize) -> usize {
    requested.min(max_instances)
}

/// Decide whether an "N image placements dropped" warning should fire this
/// frame, given how many placements are being dropped and what was last
/// warned about. Returns `(should_warn, next_last_warned)`.
///
/// Only warns on a *transition* to a new drop count (including 0 -> N and any
/// change in N), never every frame of a steady-state overflow — a REPL or TUI
/// pinned above the placement budget would otherwise spam one `log::warn!`
/// per frame forever. Dropping back to 0 clears the memory, so the next
/// overflow (even at the same count) is reported again as a fresh event.
fn dropped_warn_transition(dropped: usize, last_warned: Option<usize>) -> (bool, Option<usize>) {
    if dropped == 0 {
        (false, None)
    } else if last_warned == Some(dropped) {
        (false, last_warned)
    } else {
        (true, Some(dropped))
    }
}

fn record_draw(draws: &mut Vec<(usize, bool, u32, u32)>, key: usize, repeat: bool, index: u32) {
    if let Some((last_key, last_repeat, start, count)) = draws.last_mut()
        && *last_key == key
        && *last_repeat == repeat
        && start.saturating_add(*count) == index
    {
        *count += 1;
    } else {
        draws.push((key, repeat, index, 1));
    }
}

fn retained_upload_key(screen: [f32; 2], items: &[ImageItem]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::hash::DefaultHasher::new();
    for value in screen {
        value.to_bits().hash(&mut hash);
    }
    items.len().hash(&mut hash);
    for item in items {
        item.image.allocation_key().hash(&mut hash);
        item.image.width.hash(&mut hash);
        item.image.height.hash(&mut hash);
        for value in item.rect {
            value.to_bits().hash(&mut hash);
        }
        item.source_rect
            .map(|r| (r.x, r.y, r.width, r.height))
            .hash(&mut hash);
        item.source_crop
            .map(|c| (c.top.to_bits(), c.bottom.to_bits()))
            .hash(&mut hash);
        item.clip_rect
            .map(|rect| rect.map(f32::to_bits))
            .hash(&mut hash);
        item.uv_override
            .map(|(origin, size)| (origin.map(f32::to_bits), size.map(f32::to_bits)))
            .hash(&mut hash);
        item.repeat.hash(&mut hash);
    }
    hash.finish()
}

/// What every [`ImagePipeline`] a renderer builds for one surface format can
/// share: the render pipeline, both bind group layouts and both samplers. Each
/// image layer keeps its own uniform, instance buffer, texture cache and
/// graphics-budget reservations. Cloning a wgpu handle adds a reference, not a
/// GPU object.
pub(crate) struct ImageShared {
    pipeline: wgpu::RenderPipeline,
    screen_bgl: wgpu::BindGroupLayout,
    tex_bgl: wgpu::BindGroupLayout,
    clamp_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
}

impl ImageShared {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-img"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img-screen-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img-layout"),
            bind_group_layouts: &[Some(&screen_bgl), Some(&tex_bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("img-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x2,
                        3 => Float32x2
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The fragment shader returns PREMULTIPLIED color
                    // (`rgb * a`), so the blend must not apply alpha again.
                    // `ALPHA_BLENDING` uses `SrcAlpha` as the source factor,
                    // which would yield `rgb * a * a` and draw a 50%-opaque
                    // image at 25%. (`glyphpipe` deliberately returns STRAIGHT
                    // alpha and correctly keeps `ALPHA_BLENDING`.)
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let repeat_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img-repeat-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            screen_bgl,
            tex_bgl,
            clamp_sampler,
            repeat_sampler,
        }
    }

    /// An inline-image layer, allowed the budget's placements per frame.
    pub(crate) fn layer(
        &self,
        device: &wgpu::Device,
        budget: GraphicsBudget,
    ) -> Option<ImagePipeline> {
        let max_instances = budget.limits().placements;
        self.layer_with_instance_limit(device, budget, max_instances)
    }

    /// A layer allowed `max_instances` placements per frame. `None` when it
    /// may draw none or the budget cannot hold its buffers.
    pub(crate) fn layer_with_instance_limit(
        &self,
        device: &wgpu::Device,
        budget: GraphicsBudget,
        max_instances: usize,
    ) -> Option<ImagePipeline> {
        ImagePipeline::with_shared(device, self, budget, max_instances)
    }
}

pub struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    tex_bgl: wgpu::BindGroupLayout,
    screen_buf: wgpu::Buffer,
    screen_bg: wgpu::BindGroup,
    clamp_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
    _screen_gpu: GraphicsReservation,
    instances: wgpu::Buffer,
    instance_gpu: GraphicsReservation,
    cap: usize,
    cache: HashMap<usize, CachedTexture>,
    reusable: ReusableTextures,
    draws: Vec<(usize, bool, u32, u32)>, // (cache key, repeat, first instance, count)
    budget: GraphicsBudget,
    max_instances: usize,
    /// Drop count from the last frame a "skipping N image placements"
    /// warning fired, so `upload` logs once per exceedance transition
    /// instead of every frame of a steady-state overflow. `None` once the
    /// backlog clears (or on startup).
    last_dropped_warn: Option<usize>,
    retained_key: Option<u64>,
    screen_held: RetainedBytes,
    instances_held: RetainedBytes,
    counters: UploadCounters,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<Self> {
        Self::new_with_budget(device, format, GraphicsBudget::default())
    }

    pub fn new_with_budget(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        budget: GraphicsBudget,
    ) -> Option<Self> {
        let max_instances = budget.limits().placements;
        Self::new_with_budget_and_instance_limit(device, format, budget, max_instances)
    }

    /// A standalone layer that compiles its own pipeline, for paths that draw
    /// one frame. A renderer builds its layers through [`ImageShared`] instead.
    pub(crate) fn new_with_budget_and_instance_limit(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        budget: GraphicsBudget,
        max_instances: usize,
    ) -> Option<Self> {
        if max_instances == 0 {
            return None;
        }
        ImageShared::new(device, format).layer_with_instance_limit(device, budget, max_instances)
    }

    /// A layer drawing with `shared`, with its own uniform, instance buffer and
    /// texture cache.
    fn with_shared(
        device: &wgpu::Device,
        shared: &ImageShared,
        budget: GraphicsBudget,
        max_instances: usize,
    ) -> Option<Self> {
        if max_instances == 0 {
            return None;
        }
        let screen_bytes = std::mem::size_of::<Screen>();
        let screen_gpu = budget.reserve_gpu(screen_bytes)?;
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("img-screen-bg"),
            layout: &shared.screen_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let cap = 64.min(max_instances);
        let instance_bytes = cap.checked_mul(std::mem::size_of::<Inst>())?;
        let instance_gpu = budget.reserve_gpu(instance_bytes)?;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img-instances"),
            size: (cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Self {
            pipeline: shared.pipeline.clone(),
            tex_bgl: shared.tex_bgl.clone(),
            screen_buf,
            screen_bg,
            clamp_sampler: shared.clamp_sampler.clone(),
            repeat_sampler: shared.repeat_sampler.clone(),
            _screen_gpu: screen_gpu,
            instances,
            instance_gpu,
            cap,
            cache: HashMap::new(),
            reusable: HashMap::new(),
            draws: Vec::new(),
            budget,
            max_instances,
            last_dropped_warn: None,
            retained_key: None,
            screen_held: RetainedBytes::default(),
            instances_held: RetainedBytes::default(),
            counters: UploadCounters::default(),
        })
    }

    /// The render pipeline this layer draws with.
    #[cfg(test)]
    pub(crate) fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    /// What this pipeline has written to the GPU so far.
    pub(crate) fn upload_counts(&self) -> UploadCounts {
        self.counters.snapshot()
    }

    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        img: &ImageData,
        reusable: &mut ReusableTextures,
    ) -> Option<usize> {
        // Defense-in-depth: never hand wgpu a texture larger than the device
        // supports. wgpu's default (no error-scope) handler turns that
        // validation error into a panic, and panic=abort makes it a
        // whole-process abort. Every `ImageData` constructor caps dims at
        // MAX_IMAGE_DIM (8192), but the device limit comes from the adapter
        // and can be lower, so this is the last guard before `create_texture`.
        // Skipping the draw is strictly better than aborting the renderer.
        let Some(texture_bytes) = valid_texture_bytes(
            img,
            device.limits().max_texture_dimension_2d,
            self.budget.limits().image_bytes,
        ) else {
            log::warn!(
                "skipping {}x{} image: {} bytes exceeds/mismatches GPU dimensions or texture budget",
                img.width,
                img.height,
                img.byte_len()
            );
            return None;
        };
        let key = img.allocation_key();
        if self.cache.contains_key(&key) {
            return Some(key);
        }
        if let Some(mut cached) = reusable
            .get_mut(&[img.width, img.height])
            .and_then(Vec::pop)
        {
            self.write_pixels(queue, &cached.texture, img);
            cached._pixels = Arc::downgrade(&img.rgba);
            self.cache.insert(key, cached);
            return Some(key);
        }
        // Reserve before creating or uploading. The cache's RAII token keeps
        // both per-window and process GPU counters charged until eviction.
        let Some(gpu_reservation) = self.budget.reserve_gpu(texture_bytes) else {
            log::warn!("skipping image texture: GPU graphics budget exhausted");
            return None;
        };
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-image"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.write_pixels(queue, &tex, img);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let make_bind_group = |label, sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.tex_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let clamp_bind_group = make_bind_group("img-tex-bg", &self.clamp_sampler);
        let repeat_bind_group = make_bind_group("img-repeat-tex-bg", &self.repeat_sampler);
        self.cache.insert(
            key,
            CachedTexture {
                texture: tex,
                _pixels: Arc::downgrade(&img.rgba),
                _gpu: gpu_reservation,
                clamp_bind_group,
                repeat_bind_group,
            },
        );
        Some(key)
    }

    fn write_pixels(&self, queue: &wgpu::Queue, texture: &wgpu::Texture, img: &ImageData) {
        write_texture_counted(
            queue,
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: img.width.checked_mul(4),
                rows_per_image: Some(img.height),
            },
            wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            &self.counters,
        );
    }

    /// Image rectangles are in physical pixels; source rectangles are in the
    /// referenced image's pixel coordinates.
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        items: &[ImageItem],
    ) {
        self.retained_key = None;
        let _ = self.upload_inner(device, queue, screen, items);
    }

    pub(crate) fn upload_retained(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        items: &[ImageItem],
    ) -> bool {
        let key = retained_upload_key(screen, items);
        if self.retained_key == Some(key) {
            return true;
        }
        let complete = self.upload_inner(device, queue, screen, items);
        self.retained_key = complete.then_some(key);
        complete
    }

    fn upload_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        items: &[ImageItem],
    ) -> bool {
        // Local ownership releases unused handoff textures on every return,
        // including empty frames and instance-buffer admission failures.
        let mut reusable = std::mem::take(&mut self.reusable);
        write_buffer_if_changed(
            queue,
            &self.screen_buf,
            &mut self.screen_held,
            bytemuck::bytes_of(&Screen {
                size: screen,
                _pad: [0.0; 2],
            }),
            &self.counters,
        );
        self.draws.clear();
        if items.is_empty() {
            // No placements this frame, so nothing is being dropped; clear the
            // transition memory so a later overflow (even at the same count)
            // is reported as a fresh event rather than staying suppressed.
            self.last_dropped_warn = None;
            return true;
        }
        let item_count = capped_instance_count(items.len(), self.max_instances);
        let dropped = items.len() - item_count;
        let (should_warn, next_warn_state) =
            dropped_warn_transition(dropped, self.last_dropped_warn);
        self.last_dropped_warn = next_warn_state;
        if should_warn {
            log::warn!(
                "skipping {dropped} image placement(s): per-frame budget of {} exceeded ({} requested this frame)",
                self.max_instances,
                items.len()
            );
        }
        if item_count > self.cap {
            let Some(next_cap) = item_count.checked_next_power_of_two() else {
                log::warn!("image instance count overflow; skipping frame images");
                return false;
            };
            let next_cap = next_cap.min(self.max_instances);
            let Some(bytes) = next_cap.checked_mul(std::mem::size_of::<Inst>()) else {
                log::warn!("image instance buffer size overflow; skipping frame images");
                return false;
            };
            let Some(instance_gpu) = self.budget.reserve_gpu(bytes) else {
                log::warn!("image instance buffer growth exceeds GPU graphics budget");
                return false;
            };
            let instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img-instances"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instances = instances;
            self.instance_gpu = instance_gpu;
            self.cap = next_cap;
            self.instances_held.invalidate();
        }
        let mut insts = Vec::with_capacity(item_count);
        let mut complete = true;
        for (i, item) in items.iter().take(item_count).enumerate() {
            // Push the instance for every item so buffer slot `i` stays aligned
            // with the enumerate index stored in `draws`. Invalid or wholly
            // clipped items receive a zero-sized slot and no draw.
            let uv = item
                .uv_override
                .or_else(|| source_uv(&item.image, item.source_rect, item.source_crop));
            let Some((uv_origin, uv_size)) = uv else {
                insts.push(Inst::zeroed());
                log::warn!("skipping image placement with an invalid source rectangle");
                continue;
            };
            let Some(instance) = clipped_instance(item.rect, uv_origin, uv_size, item.clip_rect)
            else {
                insts.push(Inst::zeroed());
                continue;
            };
            insts.push(instance);
            if let Some(key) = self.ensure_texture(device, queue, &item.image, &mut reusable) {
                record_draw(&mut self.draws, key, item.repeat, i as u32);
            } else {
                complete = false;
            }
        }
        write_buffer_if_changed(
            queue,
            &self.instances,
            &mut self.instances_held,
            bytemuck::cast_slice(&insts),
            &self.counters,
        );
        complete
    }

    /// Whether the last upload left anything to draw.
    pub(crate) fn has_draws(&self) -> bool {
        !self.draws.is_empty()
    }

    /// Destination rects `[x, y, w, h]` of the images the last upload draws,
    /// read back from the retained copy of the instances. `None` when that
    /// copy does not cover them.
    pub(crate) fn drawn_rects(&self) -> Option<Vec<[f32; 4]>> {
        let size = std::mem::size_of::<Inst>();
        let bytes = self.instances_held.bytes();
        let mut rects = Vec::new();
        for &(_, _, first, count) in &self.draws {
            for index in first..first.saturating_add(count) {
                let start = (index as usize).checked_mul(size)?;
                let inst: Inst = bytemuck::pod_read_unaligned(bytes.get(start..start + size)?);
                rects.push([inst.pos[0], inst.pos[1], inst.size[0], inst.size[1]]);
            }
        }
        Some(rects)
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.screen_bg, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (key, repeat, first, count) in &self.draws {
            if let Some(cached) = self.cache.get(key) {
                let bind_group = if *repeat {
                    &cached.repeat_bind_group
                } else {
                    &cached.clamp_bind_group
                };
                pass.set_bind_group(1, bind_group, &[]);
                pass.draw(0..4, *first..first.saturating_add(*count));
            }
        }
    }

    /// Protect the entire frame before selecting exact-size reusable textures.
    /// Release incompatible/excess entries before any new GPU reservation.
    pub(crate) fn prepare_frame(&mut self, device: &wgpu::Device, items: &[ImageItem]) {
        self.reusable.clear();
        let mut needed = HashMap::<[u32; 2], usize>::new();
        let mut live = HashSet::new();
        for item in items.iter().take(self.max_instances) {
            let img = &item.image;
            let key = img.allocation_key();
            if item.instance().is_some()
                && valid_texture_bytes(
                    img,
                    device.limits().max_texture_dimension_2d,
                    self.budget.limits().image_bytes,
                )
                .is_some()
                && live.insert(key)
                && !self.cache.contains_key(&key)
            {
                *needed.entry([img.width, img.height]).or_default() += 1;
            }
        }
        let dead: Vec<usize> = self
            .cache
            .keys()
            .filter(|key| !live.contains(key))
            .copied()
            .collect();
        for key in dead {
            let cached = self.cache.remove(&key).expect("retired cache entry");
            let size = [cached.texture.width(), cached.texture.height()];
            if let Some(count) = needed.get_mut(&size)
                && *count > 0
            {
                *count -= 1;
                self.reusable.entry(size).or_default().push(cached);
            }
        }
    }
}

#[cfg(test)]
mod aba_guard_tests {
    use kettle_core::{ImageData, ImageSourceCrop, ImageSourceRect};

    #[test]
    fn weak_pin_preserves_the_key_without_retaining_pixels() {
        use std::sync::Arc;
        let rgba: Arc<Vec<u8>> = Arc::new(vec![1, 2, 3, 4]);
        let key = Arc::as_ptr(&rgba) as usize;
        let pinned = Arc::downgrade(&rgba);
        assert_eq!(pinned.as_ptr() as usize, key);
        assert_eq!(Arc::strong_count(&rgba), 1);
        drop(rgba);
        assert_eq!(pinned.strong_count(), 0);
        assert!(pinned.upgrade().is_none());
        assert_eq!(pinned.as_ptr() as usize, key);
        let next = Arc::new(vec![5, 6, 7, 8]);
        assert_ne!(Arc::as_ptr(&next) as usize, key);
    }

    #[test]
    fn texture_byte_math_accepts_limit_and_identifies_one_past() {
        let limit = kettle_core::GraphicsLimits::default().image_bytes;
        assert_eq!(super::rgba_texture_bytes(8192, 2048), Some(limit));
        assert_eq!(
            super::rgba_texture_bytes(8192, 2049),
            Some(limit + 8192 * 4)
        );
        assert_eq!(super::rgba_texture_bytes(u32::MAX, u32::MAX), None);
        assert_eq!(super::rgba_pixel_bytes(1, 8192), Some(8192 * 4));
        assert_eq!(super::rgba_texture_bytes(1, 8192), Some(8192 * 256));
    }

    #[test]
    fn source_rect_uses_pixel_centers_and_rejects_out_of_bounds() {
        let image = ImageData::new(4, 2, vec![0; 4 * 2 * 4]).expect("test image");
        assert_eq!(
            super::source_uv(&image, None, None),
            Some(([0.0; 2], [1.0; 2]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                None,
            ),
            Some(([0.625, 0.25], [0.25, 0.5]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                None,
                Some(ImageSourceCrop {
                    top: 0.25,
                    bottom: 0.75,
                }),
            ),
            Some(([0.0, 0.25], [1.0, 0.5]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                Some(ImageSourceCrop {
                    top: 0.25,
                    bottom: 0.75,
                }),
            ),
            Some(([0.625, 0.375], [0.25, 0.25]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                None,
                Some(ImageSourceCrop {
                    top: 0.75,
                    bottom: 0.25,
                }),
            ),
            None
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 4,
                    y: 0,
                    width: 1,
                    height: 1,
                }),
                None,
            ),
            None
        );
    }

    #[test]
    fn pane_clip_crops_destination_and_uvs_without_squashing() {
        let inst = super::clipped_instance(
            [-4.0, -8.0, 16.0, 16.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some([0.0, 0.0, 8.0, 4.0]),
        )
        .expect("partially visible image");
        assert_eq!(inst.pos, [0.0, 0.0]);
        assert_eq!(inst.size, [8.0, 4.0]);
        assert_eq!(inst.uv_origin, [0.25, 0.5]);
        assert_eq!(inst.uv_size, [0.5, 0.25]);
    }

    #[test]
    fn pane_clip_rejects_fully_outside_or_degenerate_destinations() {
        assert!(
            super::clipped_instance(
                [-20.0, 0.0, 4.0, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
        assert!(
            super::clipped_instance(
                [0.0, 0.0, 0.0, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
        assert!(
            super::clipped_instance(
                [f32::MAX, 0.0, f32::MAX, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
    }

    #[test]
    fn wallpaper_without_clip_preserves_destination_and_uvs() {
        let inst = super::clipped_instance([-2.0, 3.0, 8.0, 9.0], [0.1, 0.2], [0.6, 0.7], None)
            .expect("valid wallpaper");
        assert_eq!(inst.pos, [-2.0, 3.0]);
        assert_eq!(inst.size, [8.0, 9.0]);
        assert_eq!(inst.uv_origin, [0.1, 0.2]);
        assert_eq!(inst.uv_size, [0.6, 0.7]);
    }

    #[test]
    fn tiled_wallpaper_is_one_fullscreen_repeating_instance() {
        let image = ImageData::new(43, 48, vec![255; 43 * 48 * 4]).expect("wallpaper");
        let item = super::ImageItem::tiled(3840.0, 2160.0, image);
        let (uv_origin, uv_size) = item.uv_override.expect("repeat UVs");
        let inst = super::clipped_instance(item.rect, uv_origin, uv_size, item.clip_rect)
            .expect("fullscreen tile instance");

        assert!(item.repeat);
        assert_eq!(inst.pos, [0.0, 0.0]);
        assert_eq!(inst.size, [3840.0, 2160.0]);
        assert_eq!(inst.uv_origin, [0.0, 0.0]);
        assert!((inst.uv_size[0] - 3840.0 / 43.0).abs() < f32::EPSILON);
        assert!((inst.uv_size[1] - 45.0).abs() < f32::EPSILON);
    }

    #[test]
    fn retained_wallpaper_key_tracks_geometry_and_surface() {
        let image = ImageData::new(2, 2, vec![255; 16]).expect("wallpaper");
        let centered = super::ImageItem::full(-1.0, -1.0, 2.0, 2.0, image.clone());
        let same = super::ImageItem::full(-1.0, -1.0, 2.0, 2.0, image.clone());
        let moved = super::ImageItem::full(0.0, -1.0, 2.0, 2.0, image.clone());

        let key = super::retained_upload_key([1.0, 1.0], &[centered]);
        assert_eq!(key, super::retained_upload_key([1.0, 1.0], &[same]));
        assert_ne!(key, super::retained_upload_key([1.0, 1.0], &[moved]));
        assert_ne!(
            key,
            super::retained_upload_key(
                [2.0, 1.0],
                &[super::ImageItem::full(-1.0, -1.0, 2.0, 2.0, image)]
            )
        );
    }

    #[test]
    fn wallpaper_pipeline_needs_one_instance_while_inline_keeps_its_budget() {
        let inline = kettle_core::GraphicsLimits::default().placements;
        assert_eq!(super::capped_instance_count(4096, inline), inline);
        assert_eq!(super::capped_instance_count(4096, 1), 1);
    }

    #[test]
    fn dropped_warn_fires_once_per_exceedance_transition() {
        // First overflow: no prior warning recorded -> warn, remember 5.
        assert_eq!(super::dropped_warn_transition(5, None), (true, Some(5)));
        // Same drop count next frame (steady-state overflow) -> stay silent.
        assert_eq!(super::dropped_warn_transition(5, Some(5)), (false, Some(5)));
        // Drop count changes (worse overflow) -> warn again, remember 9.
        assert_eq!(super::dropped_warn_transition(9, Some(5)), (true, Some(9)));
        // Backlog clears -> reset memory, no warning for zero drops.
        assert_eq!(super::dropped_warn_transition(0, Some(9)), (false, None));
        // A fresh overflow at the *same* count as before the clear is
        // reported again rather than staying suppressed.
        assert_eq!(super::dropped_warn_transition(9, None), (true, Some(9)));
    }

    #[test]
    fn consecutive_instances_of_one_texture_are_batched() {
        let mut draws = Vec::new();
        super::record_draw(&mut draws, 10, false, 0);
        super::record_draw(&mut draws, 10, false, 1);
        super::record_draw(&mut draws, 10, true, 2);
        super::record_draw(&mut draws, 10, true, 3);
        assert_eq!(draws, vec![(10, false, 0, 2), (10, true, 2, 2)]);
    }
}
