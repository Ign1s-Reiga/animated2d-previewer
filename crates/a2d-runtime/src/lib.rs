//! Deterministic animation evaluation over the normalized IR.
//!
//! This crate knows nothing about file formats, games, or GPUs. It takes IR in
//! and produces posed skeletons and renderer-neutral primitives out. Evaluation
//! is delta-time based and independent of rendering frame rate (spec §12).

#![forbid(unsafe_code)]

pub mod spine;

pub use spine::{AnimationState, FiredEvent, MixBlend, SkeletonPose};
