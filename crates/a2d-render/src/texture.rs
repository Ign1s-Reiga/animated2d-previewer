//! Texture pages: decoding, upload, and the cache the renderer looks them up in.

use std::collections::HashMap;

use a2d_core::ir::atlas::{TextureFilter, TextureWrap};
use a2d_core::TextureId;

use crate::gpu::{GpuContext, RenderError};

/// Raw pixels ready for upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8Image {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, non-premultiplied unless the
    /// source page said otherwise.
    pub pixels: Vec<u8>,
}

impl Rgba8Image {
    /// A solid single-colour image, for placeholders and tests.
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Rgba8Image {
        Rgba8Image { width, height, pixels: rgba.repeat((width * height) as usize) }
    }

    /// A stable content hash, for spotting that a rendered frame changed.
    ///
    /// FNV-1a: not cryptographic, but deterministic across platforms and runs,
    /// which is the only property a regression baseline needs.
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.width.to_le_bytes().iter().chain(&self.height.to_le_bytes()).chain(&self.pixels) {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Compares two images pixel by pixel.
    ///
    /// Returns `None` when the sizes differ, since there is no meaningful
    /// per-pixel comparison to make.
    pub fn diff(&self, other: &Rgba8Image) -> Option<ImageDiff> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        let mut max_channel_delta = 0u8;
        let mut differing_pixels = 0usize;
        for (a, b) in self.pixels.chunks_exact(4).zip(other.pixels.chunks_exact(4)) {
            let delta = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            if delta > 0 {
                differing_pixels += 1;
                max_channel_delta = max_channel_delta.max(delta);
            }
        }
        Some(ImageDiff { max_channel_delta, differing_pixels, total_pixels: (self.width * self.height) as usize })
    }

    fn validate(&self, label: &str) -> Result<(), RenderError> {
        if self.width == 0 || self.height == 0 {
            return Err(RenderError::Texture {
                label: label.to_string(),
                message: format!("zero-sized image ({}x{})", self.width, self.height),
            });
        }
        let expected = self.width as usize * self.height as usize * 4;
        if self.pixels.len() != expected {
            return Err(RenderError::Texture {
                label: label.to_string(),
                message: format!(
                    "{} bytes for a {}x{} RGBA image, expected {expected}",
                    self.pixels.len(),
                    self.width,
                    self.height
                ),
            });
        }
        Ok(())
    }
}

/// Decodes a PNG into RGBA8.
///
/// Paletted, greyscale and 16-bit sources are all normalised up to 8-bit RGBA,
/// so the renderer only ever deals with one layout. PNG is the only format
/// handled here on purpose: it is what every target export uses, and a general
/// image library would be far more dependency than one format needs.
pub fn decode_png(bytes: &[u8], label: &str) -> Result<Rgba8Image, RenderError> {
    let fail = |message: String| RenderError::ImageDecode { label: label.to_string(), message };

    // png 0.18 needs `Read + Seek`; a slice supplies only `Read`.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // `normalize_to_color8` collapses 16-bit and sub-byte depths; `ALPHA`
    // synthesises an opaque alpha channel when the source has none.
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);

    let mut reader = decoder.read_info().map_err(|e| fail(e.to_string()))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| fail("image dimensions overflow the addressable buffer size".into()))?;
    let mut buffer = vec![0u8; size];
    let info = reader.next_frame(&mut buffer).map_err(|e| fail(e.to_string()))?;

    buffer.truncate(info.buffer_size());
    // `ALPHA` adds an alpha channel but does not widen grey to RGB, so a
    // greyscale page arrives as two channels and still has to be expanded.
    // Real exports do ship greyscale masks, so this is not a defensive branch.
    let pixels = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::GrayscaleAlpha => buffer.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Rgb => buffer.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        // Paletted images are expanded by `normalize_to_color8`; reaching here
        // means the decoder declined the transformation, and guessing at the
        // layout would produce a mis-sized upload.
        other => return Err(fail(format!("unsupported colour type after normalisation: {other:?}"))),
    };

    let image = Rgba8Image { width: info.width, height: info.height, pixels };
    image.validate(label)?;
    Ok(image)
}

/// Encodes an image as a PNG.
///
/// Used to write rendered frames out, so a failing visual regression can be
/// looked at rather than only reported as a changed number.
pub fn encode_png(image: &Rgba8Image) -> Result<Vec<u8>, RenderError> {
    image.validate("encoded image")?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| RenderError::ImageDecode { label: "encoded image".into(), message: e.to_string() })?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|e| RenderError::ImageDecode { label: "encoded image".into(), message: e.to_string() })?;
    }
    Ok(out)
}

/// How two renders of the same frame differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDiff {
    /// Largest absolute difference on any single channel.
    pub max_channel_delta: u8,
    pub differing_pixels: usize,
    pub total_pixels: usize,
}

impl ImageDiff {
    pub fn is_identical(&self) -> bool {
        self.differing_pixels == 0
    }

    /// Whether the difference is within a per-channel tolerance.
    ///
    /// Spec §17.3 asks for a tolerance rather than exact equality because
    /// rasterisation differs by a least significant bit across drivers, and a
    /// test that fails on a driver update tells you nothing useful.
    pub fn within(&self, tolerance: u8) -> bool {
        self.max_channel_delta <= tolerance
    }
}

impl std::fmt::Display for ImageDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} of {} pixels differ, worst channel delta {}",
            self.differing_pixels, self.total_pixels, self.max_channel_delta
        )
    }
}

/// How a page should be sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerConfig {
    pub min_filter: TextureFilter,
    pub mag_filter: TextureFilter,
    pub u_wrap: TextureWrap,
    pub v_wrap: TextureWrap,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        SamplerConfig {
            min_filter: TextureFilter::Linear,
            mag_filter: TextureFilter::Linear,
            u_wrap: TextureWrap::ClampToEdge,
            v_wrap: TextureWrap::ClampToEdge,
        }
    }
}

/// Maps an atlas filter onto a GPU filter.
///
/// The mipmapped variants collapse onto their base filter: the renderer does
/// not generate mip chains, and claiming to honour a mip setting it does not
/// implement would be worse than ignoring it.
fn filter_mode(filter: TextureFilter) -> wgpu::FilterMode {
    match filter {
        TextureFilter::Nearest | TextureFilter::MipMapNearestNearest | TextureFilter::MipMapNearestLinear => {
            wgpu::FilterMode::Nearest
        }
        _ => wgpu::FilterMode::Linear,
    }
}

fn address_mode(wrap: TextureWrap) -> wgpu::AddressMode {
    match wrap {
        TextureWrap::Repeat => wgpu::AddressMode::Repeat,
        TextureWrap::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        TextureWrap::ClampToEdge => wgpu::AddressMode::ClampToEdge,
    }
}

/// One uploaded page.
#[derive(Debug)]
pub struct GpuTexture {
    pub width: u32,
    pub height: u32,
    /// Whether the stored pixels have alpha already multiplied in. Selects the
    /// blend factors, not a shader branch.
    pub premultiplied_alpha: bool,
    bind_group: wgpu::BindGroup,
}

impl GpuTexture {
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

/// Texture pages, indexed by the [`TextureId`] the runtime emits.
///
/// Ids are assigned in upload order, which matches manifest order, so a package
/// uploaded front to back gets exactly the ids its meshes reference.
#[derive(Debug)]
pub struct TextureCache {
    layout: wgpu::BindGroupLayout,
    textures: Vec<GpuTexture>,
    samplers: HashMap<SamplerConfig, wgpu::Sampler>,
    /// Stands in for any page that failed to decode, so one bad file degrades
    /// to a visible placeholder instead of dropping the character.
    fallback: Option<TextureId>,
}

impl TextureCache {
    pub(crate) fn new(gpu: &GpuContext) -> TextureCache {
        let layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("a2d texture page"),
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
        TextureCache { layout, textures: Vec::new(), samplers: HashMap::new(), fallback: None }
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    pub fn get(&self, id: TextureId) -> Option<&GpuTexture> {
        self.textures.get(id.0 as usize)
    }

    /// Resolves an id, falling back to the placeholder when it is out of range.
    pub(crate) fn resolve(&self, id: TextureId) -> Option<&GpuTexture> {
        self.get(id).or_else(|| self.fallback.and_then(|f| self.get(f)))
    }

    pub fn clear(&mut self) {
        self.textures.clear();
        self.fallback = None;
    }

    /// Uploads an image and returns the id assigned to it.
    pub fn upload(
        &mut self,
        gpu: &GpuContext,
        label: &str,
        image: &Rgba8Image,
        premultiplied_alpha: bool,
        sampler: SamplerConfig,
    ) -> Result<TextureId, RenderError> {
        image.validate(label)?;

        let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Srgb so the GPU linearises on sample and the blend maths happens
            // in linear space, which is what the tint formula assumes.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.sampler(gpu, sampler);
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let id = TextureId(self.textures.len() as u32);
        self.textures.push(GpuTexture { width: image.width, height: image.height, premultiplied_alpha, bind_group });
        Ok(id)
    }

    /// Uploads a page from encoded PNG bytes.
    pub fn upload_png(
        &mut self,
        gpu: &GpuContext,
        label: &str,
        png_bytes: &[u8],
        premultiplied_alpha: bool,
        sampler: SamplerConfig,
    ) -> Result<TextureId, RenderError> {
        let image = decode_png(png_bytes, label)?;
        self.upload(gpu, label, &image, premultiplied_alpha, sampler)
    }

    /// Installs a placeholder used for any page that could not be uploaded.
    ///
    /// Magenta, because a missing texture should be obvious on sight rather
    /// than blending in as a plausible-looking grey.
    pub fn install_fallback(&mut self, gpu: &GpuContext) -> Result<TextureId, RenderError> {
        let image = Rgba8Image::solid(2, 2, [255, 0, 255, 255]);
        let id = self.upload(gpu, "missing texture placeholder", &image, false, SamplerConfig::default())?;
        self.fallback = Some(id);
        Ok(id)
    }

    pub fn fallback(&self) -> Option<TextureId> {
        self.fallback
    }

    /// Returns a cached sampler, creating it on first use.
    ///
    /// Cloned rather than borrowed: a `wgpu::Sampler` is a handle, so cloning
    /// is cheap, and returning a borrow tied to `&mut self` would stop the
    /// caller reading `self.layout` in the same expression.
    fn sampler(&mut self, gpu: &GpuContext, config: SamplerConfig) -> wgpu::Sampler {
        self.samplers
            .entry(config)
            .or_insert_with(|| {
                gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("a2d page sampler"),
                    address_mode_u: address_mode(config.u_wrap),
                    address_mode_v: address_mode(config.v_wrap),
                    address_mode_w: wgpu::AddressMode::ClampToEdge,
                    mag_filter: filter_mode(config.mag_filter),
                    min_filter: filter_mode(config.min_filter),
                    mipmap_filter: wgpu::FilterMode::Nearest,
                    ..Default::default()
                })
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a valid PNG so decoding is tested against a real file rather than
    /// a header. Mirrors the fixture builder in the CLI's test support module.
    fn png(width: u32, height: u32, color_type: png::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color_type);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header should write");
            let channels = match color_type {
                png::ColorType::Grayscale => 1,
                png::ColorType::Rgb => 3,
                png::ColorType::GrayscaleAlpha => 2,
                _ => 4,
            };
            let data: Vec<u8> = (0..(width * height) as usize * channels).map(|i| (i % 251) as u8).collect();
            writer.write_image_data(&data).expect("image data should write");
        }
        out
    }

    #[test]
    fn an_rgba_png_decodes_to_its_declared_size() {
        let image = decode_png(&png(8, 4, png::ColorType::Rgba), "test.png").unwrap();
        assert_eq!((image.width, image.height), (8, 4));
        assert_eq!(image.pixels.len(), 8 * 4 * 4);
    }

    #[test]
    fn an_rgb_png_gains_an_opaque_alpha_channel() {
        let image = decode_png(&png(4, 4, png::ColorType::Rgb), "test.png").unwrap();
        assert_eq!(image.pixels.len(), 4 * 4 * 4);
        // Every fourth byte is alpha, and a source without alpha is opaque.
        assert!(image.pixels.chunks_exact(4).all(|p| p[3] == 255), "alpha should be opaque");
    }

    #[test]
    fn a_greyscale_png_expands_to_equal_rgb_channels() {
        let image = decode_png(&png(4, 4, png::ColorType::Grayscale), "test.png").unwrap();
        assert_eq!(image.pixels.len(), 4 * 4 * 4);
        for pixel in image.pixels.chunks_exact(4) {
            assert_eq!(pixel[0], pixel[1], "grey must expand to equal channels");
            assert_eq!(pixel[1], pixel[2]);
            assert_eq!(pixel[3], 255, "a source without alpha is opaque");
        }
    }

    #[test]
    fn a_greyscale_alpha_png_keeps_its_alpha_while_expanding_the_grey() {
        let image = decode_png(&png(2, 2, png::ColorType::GrayscaleAlpha), "test.png").unwrap();
        assert_eq!(image.pixels.len(), 2 * 2 * 4);
        for pixel in image.pixels.chunks_exact(4) {
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
        }
        // The generated data alternates, so the alphas are not all opaque.
        assert!(image.pixels.chunks_exact(4).any(|p| p[3] != 255), "alpha should survive expansion");
    }

    #[test]
    fn a_non_png_is_an_image_decode_error_naming_the_file() {
        let err = decode_png(b"this is not a png", "hero.png").unwrap_err();
        assert!(matches!(err, RenderError::ImageDecode { .. }), "{err}");
        assert!(err.to_string().contains("hero.png"), "{err}");
    }

    #[test]
    fn a_truncated_png_is_an_error_and_never_a_panic() {
        let full = png(16, 16, png::ColorType::Rgba);
        for cut in (0..full.len()).step_by(7) {
            let _ = decode_png(&full[..cut], "hero.png");
        }
    }

    #[test]
    fn corrupting_single_bytes_never_panics() {
        let full = png(16, 16, png::ColorType::Rgba);
        for at in (0..full.len()).step_by(11) {
            let mut bytes = full.clone();
            bytes[at] ^= 0xff;
            let _ = decode_png(&bytes, "hero.png");
        }
    }

    #[test]
    fn a_solid_image_has_the_right_length_and_colour() {
        let image = Rgba8Image::solid(3, 2, [1, 2, 3, 4]);
        assert_eq!(image.pixels.len(), 3 * 2 * 4);
        assert_eq!(&image.pixels[..4], &[1, 2, 3, 4]);
        assert!(image.validate("solid").is_ok());
    }

    #[test]
    fn a_mis_sized_image_is_rejected_before_upload() {
        let bad = Rgba8Image { width: 4, height: 4, pixels: vec![0; 10] };
        let err = bad.validate("hero.png").unwrap_err();
        assert!(err.to_string().contains("expected 64"), "{err}");
    }

    #[test]
    fn a_zero_sized_image_is_rejected() {
        let bad = Rgba8Image { width: 0, height: 4, pixels: vec![] };
        assert!(bad.validate("hero.png").unwrap_err().to_string().contains("zero-sized"));
    }

    #[test]
    fn an_encoded_image_decodes_back_to_the_same_pixels() {
        let original = Rgba8Image { width: 4, height: 3, pixels: (0..48).map(|i| i as u8).collect() };
        let encoded = encode_png(&original).unwrap();
        let decoded = decode_png(&encoded, "round-trip").unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn encoding_refuses_a_mis_sized_image() {
        let bad = Rgba8Image { width: 4, height: 4, pixels: vec![0; 3] };
        assert!(encode_png(&bad).is_err());
    }

    #[test]
    fn identical_images_share_a_fingerprint() {
        let a = Rgba8Image::solid(4, 4, [1, 2, 3, 4]);
        let b = Rgba8Image::solid(4, 4, [1, 2, 3, 4]);
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn a_single_changed_pixel_changes_the_fingerprint() {
        let a = Rgba8Image::solid(4, 4, [1, 2, 3, 4]);
        let mut b = a.clone();
        b.pixels[7] = 9;
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn the_fingerprint_covers_the_dimensions_too() {
        // Same bytes, different shape: these are not the same frame.
        let a = Rgba8Image { width: 4, height: 1, pixels: vec![7; 16] };
        let b = Rgba8Image { width: 1, height: 4, pixels: vec![7; 16] };
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn a_diff_of_identical_images_is_empty() {
        let a = Rgba8Image::solid(4, 4, [10, 20, 30, 40]);
        let diff = a.diff(&a).unwrap();
        assert!(diff.is_identical());
        assert!(diff.within(0));
        assert_eq!(diff.total_pixels, 16);
    }

    #[test]
    fn a_diff_reports_the_worst_channel_and_how_many_pixels_moved() {
        let a = Rgba8Image::solid(2, 2, [10, 10, 10, 255]);
        let mut b = a.clone();
        b.pixels[0] = 15;
        b.pixels[5] = 12;
        let diff = a.diff(&b).unwrap();
        assert_eq!(diff.differing_pixels, 2);
        assert_eq!(diff.max_channel_delta, 5);
        assert!(!diff.within(4));
        assert!(diff.within(5), "a tolerance at the delta should pass");
    }

    #[test]
    fn images_of_different_sizes_have_no_comparable_diff() {
        let a = Rgba8Image::solid(2, 2, [0, 0, 0, 0]);
        let b = Rgba8Image::solid(4, 4, [0, 0, 0, 0]);
        assert!(a.diff(&b).is_none());
    }

    #[test]
    fn a_diff_reads_as_a_sentence() {
        let a = Rgba8Image::solid(2, 1, [0, 0, 0, 0]);
        let mut b = a.clone();
        b.pixels[0] = 3;
        assert_eq!(a.diff(&b).unwrap().to_string(), "1 of 2 pixels differ, worst channel delta 3");
    }

    #[test]
    fn mipmapped_filters_collapse_onto_their_base_filter() {
        assert_eq!(filter_mode(TextureFilter::Nearest), wgpu::FilterMode::Nearest);
        assert_eq!(filter_mode(TextureFilter::MipMapNearestNearest), wgpu::FilterMode::Nearest);
        assert_eq!(filter_mode(TextureFilter::Linear), wgpu::FilterMode::Linear);
        assert_eq!(filter_mode(TextureFilter::MipMapLinearLinear), wgpu::FilterMode::Linear);
    }

    #[test]
    fn wrap_modes_map_one_to_one() {
        assert_eq!(address_mode(TextureWrap::Repeat), wgpu::AddressMode::Repeat);
        assert_eq!(address_mode(TextureWrap::MirroredRepeat), wgpu::AddressMode::MirrorRepeat);
        assert_eq!(address_mode(TextureWrap::ClampToEdge), wgpu::AddressMode::ClampToEdge);
    }
}
