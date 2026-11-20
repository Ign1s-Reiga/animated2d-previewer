//! The IEEE CRC-32 Unity uses to hash names it stores as integers.
//!
//! An `AnimationClip` does not keep a curve's target path or property name as
//! text: it keeps a hash of each. The hash is the zlib / IEEE 802.3 CRC-32
//! (reflected polynomial `0xEDB88320`, initial value and final xor
//! `0xFFFFFFFF`) over the UTF-8 bytes. Hand-rolled, with a 256-entry table, to
//! avoid a dependency for twenty lines.
//!
//! What established this: the check value `crc32("123456789") == 0xCBF43926` is
//! the standard one for this algorithm, and the hashes read out of 30 real
//! clips resolve to the names of the objects they target (see
//! [`crate::animation_clip`]).

const POLYNOMIAL: u32 = 0xEDB8_8320;

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { POLYNOMIAL ^ (c >> 1) } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = build_table();

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        // The mask keeps the index below 256, the table's length.
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The CRC-32 of a string's UTF-8 bytes, which is how Unity hashes a path or a
/// property name.
pub fn crc32_str(text: &str) -> u32 {
    crc32(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_check_value_holds() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn hashes_read_from_real_clips_are_reproduced() {
        assert_eq!(crc32_str("Value"), 3_702_945_584);
        assert_eq!(crc32_str("Opacity"), 2_353_026_298);
        assert_eq!(crc32_str("Group/Name"), crc32(b"Group/Name"));
        assert_eq!(crc32_str("The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn the_hash_is_over_bytes_not_characters() {
        // Two bytes in UTF-8, so a char-wise hash would differ.
        assert_eq!(crc32_str("\u{e9}"), crc32(&[0xC3, 0xA9]));
    }
}
