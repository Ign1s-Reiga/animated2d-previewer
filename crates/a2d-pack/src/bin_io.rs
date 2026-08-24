//! Primitives for the deterministic binary encoding used by `model.bin`.
//!
//! Determinism is a hard requirement (spec §10): golden tests compare bytes.
//! Every rule that guarantees it lives here.
//!
//! * Fixed little-endian layout for every scalar.
//! * Floats are written by their **bit pattern**, so a value round-trips
//!   exactly and never depends on a formatting locale or precision setting.
//! * Lengths are `u32` and always precede their payload.
//! * There are no maps anywhere in the format. Ordered vectors only, so
//!   iteration order cannot vary between runs or between hash seeds.

use a2d_core::DecodeError;

/// Appends values in the package's binary encoding.
#[derive(Debug, Default)]
pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Writer { out: Vec::with_capacity(n) }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.out
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    pub fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    pub fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    /// Writes a float by its bit pattern, which is exact and byte-stable.
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    /// Writes bytes verbatim, with no length prefix. For fixed-size headers.
    pub fn raw(&mut self, v: &[u8]) {
        self.out.extend_from_slice(v);
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.out.extend_from_slice(v);
    }

    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// `None` is a leading 0 byte, `Some` a leading 1 followed by the value.
    pub fn opt_str(&mut self, v: Option<&str>) {
        match v {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
        }
    }

    pub fn opt<T>(&mut self, v: Option<T>, write: impl FnOnce(&mut Self, T)) {
        match v {
            None => self.u8(0),
            Some(value) => {
                self.u8(1);
                write(self, value);
            }
        }
    }

    /// Writes a length-prefixed sequence.
    pub fn seq<T>(&mut self, items: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.u32(items.len() as u32);
        for item in items {
            write(self, item);
        }
    }

    pub fn f32_seq(&mut self, items: &[f32]) {
        self.u32(items.len() as u32);
        for v in items {
            self.f32(*v);
        }
    }

    pub fn u16_seq(&mut self, items: &[u16]) {
        self.u32(items.len() as u32);
        for v in items {
            self.u16(*v);
        }
    }

    pub fn u32_seq(&mut self, items: &[u32]) {
        self.u32(items.len() as u32);
        for v in items {
            self.u32(*v);
        }
    }
}

/// Reads values written by [`Writer`].
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
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

    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or_else(|| DecodeError::corrupt("length overflow"))?;
        if end > self.data.len() {
            return Err(DecodeError::corrupt_at(
                format!("package truncated reading {what}: need {n} bytes, {} left", self.remaining()),
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

    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4, "u32")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// Reads a length prefix, rejecting counts the remaining bytes cannot hold.
    ///
    /// Without this a corrupt length would reserve gigabytes before failing.
    pub fn len(&mut self, what: &str) -> Result<usize, DecodeError> {
        let n = self.u32()? as usize;
        if n > self.remaining() {
            return Err(DecodeError::corrupt_at(
                format!("implausible {what} length {n} with {} bytes left", self.remaining()),
                self.pos as u64,
            ));
        }
        Ok(n)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.len("byte array")?;
        self.take(n, "byte array")
    }

    pub fn str(&mut self) -> Result<String, DecodeError> {
        let at = self.pos as u64;
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| DecodeError::corrupt_at(format!("string is not valid UTF-8: {e}"), at))
    }

    pub fn opt_str(&mut self) -> Result<Option<String>, DecodeError> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.str()?))
        }
    }

    pub fn opt<T>(&mut self, read: impl FnOnce(&mut Self) -> Result<T, DecodeError>) -> Result<Option<T>, DecodeError> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(read(self)?))
        }
    }

    /// Reads a length-prefixed sequence.
    ///
    /// `min_item_bytes` is the smallest number of bytes one item can occupy; it
    /// bounds the count before anything is allocated.
    pub fn seq<T>(
        &mut self,
        what: &str,
        min_item_bytes: usize,
        mut read: impl FnMut(&mut Self) -> Result<T, DecodeError>,
    ) -> Result<Vec<T>, DecodeError> {
        let n = self.u32()? as usize;
        let floor = min_item_bytes.max(1);
        if n > self.remaining() / floor + 1 {
            return Err(DecodeError::corrupt_at(
                format!("implausible {what} count {n} with {} bytes left", self.remaining()),
                self.pos as u64,
            ));
        }
        let mut out = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            out.push(read(self)?);
        }
        Ok(out)
    }

    pub fn f32_seq(&mut self) -> Result<Vec<f32>, DecodeError> {
        self.seq("float", 4, |r| r.f32())
    }

    pub fn u16_seq(&mut self) -> Result<Vec<u16>, DecodeError> {
        self.seq("u16", 2, |r| r.u16())
    }

    pub fn u32_seq(&mut self) -> Result<Vec<u32>, DecodeError> {
        self.seq("u32", 4, |r| r.u32())
    }

    /// Fails when trailing bytes remain, which means the reader and writer
    /// disagree about the layout.
    pub fn expect_eof(&self) -> Result<(), DecodeError> {
        if self.is_eof() {
            Ok(())
        } else {
            Err(DecodeError::corrupt_at(format!("{} unexpected trailing bytes", self.remaining()), self.pos as u64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_round_trip() {
        let mut w = Writer::new();
        w.u8(0xab);
        w.bool(true);
        w.bool(false);
        w.u16(0x1234);
        w.u32(0xdead_beef);
        w.i32(-42);
        let bytes = w.into_bytes();

        let mut r = Reader::new(&bytes);
        assert_eq!(r.u8().unwrap(), 0xab);
        assert!(r.bool().unwrap());
        assert!(!r.bool().unwrap());
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xdead_beef);
        assert_eq!(r.i32().unwrap(), -42);
        r.expect_eof().unwrap();
    }

    #[test]
    fn floats_round_trip_bit_exactly() {
        let values = [0.0f32, -0.0, 1.0, -1.5, f32::MIN, f32::MAX, f32::EPSILON, 1.0 / 3.0];
        let mut w = Writer::new();
        for v in values {
            w.f32(v);
        }
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        for v in values {
            assert_eq!(r.f32().unwrap().to_bits(), v.to_bits(), "value {v}");
        }
    }

    #[test]
    fn signed_zero_survives() {
        // A text encoding would collapse -0.0 into 0.0 and break byte equality.
        let mut w = Writer::new();
        w.f32(-0.0);
        let mut r = Reader::new(w.as_bytes());
        assert!(r.f32().unwrap().is_sign_negative());
    }

    #[test]
    fn non_finite_floats_round_trip() {
        let mut w = Writer::new();
        w.f32(f32::INFINITY);
        w.f32(f32::NEG_INFINITY);
        w.f32(f32::NAN);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.f32().unwrap(), f32::INFINITY);
        assert_eq!(r.f32().unwrap(), f32::NEG_INFINITY);
        assert!(r.f32().unwrap().is_nan());
    }

    #[test]
    fn strings_round_trip_including_non_ascii() {
        let mut w = Writer::new();
        w.str("hip");
        w.str("");
        w.str("放置少女");
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.str().unwrap(), "hip");
        assert_eq!(r.str().unwrap(), "");
        assert_eq!(r.str().unwrap(), "放置少女");
    }

    #[test]
    fn optional_strings_distinguish_none_from_empty() {
        let mut w = Writer::new();
        w.opt_str(None);
        w.opt_str(Some(""));
        w.opt_str(Some("x"));
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.opt_str().unwrap(), None);
        assert_eq!(r.opt_str().unwrap().as_deref(), Some(""));
        assert_eq!(r.opt_str().unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn optional_values_round_trip() {
        let mut w = Writer::new();
        w.opt(None::<f32>, |w, v| w.f32(v));
        w.opt(Some(2.5f32), |w, v| w.f32(v));
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.opt(|r| r.f32()).unwrap(), None);
        assert_eq!(r.opt(|r| r.f32()).unwrap(), Some(2.5));
    }

    #[test]
    fn sequences_round_trip() {
        let mut w = Writer::new();
        w.f32_seq(&[1.0, 2.0, 3.0]);
        w.u16_seq(&[7, 8]);
        w.u32_seq(&[9]);
        w.seq(&["a".to_string(), "b".to_string()], |w, s| w.str(s));
        let bytes = w.into_bytes();

        let mut r = Reader::new(&bytes);
        assert_eq!(r.f32_seq().unwrap(), vec![1.0, 2.0, 3.0]);
        assert_eq!(r.u16_seq().unwrap(), vec![7, 8]);
        assert_eq!(r.u32_seq().unwrap(), vec![9]);
        assert_eq!(r.seq("string", 4, |r| r.str()).unwrap(), vec!["a".to_string(), "b".to_string()]);
        r.expect_eof().unwrap();
    }

    #[test]
    fn empty_sequences_round_trip() {
        let mut w = Writer::new();
        w.f32_seq(&[]);
        let mut r = Reader::new(w.as_bytes());
        assert!(r.f32_seq().unwrap().is_empty());
    }

    #[test]
    fn writing_the_same_values_twice_produces_identical_bytes() {
        let build = || {
            let mut w = Writer::new();
            w.str("model");
            w.f32(1.0 / 3.0);
            w.f32_seq(&[0.1, 0.2, 0.3]);
            w.opt_str(Some("idle"));
            w.into_bytes()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn truncated_input_is_a_located_error() {
        let mut w = Writer::new();
        w.u32(0x1234_5678);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes[..2]);
        let err = r.u32().unwrap_err();
        assert!(matches!(err, DecodeError::Corrupt { at: Some(0), .. }), "{err}");
    }

    #[test]
    fn an_implausible_length_is_rejected_before_allocating() {
        let mut w = Writer::new();
        w.u32(u32::MAX); // claims four billion bytes
        w.u8(1);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let err = r.len("byte array").unwrap_err();
        assert!(err.to_string().contains("implausible"), "{err}");
    }

    #[test]
    fn an_implausible_sequence_count_is_rejected_before_allocating() {
        let mut w = Writer::new();
        w.u32(1_000_000_000);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let err = r.f32_seq().unwrap_err();
        assert!(err.to_string().contains("implausible"), "{err}");
    }

    #[test]
    fn invalid_utf8_is_corrupt_not_a_panic() {
        let mut w = Writer::new();
        w.bytes(&[0xff, 0xfe]);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert!(r.str().unwrap_err().to_string().contains("UTF-8"));
    }

    #[test]
    fn trailing_bytes_are_reported() {
        let mut w = Writer::new();
        w.u8(1);
        w.u8(2);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        r.u8().unwrap();
        let err = r.expect_eof().unwrap_err();
        assert!(err.to_string().contains("trailing"), "{err}");
    }
}
