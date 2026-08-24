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
pub mod unity_cubism;

pub use detect::{classify, AssetKind};
pub use games::{guess_importer, Importer};
pub use generic::{normalize_asset_stem, SpineSourceSet};
pub use unity_cubism::{inspect_bundle, CubismInventory, MocPayload, MotionEntry, TextureEntry};
