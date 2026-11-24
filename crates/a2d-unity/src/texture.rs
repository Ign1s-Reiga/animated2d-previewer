//! Unity `Texture2D`: its header, and the block formats these bundles use.
//!
//! Field order, little-endian, verified against a real 2022.3 texture by
//! locating each known value in the raw object:
//!
//! ```text
//! string m_Name                    (length-prefixed, padded to four)
//! i32    m_ForcedFallbackFormat
//! u8     m_DownscaleFallback
//! u8     m_IsAlphaChannelOptional   (then align)
//! i32    m_Width
//! i32    m_Height
//! i32    m_CompleteImageSize
//! i32    m_MipsStripped
//! i32    m_TextureFormat
//! i32    m_MipCount
//! ...                               settings this does not need
//! i32    image data length
//! u8[]   image data
//! ...                               m_StreamData
//! ```
//!
//! The fields after the mip count vary by version and are not read. The image
//! data is found instead by looking for the length word that agrees with
//! `m_CompleteImageSize` and leaves only a trailer behind it — a search rather
//! than an offset, so a version that adds a settings field does not break it.

use a2d_core::DecodeError;

use crate::reader::{Endian, Reader};
use crate::serialized::{ClassId, SerializedFile};

/// The texture formats these bundles actually use.
///
/// Unity has dozens; naming only what is decoded keeps the mapping honest, and
/// anything else is refused by number rather than mis-decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// Straight RGBA, one byte per channel.
    Rgba32,
    /// BGRA order, as some platforms store it.
    Bgra32,
    Rgb24,
    /// BC1: four-by-four blocks, colour only.
    Dxt1,
    /// BC3: four-by-four blocks, colour plus interpolated alpha.
    Dxt5,
}

impl TextureFormat {
    fn from_unity(value: i32) -> Option<TextureFormat> {
        Some(match value {
            3 => TextureFormat::Rgb24,
            4 => TextureFormat::Rgba32,
            10 => TextureFormat::Dxt1,
            12 => TextureFormat::Dxt5,
            14 => TextureFormat::Bgra32,
            _ => return None,
        })
    }

    /// Bytes one image of this size occupies.
    fn encoded_size(self, width: u32, height: u32) -> u64 {
        let (w, h) = (width as u64, height as u64);
        match self {
            // Block formats round up to whole four-by-four blocks.
            TextureFormat::Dxt1 => w.div_ceil(4) * h.div_ceil(4) * 8,
            TextureFormat::Dxt5 => w.div_ceil(4) * h.div_ceil(4) * 16,
            TextureFormat::Rgb24 => w * h * 3,
            TextureFormat::Rgba32 | TextureFormat::Bgra32 => w * h * 4,
        }
    }

    /// Bytes of `levels` successive mip levels starting at `width` x `height`,
    /// each half the last (never below 1x1), stopping early at 1x1.
    fn mip_chain_size(self, width: u32, height: u32, levels: u32) -> u64 {
        let (mut w, mut h) = (width, height);
        let mut total = 0u64;
        for _ in 0..levels {
            total += self.encoded_size(w, h);
            if w == 1 && h == 1 {
                break;
            }
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        total
    }
}

/// A decoded texture page.
#[derive(Clone, PartialEq, Eq)]
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    /// Straight RGBA, four bytes per pixel, top row first.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Texture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Texture")
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("rgba", &self.rgba.len())
            .finish()
    }
}

/// Reads and decodes the one `Texture2D` with this path id.
///
/// For callers that know which texture they need: an unrelated texture in a
/// format not decoded here, or one streamed from a resource file, then costs
/// nothing instead of failing the whole read.
pub fn read_texture_object(file: &SerializedFile, path_id: i64) -> Result<Texture, DecodeError> {
    let object = file
        .find_object(path_id)
        .filter(|o| o.class_id == ClassId::TEXTURE_2D)
        .ok_or_else(|| DecodeError::corrupt(format!("no Texture2D with path id {path_id}")))?;
    read_texture(file.object_data(object)?)
}

/// Reads and decodes every `Texture2D` in a serialized file.
pub fn read_textures(file: &SerializedFile) -> Result<Vec<Texture>, DecodeError> {
    let mut out = Vec::new();
    for object in &file.objects {
        if object.class_id != ClassId::TEXTURE_2D {
            continue;
        }
        out.push(read_texture(file.object_data(object)?)?);
    }
    Ok(out)
}

fn read_texture(bytes: &[u8]) -> Result<Texture, DecodeError> {
    let mut r = Reader::new(bytes, Endian::Little);
    let name = r.string()?;
    let _forced_fallback = r.i32()?;
    let _downscale_fallback = r.u8()?;
    let _alpha_optional = r.u8()?;
    r.align(4)?;
    let width = r.i32()?;
    let height = r.i32()?;
    let complete_size = r.i32()?;
    let _mips_stripped = r.i32()?;
    let format_raw = r.i32()?;
    let mip_count = r.i32()?;

    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return Err(DecodeError::corrupt(format!("texture {name:?} declares {width}x{height}")));
    }
    if complete_size <= 0 {
        return Err(DecodeError::corrupt(format!("texture {name:?} declares {complete_size} bytes of image")));
    }
    let format = TextureFormat::from_unity(format_raw).ok_or_else(|| {
        DecodeError::UnsupportedFormat(format!(
            "texture {name:?} is Unity format {format_raw}, which is not decoded here"
        ))
    })?;

    // The declared size must agree with the geometry, which is the check that
    // the header was read correctly rather than merely plausibly. A payload may
    // also carry the whole mip chain after the base level; that is accepted
    // only when its size is the exact sum of the chain the header implies (or
    // the full chain down to 1x1), and only level 0 is decoded.
    let (w, h) = (width as u32, height as u32);
    let base = format.encoded_size(w, h);
    let declared = complete_size as u64;
    let chain_ok = declared == format.mip_chain_size(w, h, mip_count.max(1) as u32)
        || declared == format.mip_chain_size(w, h, u32::MAX);
    if declared != base && !chain_ok {
        return Err(DecodeError::corrupt(format!(
            "texture {name:?} is {width}x{height} in {format:?}, which needs {base} bytes \
             (or a whole mip chain of them), but declares {complete_size}"
        )));
    }

    let data = locate_image(bytes, complete_size as usize, &name)?;
    let rgba = decode(format, w, h, &data[..base as usize], &name)?;
    Ok(Texture { name, width: width as u32, height: height as u32, format, rgba })
}

/// Finds the image payload by its length word.
///
/// The fields between the mip count and the payload differ by Unity version, so
/// the payload is found rather than seeked to: the length word repeats
/// `m_CompleteImageSize`, and the real one is the last such word that leaves
/// only a short trailer behind it.
fn locate_image<'a>(bytes: &'a [u8], size: usize, name: &str) -> Result<&'a [u8], DecodeError> {
    let mut best = None;
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let word = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or([0; 4])) as usize;
        if word == size {
            let end = at + 4 + size;
            // `m_StreamData` follows: an offset, a size and a path. Anything
            // much longer than that means this was not the length word.
            if end <= bytes.len() && bytes.len() - end <= 64 {
                best = Some(at + 4);
            }
        }
        at += 4;
    }
    let start = best.ok_or_else(|| {
        DecodeError::corrupt(format!(
            "texture {name:?} has no image payload of {size} bytes in its {} byte object; \
             it may be stored in a separate resource file, which is not read here",
            bytes.len()
        ))
    })?;
    Ok(&bytes[start..start + size])
}

fn decode(format: TextureFormat, width: u32, height: u32, data: &[u8], name: &str) -> Result<Vec<u8>, DecodeError> {
    let pixels = width as usize * height as usize;
    let mut out = vec![0u8; pixels * 4];
    match format {
        TextureFormat::Rgba32 => out.copy_from_slice(&data[..pixels * 4]),
        TextureFormat::Bgra32 => {
            for (dst, src) in out.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
            }
        }
        TextureFormat::Rgb24 => {
            for (dst, src) in out.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                dst.copy_from_slice(&[src[0], src[1], src[2], 255]);
            }
        }
        TextureFormat::Dxt1 => return decode_blocks(width, height, data, &mut out, false, name).map(|()| out),
        TextureFormat::Dxt5 => return decode_blocks(width, height, data, &mut out, true, name).map(|()| out),
    }
    // The uncompressed formats are copied row for row above, so they still
    // carry Unity's bottom-up order; the block formats undo it themselves as
    // they go. Both must come out top row first, or an atlas built on one
    // samples upside down against coordinates written for the other -- which
    // is exactly what happened: a rig whose page was Rgba32 rather than DXT5
    // drew a correct silhouette out of mostly empty texture.
    flip_rows(&mut out, width, height);
    Ok(out)
}

/// Turns a bottom-up image top-down, in place.
fn flip_rows(pixels: &mut [u8], width: u32, height: u32) {
    let stride = width as usize * 4;
    if stride == 0 || height < 2 {
        return;
    }
    for row in 0..height as usize / 2 {
        let (top, bottom) = (row * stride, (height as usize - 1 - row) * stride);
        if bottom + stride > pixels.len() {
            return;
        }
        for i in 0..stride {
            pixels.swap(top + i, bottom + i);
        }
    }
}

/// Decodes BC1 or BC3 blocks.
///
/// Both pack a four-by-four tile: BC1 as two endpoint colours and two-bit
/// indices, BC3 as the same plus an eight-byte alpha block ahead of it. Unity
/// stores rows bottom-up, which is undone here so the result reads top row
/// first like every other image in this project.
fn decode_blocks(
    width: u32,
    height: u32,
    data: &[u8],
    out: &mut [u8],
    with_alpha: bool,
    name: &str,
) -> Result<(), DecodeError> {
    let block_bytes = if with_alpha { 16 } else { 8 };
    let (bw, bh) = (width.div_ceil(4) as usize, height.div_ceil(4) as usize);
    if data.len() < bw * bh * block_bytes {
        return Err(DecodeError::corrupt(format!(
            "texture {name:?} needs {} bytes of blocks but has {}",
            bw * bh * block_bytes,
            data.len()
        )));
    }

    for by in 0..bh {
        for bx in 0..bw {
            let block = &data[(by * bw + bx) * block_bytes..][..block_bytes];
            let (alpha, colour) = if with_alpha { block.split_at(8) } else { (&[][..], block) };

            // Alpha: two endpoints then sixteen three-bit indices.
            let mut alphas = [255u8; 16];
            if with_alpha {
                let (a0, a1) = (alpha[0], alpha[1]);
                let mut table = [0u8; 8];
                table[0] = a0;
                table[1] = a1;
                if a0 > a1 {
                    for i in 1..7 {
                        table[i + 1] = (((7 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 7) as u8;
                    }
                } else {
                    for i in 1..5 {
                        table[i + 1] = (((5 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 5) as u8;
                    }
                    table[6] = 0;
                    table[7] = 255;
                }
                let bits = u64::from_le_bytes([alpha[2], alpha[3], alpha[4], alpha[5], alpha[6], alpha[7], 0, 0]);
                for (i, slot) in alphas.iter_mut().enumerate() {
                    *slot = table[(bits >> (i * 3) & 7) as usize];
                }
            }

            let c0 = u16::from_le_bytes([colour[0], colour[1]]);
            let c1 = u16::from_le_bytes([colour[2], colour[3]]);
            let rgb = |c: u16| -> [u8; 3] {
                let (r, g, b) = ((c >> 11 & 0x1F) as u8, (c >> 5 & 0x3F) as u8, (c & 0x1F) as u8);
                // Bit replication, not rounded arithmetic: it is what the format
                // specifies and what other decoders produce, so the results can
                // be compared byte for byte.
                [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
            };
            let (e0, e1) = (rgb(c0), rgb(c1));
            let mut palette = [[0u8; 3]; 4];
            palette[0] = e0;
            palette[1] = e1;
            if c0 > c1 || with_alpha {
                for i in 0..3 {
                    palette[2][i] = ((2 * e0[i] as u16 + e1[i] as u16) / 3) as u8;
                    palette[3][i] = ((e0[i] as u16 + 2 * e1[i] as u16) / 3) as u8;
                }
            } else {
                for i in 0..3 {
                    palette[2][i] = ((e0[i] as u16 + e1[i] as u16) / 2) as u8;
                }
                palette[3] = [0, 0, 0];
            }
            let indices = u32::from_le_bytes([colour[4], colour[5], colour[6], colour[7]]);

            for row in 0..4 {
                for col in 0..4 {
                    let (x, y) = (bx * 4 + col, by * 4 + row);
                    if x >= width as usize || y >= height as usize {
                        continue;
                    }
                    let i = row * 4 + col;
                    let c = palette[(indices >> (i * 2) & 3) as usize];
                    // Unity's rows run bottom-up.
                    let flipped = height as usize - 1 - y;
                    let at = (flipped * width as usize + x) * 4;
                    out[at..at + 4].copy_from_slice(&[c[0], c[1], c[2], alphas[i]]);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_is_named_or_refused_by_number() {
        assert_eq!(TextureFormat::from_unity(12), Some(TextureFormat::Dxt5));
        assert_eq!(TextureFormat::from_unity(4), Some(TextureFormat::Rgba32));
        assert_eq!(TextureFormat::from_unity(999), None);
    }

    #[test]
    fn block_formats_size_by_whole_blocks() {
        // Four-by-four blocks, so a 5-pixel edge still costs two of them.
        assert_eq!(TextureFormat::Dxt5.encoded_size(2048, 2048), 4_194_304);
        assert_eq!(TextureFormat::Dxt1.encoded_size(2048, 2048), 2_097_152);
        assert_eq!(TextureFormat::Dxt5.encoded_size(5, 5), 2 * 2 * 16);
        assert_eq!(TextureFormat::Rgba32.encoded_size(10, 10), 400);
    }

    #[test]
    fn an_uncompressed_texture_comes_out_top_row_first() {
        // Unity stores rows bottom-up whatever the format. The block decoders
        // undo that as they go; the uncompressed paths copy row for row, so
        // they have to be turned afterwards. A page that skipped this drew a
        // rig's correct silhouette out of mostly empty texture, because every
        // atlas coordinate landed on the wrong row.
        let (w, h) = (2u32, 2u32);
        // Bottom row red, top row blue, in Unity's order: red comes first.
        let data: Vec<u8> = [[255u8, 0, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255], [0, 0, 255, 255]].concat();
        let out = decode(TextureFormat::Rgba32, w, h, &data, "test").expect("should decode");
        assert_eq!(&out[..4], &[0, 0, 255, 255], "the first row read out must be the one Unity stored last");
        assert_eq!(&out[12..16], &[255, 0, 0, 255]);
    }

    #[test]
    fn flipping_an_odd_height_leaves_the_middle_row_alone() {
        let mut pixels: Vec<u8> = (0..3u8).flat_map(|row| [row, row, row, row]).collect();
        flip_rows(&mut pixels, 1, 3);
        assert_eq!(pixels, vec![2, 2, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0]);
    }

    /// One BC3 block of solid opaque red, as the format stores it.
    fn red_block() -> Vec<u8> {
        let mut b = vec![0u8; 16];
        b[0] = 255; // both alpha endpoints opaque
        b[1] = 255;
        let red = 0xF800u16; // five bits of red, full scale
        b[8..10].copy_from_slice(&red.to_le_bytes());
        b[10..12].copy_from_slice(&red.to_le_bytes());
        b
    }

    #[test]
    fn a_solid_block_decodes_to_that_colour() {
        let mut out = vec![0u8; 4 * 4 * 4];
        decode_blocks(4, 4, &red_block(), &mut out, true, "test").expect("should decode");
        for pixel in out.chunks_exact(4) {
            assert_eq!(pixel, [255, 0, 0, 255], "every pixel of a solid block is that colour");
        }
    }

    #[test]
    fn rows_come_back_top_first() {
        // Two blocks stacked; Unity stores them bottom-up, so the block written
        // first must come out at the bottom of the image.
        let mut data = red_block();
        let mut blue = vec![0u8; 16];
        blue[0] = 255;
        blue[1] = 255;
        let b = 0x001Fu16;
        blue[8..10].copy_from_slice(&b.to_le_bytes());
        blue[10..12].copy_from_slice(&b.to_le_bytes());
        data.extend_from_slice(&blue);

        let mut out = vec![0u8; 4 * 8 * 4];
        decode_blocks(4, 8, &data, &mut out, true, "test").expect("should decode");
        assert_eq!(&out[..4], [0, 0, 255, 255], "the second block ends up on top");
        let last = out.len() - 4;
        assert_eq!(&out[last..], [255, 0, 0, 255], "the first block ends up at the bottom");
    }

    #[test]
    fn a_short_block_run_is_an_error_and_never_a_panic() {
        let mut out = vec![0u8; 4 * 4 * 4];
        assert!(decode_blocks(64, 64, &[0u8; 16], &mut out, true, "test").is_err());
    }

    #[test]
    fn a_truncated_texture_object_is_an_error_and_never_a_panic() {
        for cut in 0..80 {
            let _ = read_texture(&vec![0u8; cut]);
        }
    }
}

#[cfg(test)]
mod mip_tests {
    use super::*;
    use crate::testing::BundleBuilder;
    use crate::{Bundle, SerializedFile};

    fn read(builder: &BundleBuilder) -> Result<Vec<Texture>, DecodeError> {
        let bytes = builder.build();
        let bundle = Bundle::parse(&bytes).expect("bundle");
        let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("node");
        let file = SerializedFile::parse(bundle.node_data(node).expect("data")).expect("file");
        read_textures(&file)
    }

    fn red_block() -> Vec<u8> {
        let mut b = vec![0u8; 16];
        b[0] = 255;
        b[1] = 255;
        b[8..10].copy_from_slice(&0xF800u16.to_le_bytes());
        b[10..12].copy_from_slice(&0xF800u16.to_le_bytes());
        b
    }

    #[test]
    fn a_dxt5_payload_carrying_its_mip_chain_decodes_level_zero() {
        // 8x8: four blocks, then 4x4, 2x2 and 1x1 at one block each.
        let mut data = red_block().repeat(4);
        data.extend_from_slice(&[0u8; 48]);
        assert_eq!(TextureFormat::Dxt5.mip_chain_size(8, 8, 4), 112);
        assert_eq!(data.len(), 112);
        let textures = read(&BundleBuilder::new().texture_mips("t", 8, 8, 12, 4, &data)).expect("decodes");
        assert_eq!((textures[0].width, textures[0].height), (8, 8));
        assert!(textures[0].rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]), "level 0 only");
    }

    #[test]
    fn the_full_chain_is_accepted_when_the_header_counts_one_level() {
        let mut data = red_block().repeat(4);
        data.extend_from_slice(&[0u8; 48]);
        assert!(read(&BundleBuilder::new().texture_mips("t", 8, 8, 12, 1, &data)).is_ok());
    }

    #[test]
    fn an_uncompressed_payload_carrying_its_mip_chain_decodes_level_zero() {
        // 2x2 RGBA32 then one 1x1 level: 16 + 4 bytes.
        let mut data = [9u8, 8, 7, 255].repeat(4);
        data.extend_from_slice(&[1, 2, 3, 4]);
        let textures = read(&BundleBuilder::new().texture_mips("t", 2, 2, 4, 2, &data)).expect("decodes");
        assert!(textures[0].rgba.chunks_exact(4).all(|p| p == [9, 8, 7, 255]));
    }

    #[test]
    fn any_other_size_is_still_refused() {
        // One byte more than the base level, and not a chain.
        let err = read(&BundleBuilder::new().texture_mips("t", 2, 2, 4, 1, &[0u8; 17])).unwrap_err();
        assert!(err.to_string().contains("declares 17"), "{err}");
        // Neither the base level nor any chain.
        let err = read(&BundleBuilder::new().texture_mips("t", 2, 2, 4, 3, &[0u8; 21])).unwrap_err();
        assert!(err.to_string().contains("declares 21"), "{err}");
    }
}
