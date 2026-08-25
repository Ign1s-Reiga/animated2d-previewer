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
    let _mip_count = r.i32()?;

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
    // the header was read correctly rather than merely plausibly.
    let expected = format.encoded_size(width as u32, height as u32);
    if expected != complete_size as u64 {
        return Err(DecodeError::corrupt(format!(
            "texture {name:?} is {width}x{height} in {format:?}, which needs {expected} bytes, \
             but declares {complete_size}"
        )));
    }

    let data = locate_image(bytes, complete_size as usize, &name)?;
    let rgba = decode(format, width as u32, height as u32, data, &name)?;
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
        TextureFormat::Dxt1 => decode_blocks(width, height, data, &mut out, false, name)?,
        TextureFormat::Dxt5 => decode_blocks(width, height, data, &mut out, true, name)?,
    }
    Ok(out)
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
