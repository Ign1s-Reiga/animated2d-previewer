//! Transparent desktop host window.
//!
//! The desktop mascot shell: a frameless, transparent, optionally always-on-top
//! window that draws a character and gets out of the way.
//!
//! Most of what a user notices — dragging, scaling, selection, click-through,
//! remembering where things were — lives in [`config`], [`state`] and [`tray`],
//! which carry no `winit` and no GPU types and are therefore testable on a
//! machine with no display. [`app`] is the thin platform layer that turns
//! events into calls on those, and [`viewer`] is the glue to the runtime and
//! the renderer, shared with the CLI's headless frame export.
//!
//! # Controls
//!
//! | Input | Effect |
//! | --- | --- |
//! | drag | move the character |
//! | scroll | scale |
//! | `Space` | pause / resume |
//! | `Tab` | next animation |
//! | `M` | next model |
//! | `T` | toggle always-on-top |
//! | `C` | toggle click-through |
//! | `F` | mirror horizontally |
//! | `R` | reset size and position |
//! | `Esc` | quit |
//!
//! Everything above is also in the tray menu, which stays reachable when
//! click-through is on and the window receives no mouse events.

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod state;
pub mod tray;
pub mod viewer;

pub use app::run;
pub use config::{Config, ModelConfig, WindowConfig};
pub use state::{Action, ModelEntry, ViewerState};
pub use tray::TrayCommand;
pub use viewer::{LoadedModel, Viewer, ViewerError};
