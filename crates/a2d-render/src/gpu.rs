//! GPU device acquisition.
//!
//! Split out from the renderer so that a host which already owns a device (a
//! desktop window, an editor embedding) can hand one over instead of having a
//! second one created behind its back.

use std::sync::Arc;

/// Why rendering could not proceed.
///
/// Kept separate from [`a2d_core::DecodeError`]: none of these are properties
/// of an asset, and conflating them would make "the model is broken" and "this
/// machine has no GPU" indistinguishable to a caller trying to recover.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// No graphics adapter matched the request. Usually a headless machine, or
    /// one with no working driver for any supported backend.
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),

    #[error("could not create a GPU device: {0}")]
    DeviceRequest(String),

    /// A texture page could not be decoded from its stored bytes.
    #[error("could not decode texture {label:?}: {message}")]
    ImageDecode { label: String, message: String },

    /// A texture's declared size disagrees with its pixel data.
    #[error("texture {label:?} is malformed: {message}")]
    Texture { label: String, message: String },

    /// Reading pixels back from the GPU failed. Only offscreen targets do this.
    #[error("could not read pixels back from the GPU: {0}")]
    Readback(String),

    /// A surface or format the renderer cannot work with.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// A device and queue, plus the adapter they came from.
///
/// Cloning is cheap: every field is an `Arc` internally, and the clone refers
/// to the same GPU device.
#[derive(Debug, Clone)]
pub struct GpuContext {
    /// The instance the adapter came from.
    ///
    /// A surface is only valid with a device from the *same* instance, so a
    /// windowed host must create its surface from this one rather than making
    /// its own. `Instance` is a cheap handle, so holding it costs nothing.
    pub instance: wgpu::Instance,
    /// Kept because a surface's capabilities are queried against the adapter it
    /// was chosen for, and a windowed host needs that after the fact.
    pub adapter: Arc<wgpu::Adapter>,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    /// Human-readable adapter description, for diagnostics and bug reports.
    pub adapter_name: String,
    /// Backend the adapter runs on, e.g. `Vulkan` or `Dx12`.
    pub backend: String,
}

impl GpuContext {
    /// Acquires a device with no surface to present to.
    ///
    /// This is what tests and `animated2d` use. A windowed host wants
    /// [`GpuContext::for_surface`] so the adapter is chosen for compatibility
    /// with the surface it will actually present to.
    pub fn headless() -> Result<GpuContext, RenderError> {
        GpuContext::request(GpuContext::new_instance(), None)
    }

    /// Acquires a device compatible with a surface made from `instance`.
    ///
    /// The instance must be the one that created the surface. Requesting an
    /// adapter for a surface belonging to a different instance is invalid, and
    /// fails later and less legibly — at the first draw, as a missing surface.
    pub fn for_surface(instance: wgpu::Instance, surface: &wgpu::Surface<'_>) -> Result<GpuContext, RenderError> {
        GpuContext::request(instance, Some(surface))
    }

    /// Creates an instance suitable for making surfaces from.
    pub fn new_instance() -> wgpu::Instance {
        wgpu::Instance::new(&wgpu::InstanceDescriptor::default())
    }

    /// Wraps a device the caller already owns.
    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: Arc<wgpu::Adapter>,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
    ) -> GpuContext {
        let info = adapter.get_info();
        let backend = format!("{:?}", info.backend);
        GpuContext { instance, adapter, device, queue, adapter_name: info.name, backend }
    }

    fn request(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<GpuContext, RenderError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // 2D character rendering is not demanding. Low power keeps a laptop
            // on its integrated GPU, which is what a desktop mascot should do.
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface,
        }))
        .map_err(|e| RenderError::NoAdapter(e.to_string()))?;

        let info = adapter.get_info();
        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("a2d-render device"),
            // Nothing here needs an optional feature. Asking for none keeps the
            // renderer working on the widest set of machines.
            required_features: wgpu::Features::empty(),
            // Downlevel defaults so integrated and older GPUs are not excluded.
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| RenderError::DeviceRequest(e.to_string()))?;

        Ok(GpuContext {
            instance,
            adapter: Arc::new(adapter),
            device: Arc::new(device),
            queue: Arc::new(queue),
            adapter_name: info.name,
            backend: format!("{:?}", info.backend),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_name_what_went_wrong() {
        let e = RenderError::ImageDecode { label: "hero.png".into(), message: "bad chunk".into() };
        assert!(e.to_string().contains("hero.png"), "{e}");
        assert!(e.to_string().contains("bad chunk"), "{e}");

        let e = RenderError::NoAdapter("no backend available".into());
        assert!(e.to_string().contains("no suitable GPU adapter"), "{e}");
    }

    #[test]
    fn a_render_error_is_a_standard_error() {
        fn assert_error<T: std::error::Error>(_: &T) {}
        assert_error(&RenderError::Unsupported("x".into()));
    }
}
