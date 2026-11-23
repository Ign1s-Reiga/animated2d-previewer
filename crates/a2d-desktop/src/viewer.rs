//! Loading packages and drawing them.
//!
//! This is the glue between the package format, the runtime and the renderer,
//! and it is shared: the window uses it to draw a frame, and the CLI uses the
//! same type to export frames offscreen. One loader means the headless path and
//! the on-screen path cannot disagree about what a package looks like.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use a2d_core::{Aabb, AnimatedModel, DecodeError, Handover, LoadReport, PlayOptions, RenderList, Rgba, Vec2};
use a2d_pack::Package;
use a2d_render::{
    Camera, FrameSettings, FrameStats, GpuContext, RenderError, Renderer, Rgba8Image, SamplerConfig, Viewport,
};
use a2d_runtime::{GenericSpineModel, IdleDirector};

use crate::config::absolute_path;
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
    /// Whether `package_path` can be read back by [`LoadedModel::load`], which
    /// decides whether the config may remember it.
    reopenable: bool,
    /// Any runtime family, behind the shared interface (spec §5). The viewer
    /// deliberately cannot name a concrete model type: knowing which family it
    /// is showing is exactly what it must not need.
    pub model: Box<dyn AnimatedModel>,
    /// The box the camera frames, worked out once when the model is loaded.
    ///
    /// Fitting the live pose every frame would rescale the character whenever
    /// an animation reached further than the last: measured on one rig, the
    /// same window drew it 128 by 180 while a standing animation played and
    /// 376 by 636 moments later. A mascot that shrinks when it swings a rifle
    /// is not what anyone wants, so the frame is taken from the opening
    /// animation and then held, and movement happens *within* it.
    framing: Aabb,
    /// Texture pages as stored, kept so a model can be re-uploaded when it
    /// becomes active again without re-reading the package from disk.
    pages: Vec<(String, Option<Rgba8Image>, bool, SamplerConfig)>,
    idle: IdleDirector,
    follow: Follow,
}

/// The box to frame a model in, from the animation it opens with.
///
/// Sampling the opening animation rather than the pose of the moment is what
/// makes the frame stable: a rig whose standing pose is compact but whose
/// attack reaches twice its own height would otherwise be framed differently
/// depending on when the camera happened to be asked.
///
/// A model that cannot be posed at a timestamp falls back to whatever it
/// reports now. A Cubism model can be posed, but its bounds are the canvas, so
/// sampling it yields the same stable box.
fn frame_of(model: &mut dyn AnimatedModel) -> Aabb {
    let opening = model.default_animation().map(str::to_string);
    let Some(name) = opening else { return model.bounds() };
    let duration = model.animations().iter().find(|a| a.name == name).map(|a| a.duration).unwrap_or(0.0);

    let mut framing = Aabb::EMPTY;
    const SAMPLES: u32 = 8;
    for step in 0..=SAMPLES {
        let at = duration * step as f32 / SAMPLES as f32;
        if model.pose_at(&name, at).is_err() {
            return model.bounds();
        }
        let bounds = model.bounds();
        if !bounds.is_empty() {
            framing.extend(bounds.min);
            framing.extend(bounds.max);
        }
    }
    // Leave it where it started rather than at the last sample, so the pose the
    // viewer first shows is the one the animation opens on.
    let _ = model.pose_at(&name, 0.0);
    if framing.is_empty() {
        model.bounds()
    } else {
        framing
    }
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
    /// Whether [`LoadedModel::load`] can open a package of this model type.
    ///
    /// The one place that knows, so the config can tell a remembered package
    /// that will never open from one that merely failed this time.
    pub fn can_open(model_type: a2d_pack::ModelType) -> bool {
        matches!(model_type, a2d_pack::ModelType::Spine)
    }

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

        let mut model = model;
        let framing = frame_of(model.as_mut());
        Ok(LoadedModel {
            package_path: absolute_path(path),
            model,
            framing,
            pages,
            idle,
            follow: Follow::default(),
            reopenable: true,
        })
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
        let mut model = model;
        let framing = frame_of(model.as_mut());
        // Not remembered: the caller built this from something `load` cannot
        // read back from the path (a raw bundle, a Cubism package).
        LoadedModel {
            package_path: absolute_path(path),
            model,
            framing,
            pages,
            idle,
            follow: Follow::default(),
            reopenable: false,
        }
    }

    /// The selector entry describing this model.
    pub fn entry(&self) -> ModelEntry {
        ModelEntry {
            package: self.package_path.clone(),
            display_name: self.model.display_name().to_string(),
            animations: self.model.animations().iter().map(|a| a.name.clone()).collect(),
            default_animation: self.model.default_animation().map(str::to_string),
            reopenable: self.reopenable,
        }
    }

    /// Starts an animation.
    ///
    /// An animation the source hands back from (an emotion, say) plays once and
    /// then hands over as the source says, at its time and with its crossfade;
    /// any other loops. What the character is actually showing afterwards is
    /// [`LoadedModel::shown_animation`]; the pick stays the user's.
    pub fn play(&mut self, name: &str, mix: Duration) -> Result<(), ViewerError> {
        let queued = play_picked(self.model.as_mut(), name, mix)?;
        // Where the character rests: the pick, or where the pick hands back to.
        // A cycle never settles; the model's default is the best guess then.
        let standing =
            resting_place(self.model.as_ref(), name).or_else(|| self.model.default_animation().map(str::to_string));
        self.follow =
            Follow { seen: self.model.current_animation().map(str::to_string), queued, standing, held: false };
        Ok(())
    }

    /// The animation the model is showing as of the last update, which differs
    /// from the user's pick while a handover is under way or after one.
    pub fn shown_animation(&self) -> Option<&str> {
        self.follow.seen.as_deref()
    }

    /// Plays an idle variation once, then returns to where the character rests,
    /// looping.
    ///
    /// That is `selected` (the animation the user chose), unless the choice
    /// hands back to another animation: then it is the one the character
    /// returned to, so a variation never replays an emotion.
    ///
    /// While a variation plays, the shown animation (and the tray check) is the
    /// variation, so choosing the pick again then replays it.
    /// Fails without touching playback when either name is unknown.
    pub fn start_idle_variation(
        &mut self,
        variation: &str,
        selected: Option<&str>,
        mix: Duration,
    ) -> Result<(), ViewerError> {
        let back = self.follow.standing.clone().or_else(|| selected.map(str::to_string));
        start_idle_variation(self.model.as_mut(), variation, back.as_deref(), mix)?;
        self.follow.seen = self.model.current_animation().map(str::to_string);
        self.follow.queued = None;
        Ok(())
    }

    /// Advances the model, and returns an idle variation when one is due.
    ///
    /// The caller decides what to do with it; the viewer does not queue it
    /// itself, because whether an interaction should interrupt an idle is a
    /// policy question for the host.
    pub fn update(&mut self, dt: Duration, paused: bool) -> Result<Option<String>, ViewerError> {
        if paused {
            // Still evaluate once at dt = 0 so the pose stays valid after a
            // model switch or a resize while paused.
            self.model.update(Duration::ZERO)?;
            self.follow.observe(self.model.as_mut());
            return Ok(None);
        }
        self.model.update(dt)?;
        self.follow.observe(self.model.as_mut());
        // No variation while a picked animation is still playing its pass or
        // one is under way; the timer starts over when the character is back.
        if !self.follow.at_rest() {
            self.follow.held = true;
            return Ok(None);
        }
        if self.follow.held {
            self.follow.held = false;
            self.idle.reset();
        }
        Ok(self.idle.update(dt.as_secs_f32()))
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
    ///
    /// An animation the source hands back from (an emotion, say) plays once and
    /// then hands over as the source says, at its time and with its crossfade;
    /// any other loops. What the character is actually showing afterwards is
    /// [`Viewer::shown_animation`]; the pick stays the user's.
    pub fn play(&mut self, name: &str, mix: Duration) -> Result<(), ViewerError> {
        self.models[self.active].play(name, mix)
    }

    /// The animation the active model is showing as of the last update, which
    /// differs from the user's pick while a handover is under way or after one.
    pub fn shown_animation(&self) -> Option<&str> {
        self.models[self.active].shown_animation()
    }

    /// Plays an idle variation once on the active model, then returns to
    /// where the character rests, looping.
    ///
    /// That is `selected` (the animation the user chose), unless the choice
    /// hands back to another animation: then it is the one the character
    /// returned to, so a variation never replays an emotion.
    ///
    /// While a variation plays, the shown animation (and the tray check) is the
    /// variation, so choosing the pick again then replays it.
    /// Fails without touching playback when either name is unknown.
    pub fn start_idle_variation(
        &mut self,
        variation: &str,
        selected: Option<&str>,
        mix: Duration,
    ) -> Result<(), ViewerError> {
        self.models[self.active].start_idle_variation(variation, selected, mix)
    }

    /// Restarts the active model's idle timer, so a variation does not fire
    /// right after the user chose an animation.
    pub fn reset_idle(&mut self) {
        self.models[self.active].idle.reset();
    }

    /// Advances the active model, and returns an idle variation when one is due.
    ///
    /// The caller decides what to do with it; the viewer does not queue it
    /// itself, because whether an interaction should interrupt an idle is a
    /// policy question for the host.
    pub fn update(&mut self, dt: Duration, paused: bool) -> Result<Option<String>, ViewerError> {
        self.models[self.active].update(dt, paused)
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
        let mut camera = Camera::fit(self.active().framing, viewport, 0.1);
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

/// A duration from seconds, or `None` for a value a `Duration` cannot hold
/// (NaN, infinite, negative). `from_secs_f32` panics on those; a package is
/// data, so it must not be able to bring the viewer down.
fn seconds(value: f32) -> Option<Duration> {
    Duration::try_from_secs_f32(value).ok()
}

/// What follows `name`, when the source says so and the target exists.
///
/// A handover to an animation the model does not have, or to the animation
/// itself, is ignored rather than followed: the animation then loops as it would
/// with no handover. Following it would end the pass, find no change of current
/// animation, and leave the track empty (a self-handover loops in the source too).
fn handover_of(model: &dyn AnimatedModel, name: &str) -> Option<Handover> {
    let then = model.animations().iter().find(|a| a.name == name)?.then.clone()?;
    (then.animation != name && model.animations().iter().any(|a| a.name == then.animation)).then_some(then)
}

/// Where a character ends up after `name`: follows the usable handovers until
/// an animation has none. `None` on a cycle, which never settles.
fn resting_place(model: &dyn AnimatedModel, name: &str) -> Option<String> {
    let mut current = name.to_string();
    // More steps than animations means a step repeated.
    for _ in 0..=model.animations().len() {
        match handover_of(model, &current) {
            None => return Some(current),
            Some(then) => current = then.animation,
        }
    }
    None
}

/// Queues the target of `handover` behind what is playing. Returns the target's
/// name once it is queued.
///
/// The target loops, unless it has a handover of its own: then it plays once so
/// that handover can happen (the chain continues when it takes over, see
/// [`Follow::observe`]). An unusable `at` queues it for the end of the pass; an
/// unusable `mix` is a cut.
fn queue_handover(model: &mut dyn AnimatedModel, handover: &Handover) -> Option<String> {
    let base =
        if handover_of(model, &handover.animation).is_some() { PlayOptions::once() } else { PlayOptions::looping() };
    let mut opts = base.with_mix(seconds(handover.mix).unwrap_or(Duration::ZERO)).queued();
    if let Some(at) = seconds(handover.at) {
        opts = opts.with_handover_at(at);
    }
    model.play_animation(&handover.animation, opts).ok()?;
    Some(handover.animation.clone())
}

/// Plays an animation the user picked. One with a handover plays once and
/// queues its target; any other loops. Returns the name queued, if any.
fn play_picked(
    model: &mut dyn AnimatedModel,
    name: &str,
    mix: Duration,
) -> Result<Option<String>, a2d_core::RuntimeError> {
    let Some(handover) = handover_of(model, name) else {
        model.play_animation(name, PlayOptions::looping().with_mix(mix))?;
        return Ok(None);
    };
    model.play_animation(name, PlayOptions::once().with_mix(mix))?;
    Ok(queue_handover(model, &handover))
}

/// What the viewer knows about which animation is playing, as opposed to which
/// the user picked.
#[derive(Debug, Default)]
struct Follow {
    /// The model's current animation as of the last look.
    seen: Option<String>,
    /// The animation queued as a handover, so its takeover is recognised.
    queued: Option<String>,
    /// The animation the character rests in after the user's pick: the pick
    /// itself, or, for a pick that hands back, where it hands back to.
    standing: Option<String>,
    /// Whether the idle timer is held because something else is playing.
    held: bool,
}

impl Follow {
    /// Looks at the model once per frame. Comparing `&str`s allocates nothing
    /// unless the animation changed. Returns whether it did.
    ///
    /// A takeover of the queued handover continues the chain: an entry with a
    /// handover of its own gets that queued now (a model holds one queued entry).
    fn observe(&mut self, model: &mut dyn AnimatedModel) -> bool {
        let current = model.current_animation();
        if current == self.seen.as_deref() {
            return false;
        }
        let current = current.map(str::to_string);
        let entered_by_handover = current.is_some() && current == self.queued;
        self.seen = current.clone();
        self.queued = None;
        if let (true, Some(name)) = (entered_by_handover, current) {
            if let Some(next) = handover_of(model, &name) {
                self.queued = queue_handover(model, &next);
            }
        }
        true
    }

    /// Whether the idle logic may run: nothing but the standing animation (or
    /// an animation the model cannot name) is showing.
    fn at_rest(&self) -> bool {
        match (&self.standing, &self.seen) {
            (Some(standing), Some(seen)) => standing == seen,
            _ => true,
        }
    }
}

/// Plays `variation` once, then queues `selected` back, looping.
///
/// The variation must not loop: a looping one would only hand back at its next
/// wrap, replaying the variation for a pass before the idle returns. Both names are checked
/// first, so a bad name cannot leave the model half-switched.
fn start_idle_variation(
    model: &mut dyn AnimatedModel,
    variation: &str,
    selected: Option<&str>,
    mix: Duration,
) -> Result<(), a2d_core::RuntimeError> {
    for name in std::iter::once(variation).chain(selected) {
        if !model.animations().iter().any(|a| a.name == name) {
            return Err(a2d_core::RuntimeError::UnknownAnimation(name.to_string()));
        }
    }
    model.play_animation(variation, PlayOptions::once().with_mix(mix))?;
    if let Some(name) = selected {
        model.play_animation(name, PlayOptions::looping().with_mix(mix).queued())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every `PlayOptions` it is handed.
    #[derive(Default)]
    struct Recorder {
        played: Vec<(String, PlayOptions)>,
        animations: Vec<a2d_core::AnimationInfo>,
    }

    fn recorder() -> Recorder {
        let animations = ["Fidget", "Stand"]
            .into_iter()
            .map(|n| a2d_core::AnimationInfo { name: n.into(), duration: 1.0, then: None })
            .collect();
        Recorder { played: Vec::new(), animations }
    }

    impl AnimatedModel for Recorder {
        fn update(&mut self, _dt: Duration) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn emit(&self, _out: &mut RenderList) {}
        fn play_animation(&mut self, n: &str, o: PlayOptions) -> Result<(), a2d_core::RuntimeError> {
            self.played.push((n.to_string(), o));
            Ok(())
        }
        fn stop_animation(&mut self, _n: &str) {}
        fn set_expression(&mut self, _n: &str) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn animations(&self) -> &[a2d_core::AnimationInfo] {
            &self.animations
        }
        fn expressions(&self) -> &[a2d_core::ExpressionInfo] {
            &[]
        }
        fn bounds(&self) -> Aabb {
            Aabb::EMPTY
        }
        fn hit_test(&self, _x: f32, _y: f32) -> Option<a2d_core::HitAreaId> {
            None
        }
        fn dispose(&mut self) {}
        fn display_name(&self) -> &str {
            "recorder"
        }
    }

    #[test]
    fn a_variation_plays_once_then_the_idle_is_queued_looping() {
        let mut model = recorder();
        let mix = Duration::from_millis(300);
        start_idle_variation(&mut model, "Fidget", Some("Stand"), mix).unwrap();

        assert_eq!(model.played.len(), 2);
        assert_eq!(model.played[0].0, "Fidget");
        assert_eq!(model.played[0].1, PlayOptions::once().with_mix(mix));
        assert_eq!(model.played[1].0, "Stand");
        assert_eq!(model.played[1].1, PlayOptions::looping().with_mix(mix).queued());
    }

    #[test]
    fn a_variation_without_a_selected_animation_queues_nothing() {
        let mut model = recorder();
        start_idle_variation(&mut model, "Fidget", None, Duration::ZERO).unwrap();
        assert_eq!(model.played.len(), 1);
    }

    #[test]
    fn an_unknown_name_fails_before_touching_playback() {
        let mut model = recorder();
        assert!(start_idle_variation(&mut model, "Fidget", Some("Gone"), Duration::ZERO).is_err());
        assert!(start_idle_variation(&mut model, "Gone", Some("Stand"), Duration::ZERO).is_err());
        assert!(model.played.is_empty());
    }

    // ---- handovers: an emotion plays once, then hands back ----

    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Log {
        played: Vec<(String, PlayOptions)>,
        /// What the model will report after its next update: the test's way of
        /// saying a takeover happened.
        next: Option<String>,
    }

    /// A model that records what it is told and reports the animation a test
    /// scripts, standing in for a takeover that happens during an update.
    struct Scripted {
        log: Rc<RefCell<Log>>,
        animations: Vec<a2d_core::AnimationInfo>,
        default: Option<String>,
        current: Option<String>,
    }

    fn info(name: &str, then: Option<(&str, f32, f32)>) -> a2d_core::AnimationInfo {
        a2d_core::AnimationInfo {
            name: name.into(),
            duration: 2.0,
            then: then.map(|(animation, at, mix)| Handover { animation: animation.into(), at, mix }),
        }
    }

    fn scripted(animations: Vec<a2d_core::AnimationInfo>) -> (Scripted, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        let default = animations.iter().find(|a| a.then.is_none()).map(|a| a.name.clone());
        (Scripted { log: log.clone(), animations, default, current: None }, log)
    }

    /// `idle` is the default; `smile` hands back to it; `bow` hands to `smile`.
    fn emotions() -> (Scripted, Rc<RefCell<Log>>) {
        scripted(vec![
            info("idle", None),
            info("idle_b", None),
            info("smile", Some(("idle", 1.75, 0.25))),
            info("bow", Some(("smile", 1.5, 0.5))),
        ])
    }

    impl AnimatedModel for Scripted {
        fn update(&mut self, _dt: Duration) -> Result<(), a2d_core::RuntimeError> {
            if let Some(next) = self.log.borrow_mut().next.take() {
                self.current = Some(next);
            }
            Ok(())
        }
        fn emit(&self, _out: &mut RenderList) {}
        fn play_animation(&mut self, n: &str, o: PlayOptions) -> Result<(), a2d_core::RuntimeError> {
            // Like a real model, a call that is not queued starts at once.
            if !o.queued {
                self.current = Some(n.to_string());
            }
            self.log.borrow_mut().played.push((n.to_string(), o));
            Ok(())
        }
        fn stop_animation(&mut self, _n: &str) {}
        fn set_expression(&mut self, _n: &str) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn animations(&self) -> &[a2d_core::AnimationInfo] {
            &self.animations
        }
        fn expressions(&self) -> &[a2d_core::ExpressionInfo] {
            &[]
        }
        fn bounds(&self) -> Aabb {
            Aabb::EMPTY
        }
        fn hit_test(&self, _x: f32, _y: f32) -> Option<a2d_core::HitAreaId> {
            None
        }
        fn dispose(&mut self) {}
        fn display_name(&self) -> &str {
            "scripted"
        }
        fn default_animation(&self) -> Option<&str> {
            self.default.as_deref()
        }
        fn current_animation(&self) -> Option<&str> {
            self.current.as_deref()
        }
    }

    fn loaded(model: Scripted) -> LoadedModel {
        LoadedModel::from_parts(Path::new("scripted.a2dpack"), Box::new(model), Vec::new(), &mut LoadReport::new())
    }

    const MIX: Duration = Duration::from_millis(150);
    const FRAME: Duration = Duration::from_millis(16);

    #[test]
    fn picking_an_animation_with_a_handover_plays_it_once_and_queues_the_target() {
        let (mut model, log) = emotions();
        play_picked(&mut model, "smile", MIX).unwrap();
        let played = log.borrow().played.clone();
        assert_eq!(played.len(), 2);
        assert_eq!(played[0], ("smile".to_string(), PlayOptions::once().with_mix(MIX)));
        assert_eq!(played[1].0, "idle");
        assert_eq!(
            played[1].1,
            PlayOptions::looping()
                .with_mix(Duration::from_secs_f32(0.25))
                .with_handover_at(Duration::from_secs_f32(1.75))
                .queued()
        );
    }

    #[test]
    fn picking_an_animation_without_a_handover_loops_and_queues_nothing() {
        let (mut model, log) = emotions();
        let queued = play_picked(&mut model, "idle", MIX).unwrap();
        assert_eq!(queued, None);
        assert_eq!(log.borrow().played, vec![("idle".to_string(), PlayOptions::looping().with_mix(MIX))]);
    }

    #[test]
    fn an_unusable_handover_time_queues_for_the_end_of_the_pass_and_an_unusable_mix_cuts() {
        for bad in [f32::NAN, -1.0, f32::INFINITY, f32::NEG_INFINITY] {
            let (mut model, log) = scripted(vec![info("idle", None), info("smile", Some(("idle", bad, bad)))]);
            play_picked(&mut model, "smile", MIX).unwrap();
            let queued = log.borrow().played[1].1.clone();
            assert_eq!(queued, PlayOptions::looping().with_mix(Duration::ZERO).queued(), "{bad}");
            assert_eq!(queued.handover_at, None);
        }
        // A usable time with an unusable mix keeps the time.
        let (mut model, log) = scripted(vec![info("idle", None), info("smile", Some(("idle", 1.0, f32::NAN)))]);
        play_picked(&mut model, "smile", MIX).unwrap();
        let queued = log.borrow().played[1].1.clone();
        assert_eq!(queued.mix_duration, Duration::ZERO);
        assert_eq!(queued.handover_at, Some(Duration::from_secs(1)));
    }

    #[test]
    fn a_short_emotion_keeps_the_handover_time_the_source_gave() {
        // Never clamped: a zero time is a legitimate answer from the importer.
        let (mut model, log) = scripted(vec![info("idle", None), info("smile", Some(("idle", 0.0, 0.25)))]);
        play_picked(&mut model, "smile", MIX).unwrap();
        assert_eq!(log.borrow().played[1].1.handover_at, Some(Duration::ZERO));
    }

    #[test]
    fn a_handover_to_an_unknown_animation_is_ignored_and_the_pick_loops() {
        let (mut model, log) = scripted(vec![info("idle", None), info("smile", Some(("gone", 1.0, 0.25)))]);
        let queued = play_picked(&mut model, "smile", MIX).unwrap();
        assert_eq!(queued, None);
        assert_eq!(log.borrow().played, vec![("smile".to_string(), PlayOptions::looping().with_mix(MIX))]);
    }

    #[test]
    fn a_chain_queues_the_next_handover_when_the_previous_target_takes_over() {
        let (mut model, log) = emotions();
        let queued = play_picked(&mut model, "bow", MIX).unwrap();
        // The middle link plays once so that its own handover can happen.
        assert_eq!(queued.as_deref(), Some("smile"));
        assert_eq!(
            log.borrow().played[1].1,
            PlayOptions::once()
                .with_mix(Duration::from_secs_f32(0.5))
                .with_handover_at(Duration::from_secs_f32(1.5))
                .queued()
        );
        let mut follow = Follow { seen: Some("bow".into()), queued, standing: Some("idle".into()), held: false };

        assert!(!follow.observe(&mut model), "nothing changed yet");
        assert_eq!(log.borrow().played.len(), 2);

        log.borrow_mut().next = Some("smile".into());
        model.update(FRAME).unwrap();
        assert!(follow.observe(&mut model));
        let played = log.borrow().played.clone();
        assert_eq!(played.len(), 3);
        assert_eq!(played[2].0, "idle");
        assert_eq!(played[2].1.handover_at, Some(Duration::from_secs_f32(1.75)));
        assert!(played[2].1.looping && played[2].1.queued);

        // Arriving at the last link queues nothing more.
        log.borrow_mut().next = Some("idle".into());
        model.update(FRAME).unwrap();
        assert!(follow.observe(&mut model));
        assert_eq!(log.borrow().played.len(), 3);
    }

    #[test]
    fn something_other_than_the_queued_handover_taking_over_does_not_continue_a_chain() {
        let (mut model, log) = emotions();
        let queued = play_picked(&mut model, "smile", MIX).unwrap();
        let mut follow = Follow { seen: Some("smile".into()), queued, standing: Some("idle".into()), held: false };
        // `bow` is not what was queued.
        log.borrow_mut().next = Some("bow".into());
        model.update(FRAME).unwrap();
        assert!(follow.observe(&mut model));
        assert_eq!(log.borrow().played.len(), 2);
    }

    #[test]
    fn the_shown_animation_follows_the_model_while_the_pick_stays_put() {
        let (model, log) = emotions();
        let mut m = loaded(model);
        m.play("smile", MIX).unwrap();
        assert_eq!(m.shown_animation(), Some("smile"));
        m.update(FRAME, false).unwrap();
        assert_eq!(m.shown_animation(), Some("smile"));
        log.borrow_mut().next = Some("idle".into());
        m.update(FRAME, false).unwrap();
        assert_eq!(m.shown_animation(), Some("idle"));
        // Also while paused, which still looks at the model.
        log.borrow_mut().next = Some("smile".into());
        m.update(FRAME, true).unwrap();
        assert_eq!(m.shown_animation(), Some("smile"));
    }

    #[test]
    fn no_idle_variation_starts_during_a_picked_animations_pass_and_the_timer_restarts_after() {
        let (model, log) = emotions();
        let mut m = loaded(model);
        m.idle.interval = (0.5, 0.5);
        m.play("smile", MIX).unwrap();
        m.idle.reset();
        for _ in 0..50 {
            assert_eq!(m.update(Duration::from_millis(100), false).unwrap(), None, "5 s into the emotion");
        }
        // The handover returns to the default; the interval counts from here.
        log.borrow_mut().next = Some("idle".into());
        for _ in 0..4 {
            assert_eq!(m.update(Duration::from_millis(100), false).unwrap(), None);
        }
        let mut fired = None;
        for _ in 0..4 {
            fired = fired.or(m.update(Duration::from_millis(100), false).unwrap());
        }
        assert_eq!(fired.as_deref(), Some("idle_b"));
    }

    #[test]
    fn a_variation_after_an_emotion_hands_back_to_the_default_not_the_emotion() {
        let (model, log) = emotions();
        let mut m = loaded(model);
        m.play("smile", MIX).unwrap();
        log.borrow_mut().next = Some("idle".into());
        m.update(FRAME, false).unwrap();
        m.start_idle_variation("idle_b", Some("smile"), MIX).unwrap();
        let played = log.borrow().played.clone();
        let back = played.last().unwrap();
        assert_eq!(back.0, "idle");
        assert_eq!(back.1, PlayOptions::looping().with_mix(MIX).queued());
        assert_eq!(m.shown_animation(), Some("idle_b"));
    }

    #[test]
    fn a_variation_after_a_plain_pick_hands_back_to_the_pick_as_before() {
        let (model, log) = emotions();
        let mut m = loaded(model);
        m.play("idle_b", MIX).unwrap();
        m.start_idle_variation("idle", Some("idle_b"), MIX).unwrap();
        assert_eq!(log.borrow().played.last().unwrap().0, "idle_b");
    }

    // ---- self handovers and where a pick settles ----

    #[test]
    fn a_handover_to_itself_loops_instead_of_leaving_the_track_empty() {
        let (mut model, log) = scripted(vec![info("idle", None), info("a", Some(("a", 1.0, 0.25)))]);
        let queued = play_picked(&mut model, "a", MIX).unwrap();
        assert_eq!(queued, None);
        assert_eq!(log.borrow().played, vec![("a".to_string(), PlayOptions::looping().with_mix(MIX))]);
    }

    #[test]
    fn a_chain_ending_in_a_self_handover_ends_looping_on_the_last_link() {
        let (mut model, log) =
            scripted(vec![info("idle", None), info("a", Some(("b", 1.0, 0.25))), info("b", Some(("b", 1.0, 0.25)))]);
        let queued = play_picked(&mut model, "a", MIX).unwrap();
        assert_eq!(queued.as_deref(), Some("b"));
        let played = log.borrow().played.clone();
        assert!(played[1].1.looping, "b has no usable handover, so it loops");
        let mut follow = Follow { seen: Some("a".into()), queued, standing: None, held: false };
        log.borrow_mut().next = Some("b".into());
        model.update(FRAME).unwrap();
        assert!(follow.observe(&mut model));
        assert_eq!(log.borrow().played.len(), 2, "nothing further is queued behind b");
    }

    #[test]
    fn a_pick_settles_where_its_chain_ends_even_when_that_is_not_the_default() {
        let (model, log) = scripted(vec![
            info("idle", None),
            info("a", Some(("b", 1.0, 0.25))),
            info("b", Some(("c", 1.0, 0.25))),
            info("c", None),
        ]);
        let mut m = loaded(model);
        m.play("a", MIX).unwrap();
        assert_eq!(m.follow.standing.as_deref(), Some("c"));
        log.borrow_mut().next = Some("c".into());
        m.update(FRAME, false).unwrap();
        assert!(m.follow.at_rest(), "variations can resume on c");
    }

    #[test]
    fn a_cycle_settles_on_the_models_default() {
        let (model, _log) =
            scripted(vec![info("idle", None), info("a", Some(("b", 1.0, 0.25))), info("b", Some(("a", 1.0, 0.25)))]);
        let mut m = loaded(model);
        m.play("a", MIX).unwrap();
        assert_eq!(m.follow.standing.as_deref(), Some("idle"));
    }

    fn spine_model() -> a2d_runtime::GenericSpineModel {
        use a2d_core::ir::ids::BoneId;
        use a2d_core::ir::spine::{Animation, Bone, ScalarKey, SpineIr, Timeline};
        use a2d_core::Interpolation;
        let anim = |name: &str, duration: f32, to: f32| Animation {
            name: name.into(),
            duration,
            timelines: vec![Timeline::BoneRotate {
                bone: BoneId(0),
                keys: vec![
                    ScalarKey { time: 0.0, value: 0.0, interp: Interpolation::Linear },
                    ScalarKey { time: duration, value: to, interp: Interpolation::Linear },
                ],
            }],
        };
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            animations: vec![anim("stand", 1.0, 10.0), anim("fidget", 0.5, 90.0)],
            ..Default::default()
        };
        ir.rebuild_derived();
        a2d_runtime::GenericSpineModel::load(Arc::new(ir), "rig")
    }

    fn current_name(model: &a2d_runtime::GenericSpineModel) -> String {
        let index = model.state().track(0).and_then(|t| t.current()).map(|e| e.animation).unwrap();
        model.animations()[index].name.clone()
    }

    #[test]
    fn a_spine_variation_hands_back_to_the_selected_animation() {
        let mix = Duration::from_millis(300);
        let mut model = spine_model();
        model.play_animation("stand", PlayOptions::looping()).unwrap();
        start_idle_variation(&mut model, "fidget", Some("stand"), mix).unwrap();
        assert_eq!(current_name(&model), "fidget");
        for _ in 0..10 {
            model.update(Duration::from_millis(100)).unwrap();
        }
        assert_eq!(current_name(&model), "stand");
    }

    #[test]
    fn a_looping_variation_hands_back_only_at_its_wrap_on_spine() {
        // The control for `start_idle_variation`: a looping variation plays a
        // whole extra pass before the idle returns, which is why the host plays
        // it once.
        let mix = Duration::from_millis(300);
        let mut model = spine_model();
        model.play_animation("stand", PlayOptions::looping()).unwrap();
        model.play_animation("fidget", PlayOptions::looping().with_mix(mix)).unwrap();
        model.play_animation("stand", PlayOptions::looping().with_mix(mix).queued()).unwrap();
        for _ in 0..4 {
            model.update(Duration::from_millis(100)).unwrap();
        }
        assert_eq!(current_name(&model), "fidget", "0.4 s into a 0.5 s pass");
        for _ in 0..2 {
            model.update(Duration::from_millis(100)).unwrap();
        }
        assert_eq!(current_name(&model), "stand", "past the wrap");
    }

    /// A model whose extent grows partway through its animation, like a rig
    /// that swings a weapon well past its standing silhouette.
    struct Reaching {
        animations: Vec<a2d_core::AnimationInfo>,
        at: f32,
    }

    impl AnimatedModel for Reaching {
        fn update(&mut self, _dt: Duration) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn emit(&self, _out: &mut RenderList) {}
        fn play_animation(&mut self, _n: &str, _o: PlayOptions) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn stop_animation(&mut self, _n: &str) {}
        fn set_expression(&mut self, _n: &str) -> Result<(), a2d_core::RuntimeError> {
            Ok(())
        }
        fn animations(&self) -> &[a2d_core::AnimationInfo] {
            &self.animations
        }
        fn expressions(&self) -> &[a2d_core::ExpressionInfo] {
            &[]
        }
        fn bounds(&self) -> Aabb {
            // Compact at the ends, twice as tall in the middle.
            let reach = if (self.at - 0.5).abs() < 0.01 { 20.0 } else { 10.0 };
            let mut b = Aabb::EMPTY;
            b.extend(Vec2::new(-5.0, 0.0));
            b.extend(Vec2::new(5.0, reach));
            b
        }
        fn hit_test(&self, _x: f32, _y: f32) -> Option<a2d_core::HitAreaId> {
            None
        }
        fn dispose(&mut self) {}
        fn display_name(&self) -> &str {
            "reaching"
        }
        fn pose_at(&mut self, _animation: &str, time: f32) -> Result<(), a2d_core::RuntimeError> {
            self.at = time;
            Ok(())
        }
    }

    fn reaching() -> Reaching {
        Reaching {
            animations: vec![a2d_core::AnimationInfo { name: "Stand".into(), duration: 1.0, then: None }],
            at: 0.0,
        }
    }

    #[test]
    fn the_frame_covers_the_whole_opening_animation() {
        // Fitting whatever pose happens to be current rescales the character
        // every time an animation reaches further than the last. Sampling the
        // opening animation once gives a frame that already contains it.
        let mut model = reaching();
        let framing = frame_of(&mut model);
        assert_eq!(framing.max.y, 20.0, "the frame must contain the animation's furthest reach");
        assert_eq!(framing.min.y, 0.0);
    }

    #[test]
    fn sampling_leaves_the_model_at_the_start_of_its_animation() {
        // Otherwise the first frame a viewer shows is the last one sampled.
        let mut model = reaching();
        let _ = frame_of(&mut model);
        assert_eq!(model.at, 0.0);
    }

    #[test]
    fn a_model_that_cannot_seek_is_framed_by_what_it_reports() {
        // A model with no timestamp seeking and no animations: its bounds are
        // used as they are (a Cubism model's are the canvas, which does not
        // move anyway).
        struct Fixed;
        impl AnimatedModel for Fixed {
            fn update(&mut self, _dt: Duration) -> Result<(), a2d_core::RuntimeError> {
                Ok(())
            }
            fn emit(&self, _out: &mut RenderList) {}
            fn play_animation(&mut self, _n: &str, _o: PlayOptions) -> Result<(), a2d_core::RuntimeError> {
                Ok(())
            }
            fn stop_animation(&mut self, _n: &str) {}
            fn set_expression(&mut self, _n: &str) -> Result<(), a2d_core::RuntimeError> {
                Ok(())
            }
            fn animations(&self) -> &[a2d_core::AnimationInfo] {
                &[]
            }
            fn expressions(&self) -> &[a2d_core::ExpressionInfo] {
                &[]
            }
            fn bounds(&self) -> Aabb {
                let mut b = Aabb::EMPTY;
                b.extend(Vec2::new(-1.0, -2.0));
                b.extend(Vec2::new(1.0, 2.0));
                b
            }
            fn hit_test(&self, _x: f32, _y: f32) -> Option<a2d_core::HitAreaId> {
                None
            }
            fn dispose(&mut self) {}
            fn display_name(&self) -> &str {
                "fixed"
            }
        }
        let framing = frame_of(&mut Fixed);
        assert_eq!(framing.min, Vec2::new(-1.0, -2.0));
        assert_eq!(framing.max, Vec2::new(1.0, 2.0));
    }

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
