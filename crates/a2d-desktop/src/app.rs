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

use a2d_core::{LoadReport, Vec2};
use a2d_render::{GpuContext, RenderError, Viewport};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId, WindowLevel};

use crate::config::{absolute_path, has_manifest, is_dead_entry, Config, CONFIG_VERSION, MAX_WINDOW, MIN_WINDOW};
use crate::edge::{self, EdgeZone};
use crate::pacing::{FrameClock, PacingStats};
use crate::state::{Action, ViewerState};
use crate::tray::{Tray, TrayCommand};
use crate::viewer::{LoadedModel, Viewer, ViewerError};

/// Crossfade applied when the user switches animation by hand.
const MANUAL_MIX: Duration = Duration::from_millis(150);
/// Crossfade into an idle variation.
const IDLE_MIX: Duration = Duration::from_millis(300);
/// The crossfade into a reaction a click starts.
const REACTION_MIX: Duration = Duration::from_millis(200);
/// Largest step the animation will take in one frame.
///
/// Without a cap, a machine waking from sleep or a blocked event loop would
/// hand over a delta of minutes and teleport the character mid-animation.
const MAX_FRAME_STEP: Duration = Duration::from_millis(100);

/// How to run the viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// Packages to open, tried in order. Whatever loads becomes the model list.
    pub packages: Vec<PathBuf>,
    /// Quit automatically after this long.
    ///
    /// For scripted screenshots, smoke tests and CI. It takes exactly the same
    /// shutdown path as quitting by hand, so settings are saved the same way —
    /// which is what makes it usable to *test* that path rather than a
    /// separate one that only tests itself.
    pub exit_after: Option<Duration>,
    /// Create the window invisible and unfocused.
    ///
    /// For scripted runs and tests, so a smoke test does not throw a window
    /// onto the desktop of whoever is running it. The event loop, device,
    /// surface, render, present, quit and save are the real ones, but the draw
    /// trigger is not: a hidden window gets no `RedrawRequested` on Windows, so
    /// hidden mode draws straight from `about_to_wait` instead. The visible
    /// trigger (`request_redraw` and `RedrawRequested`) is therefore not
    /// exercised by a hidden run.
    ///
    /// On Wayland winit cannot hide a window, so this has no effect there.
    pub hidden: bool,
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// Frames actually presented. Zero means the window never drew, which is a
    /// failure even when nothing errored.
    pub frames: u64,
    pub elapsed: Duration,
    /// The graphics backend that drew, e.g. `Dx12`. `none` if no window came up.
    pub backend: String,
    /// The composite alpha mode the surface was configured with, e.g.
    /// `PreMultiplied`. `Opaque` means the window background is not transparent.
    pub alpha_mode: String,
    /// Display refresh period the run was paced against, if the platform said.
    pub refresh_period: Option<Duration>,
    /// Presented frames that arrived more than 1.5 refresh periods after the
    /// previous one. Zero, and not measured, when `refresh_period` is `None`
    /// (unknown, or a hidden run with no display behind it).
    pub late_frames: u64,
    /// The longest gap between two presented frames.
    pub worst_interval: Duration,
}

impl RunSummary {
    /// One line describing frame pacing, for the CLI summary.
    pub fn pacing_line(&self) -> String {
        let worst = self.worst_interval.as_secs_f64() * 1000.0;
        match self.refresh_period {
            None => {
                format!("Frame pacing: late frames not measured (refresh rate unknown), worst interval {worst:.1} ms.")
            }
            Some(p) => format!(
                "Frame pacing: {} late {} (interval over 1.5x the {:.1} Hz period), worst interval {worst:.1} ms.",
                self.late_frames,
                if self.late_frames == 1 { "frame" } else { "frames" },
                1.0 / p.as_secs_f64()
            ),
        }
    }
}

/// Runs the viewer until the user quits.
///
/// The config supplies the remembered window placement and selection.
pub fn run(options: RunOptions, report: &mut LoadReport) -> Result<RunSummary, ViewerError> {
    run_with(options, Vec::new(), report)
}

/// Runs the viewer, showing models the caller decoded itself as well.
///
/// The importers sit above this crate in the dependency order (spec §3), so a
/// model that only an importer can produce is built by the caller and handed
/// over ready to show. The viewer never learns where it came from.
pub fn run_with(
    options: RunOptions,
    preloaded: Vec<LoadedModel>,
    report: &mut LoadReport,
) -> Result<RunSummary, ViewerError> {
    let (mut config, config_report) = Config::load();
    report.absorb(config_report);

    // Forget what has been deleted, once, and write that down, so the same
    // missing path is not reported on every start.
    // Relative entries first: loaded models carry absolute paths, and an
    // entry that cannot match its own model is shown twice and loses its
    // settings.
    let migrated = config.migrate_relative_entries(has_manifest);
    let forgot = config.forget_dead_models(is_dead_entry, report);
    if migrated || forgot {
        if let Err(e) = config.save() {
            report.note(format!("settings could not be saved: {e}"));
        }
    }

    // Packages named on the command line come first, but remembered ones are
    // kept so the selector still lists everything the user has opened before.
    let mut paths: Vec<_> = options.packages.iter().map(|p| absolute_path(p)).collect();
    // What the caller named is what they want to see, not whichever model
    // the config last had active.
    let preferred = preloaded.first().map(|m| m.package_path.clone()).or_else(|| paths.first().cloned());
    for model in &config.models {
        if !paths.contains(&model.package) {
            paths.push(model.package.clone());
        }
    }

    // A preloaded model already covers its own path. Without this the viewer
    // would try to read it again as a package the moment the config remembered
    // it, and report a failure for a model that is on screen.
    paths.retain(|path| !preloaded.iter().any(|m| m.package_path == *path));

    let mut models = preloaded;
    models.extend(Viewer::load_all(&paths, report));
    if models.is_empty() {
        return Err(ViewerError::NoModels);
    }
    let entries: Vec<_> = models.iter().map(LoadedModel::entry).collect();
    let state = ViewerState::from_config_preferring(&config, entries, preferred.as_deref());

    let event_loop = EventLoop::new().map_err(|e| unsupported(format!("could not create an event loop: {e}")))?;
    // Poll rather than Wait: the character animates continuously, so there is
    // always a next frame to draw.
    event_loop.set_control_flow(ControlFlow::Poll);

    let started = Instant::now();
    let mut app = App {
        models,
        state,
        config,
        active: None,
        tray: None,
        last_frame: Instant::now(),
        clock: FrameClock::default(),
        stats: PacingStats::default(),
        last_present: None,
        refresh_period: None,
        cursor: None,
        hovered: false,
        edge_cursor: None,
        resizing: false,
        resize_unsupported: false,
        report: std::mem::take(report),
        exit_after: options.exit_after,
        hidden: options.hidden,
        deadline: None,
        active_since: None,
        frames: 0,
        saved: false,
        backend: "none".to_string(),
        alpha_mode: "none".to_string(),
        failure: None,
    };
    let result = event_loop.run_app(&mut app);
    *report = std::mem::take(&mut app.report);
    // A run that ended without passing through a quit path — a platform error,
    // say — still gets its settings written.
    app.save_config(report);
    result.map_err(|e| unsupported(format!("the event loop failed: {e}")))?;
    if let Some(failure) = app.failure.take() {
        return Err(unsupported(format!("could not start the viewer: {failure}")));
    }
    // Time spent drawing, falling back to the whole run if a window never came
    // up. Counting start-up would report a frame rate the viewer never had.
    let elapsed = app.active_since.map_or_else(|| started.elapsed(), |at| at.elapsed());
    Ok(RunSummary {
        frames: app.frames,
        elapsed,
        backend: app.backend,
        alpha_mode: app.alpha_mode,
        refresh_period: app.refresh_period,
        late_frames: app.stats.late_frames,
        worst_interval: app.stats.worst_interval,
    })
}

fn unsupported(message: String) -> ViewerError {
    ViewerError::Render(RenderError::Unsupported(message))
}

/// Everything that only exists once a window does.
///
/// The device is requested *from* the surface rather than before it: an adapter
/// chosen without knowing the surface may not be able to present to it, and a
/// surface made by a different `Instance` than the device cannot be used at all.
///
/// Field order is drop order. The surface goes first, then the composition it
/// was made from, and the window last, which is what DirectComposition needs.
struct Active {
    surface: wgpu::Surface<'static>,
    composition: Option<CompositionGuard>,
    window: Arc<Window>,
    format: wgpu::TextureFormat,
    surface_config: wgpu::SurfaceConfiguration,
    viewer: Viewer,
}

/// A window, the surface presenting into it, and a device that can draw there.
struct Target {
    surface: wgpu::Surface<'static>,
    composition: Option<CompositionGuard>,
    window: Arc<Window>,
    gpu: GpuContext,
}

/// A [`Target`] whose surface has been configured once.
struct Opened {
    target: Target,
    format: wgpu::TextureFormat,
    surface_config: wgpu::SurfaceConfiguration,
}

/// Picks a format and alpha mode and configures the surface for the first time.
///
/// Errors are captured with error scopes and returned: left alone, wgpu sends
/// them to its uncaptured-error handler, which panics. On the composition path
/// this configure is where the swapchain is really created, so a failure here
/// has to reach the caller as a value for the fallback to run.
fn configure_first(target: &Target) -> Result<(wgpu::TextureFormat, wgpu::SurfaceConfiguration), String> {
    let caps = target.surface.get_capabilities(&target.gpu.adapter);
    if caps.formats.is_empty() {
        return Err("the surface offers no usable texture format".into());
    }
    // An sRGB target keeps on-screen output matching the offscreen path;
    // without one the character would render noticeably too bright.
    let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);

    // The renderer writes premultiplied alpha, so ask the compositor to
    // treat it that way. Falling back to opaque loses transparency but is
    // better than refusing to open at all.
    let alpha_mode = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied]
        .into_iter()
        .find(|m| caps.alpha_modes.contains(m))
        .or_else(|| caps.alpha_modes.first().copied())
        .unwrap_or(wgpu::CompositeAlphaMode::Opaque);

    let size = target.window.inner_size();
    let limit = target.gpu.device.limits().max_texture_dimension_2d.max(1);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        // Never past what the device can make: wgpu would panic.
        width: size.width.clamp(1, limit),
        height: size.height.clamp(1, limit),
        present_mode: wgpu::PresentMode::AutoVsync,
        // Two frames in flight, kept after measuring three on a 60 Hz DX12
        // composition swapchain: 60 s runs of 3592 frames showed 2 late
        // frames at 2 and 6 at 3 without smoothing, 1 and 1 with it, which is
        // inside run-to-run noise. A third frame buys no measurable slack and
        // adds a frame (16.7 ms) of lag between a drag and the window
        // following it. Smoothing, not queue depth, is what evens out pacing.
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: vec![],
    };

    let device = &target.gpu.device;
    let filters = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal];
    for filter in filters {
        device.push_error_scope(filter);
    }
    target.surface.configure(device, &config);
    // One pop per push. Native wgpu resolves these immediately; blocking is how
    // the rest of this crate already waits on wgpu futures.
    let mut failure = None;
    for _ in filters {
        if let Some(e) = pollster::block_on(device.pop_error_scope()) {
            failure.get_or_insert(e.to_string());
        }
    }
    match failure {
        Some(e) => Err(format!("configuring the surface failed: {e}")),
        None => Ok((format, config)),
    }
}

#[cfg(windows)]
type CompositionGuard = crate::composition::Composition;
/// Only Windows has a composition step; elsewhere this is never `Some`.
#[cfg(not(windows))]
type CompositionGuard = std::convert::Infallible;

/// Publishes a DirectComposition tree after a (re)configure. wgpu never does,
/// and without it the first frame, or the new size, does not show.
#[cfg(windows)]
fn commit(composition: &Option<CompositionGuard>, report: &mut LoadReport) {
    if let Some(Err(e)) = composition.as_ref().map(crate::composition::Composition::commit) {
        report.note(e);
    }
}

#[cfg(not(windows))]
fn commit(_composition: &Option<CompositionGuard>, _report: &mut LoadReport) {}

/// The pointer in physical screen pixels. Windows asks the system, since a
/// click-through window gets no pointer events.
#[cfg(windows)]
fn global_cursor() -> Option<(i32, i32)> {
    crate::composition::cursor_position()
}

/// Elsewhere there is no global query: the caller falls back to the last
/// position the window itself saw.
#[cfg(not(windows))]
fn global_cursor() -> Option<(i32, i32)> {
    None
}

fn resize_direction(zone: EdgeZone) -> ResizeDirection {
    match zone {
        EdgeZone::North => ResizeDirection::North,
        EdgeZone::South => ResizeDirection::South,
        EdgeZone::East => ResizeDirection::East,
        EdgeZone::West => ResizeDirection::West,
        EdgeZone::NorthEast => ResizeDirection::NorthEast,
        EdgeZone::NorthWest => ResizeDirection::NorthWest,
        EdgeZone::SouthEast => ResizeDirection::SouthEast,
        EdgeZone::SouthWest => ResizeDirection::SouthWest,
    }
}

fn cursor_icon(zone: EdgeZone) -> CursorIcon {
    match zone {
        EdgeZone::East | EdgeZone::West => CursorIcon::EwResize,
        EdgeZone::North | EdgeZone::South => CursorIcon::NsResize,
        EdgeZone::NorthEast | EdgeZone::SouthWest => CursorIcon::NeswResize,
        EdgeZone::NorthWest | EdgeZone::SouthEast => CursorIcon::NwseResize,
    }
}

struct App {
    /// Decoded models, moved into the viewer once a device exists.
    models: Vec<LoadedModel>,
    state: ViewerState,
    config: Config,
    active: Option<Active>,
    tray: Option<Tray>,
    last_frame: Instant,
    /// Smooths the measured frame delta against the display refresh period.
    clock: FrameClock,
    stats: PacingStats,
    /// When the previous frame was presented, for the late-frame statistics.
    last_present: Option<Instant>,
    /// Refresh period of the monitor the window is on, when known.
    refresh_period: Option<Duration>,
    /// Cursor position in physical pixels. Tracked because a drag needs it and
    /// button events do not carry it.
    cursor: Option<Vec2>,
    /// Whether the pointer is over the window, for the hover tint.
    hovered: bool,
    /// The resize cursor currently set, so it is only changed on a transition.
    edge_cursor: Option<EdgeZone>,
    /// A native resize is in progress: no `CursorMoved` arrives meanwhile, so
    /// the cursor icon must be left alone.
    resizing: bool,
    /// The platform refused a native resize; zones are off for the run.
    resize_unsupported: bool,
    report: LoadReport,
    /// How long to run for, when a run is time-limited.
    ///
    /// Measured from when the window is up rather than from process start:
    /// creating a device and uploading textures can take seconds on a cold
    /// start, and a deadline ticking through that would quit before the first
    /// frame was ever drawn.
    exit_after: Option<Duration>,
    /// Whether the window is created invisible and unfocused.
    hidden: bool,
    /// The instant that deadline falls due, once there is a window.
    deadline: Option<Instant>,
    /// When the window came up, so the reported frame rate measures drawing
    /// rather than start-up.
    active_since: Option<Instant>,
    /// Frames presented so far.
    frames: u64,
    /// Whether settings have already been written, so the belt-and-braces save
    /// after the loop does not overwrite them with a torn-down state.
    saved: bool,
    /// Backend and alpha mode of the surface in use, for the run summary.
    backend: String,
    alpha_mode: String,
    /// Why the window could not be brought up, if it could not. Returned from
    /// `run_with` so a failed start is an error and not a run that drew nothing.
    failure: Option<ViewerError>,
}

impl App {
    /// Re-reads the refresh period of the monitor the window is on.
    ///
    /// Smoothing is off for hidden runs: they draw from `about_to_wait` with no
    /// display behind them, so there is no vsync to smooth against.
    fn update_refresh(&mut self) {
        let Some(active) = &self.active else { return };
        let period = active
            .window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            // Windows reports 0 or 1 for "hardware default"; anything outside a
            // plausible range is treated as unknown rather than believed.
            .filter(|mhz| (20_000..=1_000_000).contains(mhz))
            .map(|mhz| Duration::from_secs_f64(1000.0 / f64::from(mhz)));
        // A hidden run draws with no display behind it: nothing to smooth
        // against and no meaningful late-frame count.
        let period = if self.hidden { None } else { period };
        self.refresh_period = period;
        self.clock.set_period(period);
    }

    /// Brings up the window, the device and the viewer, in that order.
    fn open_window(
        &self,
        event_loop: &ActiveEventLoop,
        no_redirection_bitmap: bool,
    ) -> Result<Arc<Window>, ViewerError> {
        let (w, h) = self.config.window.size;
        // A version-1 config holds a physical size; later ones a logical one.
        let inner_size: winit::dpi::Size = if self.config.window.size_is_physical {
            PhysicalSize::new(w, h).into()
        } else {
            LogicalSize::new(w, h).into()
        };
        let attributes = Window::default_attributes()
            .with_title("animated2d")
            // Frameless and transparent: the character *is* the window.
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            // A drag can then never leave what the config would clamp to.
            .with_min_inner_size(LogicalSize::new(MIN_WINDOW, MIN_WINDOW))
            .with_max_inner_size(LogicalSize::new(MAX_WINDOW, MAX_WINDOW))
            // Neither shown nor focused when hidden: `with_active(false)` alone
            // still shows the window, it just declines to take the focus.
            .with_visible(!self.hidden)
            .with_active(!self.hidden)
            .with_inner_size(inner_size);
        #[cfg(windows)]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes.with_no_redirection_bitmap(no_redirection_bitmap)
        };
        #[cfg(not(windows))]
        let _ = no_redirection_bitmap;

        let window = Arc::new(
            event_loop.create_window(attributes).map_err(|e| unsupported(format!("could not create a window: {e}")))?,
        );
        if let Some((x, y)) = self.config.window.position {
            window.set_outer_position(PhysicalPosition::new(x, y));
            // The window was created on the primary monitor; moving it to one
            // with another scale factor makes Windows rescale it. A physical
            // size from a version-1 config is asked for again, now it is there.
            if self.config.window.size_is_physical {
                let _ = window.request_inner_size(PhysicalSize::new(w, h));
            }
        }
        Ok(window)
    }

    /// The ordinary route: a surface bound to the window handle.
    fn open_plain(&self, event_loop: &ActiveEventLoop) -> Result<Opened, ViewerError> {
        let window = self.open_window(event_loop, false)?;
        // One instance for both: a surface is only valid with a device from
        // the same instance, and mismatching them fails at the first draw with
        // a bare "surface does not exist" rather than at creation.
        let instance = GpuContext::new_instance();
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| unsupported(format!("could not create a window surface: {e}")))?;
        let gpu = GpuContext::for_surface(instance, &surface)?;
        let target = Target { surface, composition: None, window, gpu };
        let (format, surface_config) = configure_first(&target).map_err(unsupported)?;
        Ok(Opened { target, format, surface_config })
    }

    /// Windows: a surface presenting through a DirectComposition visual, the
    /// only way to a premultiplied-alpha swapchain there. The error says which
    /// step failed, for the report; the caller falls back to [`Self::open_plain`].
    ///
    /// The first configure happens here, not in the caller, because that is
    /// where wgpu actually creates the composition swapchain. Its failure must
    /// come back as an `Err` so the fallback runs, not panic in wgpu's
    /// uncaptured-error handler.
    #[cfg(windows)]
    fn open_composited(&self, event_loop: &ActiveEventLoop) -> Result<Opened, String> {
        let window = self.open_window(event_loop, true).map_err(|e| e.to_string())?;
        let instance = GpuContext::new_instance();
        let (surface, composition) = crate::composition::create_surface(&instance, &window)?;
        let gpu = match GpuContext::for_surface(instance, &surface) {
            Ok(gpu) => gpu,
            Err(e) => {
                // Surface, then composition, then the window (dropped last as
                // the earliest local): the order DirectComposition needs.
                drop(surface);
                drop(composition);
                return Err(e.to_string());
            }
        };
        // From here the fields of `Target` drop in declaration order on any
        // early return: surface, composition, window.
        let target = Target { surface, composition: Some(composition), window, gpu };
        if target.gpu.backend != "Dx12" {
            return Err(format!("the composition surface was given a {} adapter, not Dx12", target.gpu.backend));
        }
        let (format, surface_config) = configure_first(&target)?;
        Ok(Opened { target, format, surface_config })
    }

    /// Brings up the window, the device and the viewer, in that order.
    fn activate(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, ViewerError> {
        #[cfg(windows)]
        let (opened, fallback_reason) = match self.open_composited(event_loop) {
            Ok(opened) => (opened, None),
            // The failed window and composition were dropped inside, so the
            // fallback starts from nothing: a window created *without* the
            // no-redirection-bitmap flag, which an HWND swapchain needs.
            Err(reason) => match self.open_plain(event_loop) {
                Ok(opened) => (opened, Some(reason)),
                Err(e) => {
                    // Do not lose why the better path was abandoned.
                    self.report.note(format!("DirectComposition was unavailable ({reason})"));
                    return Err(e);
                }
            },
        };
        #[cfg(not(windows))]
        let (opened, fallback_reason): (Opened, Option<String>) = (self.open_plain(event_loop)?, None);
        let Opened { target, format, surface_config } = opened;
        let alpha_mode = surface_config.alpha_mode;

        if !format.is_srgb() {
            self.report.note("this surface offers no sRGB format; colours may look brighter than intended");
        }
        if let Some(reason) = &fallback_reason {
            self.report.note(format!("DirectComposition was unavailable ({reason}); using a plain window surface"));
        }
        if alpha_mode == wgpu::CompositeAlphaMode::Opaque {
            let why = match &fallback_reason {
                Some(reason) => format!("DirectComposition unavailable: {reason}"),
                None => "the platform offers nothing else".to_string(),
            };
            self.report.note(format!(
                "this compositor offers no transparent surface mode on {} ({why}); the window background will be opaque",
                target.gpu.backend
            ));
        }
        self.backend = target.gpu.backend.clone();
        self.alpha_mode = format!("{alpha_mode:?}");
        commit(&target.composition, &mut self.report);

        // `target` is not destructured into locals, because locals drop in
        // reverse order. Only the device moves out (a partial move), into the
        // viewer; the other fields stay in `target` and an early return drops
        // them in declaration order: surface, composition, window.
        let mut viewer = Viewer::new(target.gpu, std::mem::take(&mut self.models))?;
        viewer.set_active(self.state.active_index())?;
        if let Some(name) = self.state.animation_name() {
            viewer.play(name, Duration::ZERO)?;
        }

        Ok(Active {
            surface: target.surface,
            composition: target.composition,
            window: target.window,
            format,
            surface_config,
            viewer,
        })
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
        // Never configure past what the device can make: wgpu would panic.
        let limit = active.viewer.gpu().device.limits().max_texture_dimension_2d.max(1);
        active.surface_config.width = width.min(limit);
        active.surface_config.height = height.min(limit);
        active.surface.configure(&active.viewer.gpu().device, &active.surface_config);
        commit(&active.composition, &mut self.report);
    }

    /// The pointer in physical pixels from the window's client-area origin.
    /// It may lie outside the window: following the pointer anywhere on the
    /// desktop is intended. `None` when the option is off, the window is
    /// minimized (its coordinates are not a real position), or no position is
    /// known.
    fn pointer_in_window(&self) -> Option<Vec2> {
        if !self.state.follow_cursor() {
            return None;
        }
        let active = self.active.as_ref()?;
        if active.window.is_minimized() == Some(true) {
            return None;
        }
        if let (Some(global), Ok(origin)) = (global_cursor(), active.window.inner_position()) {
            return Some(crate::viewer::client_pointer(global, (origin.x, origin.y)));
        }
        self.cursor
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let measured = now.duration_since(self.last_frame);
        let dt = self.clock.step(measured, MAX_FRAME_STEP).min(MAX_FRAME_STEP);
        self.last_frame = now;

        let paused = self.state.is_paused();
        let selected = self.state.animation_name().map(str::to_string);
        let viewport = self.viewport();
        let (scale, offset, flip) = (self.state.scale(), self.state.offset(), self.state.flip_x());
        let pointer = self.pointer_in_window();
        let follow = self.state.follow_cursor();
        let (hovered, click_through) = (self.hovered, self.state.click_through());
        let Some(active) = &mut self.active else { return };

        // The viewer applies these in `update` through `viewer::step_model`
        // (scale, look target, update), then `render` emits.
        let camera = active.viewer.camera(viewport, scale, offset);
        active.viewer.set_frame_inputs(crate::viewer::FrameInputs { pointer, follow, flip_x: flip, camera, viewport });

        match active.viewer.update(dt, paused) {
            Ok(Some(variation)) => {
                // Play the variation once, then queue the standing idle back.
                if let Err(e) = active.viewer.start_idle_variation(&variation, selected.as_deref(), IDLE_MIX) {
                    self.report.note(format!("idle variation {variation:?} could not start: {e}"));
                }
            }
            Ok(None) => {}
            Err(e) => self.report.note(format!("update failed: {e}")),
        }
        // Follow a handover: the selector shows what is playing, while the pick
        // (what Tab continues from and the config remembers) stays the user's.
        if self.state.note_shown(active.viewer.shown_animation()) {
            if let Some(tray) = &mut self.tray {
                tray.sync(&self.state);
            }
        }

        let frame = match active.surface.get_current_texture() {
            Ok(frame) => frame,
            // Routine and self-correcting: the compositor was busy this frame.
            Err(wgpu::SurfaceError::Timeout) => return,
            // Anything else means the swapchain no longer matches the window --
            // a resize, a display change, a driver reset, or a cold start where
            // it was not ready yet. Reconfiguring and drawing next frame is the
            // fix; returning alone would leave a recoverable surface stuck for
            // the rest of the run.
            Err(e) => {
                if !matches!(e, wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) {
                    self.report.note(format!("the surface had to be rebuilt: {e}"));
                }
                let (w, h) = (active.surface_config.width, active.surface_config.height);
                self.resize(w, h);
                return;
            }
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let Some(active) = &mut self.active else { return };
        let clear = edge::clear_color(hovered, click_through, self.hidden, active.surface_config.alpha_mode);
        if let Err(e) = active.viewer.render_with_clear(&view, active.format, viewport, camera, flip, clear) {
            self.report.note(format!("render failed: {e}"));
            return;
        }
        frame.present();
        self.frames += 1;
        let presented = Instant::now();
        if let Some(previous) = self.last_present.replace(presented) {
            self.stats.record(presented.duration_since(previous), self.refresh_period);
        }
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

    /// The pointer in physical pixels from the client area's origin, as fresh
    /// as the platform allows. On Windows it is read from the system now,
    /// because no `CursorMoved` arrives during a native resize, while a stale
    /// position would pick the wrong zone. Elsewhere it is the last event.
    fn window_cursor(&self) -> Option<Vec2> {
        let active = self.active.as_ref()?;
        if let (Some(global), Ok(origin)) = (global_cursor(), active.window.inner_position()) {
            return Some(crate::viewer::client_pointer(global, (origin.x, origin.y)));
        }
        self.cursor
    }

    /// The border zone under the pointer, if any.
    fn zone_at_cursor(&self) -> Option<EdgeZone> {
        let active = self.active.as_ref()?;
        let cursor = self.window_cursor()?;
        let size = active.window.inner_size();
        edge::edge_zone(
            (f64::from(cursor.x), f64::from(cursor.y)),
            (size.width, size.height),
            active.window.scale_factor(),
            self.state.click_through() || self.resize_unsupported,
        )
    }

    /// Applies the size limits: the configured bounds, and a maximum that the
    /// device's largest texture caps (the surface is as big as the window).
    fn apply_size_limits(&self) {
        let Some(active) = &self.active else { return };
        let scale = active.window.scale_factor();
        let texture = active.viewer.gpu().device.limits().max_texture_dimension_2d;
        let max = edge::max_physical_size(MAX_WINDOW, MIN_WINDOW, scale, texture);
        active.window.set_max_inner_size(Some(PhysicalSize::new(max, max)));
    }

    /// Shows the resize cursor for the zone under the pointer, or the default
    /// one. Only touches the window when the zone changes, and not at all during
    /// a native resize, which owns the cursor.
    fn update_cursor_icon(&mut self) {
        if self.resizing {
            return;
        }
        let zone = self.zone_at_cursor();
        if zone == self.edge_cursor {
            return;
        }
        self.edge_cursor = zone;
        if let Some(active) = &self.active {
            active.window.set_cursor(zone.map_or(CursorIcon::Default, cursor_icon));
        }
    }

    /// Whether the pointer is over the window, requesting a redraw when that
    /// changes so the tint appears and disappears at once.
    ///
    /// On Windows the pointer is polled with `GetCursorPos` against the
    /// window's client rectangle, the same source follow-cursor uses. The
    /// enter/leave events are not trusted there: `CursorLeft` is not sent while
    /// a native resize or move loop owns the mouse, which would leave the tint
    /// stuck on, and a click-through window gets none at all. Elsewhere the
    /// last `CursorMoved` / `CursorLeft` is all there is.
    fn update_hover(&mut self) {
        let Some(active) = &self.active else { return };
        let polled = match (global_cursor(), active.window.inner_position()) {
            (Some(global), Ok(origin)) if active.window.is_minimized() != Some(true) => {
                let size = active.window.inner_size();
                Some(edge::point_in_rect(global, (origin.x, origin.y), (size.width, size.height)))
            }
            _ => None,
        };
        let hovered = polled.unwrap_or(self.cursor.is_some());
        if hovered != self.hovered {
            self.hovered = hovered;
            active.window.request_redraw();
        }
    }

    /// A click on the character at `at` (physical pixels from the client
    /// origin): plays a reaction when the model has one for it.
    fn click(&mut self, at: Vec2) {
        let viewport = self.viewport();
        let (scale, offset, paused) = (self.state.scale(), self.state.offset(), self.state.is_paused());
        let Some(active) = &mut self.active else { return };
        let camera = active.viewer.camera(viewport, scale, offset);
        match active.viewer.react_to_click(&camera, viewport, at, REACTION_MIX, paused) {
            Ok(_) => {}
            Err(e) => self.report.note(format!("reaction could not start: {e}")),
        }
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
                if self.state.click_through() {
                    // The pointer position is no longer known to be current.
                    self.cursor = None;
                }
                self.update_cursor_icon();
            }
            TrayCommand::ToggleFollowCursor => {
                self.state.toggle_follow_cursor();
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
            active.viewer.reset_idle();
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
        active.viewer.reset_idle();
        // The tray's animation items belong to the model it was built for, so
        // build it again for the new one (the old icon goes first).
        if self.tray.is_some() {
            self.tray = None;
            match Tray::new(&self.state) {
                Ok(tray) => self.tray = Some(tray),
                Err(e) => self.report.note(format!("tray could not be rebuilt: {e}")),
            }
        }
    }

    fn save_config(&mut self, report: &mut LoadReport) {
        if self.saved {
            return;
        }
        self.saved = true;
        if let Some(active) = &self.active {
            // A minimized window reports a parked position and a 0x0 size;
            // saving them would reopen it off-screen at the minimum size.
            let size = active.window.inner_size();
            let usable = active.window.is_minimized() != Some(true) && size.width > 0 && size.height > 0;
            if usable {
                if let Ok(position) = active.window.outer_position() {
                    self.config.window.position = Some((position.x, position.y));
                }
                // Logical, as `open_window` reads it back: the physical size
                // would grow by the scale factor on every restart.
                self.config.window.size = edge::logical_size((size.width, size.height), active.window.scale_factor());
                self.config.window.size_is_physical = false;
                self.config.version = CONFIG_VERSION;
            }
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
                self.apply_size_limits();
            }
            Err(e) => {
                self.failure = Some(e);
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
        self.update_refresh();
        let now = Instant::now();
        self.last_frame = now;
        self.clock.reset();
        self.active_since = Some(now);
        self.deadline = self.exit_after.map(|d| now + d);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                let mut report = LoadReport::new();
                self.save_config(&mut report);
                self.report.absorb(report);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.resize(size.width, size.height);
                self.update_cursor_icon();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            // The window may now be on a display with another refresh rate.
            WindowEvent::ScaleFactorChanged { .. } => {
                self.update_refresh();
                self.apply_size_limits();
            }
            WindowEvent::Moved(_) => self.update_refresh(),

            WindowEvent::CursorMoved { position, .. } => {
                let at = Vec2::new(position.x as f32, position.y as f32);
                self.cursor = Some(at);
                // Moves arrive again only once the native resize has ended, so
                // this also clears a flag whose release was never seen.
                self.resizing = false;
                let action = self.state.drag_to(at);
                self.apply(action);
                self.update_cursor_icon();
            }
            // Off Windows the last position is the only pointer source, so a
            // position from before the pointer left must not be used forever.
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.update_cursor_icon();
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // A press in a border zone resizes natively and never drags;
                // anywhere else (or in click-through, which has no zones) it
                // is the character drag, as before.
                let mut resized = false;
                if let (Some(zone), Some(active)) = (self.zone_at_cursor(), &self.active) {
                    match active.window.drag_resize_window(resize_direction(zone)) {
                        Ok(()) => resized = true,
                        Err(e) => {
                            // Not supported here (macOS, say): stop offering
                            // zones and let this very press drag instead.
                            self.resize_unsupported = true;
                            self.report.note(format!("resizing by dragging an edge is unavailable: {e}"));
                        }
                    }
                }
                if resized {
                    self.resizing = true;
                } else {
                    if self.resize_unsupported {
                        self.update_cursor_icon();
                    }
                    // A fresh position, and no drag at all without one: a guess
                    // of the origin would make the window jump by its offset.
                    if let Some(at) = self.window_cursor() {
                        let scale = self.active.as_ref().map_or(1.0, |a| a.window.scale_factor() as f32);
                        self.state.begin_press(at, scale);
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // winit posts this when the native resize loop exits.
                if self.resizing {
                    self.resizing = false;
                    self.update_cursor_icon();
                }
                // A press that never went past the slop is a click; a resize
                // press never armed one, so it cannot be.
                if let Some(at) = self.state.end_press() {
                    self.click(at);
                }
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
                    Key::Character("l") => Some(TrayCommand::ToggleFollowCursor),
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
        // An elapsed deadline quits through `TrayCommand::Quit`, the same path
        // Esc and the tray take, so a scripted run exercises the real shutdown.
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            self.deadline = None;
            self.handle_command(TrayCommand::Quit, event_loop);
            return;
        }
        self.update_hover();
        if self.hidden {
            // Windows delivers no `RedrawRequested` to an invisible window
            // (there is nothing to paint), so asking would leave a scripted run
            // at zero frames. Draw directly instead: the surface, device,
            // render and present are the same calls the visible path makes.
            self.redraw();
        } else if let Some(active) = &self.active {
            active.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_zone_resizes_in_its_own_direction() {
        let pairs = [
            (EdgeZone::North, ResizeDirection::North),
            (EdgeZone::South, ResizeDirection::South),
            (EdgeZone::East, ResizeDirection::East),
            (EdgeZone::West, ResizeDirection::West),
            (EdgeZone::NorthEast, ResizeDirection::NorthEast),
            (EdgeZone::NorthWest, ResizeDirection::NorthWest),
            (EdgeZone::SouthEast, ResizeDirection::SouthEast),
            (EdgeZone::SouthWest, ResizeDirection::SouthWest),
        ];
        for (zone, direction) in pairs {
            assert_eq!(resize_direction(zone), direction, "{zone:?}");
        }
    }

    #[test]
    fn every_zone_shows_the_cursor_along_its_axis() {
        let pairs = [
            (EdgeZone::East, CursorIcon::EwResize),
            (EdgeZone::West, CursorIcon::EwResize),
            (EdgeZone::North, CursorIcon::NsResize),
            (EdgeZone::South, CursorIcon::NsResize),
            // NE and SW lie on the "/" diagonal; NW and SE on the "\" one.
            (EdgeZone::NorthEast, CursorIcon::NeswResize),
            (EdgeZone::SouthWest, CursorIcon::NeswResize),
            (EdgeZone::NorthWest, CursorIcon::NwseResize),
            (EdgeZone::SouthEast, CursorIcon::NwseResize),
        ];
        for (zone, icon) in pairs {
            assert_eq!(cursor_icon(zone), icon, "{zone:?}");
        }
    }
}
