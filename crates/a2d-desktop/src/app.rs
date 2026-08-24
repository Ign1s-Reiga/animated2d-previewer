//! The window, its event loop, and the tray icon.
//!
//! Deliberately thin. Anything that can be decided without a window is decided
//! in [`crate::state`], [`crate::tray`] or [`crate::viewer`]; this module turns
//! platform events into calls on those and carries out the actions they return.
//! That split is what keeps the interesting behaviour testable on a machine
//! with no display.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use a2d_core::{AnimatedModel, LoadReport, PlayOptions, Vec2};
use a2d_render::{GpuContext, RenderError, Viewport};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId, WindowLevel};

use crate::config::Config;
use crate::state::{Action, ViewerState};
use crate::tray::{Tray, TrayCommand};
use crate::viewer::{LoadedModel, Viewer, ViewerError};

/// Crossfade applied when the user switches animation by hand.
const MANUAL_MIX: Duration = Duration::from_millis(150);
/// Crossfade into an idle variation.
const IDLE_MIX: Duration = Duration::from_millis(300);
/// Largest step the animation will take in one frame.
///
/// Without a cap, a machine waking from sleep or a blocked event loop would
/// hand over a delta of minutes and teleport the character mid-animation.
const MAX_FRAME_STEP: Duration = Duration::from_millis(100);

/// Runs the viewer until the user quits.
///
/// `packages` are tried in order; whatever loads becomes the model list. The
/// config supplies the remembered window placement and selection.
pub fn run(packages: Vec<PathBuf>, report: &mut LoadReport) -> Result<(), ViewerError> {
    let (config, config_report) = Config::load();
    report.absorb(config_report);

    // Packages named on the command line come first, but remembered ones are
    // kept so the selector still lists everything the user has opened before.
    let mut paths = packages;
    for model in &config.models {
        if !paths.contains(&model.package) {
            paths.push(model.package.clone());
        }
    }

    let models = Viewer::load_all(&paths, report);
    if models.is_empty() {
        return Err(ViewerError::NoModels);
    }
    let entries: Vec<_> = models.iter().map(LoadedModel::entry).collect();
    let state = ViewerState::from_config(&config, entries);

    let event_loop = EventLoop::new().map_err(|e| unsupported(format!("could not create an event loop: {e}")))?;
    // Poll rather than Wait: the character animates continuously, so there is
    // always a next frame to draw.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App {
        models,
        state,
        config,
        active: None,
        tray: None,
        last_frame: Instant::now(),
        cursor: Vec2::ZERO,
        report: std::mem::take(report),
    };
    let result = event_loop.run_app(&mut app);
    *report = std::mem::take(&mut app.report);
    app.save_config(report);
    result.map_err(|e| unsupported(format!("the event loop failed: {e}")))?;
    Ok(())
}

fn unsupported(message: String) -> ViewerError {
    ViewerError::Render(RenderError::Unsupported(message))
}

/// Everything that only exists once a window does.
///
/// The device is requested *from* the surface rather than before it: an adapter
/// chosen without knowing the surface may not be able to present to it, and a
/// surface made by a different `Instance` than the device cannot be used at all.
struct Active {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    surface_config: wgpu::SurfaceConfiguration,
    viewer: Viewer,
}

struct App {
    /// Decoded models, moved into the viewer once a device exists.
    models: Vec<LoadedModel>,
    state: ViewerState,
    config: Config,
    active: Option<Active>,
    tray: Option<Tray>,
    last_frame: Instant,
    /// Cursor position in physical pixels. Tracked because a drag needs it and
    /// button events do not carry it.
    cursor: Vec2,
    report: LoadReport,
}

impl App {
    /// Brings up the window, the device and the viewer, in that order.
    fn activate(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, ViewerError> {
        let (w, h) = self.config.window.size;
        let attributes = Window::default_attributes()
            .with_title("animated2d")
            // Frameless and transparent: the character *is* the window.
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_inner_size(LogicalSize::new(w, h));

        let window = Arc::new(
            event_loop.create_window(attributes).map_err(|e| unsupported(format!("could not create a window: {e}")))?,
        );
        if let Some((x, y)) = self.config.window.position {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }

        // One instance for both: a surface is only valid with a device from
        // the same instance, and mismatching them fails at the first draw with
        // a bare "surface does not exist" rather than at creation.
        let instance = GpuContext::new_instance();
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| unsupported(format!("could not create a window surface: {e}")))?;
        let gpu = GpuContext::for_surface(instance.clone(), &surface)?;

        let caps = surface.get_capabilities(&gpu.adapter);
        if caps.formats.is_empty() {
            return Err(unsupported("the surface offers no usable texture format".into()));
        }
        // An sRGB target keeps on-screen output matching the offscreen path;
        // without one the character would render noticeably too bright.
        let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        if !format.is_srgb() {
            self.report.note("this surface offers no sRGB format; colours may look brighter than intended");
        }

        // The renderer writes premultiplied alpha, so ask the compositor to
        // treat it that way. Falling back to opaque loses transparency but is
        // better than refusing to open at all.
        let alpha_mode = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied]
            .into_iter()
            .find(|m| caps.alpha_modes.contains(m))
            .unwrap_or(caps.alpha_modes[0]);
        if alpha_mode == wgpu::CompositeAlphaMode::Opaque {
            self.report
                .note("this compositor offers no transparent surface mode; the window background will be opaque");
        }

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &surface_config);

        let mut viewer = Viewer::new(gpu, std::mem::take(&mut self.models))?;
        viewer.set_active(self.state.active_index())?;
        if let Some(name) = self.state.animation_name() {
            viewer.play(name, Duration::ZERO)?;
        }

        Ok(Active { window, surface, format, surface_config, viewer })
    }

    fn viewport(&self) -> Viewport {
        let Some(active) = &self.active else { return Viewport::new(1, 1) };
        Viewport {
            width: active.surface_config.width.max(1),
            height: active.surface_config.height.max(1),
            scale_factor: active.window.scale_factor() as f32,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let Some(active) = &mut self.active else { return };
        active.surface_config.width = width;
        active.surface_config.height = height;
        active.surface.configure(&active.viewer.gpu().device, &active.surface_config);
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).min(MAX_FRAME_STEP);
        self.last_frame = now;

        let paused = self.state.is_paused();
        let standing_idle = self.state.animation_name().map(str::to_string);
        let Some(active) = &mut self.active else { return };

        match active.viewer.update(dt, paused) {
            Ok(Some(variation)) => {
                // Play the variation once, then queue the standing idle back.
                let _ = active.viewer.play(&variation, IDLE_MIX);
                if let Some(name) = standing_idle {
                    let _ = active
                        .viewer
                        .active_mut()
                        .model
                        .play_animation(&name, PlayOptions::looping().with_mix(IDLE_MIX).queued());
                }
            }
            Ok(None) => {}
            Err(e) => self.report.note(format!("update failed: {e}")),
        }

        let frame = match active.surface.get_current_texture() {
            Ok(frame) => frame,
            // Lost and outdated surfaces are routine on resize or a display
            // change; reconfiguring and drawing next frame is the fix.
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let (w, h) = (active.surface_config.width, active.surface_config.height);
                self.resize(w, h);
                return;
            }
            Err(_) => return,
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let viewport = self.viewport();
        let (scale, offset, flip) = (self.state.scale(), self.state.offset(), self.state.flip_x());
        let Some(active) = &mut self.active else { return };
        let camera = active.viewer.camera(viewport, scale, offset);
        if let Err(e) = active.viewer.render(&view, active.format, viewport, camera, flip) {
            self.report.note(format!("render failed: {e}"));
        }
        frame.present();
    }

    /// Applies the current state to the window.
    fn sync_window(&self) {
        let Some(active) = &self.active else { return };
        active.window.set_window_level(if self.state.always_on_top() {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        });
        // Ignored where the platform cannot do it. The state still records the
        // user's choice, so it persists and can be turned back off.
        let _ = active.window.set_cursor_hittest(!self.state.click_through());
    }

    fn apply(&mut self, action: Action) {
        let Action::MoveWindow(delta) = action else { return };
        let Some(active) = &self.active else { return };
        if let Ok(position) = active.window.outer_position() {
            active
                .window
                .set_outer_position(PhysicalPosition::new(position.x + delta.x as i32, position.y + delta.y as i32));
        }
    }

    fn handle_command(&mut self, command: TrayCommand, event_loop: &ActiveEventLoop) {
        match command {
            TrayCommand::Quit => {
                let mut report = LoadReport::new();
                self.save_config(&mut report);
                self.report.absorb(report);
                event_loop.exit();
                return;
            }
            TrayCommand::TogglePause => {
                self.state.toggle_pause();
            }
            TrayCommand::ToggleAlwaysOnTop => {
                self.state.toggle_always_on_top();
                self.sync_window();
            }
            TrayCommand::ToggleClickThrough => {
                self.state.toggle_click_through();
                self.sync_window();
            }
            TrayCommand::NextAnimation => self.change_animation(|s| s.next_animation()),
            TrayCommand::SelectAnimation(index) => self.change_animation(move |s| s.select_animation(index)),
            TrayCommand::NextModel => self.change_model(|s| s.next_model()),
            TrayCommand::SelectModel(index) => self.change_model(move |s| s.select_model(index)),
            TrayCommand::ResetPlacement => {
                self.state.set_scale(1.0);
                self.state.set_offset(Vec2::ZERO);
            }
        }
        if let Some(tray) = &mut self.tray {
            tray.sync(&self.state);
        }
    }

    fn change_animation(&mut self, change: impl FnOnce(&mut ViewerState) -> bool) {
        if !change(&mut self.state) {
            return;
        }
        let Some(name) = self.state.animation_name().map(str::to_string) else { return };
        if let Some(active) = &mut self.active {
            let _ = active.viewer.play(&name, MANUAL_MIX);
        }
    }

    fn change_model(&mut self, change: impl FnOnce(&mut ViewerState) -> bool) {
        if !change(&mut self.state) {
            return;
        }
        let index = self.state.active_index();
        let name = self.state.animation_name().map(str::to_string);
        let Some(active) = &mut self.active else { return };
        if let Err(e) = active.viewer.set_active(index) {
            self.report.note(format!("could not switch model: {e}"));
            return;
        }
        if let Some(name) = name {
            let _ = active.viewer.play(&name, Duration::ZERO);
        }
    }

    fn save_config(&mut self, report: &mut LoadReport) {
        if let Some(active) = &self.active {
            if let Ok(position) = active.window.outer_position() {
                self.config.window.position = Some((position.x, position.y));
            }
            let size = active.window.inner_size();
            self.config.window.size = (size.width, size.height);
        }
        self.state.write_into(&mut self.config);
        if let Err(e) = self.config.save() {
            report.note(format!("settings could not be saved: {e}"));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        match self.activate(event_loop) {
            Ok(active) => {
                self.active = Some(active);
                self.sync_window();
            }
            Err(e) => {
                self.report.note(format!("could not start the viewer: {e}"));
                event_loop.exit();
                return;
            }
        }

        match Tray::new(&self.state) {
            Ok(tray) => self.tray = Some(tray),
            // A missing tray is degraded, not fatal: the window and its
            // keyboard shortcuts still work.
            Err(e) => self.report.note(format!("the tray icon is unavailable: {e}")),
        }
        self.last_frame = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                let mut report = LoadReport::new();
                self.save_config(&mut report);
                self.report.absorb(report);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => self.redraw(),

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Vec2::new(position.x as f32, position.y as f32);
                let action = self.state.drag_to(self.cursor);
                self.apply(action);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.state.begin_drag(self.cursor);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.state.end_drag();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // A pixel delta is a trackpad; 120 units is one notch.
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 120.0,
                };
                if steps != 0.0 {
                    // 1.1 per notch: fine enough to tune, quick enough to use.
                    self.state.zoom_by(1.1f32.powf(steps));
                }
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let command = match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => Some(TrayCommand::Quit),
                    Key::Named(NamedKey::Space) => Some(TrayCommand::TogglePause),
                    Key::Named(NamedKey::Tab) => Some(TrayCommand::NextAnimation),
                    Key::Character("m") => Some(TrayCommand::NextModel),
                    Key::Character("t") => Some(TrayCommand::ToggleAlwaysOnTop),
                    Key::Character("c") => Some(TrayCommand::ToggleClickThrough),
                    Key::Character("r") => Some(TrayCommand::ResetPlacement),
                    Key::Character("f") => {
                        self.state.toggle_flip();
                        None
                    }
                    _ => None,
                };
                if let Some(command) = command {
                    self.handle_command(command, event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Tray activations arrive on their own channel, not through winit.
        while let Some(command) = self.tray.as_ref().and_then(Tray::poll) {
            self.handle_command(command, event_loop);
        }
        if let Some(active) = &self.active {
            active.window.request_redraw();
        }
    }
}
