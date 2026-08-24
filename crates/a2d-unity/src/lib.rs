//! Unity serialized file and AssetBundle object graph reading.
//!
//! # Status: not implemented
//!
//! Needed by the depose_girls and nikke importers, neither of which is built.
//! Whether this becomes a purpose-built minimal reader or wraps an existing
//! crate is an open decision (CLAUDE.md §13.3): the choice should be made
//! against a real bundle rather than in the abstract.
//!
//! Container detection already works and lives in `a2d-import`:
//! [`a2d_import::classify`] recognises the `UnityFS` / `UnityWeb` / `UnityRaw` /
//! `UnityArchive` signatures and the bare serialized-file header, so
//! `animated2d inspect` can already say what a bundle is before anything can
//! read inside it.

#![forbid(unsafe_code)]
