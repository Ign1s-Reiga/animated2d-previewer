//! Windows-only: a DirectComposition visual for the swapchain to present into.
//!
//! # Why this exists
//!
//! A swapchain bound to a plain window handle (HWND) cannot be transparent on
//! Windows through wgpu. wgpu's DX12 backend hard-codes `Opaque` for HWND
//! surfaces, and the Vulkan and GL backends pass the driver's answer through,
//! which on NVIDIA is `Opaque` only. A swapchain created *for composition* and
//! attached to a DirectComposition visual is premultiplied-alpha aware, and
//! the desktop window manager blends it over whatever is behind the window.
//!
//! The window must also be created with `WS_EX_NOREDIRECTIONBITMAP`
//! (`with_no_redirection_bitmap(true)` in winit). Without it the window manager
//! keeps an opaque redirection surface under the visual and the background
//! comes out white.
//!
//! wgpu never calls `IDCompositionDevice::Commit`, so the caller must, after
//! the surface is configured and again after every reconfigure.
//!
//! # Scope of the unsafe code
//!
//! This is the only module in the crate allowed to use `unsafe`. Everything
//! unsafe here is a COM call or the hand-off of a raw visual pointer to wgpu.
//! No Win32 type escapes: callers see [`Composition`] as an opaque guard.

#![allow(unsafe_code)]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use winit::window::Window;

/// The DirectComposition objects that keep a visual attached to a window.
///
/// Must outlive the surface made from the visual, so it is stored after the
/// surface and dropped after it.
pub struct Composition {
    device: IDCompositionDevice,
    // Held only to keep the objects alive; releasing the target detaches the
    // visual tree from the window.
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

impl Composition {
    /// Publishes pending changes to the window manager.
    ///
    /// Failure is returned rather than ignored so the caller can report it,
    /// but it is not fatal: the next commit will publish everything again.
    pub fn commit(&self) -> Result<(), String> {
        // SAFETY: `device` is a live COM interface owned by `self`, and
        // `Commit` takes no pointers.
        unsafe { self.device.Commit() }.map_err(|e| format!("DirectComposition commit failed: {e}"))
    }
}

/// Makes a wgpu surface that presents through a DirectComposition visual
/// attached to `window`.
///
/// The surface is created from `instance`, which must be the instance the
/// device is later requested from.
pub fn create_surface(
    instance: &wgpu::Instance,
    window: &Window,
) -> Result<(wgpu::Surface<'static>, Composition), String> {
    let hwnd = hwnd_of(window)?;

    // SAFETY: each call below is a COM method invoked on an interface this
    // function created and still holds. `hwnd` came from a live winit window
    // that the caller keeps alive for at least as long as the returned
    // `Composition`. `visual.as_raw()` is a valid `IDCompositionVisual`
    // pointer; wgpu takes its own reference to it while creating the
    // swapchain, and `Composition` keeps ours alive for the surface's lifetime.
    unsafe {
        let device: IDCompositionDevice =
            DCompositionCreateDevice2(None).map_err(|e| format!("DCompositionCreateDevice2 failed: {e}"))?;
        let target = device.CreateTargetForHwnd(hwnd, true).map_err(|e| format!("CreateTargetForHwnd failed: {e}"))?;
        let visual = device.CreateVisual().map_err(|e| format!("CreateVisual failed: {e}"))?;
        target.SetRoot(&visual).map_err(|e| format!("SetRoot failed: {e}"))?;
        let surface = instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CompositionVisual(visual.as_raw()))
            .map_err(|e| format!("could not create a composition surface: {e}"))?;
        Ok((surface, Composition { device, _target: target, _visual: visual }))
    }
}

fn hwnd_of(window: &Window) -> Result<HWND, String> {
    let handle = window.window_handle().map_err(|e| format!("the window has no native handle: {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        other => Err(format!("expected a Win32 window handle, got {other:?}")),
    }
}
