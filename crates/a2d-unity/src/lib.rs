//! Unity serialized file and AssetBundle object graph reading.
//!
//! # Scope
//!
//! Enough of Unity's containers to find and reconstruct assets, and no more.
//! This is deliberately a *reader*: it never evaluates a scene, resolves a
//! prefab, or interprets a component beyond the few fields an importer needs.
//!
//! CLAUDE.md §13.3 left the choice between wrapping an existing crate and
//! writing a minimal reader open, to be settled against a real bundle. Settled:
//! purpose-built. The Rust options for Unity serialized files are young, the
//! surface actually needed here is small, and the importer boundary contains
//! the cost either way.
//!
//! # What is read, and what is not
//!
//! Type trees are stripped from shipping bundles, so a `MonoBehaviour` cannot be
//! parsed generically — there is no schema in the file to parse it against.
//! What survives is enough to identify every object (its class, its script, its
//! name, its authored path) and to hand an importer the raw bytes of the ones it
//! recognises. Interpreting those bytes is the importer's job, because the
//! layout depends on the C# type and that is source-specific knowledge (§2).

#![forbid(unsafe_code)]

pub mod bundle;
pub mod objects;
pub mod reader;
pub mod serialized;

pub use bundle::{Bundle, Compression, Node};
pub use objects::{Inventory, ObjectInfo, ScriptInfo};
pub use serialized::{ClassId, SerializedFile};
