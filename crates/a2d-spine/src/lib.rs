//! Spine version detection, decoders, and normalization into the Generic Spine IR.
//!
//! Everything that differs between Spine 2.x, 3.x and 4.x is confined to this
//! crate. Consumers receive [`a2d_core::ir::spine::SpineIr`] and cannot tell
//! which version produced it (spec §7).

#![forbid(unsafe_code)]

pub mod atlas;
pub mod binary;
pub mod detect;
pub mod json;
pub mod normalize;
pub mod reader;

pub use atlas::parse_atlas;
pub use detect::{detect, SpineDetection, SpineEncoding, SpineFamily, SpineVersion};
pub use reader::BinaryReader;

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::spine::SpineIr;
use a2d_core::{DecodeError, LoadReport};

/// Decodes a Spine skeleton of any supported encoding and version.
///
/// This is the only entry point importers should need: detection picks the
/// encoding and version, and the matching decoder normalises into the IR.
pub fn decode_skeleton(
    bytes: &[u8],
    atlas: Atlas,
    report: &mut LoadReport,
) -> Result<(SpineIr, SpineDetection), DecodeError> {
    let detection = detect::detect(bytes)?;
    detect::require_supported(&detection)?;
    let ir = match detection.encoding {
        SpineEncoding::Json => json::decode(bytes, &detection, atlas, report)?,
        SpineEncoding::Binary => binary::decode(bytes, &detection, atlas, report)?,
    };
    Ok((ir, detection))
}
