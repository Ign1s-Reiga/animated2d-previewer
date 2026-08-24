//! Endian-aware primitives for Unity's containers.
//!
//! Unity mixes endianness: the `UnityFS` container is big-endian throughout,
//! while a serialized file inside it declares its own byte order in its header
//! and is little-endian on every platform this project targets. One reader that
//! carries a flag is simpler than two that look identical.

use a2d_core::DecodeError;

/// Byte order of the stream being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

/// A bounds-checked cursor over a byte slice.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    endian: Endian,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], endian: Endian) -> Self {
        Reader { data, pos: 0, endian }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn set_endian(&mut self, endian: Endian) {
        self.endian = endian;
    }

    pub fn seek(&mut self, pos: usize) -> Result<(), DecodeError> {
        if pos > self.data.len() {
            return Err(self.short(pos - self.data.len(), "seek"));
        }
        self.pos = pos;
        Ok(())
    }

    pub fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        let target = self.pos.checked_add(n).ok_or_else(|| self.short(n, "skip"))?;
        self.seek(target)
    }

    /// Advances to the next multiple of `align`, as Unity pads most records.
    pub fn align(&mut self, align: usize) -> Result<(), DecodeError> {
        debug_assert!(align.is_power_of_two());
        let rem = self.pos % align;
        if rem != 0 {
            self.skip(align - rem)?;
        }
        Ok(())
    }

    fn short(&self, want: usize, what: &str) -> DecodeError {
        DecodeError::corrupt_at(
            format!("needed {want} more bytes for {what}, {} remain", self.remaining()),
            self.pos as u64,
        )
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.short(n, "a byte run"))?;
        if end > self.data.len() {
            return Err(self.short(n, "a byte run"));
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.bytes(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.u8()? != 0)
    }

    pub fn i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.u8()? as i8)
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let b: [u8; 2] = self.bytes(2)?.try_into().unwrap_or([0; 2]);
        Ok(match self.endian {
            Endian::Big => u16::from_be_bytes(b),
            Endian::Little => u16::from_le_bytes(b),
        })
    }

    pub fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let b: [u8; 4] = self.bytes(4)?.try_into().unwrap_or([0; 4]);
        Ok(match self.endian {
            Endian::Big => u32::from_be_bytes(b),
            Endian::Little => u32::from_le_bytes(b),
        })
    }

    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        let b: [u8; 8] = self.bytes(8)?.try_into().unwrap_or([0; 8]);
        Ok(match self.endian {
            Endian::Big => u64::from_be_bytes(b),
            Endian::Little => u64::from_le_bytes(b),
        })
    }

    pub fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(self.u64()? as i64)
    }

    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// A null-terminated string, as the container header uses.
    pub fn cstring(&mut self) -> Result<String, DecodeError> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(self.short(1, "a null terminator"));
        }
        let text = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        self.pos += 1;
        Ok(text)
    }

    /// A length-prefixed string, padded to four bytes, as serialized files use.
    pub fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.i32()?;
        if len < 0 {
            return Err(DecodeError::corrupt_at(format!("string length {len} is negative"), self.pos as u64));
        }
        let raw = self.bytes(len as usize)?;
        let text = String::from_utf8_lossy(raw).into_owned();
        self.align(4)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_respect_the_declared_byte_order() {
        let data = [0x00, 0x00, 0x01, 0x00];
        assert_eq!(Reader::new(&data, Endian::Big).u32().unwrap(), 256);
        assert_eq!(Reader::new(&data, Endian::Little).u32().unwrap(), 0x0001_0000);
    }

    #[test]
    fn a_reader_can_switch_byte_order_midway() {
        // Which is exactly what reading a serialized file inside a big-endian
        // container requires.
        let data = [0x00, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data, Endian::Big);
        assert_eq!(r.u32().unwrap(), 1);
        r.set_endian(Endian::Little);
        assert_eq!(r.u32().unwrap(), 2);
    }

    #[test]
    fn alignment_rounds_up_and_never_down() {
        let data = [0u8; 16];
        let mut r = Reader::new(&data, Endian::Big);
        r.skip(5).unwrap();
        r.align(4).unwrap();
        assert_eq!(r.position(), 8);
        r.align(4).unwrap();
        assert_eq!(r.position(), 8, "an aligned cursor must not move");
    }

    #[test]
    fn a_length_prefixed_string_consumes_its_padding() {
        // "abc" is three bytes, so five more are needed to reach the next
        // multiple of four after the length word.
        let mut data = vec![3, 0, 0, 0];
        data.extend_from_slice(b"abc");
        data.push(0);
        data.extend_from_slice(&[7, 0, 0, 0]);
        let mut r = Reader::new(&data, Endian::Little);
        assert_eq!(r.string().unwrap(), "abc");
        assert_eq!(r.i32().unwrap(), 7, "the next field must start after the padding");
    }

    #[test]
    fn a_null_terminated_string_stops_at_the_terminator() {
        let data = b"UnityFS\0rest";
        let mut r = Reader::new(data, Endian::Big);
        assert_eq!(r.cstring().unwrap(), "UnityFS");
        assert_eq!(r.position(), 8);
    }

    #[test]
    fn reading_past_the_end_is_an_error_and_never_a_panic() {
        let data = [1u8, 2];
        let mut r = Reader::new(&data, Endian::Big);
        assert!(r.u32().is_err());
        assert!(r.seek(99).is_err());
        assert!(Reader::new(b"no terminator", Endian::Big).cstring().is_err());
    }

    #[test]
    fn a_negative_string_length_is_refused_rather_than_cast() {
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(Reader::new(&data, Endian::Little).string().is_err());
    }
}
