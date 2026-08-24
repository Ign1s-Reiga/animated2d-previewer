//! The `animated2d` CLI, as a library.
//!
//! The command bodies live here rather than in `main.rs` so that integration
//! tests can drive them directly and assert on what they print, rather than
//! shelling out and parsing a subprocess.

#![forbid(unsafe_code)]

pub mod args;
pub mod commands;

pub use args::{parse, ArgError, Command, HELP};
pub use commands::{import, inspect, preview, print_report, validate, CliError};
