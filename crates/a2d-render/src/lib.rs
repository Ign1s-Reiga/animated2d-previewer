//! Source-format-neutral GPU renderer.
//!
//! # Status: not implemented
//!
//! This crate will consume [`a2d_core::RenderList`] and draw it with `wgpu`.
//! Nothing else is decided yet, and nothing is stubbed out here on purpose:
//! rule §4.10 says not to invent an abstraction before two implementations
//! need it, and there is not yet one.
//!
//! What it must support (spec §11): textured triangle meshes, alpha / additive
//! / multiply blending, draw ordering, mesh deformation, weighted skinning,
//! clipping masks, per-slot colour, high-DPI scaling, transparent backgrounds.
//!
//! The contract it consumes is already fixed and tested:
//! [`RenderMesh`](a2d_core::RenderMesh) carries world-space vertices, atlas
//! UVs, a texture handle, tint, dark tint, blend mode, an optional mask handle,
//! and a z-order. [`RenderList`](a2d_core::RenderList) additionally carries the
//! clipping polygons those mask handles refer to.
//!
//! **This crate must never depend on `a2d-spine`, `a2d-cubism`, `a2d-unity` or
//! `a2d-import`.** That is checked by `crates/a2d-cli/tests/architecture.rs`.

#![forbid(unsafe_code)]
