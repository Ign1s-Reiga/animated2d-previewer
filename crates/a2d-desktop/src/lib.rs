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
//!
//! # Transparency on Windows
//!
//! A swapchain bound to a window handle is opaque on Windows: wgpu's DX12
//! backend only offers `Opaque` for it, and the Vulkan and GL backends offer
//! only what the driver says, which on NVIDIA is also `Opaque`. So on Windows
//! the viewer creates its window with `WS_EX_NOREDIRECTIONBITMAP` and presents
//! through a DirectComposition visual on DX12, which offers premultiplied
//! alpha. If that cannot be set up the viewer falls back to the plain window
//! handle path and says why in its report; the window then has an opaque
//! background. Other platforms use the ordinary surface path unchanged.
//!
//! That setup is the one place this crate uses `unsafe` (the `composition`
//! module, Windows only). Everything else is `deny(unsafe_code)`.

#![deny(unsafe_code)]

pub mod app;
#[cfg(windows)]
mod composition;
pub mod config;
pub mod pacing;
pub mod state;
pub mod tray;
pub mod viewer;

pub use app::{run, run_with, RunOptions, RunSummary};
pub use config::{Config, ModelConfig, WindowConfig, CONFIG_DIR_ENV};
pub use state::{Action, ModelEntry, ViewerState};
pub use tray::TrayCommand;
pub use viewer::{LoadedModel, Viewer, ViewerError};
