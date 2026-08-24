//! Offscreen render targets.
//!
//! The renderer draws into a plain `TextureView`, so a windowed host can hand
//! it a surface texture and tests can hand it one of these. Having the headless
//! path be the *same* path is what makes visual regression testing possible at
//! all (spec §17.3) — there is no second renderer to drift.

use crate::gpu::{GpuContext, RenderError};
use crate::texture::Rgba8Image;

/// A texture the renderer can draw into and the CPU can read back.
#[derive(Debug)]
pub struct OffscreenTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl OffscreenTarget {
    /// Creates a target of the given pixel size.
    ///
    /// The format is sRGB so that read-back bytes are directly comparable with
    /// the PNG a designer would export, rather than being linear values that
    /// only match after a conversion nobody remembers to apply.
    pub fn new(gpu: &GpuContext, width: u32, height: u32) -> Result<OffscreenTarget, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::Unsupported(format!("offscreen target must have area, got {width}x{height}")));
        }
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("a2d offscreen target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(OffscreenTarget { texture, view, width, height, format })
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Copies the rendered pixels back to the CPU.
    ///
    /// Row pitch on the GPU is padded to a 256-byte alignment, so the copy
    /// cannot go straight into a tightly packed buffer; the padding is stripped
    /// row by row here.
    pub fn read_pixels(&self, gpu: &GpuContext) -> Result<Rgba8Image, RenderError> {
        let unpadded_row = self.width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(align) * align;

        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("a2d readback"),
            size: (padded_row * self.height) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder =
            gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a2d readback") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        gpu.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            // A closed channel just means the caller gave up first.
            let _ = sender.send(result);
        });
        gpu.device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| RenderError::Readback(format!("waiting for the GPU failed: {e}")))?;
        receiver
            .recv()
            .map_err(|_| RenderError::Readback("the map callback never ran".into()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;

        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded_row * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded_row) as usize;
            pixels.extend_from_slice(&mapped[start..start + unpadded_row as usize]);
        }
        drop(mapped);
        buffer.unmap();

        Ok(Rgba8Image { width: self.width, height: self.height, pixels })
    }
}

/// The depth-stencil attachment clipping needs, resized to match its target.
#[derive(Debug)]
pub(crate) struct DepthStencil {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl DepthStencil {
    pub(crate) fn new(gpu: &GpuContext, width: u32, height: u32) -> DepthStencil {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("a2d depth-stencil"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::DEPTH_STENCIL_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        DepthStencil {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub(crate) fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width.max(1) && self.height == height.max(1)
    }
}

#[cfg(test)]
mod tests {
    /// Row padding is what makes read-back subtle, so the arithmetic is checked
    /// on its own — it holds regardless of whether a GPU is present.
    fn padded_row(width: u32) -> u32 {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        (width * 4).div_ceil(align) * align
    }

    #[test]
    fn row_pitch_is_rounded_up_to_the_copy_alignment() {
        assert_eq!(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 256);
        // 64 px * 4 bytes is exactly 256, so no padding is added.
        assert_eq!(padded_row(64), 256);
        // 65 px needs 260 bytes, which rounds up to two alignment units.
        assert_eq!(padded_row(65), 512);
        assert_eq!(padded_row(1), 256);
    }

    #[test]
    fn padded_rows_are_never_shorter_than_the_real_row() {
        for width in [1u32, 7, 63, 64, 65, 100, 255, 256, 1023] {
            assert!(padded_row(width) >= width * 4, "width {width}");
            assert_eq!(padded_row(width) % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0, "width {width}");
        }
    }
}
