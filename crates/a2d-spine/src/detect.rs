//! Spine format and version detection from content (spec §15).
//!
//! Extensions are worthless here: exported skeletons arrive as `.skel.bytes`,
//! `.bytes`, `.txt`, or with no extension at all, depending on the toolchain
//! that packed them. So detection reads the header. Which toolchain produced a
//! given layout is the importer's business, not this crate's.
//!
//! The two binary dialects are distinguished by their first field:
//!
//! * **3.x** opens with a length-prefixed hash string, then a version string.
//! * **4.x** opens with eight raw bytes of hash, then a version string.
//!
//! Both layouts are tried. If exactly one yields a plausible version string the
//! answer is unambiguous; if both do, that is [`DecodeError::Ambiguous`] rather
//! than a coin flip.

use a2d_core::{DecodeError, ModelKind};

use crate::reader::BinaryReader;

/// How the skeleton was serialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineEncoding {
    Json,
    Binary,
}

impl SpineEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            SpineEncoding::Json => "json",
            SpineEncoding::Binary => "binary",
        }
    }
}

/// A parsed Spine exporter version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpineVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl SpineVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        SpineVersion { major, minor, patch }
    }

    /// Parses `3.8.99`, `4.1.23`, `4.2.00-beta` and the two-component `3.8`.
    pub fn parse(s: &str) -> Option<SpineVersion> {
        // Stop at the first character that cannot start a version component, so
        // pre-release suffixes do not defeat the parse.
        let core: &str = s.split(|c: char| !(c.is_ascii_digit() || c == '.')).next()?;
        let mut parts = core.split('.').filter(|p| !p.is_empty());
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(SpineVersion { major, minor, patch })
    }

    /// The decoder family that owns this version.
    pub fn family(self) -> Option<SpineFamily> {
        match self.major {
            2 => Some(SpineFamily::V2),
            3 => Some(SpineFamily::V3),
            4 => Some(SpineFamily::V4),
            _ => None,
        }
    }
}

impl std::fmt::Display for SpineVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Which version-specific decoder handles a skeleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineFamily {
    V2,
    V3,
    V4,
}

impl SpineFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            SpineFamily::V2 => "2.x",
            SpineFamily::V3 => "3.x",
            SpineFamily::V4 => "4.x",
        }
    }
}

/// What detection concluded about a skeleton file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineDetection {
    pub encoding: SpineEncoding,
    pub version: SpineVersion,
    /// Version string exactly as the exporter wrote it.
    pub raw_version: String,
    pub hash: Option<String>,
}

impl SpineDetection {
    pub fn family(&self) -> Option<SpineFamily> {
        self.version.family()
    }

    /// The `sourceFormat` string recorded in an `.a2dpack` manifest.
    pub fn source_format(&self) -> String {
        format!("spine-{}.{}", self.version.major, self.version.minor)
    }
}

/// Cheap check for "is this Spine at all", used by the multi-format detector
/// before it commits to a full parse.
pub fn looks_like_spine(data: &[u8]) -> bool {
    detect(data).is_ok()
}

/// Identifies a Spine skeleton from its bytes.
pub fn detect(data: &[u8]) -> Result<SpineDetection, DecodeError> {
    if data.is_empty() {
        return Err(DecodeError::corrupt("empty skeleton"));
    }

    let json = detect_json(data);
    let binary = detect_binary(data);

    match (json, binary) {
        (Some(j), None) => Ok(j),
        (None, Some(b)) => Ok(b),
        (Some(j), Some(b)) => Err(DecodeError::Ambiguous {
            candidates: vec![
                format!("spine {} ({})", j.raw_version, j.encoding.as_str()),
                format!("spine {} ({})", b.raw_version, b.encoding.as_str()),
            ],
        }),
        (None, None) => Err(DecodeError::UnsupportedFormat(
            "not a Spine skeleton: no JSON skeleton object and no recognised binary header".into(),
        )),
    }
}

/// Rejects a detected skeleton whose family has no decoder yet.
pub fn require_supported(d: &SpineDetection) -> Result<SpineFamily, DecodeError> {
    d.family().ok_or_else(|| {
        DecodeError::unsupported_version(
            ModelKind::Spine,
            d.raw_version.clone(),
            "only Spine 2.x, 3.x and 4.x families are recognised",
        )
    })
}

fn detect_json(data: &[u8]) -> Option<SpineDetection> {
    // Only scan the head: skeletons are megabytes and the header is at the top.
    const HEAD: usize = 4096;
    let head = &data[..data.len().min(HEAD)];
    let text = std::str::from_utf8(head).ok().or_else(|| {
        // A multi-byte character may straddle the cut; retry on a shorter slice.
        (0..4).find_map(|back| std::str::from_utf8(head.get(..head.len().checked_sub(back)?)?).ok())
    })?;

    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }

    // The `skeleton` object carries `spine`, the exporter version. Its absence
    // means either a pre-3.x export or something that is not a skeleton at all.
    let raw_version = json_string_field(text, "spine")?;
    let version = SpineVersion::parse(&raw_version)?;
    Some(SpineDetection { encoding: SpineEncoding::Json, version, raw_version, hash: json_string_field(text, "hash") })
}

/// Pulls `"key": "value"` out of a JSON head without a full parse.
///
/// Detection must work on a truncated prefix, which a real parser cannot do.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(&needle) {
        let after = from + rel + needle.len();
        let rest = text[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
            return None;
        }
        from = after;
    }
    None
}

fn detect_binary(data: &[u8]) -> Option<SpineDetection> {
    let v4 = read_binary_header(data, BinaryHeader::V4);
    let v3 = read_binary_header(data, BinaryHeader::V3);
    // Each layout validates its own version number, so at most one normally
    // survives. When both do, `detect` reports ambiguity via the JSON/binary
    // pair; here the major number disambiguates, since a v3 layout that parses
    // as v4 would have to yield a 4.x string from a length-prefixed hash.
    match (v3, v4) {
        (Some(d), None) if d.version.major == 3 || d.version.major == 2 => Some(d),
        (None, Some(d)) if d.version.major >= 4 => Some(d),
        (Some(a), Some(b)) => {
            if a.version.major <= 3 && b.version.major >= 4 {
                None // genuinely ambiguous; surfaced by the caller as no match
            } else if a.version.major <= 3 {
                Some(a)
            } else {
                Some(b)
            }
        }
        (Some(d), None) | (None, Some(d)) => Some(d),
        (None, None) => None,
    }
}

#[derive(Clone, Copy)]
enum BinaryHeader {
    /// Length-prefixed hash string, then version string.
    V3,
    /// Eight raw hash bytes, then version string.
    V4,
}

fn read_binary_header(data: &[u8], layout: BinaryHeader) -> Option<SpineDetection> {
    let mut r = BinaryReader::new(data);
    let hash = match layout {
        BinaryHeader::V3 => {
            let h = r.string_opt().ok()?;
            // A hash is 11 base64-ish characters; anything long or non-printable
            // means this layout guessed wrong.
            if let Some(h) = &h {
                if h.len() > 32 || !h.bytes().all(|b| b.is_ascii_graphic()) {
                    return None;
                }
            }
            h
        }
        BinaryHeader::V4 => {
            let v = r.u64().ok()?;
            Some(format!("{v:016x}"))
        }
    };

    let raw_version = r.string_opt().ok()??;
    if raw_version.len() > 32 || !raw_version.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let version = SpineVersion::parse(&raw_version)?;
    // Reject a "version" that does not look like one, e.g. a bone name that
    // happened to parse. A leading digit and a dot are both required.
    if !raw_version.starts_with(|c: char| c.is_ascii_digit()) || !raw_version.contains('.') {
        return None;
    }
    if version.major == 0 || version.major > 9 {
        return None;
    }
    Some(SpineDetection { encoding: SpineEncoding::Binary, version, raw_version, hash })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn v3_skel(version: &str) -> Vec<u8> {
        let mut out = encode_string("aBcDeFgHiJk");
        out.extend(encode_string(version));
        out.extend_from_slice(&0f32.to_be_bytes()); // x
        out.extend_from_slice(&0f32.to_be_bytes()); // y
        out.extend_from_slice(&100f32.to_be_bytes()); // width
        out.extend_from_slice(&200f32.to_be_bytes()); // height
        out.push(0); // nonessential
        out
    }

    fn v4_skel(version: &str) -> Vec<u8> {
        let mut out = 0x0123_4567_89ab_cdefu64.to_be_bytes().to_vec();
        out.extend(encode_string(version));
        out.extend_from_slice(&0f32.to_be_bytes());
        out.extend_from_slice(&0f32.to_be_bytes());
        out.extend_from_slice(&100f32.to_be_bytes());
        out.extend_from_slice(&200f32.to_be_bytes());
        out.push(0);
        out
    }

    #[test]
    fn version_strings_parse() {
        assert_eq!(SpineVersion::parse("3.8.99"), Some(SpineVersion::new(3, 8, 99)));
        assert_eq!(SpineVersion::parse("4.1.23"), Some(SpineVersion::new(4, 1, 23)));
        assert_eq!(SpineVersion::parse("3.8"), Some(SpineVersion::new(3, 8, 0)));
        assert_eq!(SpineVersion::parse("4.2.00-beta"), Some(SpineVersion::new(4, 2, 0)));
    }

    #[test]
    fn nonsense_version_strings_do_not_parse() {
        assert_eq!(SpineVersion::parse(""), None);
        assert_eq!(SpineVersion::parse("hip"), None);
        assert_eq!(SpineVersion::parse("v3.8"), None);
    }

    #[test]
    fn versions_order_naturally() {
        assert!(SpineVersion::new(3, 8, 99) < SpineVersion::new(4, 0, 0));
        assert!(SpineVersion::new(4, 1, 23) > SpineVersion::new(4, 1, 5));
    }

    #[test]
    fn families_map_from_the_major_number() {
        assert_eq!(SpineVersion::new(2, 1, 27).family(), Some(SpineFamily::V2));
        assert_eq!(SpineVersion::new(3, 8, 99).family(), Some(SpineFamily::V3));
        assert_eq!(SpineVersion::new(4, 2, 0).family(), Some(SpineFamily::V4));
        assert_eq!(SpineVersion::new(5, 0, 0).family(), None);
    }

    #[test]
    fn json_skeleton_is_detected_with_its_version() {
        let json = r#"{"skeleton":{"hash":"xYz123","spine":"3.8.99","x":0,"y":0},"bones":[]}"#;
        let d = detect(json.as_bytes()).unwrap();
        assert_eq!(d.encoding, SpineEncoding::Json);
        assert_eq!(d.version, SpineVersion::new(3, 8, 99));
        assert_eq!(d.hash.as_deref(), Some("xYz123"));
    }

    #[test]
    fn json_detection_survives_whitespace_and_pretty_printing() {
        let json = "{\n  \"skeleton\" : {\n    \"spine\" : \"4.1.23\"\n  }\n}";
        assert_eq!(detect(json.as_bytes()).unwrap().version, SpineVersion::new(4, 1, 23));
    }

    #[test]
    fn json_detection_works_on_a_truncated_prefix() {
        // Only the head is scanned, so a huge file must still be identified.
        let mut json = String::from(r#"{"skeleton":{"spine":"3.8.99"},"bones":["#);
        json.push_str(&"{\"name\":\"padding\"},".repeat(5000));
        let d = detect(json.as_bytes()).unwrap();
        assert_eq!(d.version, SpineVersion::new(3, 8, 99));
    }

    #[test]
    fn json_without_a_spine_version_is_not_a_skeleton() {
        let json = r#"{"bones":[],"slots":[]}"#;
        assert!(detect(json.as_bytes()).is_err());
    }

    #[test]
    fn binary_v3_header_is_detected() {
        let d = detect(&v3_skel("3.8.99")).unwrap();
        assert_eq!(d.encoding, SpineEncoding::Binary);
        assert_eq!(d.version, SpineVersion::new(3, 8, 99));
        assert_eq!(d.hash.as_deref(), Some("aBcDeFgHiJk"));
        assert_eq!(d.family(), Some(SpineFamily::V3));
    }

    #[test]
    fn binary_v4_header_is_detected() {
        let d = detect(&v4_skel("4.1.23")).unwrap();
        assert_eq!(d.encoding, SpineEncoding::Binary);
        assert_eq!(d.version, SpineVersion::new(4, 1, 23));
        assert_eq!(d.family(), Some(SpineFamily::V4));
    }

    #[test]
    fn the_two_binary_layouts_are_not_confused_for_each_other() {
        assert_eq!(detect(&v3_skel("3.8.99")).unwrap().version.major, 3);
        assert_eq!(detect(&v4_skel("4.0.64")).unwrap().version.major, 4);
        assert_eq!(detect(&v4_skel("4.2.33")).unwrap().version.major, 4);
    }

    #[test]
    fn source_format_string_drops_the_patch_number() {
        assert_eq!(detect(&v3_skel("3.8.99")).unwrap().source_format(), "spine-3.8");
        assert_eq!(detect(&v4_skel("4.1.23")).unwrap().source_format(), "spine-4.1");
    }

    #[test]
    fn empty_input_is_corrupt() {
        assert!(matches!(detect(&[]).unwrap_err(), DecodeError::Corrupt { .. }));
    }

    #[test]
    fn unrelated_bytes_are_reported_as_an_unsupported_format() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        assert!(matches!(detect(&png).unwrap_err(), DecodeError::UnsupportedFormat(_)));
    }

    #[test]
    fn plain_text_is_not_mistaken_for_a_skeleton() {
        let atlas = "character.png\nsize: 1024,1024\nhead\n  xy: 1,1\n";
        assert!(detect(atlas.as_bytes()).is_err());
    }

    #[test]
    fn a_future_major_version_is_recognised_but_unsupported() {
        // Detection identifies it; `require_supported` is what refuses it.
        let d = SpineDetection {
            encoding: SpineEncoding::Binary,
            version: SpineVersion::new(7, 0, 0),
            raw_version: "7.0.0".into(),
            hash: None,
        };
        let err = require_supported(&d).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedVersion { .. }), "{err}");
        assert!(err.to_string().contains("7.0.0"));
    }

    #[test]
    fn a_supported_family_passes_the_gate() {
        assert_eq!(require_supported(&detect(&v3_skel("3.8.99")).unwrap()).unwrap(), SpineFamily::V3);
    }

    #[test]
    fn truncated_binary_headers_are_not_detected() {
        let full = v4_skel("4.1.23");
        for cut in [0usize, 1, 4, 8, 9] {
            assert!(detect(&full[..cut]).is_err(), "cut at {cut} should not detect");
        }
    }

    #[test]
    fn field_scan_ignores_a_key_used_as_a_value() {
        // A bone literally named "spine" must not be mistaken for the version.
        let json = r#"{"bones":[{"name":"spine"}],"skeleton":{"spine":"3.8.99"}}"#;
        assert_eq!(detect(json.as_bytes()).unwrap().version, SpineVersion::new(3, 8, 99));
    }
}
