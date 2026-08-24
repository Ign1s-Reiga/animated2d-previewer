//! Normalized internal representations.
//!
//! Two separate models, deliberately: spec §5 forbids forcing Spine and Cubism
//! into one low-level deformation model. They meet only at
//! [`AnimatedModel`](crate::model::AnimatedModel) and
//! [`RenderMesh`](crate::render::RenderMesh).

pub mod atlas;
pub mod ids;
pub mod spine;
