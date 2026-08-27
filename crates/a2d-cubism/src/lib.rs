//! Live2D Cubism decoding and normalization into the Generic Cubism model.
//!
//! # Status: the container reads, the model poses, and it has been looked at
//!
//! CLAUDE.md §13.1 is settled: MOC3 is decoded by an independent parser here,
//! and Live2D's proprietary Cubism Core is not linked. That keeps the project
//! redistributable and free of proprietary binaries (§11, §16), at the cost of
//! working out an undocumented format.
//!
//! What that cost means in practice, and how it is being paid: nothing is
//! assumed. Every field this crate reads was derived from a real model and then
//! checked against an independent source — the same model's Unity components,
//! or a relation that could only hold if the reading were right. Anything not
//! checked that way is left unparsed rather than guessed at. See [`moc3`] for
//! what has been established and how.
//!
//! Read today: identifiers, counts, the canvas, parameter ranges, geometry,
//! deformers, masks, and the evaluation that turns parameter values into a
//! posed model. Not yet: motions, physics, pose files and expressions.
//!
//! The model itself is [`CubismIr`], which lives in `a2d-core` because a
//! package stores it (§9) and `a2d-pack` may not depend on a format crate.
//! What stays here is the MOC3 container and the evaluation, reached through
//! [`CubismEval`] and [`CubismEmit`].
//!
//! Detection is separate and already complete: [`a2d_import::classify`]
//! recognises the `MOC3` magic and its version byte, and the `model3` /
//! `motion3` / `physics3` / `pose3` / `exp3` JSON sidecars.

#![forbid(unsafe_code)]

pub mod emit;
pub mod eval;
pub mod moc3;
pub mod model;

pub use emit::CubismEmit;
pub use eval::{CubismEval, Pose};
pub use moc3::{
    Canvas, Counts, CubismIr, Deformer, DeformerKind, Drawable, KeyformBinding, Keyforms, Moc3, Parameter,
    ParameterBinding, RotationDeformer, RotationKeyform, WarpDeformer,
};
pub use model::GenericCubismModel;
