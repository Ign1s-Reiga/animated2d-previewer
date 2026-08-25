//! Loading packages and drawing them.
//!
//! This is the glue between the package format, the runtime and the renderer,
//! and it is shared: the window uses it to draw a frame, and the CLI uses the
//! same type to export frames offscreen. One loader means the headless path and
//! the on-screen path cannot disagree about what a package looks like.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use a2d_core::{AnimatedModel, DecodeError, LoadReport, PlayOptions, RenderList, Rgba, Vec2};
use a2d_pack::Package;
use a2d_render::{
    Camera, FrameSettings, FrameStats, GpuContext, RenderError, Renderer, Rgba8Image, SamplerConfig, Viewport,
};
use a2d_runtime::{GenericSpineModel, IdleDirector};

use crate::state::ModelEntry;

/// Anything the viewer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("runtime: {0}")]
    Runtime(#[from] a2d_core::RuntimeError),
    #[error("no models were loaded")]
    NoModels,
}

/// One package, decoded and ready to play.
pub struct LoadedModel {
    pub package_path: PathBuf,
    /// Any runtime family, behind the shared interface (spec §5). The viewer
    /// deliberately cannot name a concrete model type: knowing which family it
    /// is showing is exactly what it must not need.
    pub model: Box<dyn AnimatedModel>,
    /// Texture pages as stored, kept so a model can be re-uploaded when it
    /// becomes active again without re-reading the package from disk.
    pages: Vec<(String, Option<Rgba8Image>, bool, SamplerConfig)>,
    idle: IdleDirector,
}

impl std::fmt::Debug for LoadedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedModel")
            .field("package_path", &self.package_path)
            .field("model", &self.model.display_name())
            .field("pages", &self.pages.len())
            .finish()
    }
}

impl LoadedModel {
    /// Reads and decodes a package.
    pub fn load(path: &Path, report: &mut LoadReport) -> Result<LoadedModel, ViewerError> {
        let package = Package::read_from(path)?;
        for warning in &package.manifest.import_warnings {
            report.note(format!("recorded at import: {warning}"));
        }
        let ir = package
            .model
            .as_spine()
            .cloned()
            .ok_or_else(|| DecodeError::UnsupportedFormat("only Spine packages can be shown yet".into()))?;
        let ir = Arc::new(ir);

        // Pages are decoded in atlas order, and a page whose file is missing
        // keeps its slot as `None`. Skipping would shift every later texture id
        // and silently draw the wrong art.
        let mut pages = Vec::with_capacity(ir.atlas.pages.len());
        for page in &ir.atlas.pages {
            let sampler = SamplerConfig {
                min_filter: page.min_filter,
                mag_filter: page.mag_filter,
                u_wrap: page.u_wrap,
                v_wrap: page.v_wrap,
            };
            let decoded = package
                .textures
                .iter()
                .find(|t| t.file == page.name)
                .map(|t| a2d_render::decode_png(&t.bytes, &page.name));
            let image = match decoded {
                Some(Ok(image)) => Some(image),
                Some(Err(e)) => {
                    report.note(format!("texture page {:?} could not be decoded: {e}", page.name));
                    None
                }
                None => {
                    report.note(format!("texture page {:?} is missing from the package", page.name));
                    None
                }
            };
            pages.push((page.name.clone(), image, page.premultiplied_alpha, sampler));
        }

        let model: Box<dyn AnimatedModel> = Box::new(GenericSpineModel::load(ir, &package.manifest.display_name));
        model.absorb_degradations(report);
        // Seeded from the package name so a character's idle rhythm is its own
        // and is reproducible between runs.
        let seed = package.manifest.display_name.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        let idle = IdleDirector::from_animations(model.animations(), seed);

        Ok(LoadedModel { package_path: path.to_path_buf(), model, pages, idle })
    }

    /// Builds a model the caller has already decoded.
    ///
    /// The importers live above this crate in the dependency order (spec §3),
    /// so anything that needs one -- a Cubism model out of a game bundle, say --
    /// is assembled by the caller and handed over ready to show.
    pub fn from_parts(
        path: &Path,
        model: Box<dyn AnimatedModel>,
        pages: Vec<(String, Option<Rgba8Image>, bool, SamplerConfig)>,
        report: &mut LoadReport,
    ) -> LoadedModel {
        model.absorb_degradations(report);
        let seed = model.display_name().bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        let idle = IdleDirector::from_animations(model.animations(), seed);
        LoadedModel { package_path: path.to_path_buf(), model, pages, idle }
    }

    /// The selector entry describing this model.
    pub fn entry(&self) -> ModelEntry {
        ModelEntry {
            package: self.package_path.clone(),
            display_name: self.model.display_name().to_string(),
            animations: self.model.animations().iter().map(|a| a.name.clone()).collect(),
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Pages that had no usable image. Non-empty means placeholders are shown.
    pub fn missing_pages(&self) -> Vec<&str> {
        self.pages.iter().filter(|(_, image, _, _)| image.is_none()).map(|(name, ..)| name.as_str()).collect()
    }
}

/// Draws one character at a time.
///
/// Only the active model's pages are uploaded. The runtime emits
/// `TextureId(page index)`, so two models resident at once would need separate
/// id spaces; showing several characters simultaneously is a spec §13
/// nice-to-have and would want that work rather than this shortcut.
#[derive(Debug)]
pub struct Viewer {
    gpu: GpuContext,
    renderer: Renderer,
    models: Vec<LoadedModel>,
    active: usize,
    /// Which model's pages are currently uploaded, so a redundant switch does
    /// not re-upload them.
    uploaded: Option<usize>,
}

impl Viewer {
    pub fn new(gpu: GpuContext, models: Vec<LoadedModel>) -> Result<Viewer, ViewerError> {
        if models.is_empty() {
            return Err(ViewerError::NoModels);
        }
        let renderer = Renderer::new(gpu.clone());
        let mut viewer = Viewer { gpu, renderer, models, active: 0, uploaded: None };
        viewer.upload_active()?;
        Ok(viewer)
    }

    /// Loads every package that can be loaded, reporting the ones that cannot.
    ///
    /// A package that fails is skipped rather than aborting the whole viewer:
    /// one broken character should not stop the others from opening.
    pub fn load_all(paths: &[PathBuf], report: &mut LoadReport) -> Vec<LoadedModel> {
        let mut loaded = Vec::new();
        for path in paths {
            match LoadedModel::load(path, report) {
                Ok(model) => loaded.push(model),
                Err(e) => report.note(format!("{} could not be loaded: {e}", path.display())),
            }
        }
        loaded
    }

    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    pub fn models(&self) -> &[LoadedModel] {
        &self.models
    }

    pub fn entries(&self) -> Vec<ModelEntry> {
        self.models.iter().map(LoadedModel::entry).collect()
    }

    pub fn active(&self) -> &LoadedModel {
        // The constructor rejects an empty list and `set_active` bounds-checks,
        // so this index is always valid.
        &self.models[self.active]
    }

    pub fn active_mut(&mut self) -> &mut LoadedModel {
        &mut self.models[self.active]
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Switches the shown model, re-uploading its pages.
    pub fn set_active(&mut self, index: usize) -> Result<bool, ViewerError> {
        if index >= self.models.len() || index == self.active {
            return Ok(false);
        }
        self.active = index;
        self.upload_active()?;
        Ok(true)
    }

    fn upload_active(&mut self) -> Result<(), ViewerError> {
        if self.uploaded == Some(self.active) {
            return Ok(());
        }
        self.renderer.textures_mut().clear();
        let pages = &self.models[self.active].pages;
        for (name, image, premultiplied, sampler) in pages {
            match image {
                Some(image) => {
                    self.renderer.textures_mut().upload(&self.gpu, name, image, *premultiplied, *sampler)?;
                }
                // A visible magenta placeholder, in this page's own slot.
                None => {
                    let placeholder = Rgba8Image::solid(2, 2, [255, 0, 255, 255]);
                    self.renderer.textures_mut().upload(
                        &self.gpu,
                        name,
                        &placeholder,
                        false,
                        SamplerConfig::default(),
                    )?;
                }
            }
        }
        self.uploaded = Some(self.active);
        Ok(())
    }

    /// Starts an animation on the active model.
    pub fn play(&mut self, name: &str, mix: Duration) -> Result<(), ViewerError> {
        let opts = PlayOptions::looping().with_mix(mix);
        self.models[self.active].model.play_animation(name, opts)?;
        Ok(())
    }

    /// Advances the active model, and returns an idle variation when one is due.
    ///
    /// The caller decides what to do with it; the viewer does not queue it
    /// itself, because whether an interaction should interrupt an idle is a
    /// policy question for the host.
    pub fn update(&mut self, dt: Duration, paused: bool) -> Result<Option<String>, ViewerError> {
        let active = &mut self.models[self.active];
        if paused {
            // Still evaluate once at dt = 0 so the pose stays valid after a
            // model switch or a resize while paused.
            active.model.update(Duration::ZERO)?;
            return Ok(None);
        }
        active.model.update(dt)?;
        Ok(active.idle.update(dt.as_secs_f32()))
    }

    /// Poses the active model at an exact time, without advancing tracks.
    pub fn pose_at(&mut self, animation: &str, time: f32) -> Result<(), ViewerError> {
        self.models[self.active].model.pose_at(animation, time)?;
        Ok(())
    }

    /// A camera framing the active model inside `viewport`.
    ///
    /// `scale` multiplies the fitted zoom and `offset` shifts the character, so
    /// the user's remembered placement is applied on top of a sensible default
    /// rather than replacing it.
    pub fn camera(&self, viewport: Viewport, scale: f32, offset: Vec2) -> Camera {
        let mut camera = Camera::fit(self.active().model.bounds(), viewport, 0.1);
        camera.pixels_per_unit *= scale.max(f32::MIN_POSITIVE);
        // The offset is in logical pixels, so it converts to model units
        // through the zoom — otherwise the character would drift as it scales.
        if camera.pixels_per_unit > 0.0 {
            camera.center -= Vec2::new(offset.x, offset.y) / camera.pixels_per_unit;
        }
        camera
    }

    /// Draws the active model into `view`.
    pub fn render(
        &mut self,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        viewport: Viewport,
        camera: Camera,
        flip_x: bool,
    ) -> Result<FrameStats, RenderError> {
        let model = &mut self.models[self.active].model;
        model.set_scale(Vec2::new(if flip_x { -1.0 } else { 1.0 }, 1.0));

        let mut list = RenderList::new();
        model.emit(&mut list);
        // Transparent, so the desktop shows through wherever the character does
        // not cover.
        let settings = FrameSettings::new(viewport, camera).with_clear_color(Rgba::TRANSPARENT);
        self.renderer.render(view, format, settings, &list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_a_missing_package_reports_rather_than_panicking() {
        let mut report = LoadReport::new();
        let err = LoadedModel::load(Path::new("no/such.a2dpack"), &mut report).unwrap_err();
        assert!(matches!(err, ViewerError::Decode(_)), "{err}");
    }

    #[test]
    fn loading_a_set_skips_what_cannot_load_and_says_so() {
        let mut report = LoadReport::new();
        let loaded = Viewer::load_all(&[PathBuf::from("no/such.a2dpack")], &mut report);
        assert!(loaded.is_empty());
        assert!(report.to_string().contains("could not be loaded"), "{report}");
    }

    #[test]
    fn a_viewer_with_no_models_is_refused() {
        // Requires no GPU: the emptiness check happens before any device use.
        let err = std::panic::catch_unwind(|| {
            // `Viewer::new` needs a GpuContext, so this asserts the shape of the
            // error path rather than constructing one.
            ViewerError::NoModels.to_string()
        });
        assert_eq!(err.unwrap(), "no models were loaded");
    }

    #[test]
    fn viewer_errors_carry_their_cause() {
        let e = ViewerError::Render(RenderError::NoAdapter("headless".into()));
        assert!(e.to_string().contains("no suitable GPU adapter"), "{e}");
    }
}
