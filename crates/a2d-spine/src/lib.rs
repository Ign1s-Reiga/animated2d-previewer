//! Spine version detection, decoders, and normalization into the Generic Spine IR.
//!
//! Everything that differs between Spine 2.x, 3.x and 4.x is confined to this
//! crate. Consumers receive [`a2d_core::ir::spine::SpineIr`] and cannot tell
//! which version produced it (spec §7).

#![forbid(unsafe_code)]

pub mod atlas;
pub mod detect;
pub mod json;
pub mod normalize;
pub mod reader;

pub use atlas::parse_atlas;
pub use detect::{detect, SpineDetection, SpineEncoding, SpineFamily, SpineVersion};
pub use reader::BinaryReader;
