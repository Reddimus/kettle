//! A one-off offscreen frame on either backend, for `kettle --screenshot`
//! (`capture.rs`): the scene is cleared, drawn by the caller through a
//! [`ScenePass`], and read back as RGBA8 bytes holding what an
//! `Rgba8UnormSrgb` target holds (sRGB-encoded colour premultiplied in
//! linear light).

use anyhow::{Result, anyhow};

use super::cpu::CpuPass;
use super::cpu_draw::SoftRaster;
use super::{DeviceRef, ScenePass};
use crate::cpu_raster::{CpuFramebuffer, clear_pixel, pixels_to_rgba8};

/// Draw one `width` x `height` frame in `format` with `draw`, after a
/// `LoadOp::Clear(clear)`, and return its pixels as RGBA8, rows packed.
pub(crate) fn render_offscreen_rgba8(
    device: DeviceRef<'_>,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    clear: wgpu::Color,
    draw: impl FnOnce(&mut ScenePass<'_, '_>) -> Result<()>,
) -> Result<Vec<u8>> {
    match device {
        DeviceRef::Gpu { device, queue } => {
            render_gpu(device, queue, format, width, height, clear, draw)
        }
        DeviceRef::Cpu => {
            // The CPU framebuffer is an sRGB scene target; it has no plain
            // UNORM mode.
            debug_assert!(format.is_srgb());
            let mut frame = CpuFramebuffer::new(width, height)
                .ok_or_else(|| anyhow!("a {width}x{height} CPU frame is too large"))?;
            frame
                .pixels_mut()
                .fill(clear_pixel([clear.r, clear.g, clear.b, clear.a]));
            let mut raster = SoftRaster::new();
            let mut records = Vec::new();
            {
                let mut pass =
                    CpuPass::new(frame.target(), [width, height], &mut raster, &mut records);
                draw(&mut ScenePass::Cpu(&mut pass))?;
            }
            Ok(pixels_to_rgba8(frame.pixels()))
        }
    }
}

fn render_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    w: u32,
    h: u32,
    clear: wgpu::Color,
    draw: impl FnOnce(&mut ScenePass<'_, '_>) -> Result<()>,
) -> Result<Vec<u8>> {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-screenshot-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

    let bpp = 4u32;
    let unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-screenshot-readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-screenshot-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        draw(&mut ScenePass::Gpu(&mut pass))?;
    }
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv()
        .map_err(|_| anyhow!("map channel closed"))?
        .map_err(|e| anyhow!("buffer map failed: {e:?}"))?;

    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("capture mapped range failed: {e:?}"))?;
    let mut pixels = Vec::with_capacity((unpadded * h) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    readback.unmap();
    Ok(pixels)
}
