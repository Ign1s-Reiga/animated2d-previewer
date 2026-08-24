//! `manifest.json` — the human-readable index of a package.
//!
//! Serialised with a fixed field order and no maps anywhere, so two runs over
//! the same model produce byte-identical JSON. That is what lets golden tests
//! diff the manifest directly.

use a2d_core::DecodeError;
use serde::{Deserialize, Serialize};

/// Current package layout version. Bump on any change to `model.bin`'s layout
/// or to this manifest's required fields.
pub const FORMAT_VERSION: u32 = 1;

/// Which runtime family loads this package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Spine,
    Cubism,
}

impl ModelType {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelType::Spine => "spine",
            ModelType::Cubism => "cubism",
        }
    }
}

/// One texture page, stored under `textures/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureEntry {
    /// File name relative to `textures/`.
    pub file: String,
    /// Pixel size, when the importer knew it. The renderer needs it to compute
    /// UVs for atlases that omitted `size:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    #[serde(default)]
    pub premultiplied_alpha: bool,
}

/// One animation, listed so a viewer can populate a selector without decoding
/// `model.bin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationEntry {
    pub name: String,
    pub duration: f32,
}

/// The package manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format_version: u32,
    pub model_type: ModelType,
    /// Name of the importer that produced this package. Provenance only: the
    /// viewer and the runtime must never branch on it, and this crate does not
    /// know which importers exist.
    pub source_game: String,
    /// Source format and version, e.g. `spine-3.8`. Informational: the viewer
    /// must never branch on it.
    pub source_format: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_animation: Option<String>,
    pub textures: Vec<TextureEntry>,
    pub animations: Vec<AnimationEntry>,
    /// Degradations recorded at import time, preserved so `validate` can repeat
    /// them without re-reading the original assets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_warnings: Vec<String>,
}

impl Manifest {
    pub fn new(model_type: ModelType, display_name: impl Into<String>) -> Self {
        Manifest {
            format_version: FORMAT_VERSION,
            model_type,
            source_game: "generic".into(),
            source_format: String::new(),
            display_name: display_name.into(),
            default_animation: None,
            textures: Vec::new(),
            animations: Vec::new(),
            import_warnings: Vec::new(),
        }
    }

    /// Serialises to deterministic pretty JSON with a trailing newline.
    ///
    /// JSON has no representation for a non-finite number, and `serde_json`
    /// writes `null` for one rather than refusing — which would produce a
    /// manifest that no longer parses back. A corrupt skeleton can yield a NaN
    /// duration, so that case is caught here and reported (rule §4.13).
    pub fn to_json(&self) -> Result<String, DecodeError> {
        for animation in &self.animations {
            if !animation.duration.is_finite() {
                return Err(DecodeError::corrupt(format!(
                    "manifest cannot be serialised: animation {:?} has a non-finite duration ({})",
                    animation.name, animation.duration
                )));
            }
        }
        // `serde_json` emits struct fields in declaration order and every
        // collection here is an ordered `Vec`, so the output is stable.
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| DecodeError::corrupt(format!("manifest cannot be serialised: {e}")))?;
        json.push('\n');
        Ok(json)
    }

    pub fn from_json(text: &str) -> Result<Manifest, DecodeError> {
        let manifest: Manifest = serde_json::from_str(text)
            .map_err(|e| DecodeError::corrupt_at(format!("manifest.json is not readable: {e}"), e.line() as u64))?;
        if manifest.format_version > FORMAT_VERSION {
            return Err(DecodeError::UnsupportedFormat(format!(
                "package format version {} is newer than this build supports ({FORMAT_VERSION})",
                manifest.format_version
            )));
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            format_version: FORMAT_VERSION,
            model_type: ModelType::Spine,
            source_game: "aeons_echo".into(),
            source_format: "spine-3.8".into(),
            display_name: "CharacterName".into(),
            default_animation: Some("idle".into()),
            textures: vec![TextureEntry {
                file: "texture_00.png".into(),
                size: Some([1024, 2048]),
                premultiplied_alpha: true,
            }],
            animations: vec![
                AnimationEntry { name: "idle".into(), duration: 1.5 },
                AnimationEntry { name: "walk".into(), duration: 0.8 },
            ],
            import_warnings: vec!["path constraint unsupported".into()],
        }
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let m = sample();
        assert_eq!(Manifest::from_json(&m.to_json().unwrap()).unwrap(), m);
    }

    #[test]
    fn serialisation_is_byte_stable() {
        let a = sample().to_json().unwrap();
        let b = sample().to_json().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn json_uses_the_field_names_the_spec_documents() {
        let json = sample().to_json().unwrap();
        for key in [
            "\"formatVersion\"",
            "\"modelType\"",
            "\"sourceGame\"",
            "\"sourceFormat\"",
            "\"displayName\"",
            "\"defaultAnimation\"",
            "\"textures\"",
            "\"animations\"",
        ] {
            assert!(json.contains(key), "missing {key} in:\n{json}");
        }
        assert!(json.contains("\"spine\""), "modelType should serialise lowercase:\n{json}");
    }

    #[test]
    fn nested_structs_use_camel_case_too() {
        // The outer `rename_all` does not reach nested types; each one declares
        // it, and this test is what keeps a new field from escaping snake_case.
        let json = sample().to_json().unwrap();
        assert!(json.contains("\"premultipliedAlpha\""), "{json}");
        assert!(!json.contains("premultiplied_alpha"), "{json}");
    }

    #[test]
    fn the_json_ends_with_a_newline() {
        assert!(sample().to_json().unwrap().ends_with("}\n"));
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let mut m = Manifest::new(ModelType::Spine, "X");
        m.textures.push(TextureEntry { file: "a.png".into(), size: None, premultiplied_alpha: false });
        let json = m.to_json().unwrap();
        assert!(!json.contains("defaultAnimation"), "{json}");
        assert!(!json.contains("importWarnings"), "{json}");
        assert!(!json.contains("\"size\""), "{json}");
    }

    #[test]
    fn a_newer_format_version_is_refused_rather_than_misread() {
        let mut m = sample();
        m.format_version = FORMAT_VERSION + 1;
        let err = Manifest::from_json(&m.to_json().unwrap()).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedFormat(_)), "{err}");
        assert!(err.to_string().contains("newer"), "{err}");
    }

    #[test]
    fn a_non_finite_duration_is_refused_rather_than_written_as_null() {
        // Left to `serde_json`, a NaN becomes `null` and the manifest silently
        // stops round-tripping. It must be an error instead.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut m = sample();
            m.animations[0].duration = bad;
            let err = m.to_json().unwrap_err();
            assert!(err.to_string().contains("non-finite duration"), "for {bad}: {err}");
            assert!(err.to_string().contains("idle"), "the animation should be named: {err}");
        }
    }

    #[test]
    fn the_current_format_version_is_accepted() {
        let mut m = sample();
        m.format_version = FORMAT_VERSION;
        assert!(Manifest::from_json(&m.to_json().unwrap()).is_ok());
    }

    #[test]
    fn malformed_json_is_a_located_error() {
        let err = Manifest::from_json("{ not json").unwrap_err();
        assert!(matches!(err, DecodeError::Corrupt { .. }), "{err}");
    }

    #[test]
    fn a_minimal_manifest_parses() {
        let json = r#"{"formatVersion":1,"modelType":"cubism","sourceGame":"depose_girls",
                       "sourceFormat":"cubism-3","displayName":"X","textures":[],"animations":[]}"#;
        let m = Manifest::from_json(json).unwrap();
        assert_eq!(m.model_type, ModelType::Cubism);
        assert_eq!(m.default_animation, None);
        assert!(m.import_warnings.is_empty());
    }

    #[test]
    fn an_unknown_model_type_is_refused() {
        let json = r#"{"formatVersion":1,"modelType":"flash","sourceGame":"x",
                       "sourceFormat":"y","displayName":"X","textures":[],"animations":[]}"#;
        assert!(Manifest::from_json(json).is_err());
    }
}
