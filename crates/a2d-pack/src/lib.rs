//! The `.a2dpack` internal package format.
//!
//! The viewer loads packages, never raw game assets (spec §10). A package holds
//! the normalized IR, the texture pages it references, and a human-readable
//! manifest. Everything about it is deterministic so that golden tests can
//! compare bytes.

#![forbid(unsafe_code)]

pub mod bin_io;
pub mod cubism_io;
pub mod manifest;
pub mod package;
pub mod spine_io;
pub mod validate;

pub use manifest::{AnimationEntry, Manifest, ModelType, TextureEntry, FORMAT_VERSION};
pub use package::{Package, PackageModel, TextureFile};
