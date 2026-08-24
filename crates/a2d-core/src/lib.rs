//! Core types for the Animated2D viewer.
//!
//! This crate is the bottom of the dependency graph. It owns the normalized IR,
//! the math primitives, the shared model trait, the renderer-neutral draw
//! primitives, and the error taxonomy. It knows nothing about any game, any
//! source format version, or any GPU API — anything that would require such
//! knowledge belongs in a layer above.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod error;
pub mod ir;
pub mod math;
pub mod model;
pub mod render;
pub mod report;

pub use error::{DecodeError, ModelKind, RuntimeError};
pub use math::{Aabb, Affine2, Bezier, Interpolation, Rgb, Rgba, Vec2};
pub use model::{AnimatedModel, AnimationInfo, ExpressionInfo, PlayOptions};
pub use render::{BlendMode, HitAreaId, MaskId, RenderList, RenderMask, RenderMesh, TextureId};
pub use report::{Degradation, LoadReport};
