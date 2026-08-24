//! Big-endian binary reader for Spine's skeleton format.
//!
//! Spine writes big-endian scalars and Kryo-style variable-length integers.
//! Every read is bounds-checked and returns a located [`DecodeError`], because
//! a truncated `.skel` is ordinary input, not a bug (rule §4.13).

use a2d_core::DecodeError;

/// A cursor over skeleton bytes.
#[derive(Debug)]
pub struct BinaryReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BinaryReader { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or_else(|| DecodeError::corrupt("byte count overflow"))?;
        if end > self.data.len() {
            return Err(DecodeError::corrupt_at(
                format!("truncated while reading {what}: need {n} bytes, {} left", self.remaining()),
                self.pos as u64,
            ));
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1, "u8")?[0])
    }

    pub fn i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.u8()? as i8)
    }

    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4, "u32")?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8, "u64")?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// Reads `n` floats into a vector.
    pub fn f32_array(&mut self, n: usize) -> Result<Vec<f32>, DecodeError> {
        // Check the whole span up front so a bad count fails before allocating.
        let bytes = self.take(n.saturating_mul(4), "float array")?;
        Ok(bytes.chunks_exact(4).map(|c| f32::from_bits(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))).collect())
    }

    /// Reads `n` varint-encoded lengths into a vector of `u16` indices.
    pub fn u16_array(&mut self, n: usize) -> Result<Vec<u16>, DecodeError> {
        let bytes = self.take(n.saturating_mul(2), "short array")?;
        Ok(bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
    }

    /// Kryo variable-length unsigned integer, up to five bytes.
    pub fn varint(&mut self) -> Result<u32, DecodeError> {
        let mut b = self.u8()? as u32;
        let mut result = b & 0x7f;
        if b & 0x80 != 0 {
            b = self.u8()? as u32;
            result |= (b & 0x7f) << 7;
            if b & 0x80 != 0 {
                b = self.u8()? as u32;
                result |= (b & 0x7f) << 14;
                if b & 0x80 != 0 {
                    b = self.u8()? as u32;
                    result |= (b & 0x7f) << 21;
                    if b & 0x80 != 0 {
                        result |= (self.u8()? as u32 & 0x7f) << 28;
                    }
                }
            }
        }
        Ok(result)
    }

    /// Zig-zag decoded variable-length signed integer.
    pub fn varint_signed(&mut self) -> Result<i32, DecodeError> {
        let raw = self.varint()?;
        Ok(((raw >> 1) as i32) ^ -((raw & 1) as i32))
    }

    /// Varint length used as an array or collection count.
    pub fn count(&mut self, what: &str) -> Result<usize, DecodeError> {
        let n = self.varint()? as usize;
        // A count larger than the bytes left cannot be satisfied by any
        // subsequent read, so reject it before allocating for it.
        if n > self.remaining().saturating_mul(8) {
            return Err(DecodeError::corrupt_at(
                format!("implausible {what} count {n} with {} bytes left", self.remaining()),
                self.pos as u64,
            ));
        }
        Ok(n)
    }

    /// Spine's length-prefixed UTF-8 string. Length 0 encodes `null`, 1 encodes
    /// the empty string, and anything else is `len - 1` bytes of UTF-8.
    pub fn string_opt(&mut self) -> Result<Option<String>, DecodeError> {
        let len = self.varint()? as usize;
        match len {
            0 => Ok(None),
            1 => Ok(Some(String::new())),
            n => {
                let bytes = self.take(n - 1, "string")?;
                let at = self.pos as u64;
                String::from_utf8(bytes.to_vec())
                    .map(Some)
                    .map_err(|e| DecodeError::corrupt_at(format!("string is not valid UTF-8: {e}"), at))
            }
        }
    }

    /// Same as [`BinaryReader::string_opt`] but maps `null` to the empty string.
    pub fn string(&mut self) -> Result<String, DecodeError> {
        Ok(self.string_opt()?.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes an unsigned value the way Spine writes `writeInt(v, true)`.
    fn encode_varint(mut v: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            if v >> 7 == 0 {
                out.push(v as u8);
                return out;
            }
            out.push(((v & 0x7f) | 0x80) as u8);
            v >>= 7;
        }
    }

    fn encode_string(s: &str) -> Vec<u8> {
        let mut out = encode_varint(s.len() as u32 + 1);
        out.extend_from_slice(s.as_bytes());
        out
    }

    #[test]
    fn scalars_are_big_endian() {
        let data = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(BinaryReader::new(&data).u32().unwrap(), 0x12345678);
        assert_eq!(BinaryReader::new(&data).u16().unwrap(), 0x1234);
    }

    #[test]
    fn floats_round_trip() {
        let data = 1234.5678f32.to_be_bytes();
        assert_eq!(BinaryReader::new(&data).f32().unwrap(), 1234.5678);
    }

    #[test]
    fn varint_matches_the_reference_encoding() {
        for v in [0u32, 1, 127, 128, 300, 16_383, 16_384, 1 << 21, 1 << 28, u32::MAX >> 1] {
            let bytes = encode_varint(v);
            assert_eq!(BinaryReader::new(&bytes).varint().unwrap(), v, "value {v}");
        }
    }

    #[test]
    fn varint_uses_the_expected_byte_count() {
        assert_eq!(encode_varint(127).len(), 1);
        assert_eq!(encode_varint(128).len(), 2);
        assert_eq!(encode_varint(16_384).len(), 3);
    }

    #[test]
    fn signed_varint_zigzags() {
        for v in [0i32, -1, 1, -2, 2, 63, -64, 100_000, -100_000] {
            let zig = ((v << 1) ^ (v >> 31)) as u32;
            let bytes = encode_varint(zig);
            assert_eq!(BinaryReader::new(&bytes).varint_signed().unwrap(), v, "value {v}");
        }
    }

    #[test]
    fn strings_round_trip() {
        let bytes = encode_string("hip");
        let mut r = BinaryReader::new(&bytes);
        assert_eq!(r.string_opt().unwrap().as_deref(), Some("hip"));
        assert!(r.is_eof());
    }

    #[test]
    fn zero_length_string_is_null_and_one_is_empty() {
        assert_eq!(BinaryReader::new(&[0]).string_opt().unwrap(), None);
        assert_eq!(BinaryReader::new(&[1]).string_opt().unwrap().as_deref(), Some(""));
        assert_eq!(BinaryReader::new(&[0]).string().unwrap(), "");
    }

    #[test]
    fn non_ascii_strings_survive() {
        let bytes = encode_string("放置少女");
        assert_eq!(BinaryReader::new(&bytes).string().unwrap(), "放置少女");
    }

    #[test]
    fn invalid_utf8_is_corrupt_not_a_panic() {
        let bytes = [3u8, 0xff, 0xfe];
        let err = BinaryReader::new(&bytes).string_opt().unwrap_err();
        assert!(err.to_string().contains("UTF-8"), "{err}");
    }

    #[test]
    fn reading_past_the_end_is_a_located_error() {
        let mut r = BinaryReader::new(&[0x01, 0x02]);
        let err = r.u32().unwrap_err();
        match err {
            DecodeError::Corrupt { at: Some(0), .. } => {}
            other => panic!("expected corruption at byte 0, got {other}"),
        }
    }

    #[test]
    fn a_truncated_string_reports_the_shortfall() {
        // Claims 10 bytes, supplies 2.
        let err = BinaryReader::new(&[11, b'a', b'b']).string_opt().unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    #[test]
    fn implausible_counts_are_rejected_before_allocating() {
        // A varint claiming a million entries in a four-byte file.
        let mut bytes = encode_varint(1_000_000);
        bytes.push(0);
        let err = BinaryReader::new(&bytes).count("bone").unwrap_err();
        assert!(err.to_string().contains("implausible"), "{err}");
    }

    #[test]
    fn plausible_counts_pass() {
        let mut bytes = encode_varint(2);
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(BinaryReader::new(&bytes).count("bone").unwrap(), 2);
    }

    #[test]
    fn float_arrays_fail_before_allocating_when_short() {
        let mut r = BinaryReader::new(&[0; 7]);
        assert!(r.f32_array(2).is_err());
        assert_eq!(r.position(), 0, "a failed read must not advance the cursor");
    }

    #[test]
    fn float_arrays_read_every_element() {
        let mut bytes = Vec::new();
        for v in [1.0f32, -2.5, 3.25] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(BinaryReader::new(&bytes).f32_array(3).unwrap(), vec![1.0, -2.5, 3.25]);
    }

    #[test]
    fn seek_is_clamped_to_the_buffer() {
        let mut r = BinaryReader::new(&[1, 2, 3]);
        r.seek(99);
        assert!(r.is_eof());
        assert_eq!(r.remaining(), 0);
    }
}
