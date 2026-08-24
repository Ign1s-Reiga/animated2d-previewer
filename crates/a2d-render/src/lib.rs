//! Source-format-neutral GPU renderer.
//!
//! Draws [`a2d_core::RenderList`]s with `wgpu`. It never learns what produced
//! one: no source format, no version, no game. The
//! [`RenderMesh`](a2d_core::RenderMesh) contract is the entire interface, and
//! an architecture test enforces that this crate cannot even depend on the
//! layers that would let it find out.
//!
//! # Shape of the API
//!
//! [`Renderer`] draws into a plain `wgpu::TextureView`. A windowed host passes
//! a surface texture; tests pass an [`OffscreenTarget`] and read the pixels
//! back. Both go through the same code, which is what makes visual regression
//! testing meaningful — there is no second path to drift from.
//!
//! ```no_run
//! use a2d_render::{Camera, FrameSettings, GpuContext, OffscreenTarget, Renderer, Viewport};
//!
//! # fn main() -> Result<(), a2d_render::RenderError> {
//! let gpu = GpuContext::headless()?;
//! let mut renderer = Renderer::new(gpu.clone());
//! let target = OffscreenTarget::new(&gpu, 256, 256)?;
//!
//! let list = a2d_core::RenderList::new(); // filled by `AnimatedModel::emit`
//! let settings = FrameSettings::new(Viewport::new(256, 256), Camera::default());
//! renderer.render(target.view(), target.format(), settings, &list)?;
//!
//! let pixels = target.read_pixels(&gpu)?;
//! # let _ = pixels;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod batch;
pub mod camera;
pub mod gpu;
pub mod pipeline;
pub mod renderer;
pub mod target;
pub mod texture;

pub use batch::{DrawBatch, FrameGeometry, MaskShape, Vertex};
pub use camera::{Camera, Viewport};
pub use gpu::{GpuContext, RenderError};
pub use pipeline::{blend_state, Pipelines, DEPTH_STENCIL_FORMAT};
pub use renderer::{FrameSettings, FrameStats, Renderer};
pub use target::OffscreenTarget;
pub use texture::{decode_png, encode_png, ImageDiff, Rgba8Image, SamplerConfig, TextureCache};
