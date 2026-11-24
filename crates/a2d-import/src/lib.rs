//! Game-specific asset discovery and reconstruction.
//!
//! Importers do discovery and reconstruction only (spec §9): they find the
//! files that belong together, hand them to the right decoder, and emit a
//! normalized package. They contain no rendering and no runtime logic, and they
//! are the only layer permitted to know a game's name.

#![forbid(unsafe_code)]

pub mod detect;
pub mod games;
pub mod generic;
mod png;
pub mod unity_cubism;
pub mod unity_spine;

#[cfg(test)]
mod test_fixtures;

pub use detect::{classify, AssetKind};
pub use generic::{normalize_asset_stem, SpineSourceSet};
pub use unity_cubism::{
    import_bundle, inspect_bundle, inspect_handovers, CubismInventory, HandoverInspection, MocPayload, MotionEntry,
    TextureEntry,
};
pub use unity_spine::{
    decode_rig, discover_single_rig, discover_spine_bundle, import_spine_bundle, inspect_spine_bundle, PageImage,
    SpineInventory, SpineRig, SpineTextAsset, SpineTextureEntry,
};
