//! Content-based asset classification.
//!
//! Spec §15: never rely on the extension alone. Every classification here comes
//! from the bytes; names are used only afterwards, to *pair* assets that the
//! content has already identified.

use a2d_core::DecodeError;

/// What a file turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetKind {
    /// A Spine skeleton, with the version detection already done.
    SpineSkeleton { version: String, encoding: &'static str },
    /// A libgdx/Spine texture atlas.
    SpineAtlas,
    /// A raster texture page.
    Texture(&'static str),
    /// A Live2D Cubism MOC3 payload.
    CubismMoc3 { version: u8 },
    /// A Cubism JSON sidecar: `model3`, `motion3`, `physics3`, `pose3`, `exp3`.
    CubismJson(&'static str),
    /// A Unity `AssetBundle` container.
    UnityBundle { signature: String },
    /// A Unity serialized file that is not wrapped in a bundle.
    UnitySerialized,
    /// Recognised as none of the above.
    Unknown,
}

impl AssetKind {
    pub fn label(&self) -> String {
        match self {
            AssetKind::SpineSkeleton { version, encoding } => format!("spine skeleton {version} ({encoding})"),
            AssetKind::SpineAtlas => "spine atlas".into(),
            AssetKind::Texture(format) => format!("texture ({format})"),
            AssetKind::CubismMoc3 { version } => format!("cubism moc3 (version {version})"),
            AssetKind::CubismJson(kind) => format!("cubism {kind}"),
            AssetKind::UnityBundle { signature } => format!("unity bundle ({signature})"),
            AssetKind::UnitySerialized => "unity serialized file".into(),
            AssetKind::Unknown => "unknown".into(),
        }
    }

    pub fn is_spine_skeleton(&self) -> bool {
        matches!(self, AssetKind::SpineSkeleton { .. })
    }
}

/// MOC3 files open with this magic.
const MOC3_MAGIC: &[u8; 4] = b"MOC3";
/// Unity bundle container signatures, in the order Unity introduced them.
const UNITY_SIGNATURES: [&str; 4] = ["UnityFS", "UnityWeb", "UnityRaw", "UnityArchive"];

/// Classifies a file from its contents.
pub fn classify(bytes: &[u8]) -> AssetKind {
    if bytes.is_empty() {
        return AssetKind::Unknown;
    }

    if let Some(kind) = classify_texture(bytes) {
        return kind;
    }
    if bytes.len() >= 5 && &bytes[..4] == MOC3_MAGIC {
        // Byte 4 holds the MOC3 version; 3 and up are Cubism 4-era files.
        return AssetKind::CubismMoc3 { version: bytes[4] };
    }
    if let Some(signature) = unity_signature(bytes) {
        return AssetKind::UnityBundle { signature };
    }

    // Spine detection covers both JSON and binary and does its own validation.
    if let Ok(detection) = a2d_spine::detect::detect(bytes) {
        return AssetKind::SpineSkeleton {
            version: detection.raw_version.clone(),
            encoding: match detection.encoding {
                a2d_spine::SpineEncoding::Json => "json",
                a2d_spine::SpineEncoding::Binary => "binary",
            },
        };
    }

    if let Some(kind) = classify_cubism_json(bytes) {
        return kind;
    }
    if looks_like_atlas(bytes) {
        return AssetKind::SpineAtlas;
    }
    if looks_like_unity_serialized(bytes) {
        return AssetKind::UnitySerialized;
    }
    AssetKind::Unknown
}

fn classify_texture(bytes: &[u8]) -> Option<AssetKind> {
    const PNG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() >= 8 && &bytes[..8] == PNG {
        return Some(AssetKind::Texture("png"));
    }
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return Some(AssetKind::Texture("jpeg"));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(AssetKind::Texture("webp"));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"DDS " {
        return Some(AssetKind::Texture("dds"));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"\x00\x00\x00\x0cjP  ".get(..4)? {
        return Some(AssetKind::Texture("jpeg2000"));
    }
    None
}

fn unity_signature(bytes: &[u8]) -> Option<String> {
    // The signature is a NUL-terminated string at offset 0.
    let end = bytes.iter().take(32).position(|b| *b == 0)?;
    let signature = std::str::from_utf8(&bytes[..end]).ok()?;
    UNITY_SIGNATURES.contains(&signature).then(|| signature.to_string())
}

/// Cubism sidecars are JSON objects with a characteristic key.
fn classify_cubism_json(bytes: &[u8]) -> Option<AssetKind> {
    let head = &bytes[..bytes.len().min(4096)];
    let text = std::str::from_utf8(head).ok()?;
    if !text.trim_start().starts_with('{') {
        return None;
    }
    // Ordered most-specific first: a `model3` also mentions `FileReferences`.
    for (needle, label) in [
        ("\"Meta\"", ""),
        ("\"FileReferences\"", "model3.json"),
        ("\"Curves\"", "motion3.json"),
        ("\"PhysicsSettings\"", "physics3.json"),
        ("\"Groups\"", "pose3.json"),
        ("\"Parameters\"", "exp3.json"),
    ] {
        if label.is_empty() {
            continue;
        }
        if text.contains(needle) {
            return Some(AssetKind::CubismJson(label));
        }
    }
    None
}

/// Probes the atlas grammar: a name line followed by indented `key: value`
/// entries, with at least one entry a known atlas key.
fn looks_like_atlas(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    let Ok(text) = std::str::from_utf8(head) else { return false };
    if text.trim().is_empty() {
        return false;
    }
    const KEYS: [&str; 8] = ["size:", "bounds:", "xy:", "orig:", "offsets:", "rotate:", "filter:", "repeat:"];
    let hits = text.lines().filter(|line| KEYS.iter().any(|k| line.trim_start().starts_with(k))).count();
    if hits < 2 {
        return false;
    }
    // Confirm by actually parsing, so a config file that happens to use
    // `size:` is not mistaken for an atlas.
    let Ok(full) = std::str::from_utf8(bytes) else { return false };
    a2d_spine::parse_atlas(full).is_ok()
}

/// Unity serialized files start with a metadata header rather than a signature.
///
/// This is a weak heuristic on purpose: it only claims a file when nothing else
/// did, and the importer confirms it by actually reading the object table.
fn looks_like_unity_serialized(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    // Big-endian header: metadata size, file size, version, data offset.
    let file_size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let version = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let data_offset = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    (6..=30).contains(&version) && file_size == bytes.len() && data_offset < bytes.len()
}

/// Rejects a file that is not a Spine skeleton, with a precise reason.
pub fn require_spine_skeleton(bytes: &[u8], what: &str) -> Result<(), DecodeError> {
    match classify(bytes) {
        AssetKind::SpineSkeleton { .. } => Ok(()),
        other => Err(DecodeError::UnsupportedFormat(format!("{what} is {}, not a Spine skeleton", other.label()))),
    }
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

    fn spine_binary() -> Vec<u8> {
        let mut out = encode_varint(12);
        out.extend_from_slice(b"aBcDeFgHiJk");
        out.extend(encode_varint(7));
        out.extend_from_slice(b"3.8.99");
        out.extend_from_slice(&[0u8; 17]);
        out
    }

    #[test]
    fn png_is_recognised() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        assert_eq!(classify(png), AssetKind::Texture("png"));
    }

    #[test]
    fn jpeg_and_webp_are_recognised() {
        assert_eq!(classify(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]), AssetKind::Texture("jpeg"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8 ");
        assert_eq!(classify(&webp), AssetKind::Texture("webp"));
    }

    #[test]
    fn moc3_is_recognised_with_its_version() {
        let mut moc = b"MOC3".to_vec();
        moc.push(4);
        moc.extend_from_slice(&[0u8; 60]);
        assert_eq!(classify(&moc), AssetKind::CubismMoc3 { version: 4 });
    }

    #[test]
    fn unity_bundle_signatures_are_recognised() {
        for signature in ["UnityFS", "UnityWeb", "UnityRaw"] {
            let mut bytes = signature.as_bytes().to_vec();
            bytes.push(0);
            bytes.extend_from_slice(&[0u8; 32]);
            assert_eq!(
                classify(&bytes),
                AssetKind::UnityBundle { signature: signature.to_string() },
                "signature {signature}"
            );
        }
    }

    #[test]
    fn a_lookalike_signature_is_not_a_unity_bundle() {
        let mut bytes = b"UnityFSX".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(&[0u8; 32]);
        assert!(!matches!(classify(&bytes), AssetKind::UnityBundle { .. }));
    }

    #[test]
    fn a_spine_json_skeleton_is_recognised_with_its_version() {
        let json = br#"{"skeleton":{"spine":"4.1.23"},"bones":[]}"#;
        assert_eq!(classify(json), AssetKind::SpineSkeleton { version: "4.1.23".into(), encoding: "json" });
    }

    #[test]
    fn a_spine_binary_skeleton_is_recognised_with_its_version() {
        assert_eq!(
            classify(&spine_binary()),
            AssetKind::SpineSkeleton { version: "3.8.99".into(), encoding: "binary" }
        );
    }

    #[test]
    fn an_atlas_is_recognised_by_its_grammar() {
        let atlas = "hero.png\nsize: 1024,1024\nfilter: Linear,Linear\nhead\nxy: 1,1\nsize: 10,10\n";
        assert_eq!(classify(atlas.as_bytes()), AssetKind::SpineAtlas);
    }

    #[test]
    fn a_modern_atlas_is_recognised_too() {
        let atlas = "hero.png\nsize: 1024, 1024\npma: true\nhead\nbounds: 1, 1, 10, 10\nrotate: 90\n";
        assert_eq!(classify(atlas.as_bytes()), AssetKind::SpineAtlas);
    }

    #[test]
    fn a_config_file_that_merely_uses_colons_is_not_an_atlas() {
        let yaml = "name: hero\nsize: 10\nother: value\n";
        assert_ne!(classify(yaml.as_bytes()), AssetKind::SpineAtlas);
    }

    #[test]
    fn cubism_sidecars_are_recognised_by_their_characteristic_keys() {
        for (json, expected) in [
            (r#"{"Version":3,"FileReferences":{"Moc":"a.moc3"}}"#, "model3.json"),
            (r#"{"Version":3,"Meta":{},"Curves":[{"Target":"Parameter"}]}"#, "motion3.json"),
            (r#"{"Version":3,"PhysicsSettings":[]}"#, "physics3.json"),
            (r#"{"Type":"Live2D Pose","Groups":[[]]}"#, "pose3.json"),
            (r#"{"Type":"Live2D Expression","Parameters":[]}"#, "exp3.json"),
        ] {
            assert_eq!(classify(json.as_bytes()), AssetKind::CubismJson(expected), "for {json}");
        }
    }

    #[test]
    fn an_empty_file_is_unknown() {
        assert_eq!(classify(&[]), AssetKind::Unknown);
    }

    #[test]
    fn arbitrary_bytes_are_unknown() {
        assert_eq!(classify(&[0x42; 64]), AssetKind::Unknown);
    }

    #[test]
    fn labels_are_human_readable() {
        assert_eq!(classify(&spine_binary()).label(), "spine skeleton 3.8.99 (binary)");
        assert_eq!(AssetKind::SpineAtlas.label(), "spine atlas");
        assert_eq!(AssetKind::Unknown.label(), "unknown");
    }

    #[test]
    fn requiring_a_skeleton_names_what_it_actually_found() {
        let err = require_spine_skeleton(b"\x89PNG\r\n\x1a\n\x00", "hero.png").unwrap_err();
        assert!(err.to_string().contains("texture (png)"), "{err}");
        assert!(err.to_string().contains("hero.png"), "{err}");
        assert!(require_spine_skeleton(&spine_binary(), "hero.skel").is_ok());
    }
}
