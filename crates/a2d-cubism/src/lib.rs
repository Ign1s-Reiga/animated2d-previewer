//! Live2D Cubism decoding and normalization into the Generic Cubism model.
//!
//! # Status: blocked on an open decision, deliberately not started
//!
//! CLAUDE.md §13.1 says to ask before writing any MOC3 code, because the answer
//! changes this crate's whole design:
//!
//! * **Use the official Cubism Core.** A proprietary native library with
//!   licence terms. Correct by construction, but adds a binary dependency that
//!   cannot be redistributed and constrains how the viewer may be shipped.
//! * **Write an independent MOC3 parser.** No proprietary dependency, but a
//!   large undertaking against an undocumented format, and the deformer
//!   evaluation semantics would have to be reverse-engineered to match.
//!
//! Nothing is stubbed here until that is settled. Guessing would produce a
//! design that has to be thrown away.
//!
//! Detection is already in place and does not depend on the decision:
//! [`a2d_import::classify`] recognises the `MOC3` magic and its version byte,
//! and the `model3` / `motion3` / `physics3` / `pose3` / `exp3` JSON sidecars,
//! so `animated2d inspect` can already inventory a Cubism character.

#![forbid(unsafe_code)]
