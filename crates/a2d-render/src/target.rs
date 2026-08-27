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

    /// Copies the rendered pixels back to the CPU, as straight alpha.
    ///
    /// Row pitch on the GPU is padded to a 256-byte alignment, so the copy
    /// cannot go straight into a tightly packed buffer; the padding is stripped
    /// row by row here.
    ///
    /// The blend states leave the frame *premultiplied*, which is what a
    /// compositor showing a transparent window wants, but [`Rgba8Image`] and
    /// PNG are both defined as straight alpha. [`straighten`] does that
    /// conversion, and it is not merely cosmetic: without it an additive draw
    /// reads back as invisible. See its documentation.
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

        straighten(&mut pixels);
        Ok(Rgba8Image { width: self.width, height: self.height, pixels })
    }
}

/// Converts a rendered frame from premultiplied alpha to straight alpha.
///
/// The blend states composite into a premultiplied frame: colour arrives
/// already scaled by its own alpha, which is what lets a transparent window be
/// handed straight to a compositor. `Rgba8Image` and PNG are both straight
/// alpha, so the read-back has to divide the colour back out.
///
/// Two details make this more than a division.
///
/// The colour channels are sRGB-encoded and alpha is linear, so the division
/// has to happen in linear space; doing it on the stored bytes would skew every
/// mid-tone.
///
/// And additive draws deliberately add colour without adding alpha, so that a
/// glow lights up what is behind the window instead of punching a hole in it.
/// That leaves pixels whose colour exceeds their alpha. Such a pixel means
/// something precise in premultiplied form — light contributed with no coverage
/// of its own — and has no straight-alpha spelling at all, so the alpha is
/// raised to whatever the colour needs to survive the division. Without that, a
/// rig whose slots are *all* additive reads back as a fully transparent image
/// however much light it drew, and effect layers do come shaped that way.
///
/// Raising alpha here rather than in the blend state is what keeps the two
/// consumers honest: the window still composites the glow additively, while the
/// exported PNG still shows it.
fn straighten(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        // An opaque pixel cannot have colour above its alpha, so it is already
        // straight. This is the common case and skipping it is worth a branch.
        if px[3] == 255 {
            continue;
        }
        let linear = [srgb_to_linear(px[0]), srgb_to_linear(px[1]), srgb_to_linear(px[2])];
        let peak = linear[0].max(linear[1]).max(linear[2]);
        let scale = (px[3] as f32 / 255.0).max(peak);
        if scale <= 0.0 {
            // Nothing was drawn here, or only pure black light, which adds
            // nothing to whatever is behind it.
            px.fill(0);
            continue;
        }
        for i in 0..3 {
            px[i] = linear_to_srgb(linear[i] / scale);
        }
        px[3] = (scale * 255.0).round() as u8;
    }
}

/// Decodes one sRGB-encoded channel byte to a linear value.
fn srgb_to_linear(byte: u8) -> f32 {
    let c = byte as f32 / 255.0;
    if c <= 0.040_448_237 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Encodes a linear value back to an sRGB channel byte.
fn linear_to_srgb(value: f32) -> u8 {
    let v = value.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
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
    use super::{linear_to_srgb, srgb_to_linear, straighten};

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

    /// Every byte has to survive a decode/encode round trip, or `straighten`
    /// would shift the colour of pixels it is not otherwise changing.
    #[test]
    fn the_srgb_transfer_round_trips_every_byte() {
        for byte in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(byte)), byte, "byte {byte}");
        }
    }

    fn straightened(pixel: [u8; 4]) -> [u8; 4] {
        let mut pixels = pixel.to_vec();
        straighten(&mut pixels);
        [pixels[0], pixels[1], pixels[2], pixels[3]]
    }

    #[test]
    fn an_opaque_pixel_is_already_straight() {
        assert_eq!(straightened([200, 100, 30, 255]), [200, 100, 30, 255]);
        assert_eq!(straightened([0, 0, 0, 255]), [0, 0, 0, 255]);
    }

    /// Also the shape an additive draw of pure black leaves behind: it adds no
    /// colour and no alpha, so it must not become an occluder.
    #[test]
    fn an_untouched_pixel_stays_empty() {
        assert_eq!(straightened([0, 0, 0, 0]), [0, 0, 0, 0]);
    }

    /// White at half alpha lands in the frame as linear 0.5 -- sRGB 188 -- and
    /// has to come back out as white, because that is the colour that was drawn.
    #[test]
    fn a_half_alpha_draw_recovers_its_full_colour() {
        let out = straightened([188, 188, 188, 128]);
        assert_eq!(out[3], 128, "alpha should not move");
        assert!(out[0] >= 253, "colour should come back to white, got {out:?}");
    }

    /// The case that made a whole rig invisible: additive leaves colour with no
    /// alpha behind it, which is meaningful premultiplied and unrepresentable
    /// straight. The alpha has to rise to carry the light.
    #[test]
    fn light_with_no_alpha_behind_it_becomes_visible() {
        let out = straightened([188, 188, 188, 0]);
        assert!(out[3] > 0, "additive light must not read back as transparent, got {out:?}");
        assert!(out[0] >= 253, "and it must keep its colour, got {out:?}");
        // Half the light of a fully lit pixel, so half the alpha.
        assert!((120..=136).contains(&out[3]), "alpha should carry the intensity, got {}", out[3]);
    }

    /// Compositing the straightened pixel over black must reproduce the colour
    /// the premultiplied frame held. That is the property the conversion exists
    /// to preserve.
    #[test]
    fn straightening_preserves_what_the_frame_shows_over_black() {
        for premultiplied in [[188u8, 94, 0, 128], [255, 128, 64, 0], [60, 60, 60, 200], [10, 250, 130, 20]] {
            let out = straightened(premultiplied);
            for channel in 0..3 {
                let want = srgb_to_linear(premultiplied[channel]);
                let got = srgb_to_linear(out[channel]) * (out[3] as f32 / 255.0);
                assert!(
                    (want - got).abs() < 0.01,
                    "channel {channel} of {premultiplied:?} -> {out:?}: want {want}, got {got}"
                );
            }
        }
    }
}
