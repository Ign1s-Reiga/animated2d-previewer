//! Error taxonomy.
//!
//! Spec §16 requires callers to be able to tell these cases apart, so they are
//! distinct variants rather than one stringly-typed error. Anything that can be
//! caused by input data must surface as a `Result`, never a panic.

use std::fmt;

/// Which source ecosystem a model came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelKind {
    Spine,
    Cubism,
}

impl fmt::Display for ModelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ModelKind::Spine => "spine",
            ModelKind::Cubism => "cubism",
        })
    }
}

/// Failure while decoding a source asset into IR.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The bytes are not a format this project decodes at all.
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// The format is recognised but this version has no decoder.
    #[error("unsupported {kind} version {version}: {detail}")]
    UnsupportedVersion { kind: ModelKind, version: String, detail: String },

    /// Content is structurally invalid: truncated, bad magic, bad grammar.
    #[error("corrupt asset{}: {message}", OptionalAt(.at))]
    Corrupt { message: String, at: Option<u64> },

    /// Detection produced more than one equally plausible answer. Spec §15
    /// forbids guessing.
    #[error("ambiguous format detection, candidates: {}", .candidates.join(", "))]
    Ambiguous { candidates: Vec<String> },

    /// A referenced texture page could not be resolved.
    #[error("missing texture: {0}")]
    MissingTexture(String),

    /// A skeleton was found without its atlas.
    #[error("missing atlas: {0}")]
    MissingAtlas(String),

    /// An atlas or texture set was found without its skeleton.
    #[error("missing skeleton: {0}")]
    MissingSkeleton(String),

    /// The importer could not rebuild a game-specific asset graph.
    #[error("reconstruction failed for {game}: {message}")]
    Reconstruction { game: String, message: String },

    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl DecodeError {
    pub fn corrupt(message: impl Into<String>) -> Self {
        DecodeError::Corrupt { message: message.into(), at: None }
    }

    pub fn corrupt_at(message: impl Into<String>, at: u64) -> Self {
        DecodeError::Corrupt { message: message.into(), at: Some(at) }
    }

    pub fn unsupported_version(kind: ModelKind, version: impl Into<String>, detail: impl Into<String>) -> Self {
        DecodeError::UnsupportedVersion { kind, version: version.into(), detail: detail.into() }
    }

    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        DecodeError::Io { path: path.into(), source }
    }
}

/// Failure during playback, after a model has loaded.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("no animation named {0:?}")]
    UnknownAnimation(String),

    #[error("no expression named {0:?}")]
    UnknownExpression(String),

    #[error("no skin named {0:?}")]
    UnknownSkin(String),

    /// The data is valid but exercises a feature the runtime does not implement.
    #[error("unsupported runtime feature: {0}")]
    UnsupportedFeature(String),

    /// An invariant that the loader was supposed to have guaranteed.
    #[error("model state invalid: {0}")]
    InvalidState(String),
}

/// Helper for the optional byte offset in `Corrupt`'s message.
struct OptionalAt<'a>(&'a Option<u64>);

impl fmt::Display for OptionalAt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(at) => write!(f, " at byte {at}"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_without_offset_omits_the_offset_clause() {
        let e = DecodeError::corrupt("bad magic");
        assert_eq!(e.to_string(), "corrupt asset: bad magic");
    }

    #[test]
    fn corrupt_with_offset_reports_it() {
        let e = DecodeError::corrupt_at("truncated vertex array", 512);
        assert_eq!(e.to_string(), "corrupt asset at byte 512: truncated vertex array");
    }

    #[test]
    fn ambiguous_detection_lists_every_candidate() {
        let e = DecodeError::Ambiguous { candidates: vec!["spine-3.8".into(), "spine-4.1".into()] };
        assert_eq!(e.to_string(), "ambiguous format detection, candidates: spine-3.8, spine-4.1");
    }

    #[test]
    fn unsupported_version_names_the_ecosystem() {
        let e = DecodeError::unsupported_version(ModelKind::Spine, "1.9", "predates the binary format");
        assert_eq!(e.to_string(), "unsupported spine version 1.9: predates the binary format");
    }
}
