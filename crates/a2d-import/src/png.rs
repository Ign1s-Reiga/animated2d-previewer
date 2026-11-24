//! PNG encoding shared by the importers that hand a package its texture pages.

use a2d_core::DecodeError;

/// Encodes decoded pixels as a PNG.
pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8], label: &str) -> Result<Vec<u8>, DecodeError> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(DecodeError::corrupt(format!(
            "texture {label} is {width}x{height} but carries {} bytes, not {expected}",
            rgba.len()
        )));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| DecodeError::corrupt(format!("could not start PNG for {label}: {e}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| DecodeError::corrupt(format!("could not encode PNG for {label}: {e}")))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_of_the_wrong_length_is_refused_and_a_right_one_encodes() {
        assert!(encode_png(2, 2, &[0; 15], "t").is_err());
        let bytes = encode_png(1, 1, &[1, 2, 3, 4], "t").expect("encodes");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
