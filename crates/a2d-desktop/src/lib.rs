//! Transparent desktop host window.
//!
//! # Status: not implemented
//!
//! Phase 6 work. Spec §20 is explicit that the desktop UI must not be built
//! first, and the renderer it would host does not exist yet.
//!
//! What it must support (spec §13): a transparent frameless window, optional
//! always-on-top, dragging the character, click-through mode, configurable
//! scale and position, animation and model selectors, play/pause, tray
//! integration, and remembering the last position and model.
//!
//! The runtime half of the mascot behaviour is already available:
//! [`a2d_runtime::IdleDirector`] picks default and random idle animations with a
//! seeded, reproducible RNG.

#![forbid(unsafe_code)]
