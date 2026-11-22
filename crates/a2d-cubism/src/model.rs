//! [`GenericCubismModel`]: a MOC3 behind the shared [`AnimatedModel`] interface.
//!
//! This is what lets one viewer show both runtime families (spec §5). The
//! renderer never learns which it is drawing; it receives triangles either way.
//!
//! # What this can and cannot do yet
//!
//! It poses, draws, and plays the motions the model is given
//! ([`CubismIr::motions`]): [`AnimatedModel::animations`] lists them and
//! [`AnimatedModel::play_animation`] starts one by name, and
//! [`AnimatedModel::pose_at`] poses the model at a timestamp. A model built
//! straight from a MOC3 has none; one built from an imported package does.
//! Crossfades and per-curve fades are not applied, and part-opacity targets are
//! not driven; only parameter curves are.
//!
//! A model's physics rig, when it has one, runs after the motion (or idle) has
//! written its values and before the pose is computed, every update. Its state
//! survives a change of motion. [`AnimatedModel::pose_at`] replays it from rest
//! so that a pose stays a function of the timestamp alone.
//!
//! When no motion is playing, [`AnimatedModel::update`] drives a small idle of
//! its own, from Cubism's conventional parameter names. That is **synthetic
//! motion**, not the character's: it exists so a viewer shows something alive
//! and so the deformation path is exercised continuously rather than only at
//! rest. It is skipped entirely for any parameter the model does not declare,
//! and while a motion plays, so the two never fight over a parameter.

use std::time::Duration;

use a2d_core::{
    preferred_animation, Aabb, AnimatedModel, AnimationInfo, ExpressionInfo, HitAreaId, PlayOptions, RenderList,
    RuntimeError, TextureId,
};

use crate::emit::CubismEmit;
use crate::eval::{CubismEval, Pose};
use crate::moc3::CubismIr;
use crate::motion::{wrap_time, MotionPlayer};
use crate::physics::PhysicsState;

/// Cubism's conventional parameter names, and how the idle moves each.
///
/// Every rigged model declares these; a model that does not simply keeps them
/// at rest. The periods are deliberately unrelated so the motion does not fall
/// into a visible loop.
const IDLE: &[(&str, f32, f32)] = &[
    // (parameter, period in seconds, amplitude as a fraction of its range)
    ("ParamAngleX", 6.1, 0.22),
    ("ParamAngleY", 8.7, 0.16),
    ("ParamAngleZ", 11.3, 0.12),
    ("ParamBodyAngleX", 9.5, 0.18),
    ("ParamBreath", 3.4, 1.0),
];

/// The frame rate [`AnimatedModel::pose_at`] replays physics at.
const POSE_AT_FPS: f32 = 60.0;

/// How much of the motion before the requested time `pose_at` replays the
/// physics over, in seconds.
const POSE_AT_WARM_UP: f32 = 10.0;

/// A Cubism model, posed and drawable.
pub struct GenericCubismModel {
    /// The normalized model. A MOC3 is one way to obtain it and a package is
    /// another, and nothing here can tell which it was.
    moc: CubismIr,
    name: String,
    /// One value per parameter, in the model's own order.
    values: Vec<f32>,
    pose: Pose,
    elapsed: f32,
    /// Whether the idle drives anything, so a model without the conventional
    /// parameters is left alone rather than jittered.
    idle: Vec<(usize, f32, f32, f32, f32)>,
    animations: Vec<AnimationInfo>,
    /// The motion currently playing, if any. While set it owns the parameters it
    /// drives and the synthetic idle stays off.
    motion: Option<MotionPlayer>,
    /// A motion queued behind the current one, as `(index, looping, speed)`. It
    /// takes over when the current motion finishes, or at the next loop wrap of
    /// a looping one. A newer queued call replaces it.
    pending: Option<(usize, bool, f32)>,
    expressions: Vec<ExpressionInfo>,
    texture: TextureId,
    /// The physics simulation, if the model has a rig with anything to simulate.
    physics: Option<PhysicsState>,
}

impl GenericCubismModel {
    /// Wraps a parsed model.
    pub fn load(moc: impl Into<CubismIr>, display_name: impl Into<String>) -> GenericCubismModel {
        let moc = moc.into();
        let values: Vec<f32> = moc.parameters.iter().map(|p| p.default).collect();

        // Resolve the idle against this model's own parameters, once.
        let mut idle = Vec::new();
        for (name, period, amount) in IDLE {
            let Some(index) = moc.parameters.iter().position(|p| p.id == *name) else { continue };
            let p = &moc.parameters[index];
            let span = (p.maximum - p.minimum) * 0.5 * amount;
            if span > f32::EPSILON {
                idle.push((index, *period, span, p.default, p.default));
            }
        }

        let animations: Vec<AnimationInfo> =
            moc.motions.iter().map(|m| AnimationInfo { name: m.name.clone(), duration: m.duration }).collect();
        let pose = moc.pose(&values);
        let physics = moc.physics.as_ref().and_then(|p| PhysicsState::new(p, &moc.parameters));
        let mut model = GenericCubismModel {
            moc,
            name: display_name.into(),
            values,
            pose,
            elapsed: 0.0,
            idle,
            animations,
            motion: None,
            pending: None,
            expressions: Vec::new(),
            texture: TextureId(0),
            physics,
        };
        // Open on the same kind of animation the Spine side does, rather than a
        // second rule for what a standing pose is called.
        if let Some(index) =
            preferred_animation(&model.animations).and_then(|name| model.animations.iter().position(|a| a.name == name))
        {
            model.start_motion(index, true, 1.0);
        }
        model
    }

    /// Puts every parameter back to its default.
    ///
    /// Whatever drives a parameter drives it afresh each update, so a parameter
    /// nothing drives any more (a finished or replaced motion) returns to rest
    /// instead of freezing at its last value.
    fn reset_values(&mut self) {
        for (v, p) in self.values.iter_mut().zip(&self.moc.parameters) {
            *v = p.default;
        }
    }

    fn start_motion(&mut self, index: usize, looping: bool, speed: f32) {
        self.motion = MotionPlayer::new(&self.moc, index, looping, speed);
        // Pose the first frame now, so the model does not open at rest for one
        // update before the motion takes over.
        self.reset_values();
        if let Some(player) = &self.motion {
            player.apply(&self.moc, &mut self.values);
        }
        // Physics outputs stay where the chain is rather than showing the new
        // motion's raw values for a frame (a paused step writes without moving).
        self.step_physics(0.0);
        self.repose();
    }

    pub fn moc(&self) -> &CubismIr {
        &self.moc
    }

    /// The current pose, for callers that want the geometry rather than a draw.
    pub fn pose(&self) -> &Pose {
        &self.pose
    }

    /// Sets a parameter by name, clamped to its own range.
    ///
    /// Returns whether the model has such a parameter, so a caller can tell a
    /// typo from a value that simply had no visible effect.
    ///
    /// The value lasts only until the next [`AnimatedModel::update`], which
    /// rebuilds every parameter from its default and whatever is driving it.
    pub fn set_parameter(&mut self, id: &str, value: f32) -> bool {
        let Some(index) = self.moc.parameters.iter().position(|p| p.id == id) else { return false };
        self.values[index] = self.moc.parameters[index].clamp(value);
        true
    }

    /// A parameter's current value by name, as the last update left it.
    pub fn parameter_value(&self, id: &str) -> Option<f32> {
        let index = self.moc.parameters.iter().position(|p| p.id == id)?;
        self.values.get(index).copied()
    }

    /// Which texture page the drawables sample.
    pub fn set_texture(&mut self, texture: TextureId) {
        self.texture = texture;
    }

    /// Drawables whose deformer chain produced nothing usable.
    pub fn unstable(&self) -> &[usize] {
        &self.pose.unstable
    }

    /// Advances the physics by `dt` seconds over the values the motion or idle
    /// just wrote. Does nothing for a model without a rig.
    fn step_physics(&mut self, dt: f32) {
        if let Some(physics) = &mut self.physics {
            physics.step(dt, &mut self.values);
        }
    }

    fn repose(&mut self) {
        self.pose = self.moc.pose(&self.values);
    }
}

impl AnimatedModel for GenericCubismModel {
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError> {
        // A long step means the host was blocked or asleep; carrying it through
        // would jump the idle rather than advance it.
        let step = dt.as_secs_f32().min(0.1);
        self.elapsed += step;
        let mut leftover = None;
        if let Some(player) = &mut self.motion {
            leftover = player.advance(&self.moc, step);
        }
        if let Some(over) = leftover {
            if let Some((index, looping, speed)) = self.pending.take() {
                self.start_motion(index, looping, speed);
                // Carry the time that was past the old pass's end into the new
                // one, so the handover does not stall for the overshoot.
                if let Some(player) = &mut self.motion {
                    player.advance(&self.moc, over);
                }
            }
        }
        if self.motion.as_ref().is_some_and(|p| p.is_finished()) {
            // A motion that does not loop leaves the track empty (see
            // `PlayOptions::once`) once its final frame has been shown.
            let player = self.motion.take();
            self.reset_values();
            if let Some(player) = player {
                player.apply(&self.moc, &mut self.values);
            }
            self.step_physics(step);
            self.repose();
            return Ok(());
        }

        if self.motion.is_none() && self.idle.is_empty() && self.physics.is_none() {
            // Nothing drives anything: only repose if something was left off rest.
            if self.values.iter().zip(&self.moc.parameters).any(|(v, p)| *v != p.default) {
                self.reset_values();
                self.repose();
            }
            return Ok(());
        }
        self.reset_values();
        if let Some(player) = &self.motion {
            // The motion owns its parameters, and the idle stays off so the two
            // never fight over one.
            player.apply(&self.moc, &mut self.values);
        } else {
            for (index, period, span, rest, _) in &self.idle {
                let phase = self.elapsed / period * std::f32::consts::TAU;
                let value = rest + phase.sin() * span;
                if let Some(p) = self.moc.parameters.get(*index) {
                    self.values[*index] = p.clamp(value);
                }
            }
        }
        self.step_physics(step);
        self.repose();
        Ok(())
    }

    fn emit(&self, out: &mut RenderList) {
        self.moc.emit(&self.pose, self.texture, out);
    }

    /// Only `looping`, `speed` and `queued` are honoured: there is one track,
    /// and crossfade and delay are not implemented.
    ///
    /// A queued call while a motion plays waits behind it, starting when a
    /// non-looping motion finishes or a looping one next wraps, and replaces any
    /// older queued entry. With nothing playing it starts at once. A call that
    /// is not queued discards whatever was waiting.
    fn play_animation(&mut self, name: &str, opts: PlayOptions) -> Result<(), RuntimeError> {
        let index = self
            .animations
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| RuntimeError::UnknownAnimation(name.to_string()))?;
        if opts.queued && self.motion.is_some() {
            self.pending = Some((index, opts.looping, opts.speed));
        } else {
            self.pending = None;
            self.start_motion(index, opts.looping, opts.speed);
        }
        Ok(())
    }

    /// Poses the model `time` seconds into `animation`, from the parameter
    /// defaults, so the result does not depend on what played before.
    ///
    /// `time` is clamped to the motion (not wrapped), matching the Spine side:
    /// asking for the end shows the final frame. Afterwards the motion is the
    /// current one, looping, with its playhead at `time`; anything queued is
    /// dropped. The synthetic idle is not applied.
    fn pose_at(&mut self, animation: &str, time: f32) -> Result<(), RuntimeError> {
        let index = self
            .animations
            .iter()
            .position(|a| a.name == animation)
            .ok_or_else(|| RuntimeError::UnknownAnimation(animation.to_string()))?;
        self.pending = None;
        self.motion = MotionPlayer::new(&self.moc, index, true, 1.0);
        let duration = self.moc.motions.get(index).map_or(0.0, |m| m.duration);
        let target = wrap_time(time, duration, false);
        // Physics is replayed from rest in fixed frames, sampling the motion at
        // each, so the pose depends on the timestamp alone. Only the last
        // `POSE_AT_WARM_UP` seconds are replayed, which bounds the cost however
        // long the motion is; a chain has all but settled by then. At least one
        // frame, so time zero shows the first frame of playback rather than a
        // rig that has never moved.
        let start = (target - POSE_AT_WARM_UP).max(0.0);
        let frames = if self.physics.is_some() { (((target - start) * POSE_AT_FPS).ceil() as u32).max(1) } else { 0 };
        if let Some(physics) = &mut self.physics {
            physics.reset();
        }
        for frame in 1..=frames {
            let at = (start + frame as f32 / POSE_AT_FPS).min(target);
            if let Some(player) = &mut self.motion {
                player.seek(&self.moc, at);
            }
            self.reset_values();
            if let Some(player) = &self.motion {
                player.apply(&self.moc, &mut self.values);
            }
            self.step_physics(1.0 / POSE_AT_FPS);
        }
        if let Some(player) = &mut self.motion {
            player.seek(&self.moc, target);
        }
        if frames == 0 {
            self.reset_values();
            if let Some(player) = &self.motion {
                player.apply(&self.moc, &mut self.values);
            }
        }
        self.repose();
        Ok(())
    }

    fn stop_animation(&mut self, name: &str) {
        let is = |i: usize| self.animations.get(i).is_some_and(|a| a.name == name);
        if self.pending.is_some_and(|(i, _, _)| is(i)) {
            self.pending = None;
        }
        if self.motion.as_ref().is_some_and(|p| is(p.motion_index())) {
            self.motion = None;
            self.reset_values();
            self.step_physics(0.0);
            self.repose();
        }
    }

    fn set_expression(&mut self, name: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::UnknownExpression(format!("{name:?}: Cubism expressions are not decoded yet")))
    }

    fn animations(&self) -> &[AnimationInfo] {
        &self.animations
    }

    fn expressions(&self) -> &[ExpressionInfo] {
        &self.expressions
    }

    fn bounds(&self) -> Aabb {
        let mut out = Aabb::EMPTY;
        // The canvas, not the geometry: a model may reach past its frame, and a
        // viewer that fits the geometry would then jitter as the pose moves.
        let canvas = self.moc.canvas;
        if canvas.pixels_per_unit > 0.0 {
            let (w, h) = (canvas.size.0 / canvas.pixels_per_unit, canvas.size.1 / canvas.pixels_per_unit);
            out.extend(a2d_core::Vec2::new(-w * 0.5, -h * 0.5));
            out.extend(a2d_core::Vec2::new(w * 0.5, h * 0.5));
        }
        out
    }

    fn hit_test(&self, _x: f32, _y: f32) -> Option<HitAreaId> {
        // Cubism hit areas are named drawables listed in the model settings,
        // which live outside the MOC3 and are not read yet.
        None
    }

    fn dispose(&mut self) {
        self.pose = Pose::default();
        self.values.clear();
        self.idle.clear();
        self.motion = None;
        self.pending = None;
        self.physics = None;
    }

    fn display_name(&self) -> &str {
        &self.name
    }

    fn absorb_degradations(&self, report: &mut a2d_core::LoadReport) {
        // A curve aimed at a parameter the model lacks is not applied; say so
        // rather than letting the motion play with a part of it missing.
        for degradation in self.moc.unresolved_motion_targets() {
            report.warn(degradation);
        }
        // The same for a physics input or output: the rig runs without it.
        for degradation in self.moc.unresolved_physics_targets() {
            report.warn(degradation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Moc3;

    fn model() -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        GenericCubismModel::load(Moc3::parse(&bytes).expect("should parse"), "test")
    }

    #[test]
    fn a_model_poses_at_its_defaults_on_load() {
        let m = model();
        assert_eq!(m.display_name(), "test");
        assert_eq!(m.pose().drawables.len(), m.moc().drawables.len());
        assert!(m.unstable().is_empty());
    }

    #[test]
    fn emitting_produces_one_mesh_per_drawable() {
        let m = model();
        let mut list = RenderList::new();
        m.emit(&mut list);
        assert_eq!(list.meshes().len(), m.moc().drawables.len());
    }

    #[test]
    fn bounds_come_from_the_canvas_rather_than_the_geometry() {
        // A pose that moves must not move the frame, or a viewer fitted to it
        // would drift as the model breathes.
        let mut m = model();
        let before = m.bounds();
        m.update(Duration::from_millis(500)).expect("update should not fail");
        assert_eq!(m.bounds(), before);
        assert!(!before.is_empty());
    }

    #[test]
    fn setting_a_parameter_reports_whether_it_exists() {
        let mut m = model();
        assert!(m.set_parameter("ParamAngleX", 10.0));
        assert!(!m.set_parameter("NoSuchParameter", 1.0), "a typo must be distinguishable from a no-op");
    }

    #[test]
    fn a_parameter_is_clamped_to_its_own_range() {
        let mut m = model();
        m.set_parameter("ParamAngleX", 9999.0);
        let index = m.moc().parameters.iter().position(|p| p.id == "ParamAngleX").expect("present");
        assert_eq!(m.values[index], m.moc().parameters[index].maximum);
    }

    #[test]
    fn a_long_step_does_not_jump_the_idle() {
        let mut m = model();
        m.update(Duration::from_secs(600)).expect("update should not fail");
        assert!(m.elapsed <= 0.1, "a blocked host must not teleport the idle: {}", m.elapsed);
    }

    use a2d_core::ir::cubism::{CurveKey, Motion, MotionCurve};

    fn k(time: f32, value: f32, slope: f32) -> CurveKey {
        CurveKey { time, value, in_slope: slope, out_slope: slope, stepped: false }
    }

    fn with_motions(motions: Vec<Motion>) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.motions = motions;
        GenericCubismModel::load(ir, "test")
    }

    /// A linear motion driving `target` from 0 to `to` over two seconds.
    fn sweep(name: &str, target: &str, to: f32) -> Motion {
        Motion {
            name: name.into(),
            duration: 2.0,
            curves: vec![MotionCurve {
                target: target.into(),
                keys: vec![k(0.0, 0.0, to / 2.0), k(2.0, to, to / 2.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    fn value(m: &GenericCubismModel, id: &str) -> f32 {
        let index = m.moc().parameters.iter().position(|p| p.id == id).expect("present");
        m.values[index]
    }

    fn max_of(m: &GenericCubismModel, id: &str) -> f32 {
        m.moc().parameter(id).expect("present").maximum
    }

    #[test]
    fn an_unknown_animation_is_an_error() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        let err = m.play_animation("Nope", PlayOptions::looping()).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownAnimation(_)), "{err}");
        assert!(model().animations().is_empty());
    }

    #[test]
    fn motions_are_listed_and_the_preferred_one_starts_on_load() {
        let m = with_motions(vec![sweep("Wave", "ParamAngleX", 5.0), sweep("Idle", "ParamAngleX", 20.0)]);
        let names: Vec<_> = m.animations().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Wave", "Idle"]);
        assert_eq!(m.animations()[1].duration, 2.0);
        assert_eq!(m.motion.as_ref().map(|p| p.motion_index()), Some(1), "Idle is preferred over Wave");
    }

    #[test]
    fn an_update_applies_the_motion_to_its_parameter() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        m.play_animation("Idle", PlayOptions::looping()).expect("known");
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        // One second in, linear 0 -> 20 over two seconds is 10, unless the
        // parameter's own range is narrower.
        let want = 10.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn speed_scales_the_playhead() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        m.play_animation("Idle", PlayOptions::looping().with_speed(2.0)).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        // 0.2 s of motion time is 2.0.
        let want = 2.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3);
    }

    #[test]
    fn a_value_outside_the_range_is_clamped() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 1.0e6)]);
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamAngleX"), max_of(&m, "ParamAngleX"));
    }

    #[test]
    fn an_unresolved_target_is_reported_and_not_applied() {
        let mut m = with_motions(vec![sweep("Idle", "ParamNoSuchThing", 20.0)]);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert_eq!(report.len(), 1);
        let text = report.warnings()[0].to_string();
        assert!(text.contains("ParamNoSuchThing") && text.contains("Idle"), "{text}");
        // The motion is playing, so the idle is off and nothing else may move.
        let before = m.values.clone();
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(m.values, before);
    }

    #[test]
    fn the_synthetic_idle_stays_off_while_a_motion_plays() {
        // The motion holds ParamAngleX constant; the idle would swing it.
        let constant = Motion {
            name: "Idle".into(),
            duration: 2.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, 3.0, 0.0), k(2.0, 3.0, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let mut m = with_motions(vec![constant]);
        for _ in 0..30 {
            m.update(Duration::from_millis(100)).expect("update");
            assert!((value(&m, "ParamAngleX") - 3.0).abs() < 1e-6);
        }
        // Once stopped, the idle takes the parameter back.
        m.stop_animation("Idle");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamAngleX") - 3.0).abs() > 1e-3);
    }

    #[test]
    fn a_motion_that_does_not_loop_ends_on_its_last_frame() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 1.0)]);
        m.stop_animation("Other");
        assert!(m.motion.is_some(), "stopping an unknown name is a no-op");
        m.play_animation("Idle", PlayOptions::once()).expect("known");
        // Stop at the update that ends it: after that the idle owns the
        // parameter again.
        let mut steps = 0;
        while m.motion.is_some() && steps < 100 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        assert!(m.motion.is_none());
        let want = 1.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3);
    }

    #[test]
    fn a_model_without_motions_has_none_playing() {
        let m = model();
        assert!(m.motion.is_none());
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert!(report.is_empty());
    }

    /// Three parameters: the test model's two, plus one the idle never drives.
    fn with_three(motions: Vec<Motion>) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameters.push(crate::moc3::Parameter {
            id: "ParamExtra".into(),
            minimum: 0.0,
            maximum: 10.0,
            default: 2.0,
        });
        ir.motions = motions;
        GenericCubismModel::load(ir, "test")
    }

    fn hold(name: &str, target: &str, v: f32) -> Motion {
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: target.into(),
                keys: vec![k(0.0, v, 0.0), k(1.0, v, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    #[test]
    fn switching_motions_returns_the_old_ones_parameters_to_rest() {
        let mut m = with_three(vec![hold("A", "ParamEyeLOpen", 0.25), hold("B", "ParamExtra", 7.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamEyeLOpen") - 0.25).abs() < 1e-6);
        m.play_animation("B", PlayOptions::looping()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamExtra") - 7.0).abs() < 1e-6);
        let eye_default = m.moc().parameter("ParamEyeLOpen").expect("present").default;
        assert_eq!(value(&m, "ParamEyeLOpen"), eye_default, "A's parameter must not stay frozen");
    }

    #[test]
    fn a_finished_one_shot_returns_its_parameters_to_their_defaults() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 7.0)]);
        m.play_animation("A", PlayOptions::once()).expect("known");
        let mut steps = 0;
        while m.motion.is_some() && steps < 100 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 2.0);
    }

    #[test]
    fn stopping_a_motion_returns_its_parameters_to_their_defaults() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 7.0)]);
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 7.0);
        m.stop_animation("A");
        assert_eq!(value(&m, "ParamExtra"), 2.0);
    }

    #[test]
    fn a_queued_idle_waits_for_the_variation_to_finish() {
        // The viewer's sequence: a variation once, then the idle queued in the
        // same frame.
        let mut m = with_three(vec![hold("Idle", "ParamExtra", 5.0), hold("Wave", "ParamExtra", 9.0)]);
        m.play_animation("Wave", PlayOptions::once()).expect("known");
        m.play_animation("Idle", PlayOptions::looping().queued()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 9.0, "the variation must be seen first");
        for _ in 0..8 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 9.0, "still inside its one second");
        for _ in 0..3 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 5.0, "then the idle takes over");
        assert!(m.pending.is_none());
    }

    #[test]
    fn a_queued_call_with_nothing_playing_starts_at_once_and_a_plain_call_clears_the_queue() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 5.0), hold("B", "ParamExtra", 9.0)]);
        m.stop_animation("A");
        assert!(m.motion.is_none());
        m.play_animation("A", PlayOptions::once().queued()).expect("known");
        assert!(m.motion.is_some() && m.pending.is_none());
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        assert!(m.pending.is_some());
        m.play_animation("B", PlayOptions::looping()).expect("known");
        assert!(m.pending.is_none());
    }

    #[test]
    fn a_queued_motion_waits_out_a_full_backwards_pass() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 5.0), hold("B", "ParamExtra", 9.0)]);
        m.play_animation("A", PlayOptions::looping().with_speed(-1.0)).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        m.update(Duration::ZERO).expect("update");
        assert!(m.pending.is_some(), "a zero step hands nothing over");
        // Nine 0.1 s steps is 0.9 s of a 1 s pass: still A.
        for _ in 0..9 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 5.0);
        for _ in 0..2 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 9.0);
    }

    #[test]
    fn a_handover_carries_the_overshoot_into_the_new_motion() {
        let mut a = hold("A", "ParamExtra", 5.0);
        a.duration = 0.95;
        let mut m = with_three(vec![a, hold("B", "ParamExtra", 9.0)]);
        m.play_animation("A", PlayOptions::once()).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        let player = m.motion.as_ref().expect("B is playing");
        assert_eq!(player.motion_index(), 1);
        // Ten steps is 1.0 s; A ended at 0.95 s, so B is 0.05 s in.
        assert!((player.playhead() - 0.05).abs() < 1e-3, "{}", player.playhead());
    }

    #[test]
    fn pose_at_samples_the_motion_at_a_timestamp() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        m.pose_at("S", 1.0).expect("known");
        // Linear 0 -> 8 over two seconds, one second in.
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        m.pose_at("S", 0.25).expect("known");
        assert!((value(&m, "ParamExtra") - 1.0).abs() < 1e-4);
    }

    #[test]
    fn pose_at_does_not_depend_on_what_happened_before() {
        let motions = vec![sweep("S", "ParamExtra", 8.0), hold("H", "ParamEyeLOpen", 0.25)];
        let mut fresh = with_three(motions.clone());
        fresh.pose_at("S", 0.5).expect("known");

        let mut used = with_three(motions);
        used.play_animation("H", PlayOptions::once()).expect("known");
        used.play_animation("S", PlayOptions::looping().queued()).expect("known");
        for _ in 0..7 {
            used.update(Duration::from_millis(130)).expect("update");
        }
        used.set_parameter("ParamAngleX", 5.0);
        used.pose_at("S", 0.5).expect("known");

        assert_eq!(used.values, fresh.values);
        assert_eq!(used.pose().drawables.len(), fresh.pose().drawables.len());
        let (mut a, mut b) = (RenderList::new(), RenderList::new());
        used.emit(&mut a);
        fresh.emit(&mut b);
        assert_eq!(a.meshes().len(), b.meshes().len());
        for (x, y) in a.meshes().iter().zip(b.meshes()) {
            assert_eq!(x.vertices, y.vertices);
        }
        // The other motion's parameter is back at rest, not left where it was.
        let eye_default = used.moc().parameter("ParamEyeLOpen").expect("present").default;
        assert_eq!(value(&used, "ParamEyeLOpen"), eye_default);
    }

    #[test]
    fn pose_at_clamps_beyond_the_ends() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        m.pose_at("S", 99.0).expect("known");
        assert!((value(&m, "ParamExtra") - 8.0).abs() < 1e-4, "the final frame, not a wrap to the start");
        m.pose_at("S", -3.0).expect("known");
        assert_eq!(value(&m, "ParamExtra"), 0.0);
        m.pose_at("S", f32::NAN).expect("known");
        assert_eq!(value(&m, "ParamExtra"), 0.0);
    }

    #[test]
    fn pose_at_an_unknown_animation_is_an_error() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        let err = m.pose_at("Nope", 0.0).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownAnimation(_)), "{err}");
        assert!(matches!(model().pose_at("S", 0.0), Err(RuntimeError::UnknownAnimation(_))));
    }

    // ---- physics ----

    use a2d_core::ir::cubism::{
        NormRange, Physics, PhysicsComponent, PhysicsGroup, PhysicsInput, PhysicsNormalization, PhysicsOutput,
        PhysicsParticle,
    };

    /// A rig where `input` (+-30) swings a two-particle chain whose first segment
    /// writes `output`. Held at its maximum the input gives an angle of -10
    /// degrees, and the chain settles on an output of +0.1745 (10 degrees in
    /// radians).
    fn rig(input: &str, output: &str, weight: f32) -> Physics {
        Physics {
            gravity: (0.0, -1.0),
            wind: (0.0, 0.0),
            groups: vec![PhysicsGroup {
                inputs: vec![PhysicsInput {
                    source: input.into(),
                    component: PhysicsComponent::Angle,
                    weight: 100.0,
                    reflect: false,
                }],
                outputs: vec![PhysicsOutput {
                    destination: output.into(),
                    particle: 1,
                    component: PhysicsComponent::Angle,
                    scale: 1.0,
                    weight,
                    reflect: false,
                }],
                particles: vec![PhysicsParticle { mobility: 1.0, delay: 1.0, acceleration: 1.0, radius: 10.0 }; 2],
                normalization: PhysicsNormalization {
                    position: NormRange::new(-5.0, 5.0, 0.0),
                    angle: NormRange::new(-10.0, 10.0, 0.0),
                },
            }],
        }
    }

    fn with_physics(motions: Vec<Motion>, physics: Physics) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.motions = motions;
        ir.physics = Some(physics);
        GenericCubismModel::load(ir, "test")
    }

    const SETTLED: f32 = 0.174_532_93;

    fn frames(m: &mut GenericCubismModel, n: usize) {
        for _ in 0..n {
            m.update(Duration::from_millis(16)).expect("update");
        }
    }

    /// A motion holding `ParamAngleX` at its maximum and `ParamEyeLOpen` at `eye`.
    fn hold_both(name: &str, eye: f32) -> Motion {
        let curve = |target: &str, v: f32| MotionCurve {
            target: target.into(),
            keys: vec![k(0.0, v, 0.0), k(1.0, v, 0.0)],
            ..MotionCurve::default()
        };
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![curve("ParamAngleX", 30.0), curve("ParamEyeLOpen", eye)],
            ..Motion::default()
        }
    }

    #[test]
    fn physics_runs_after_the_motion_and_overwrites_its_curve_at_full_weight() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.update(Duration::from_millis(16)).expect("update");
        let first = value(&m, "ParamEyeLOpen");
        assert!(first < 0.5, "the curve's 0.9 must be replaced by the chain's output, got {first}");
        frames(&mut m, 600);
        assert!((value(&m, "ParamEyeLOpen") - SETTLED).abs() < 2e-3, "{}", value(&m, "ParamEyeLOpen"));
        assert!(m.unstable().is_empty());
    }

    #[test]
    fn a_partial_weight_blends_physics_over_the_motions_curve() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 25.0));
        frames(&mut m, 600);
        // 75 % of the curve's 0.9 and 25 % of the settled output.
        let want = 0.75 * 0.9 + 0.25 * SETTLED;
        assert!((value(&m, "ParamEyeLOpen") - want).abs() < 2e-3, "{}", value(&m, "ParamEyeLOpen"));
    }

    #[test]
    fn physics_keeps_running_after_a_one_shot_motion_ends() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.play_animation("Idle", PlayOptions::once()).expect("known");
        let mut steps = 0;
        while m.motion.is_some() && steps < 200 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        assert!(m.motion.is_none());
        // The finished frame still shows the held input, so the chain still swings.
        let eye = value(&m, "ParamEyeLOpen");
        assert!(eye != 0.9 && eye != 1.0 && eye > 0.0, "{eye}");
        frames(&mut m, 5);
        assert!(value(&m, "ParamEyeLOpen") != eye, "the chain is still settling");
    }

    #[test]
    fn physics_runs_even_when_nothing_else_drives_the_model() {
        // No motion, and parameter names the idle does not know: only the physics
        // has anything to say. The input rests at 30, its maximum.
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameters = vec![
            crate::moc3::Parameter { id: "In".into(), minimum: -30.0, maximum: 30.0, default: 30.0 },
            crate::moc3::Parameter { id: "Out".into(), minimum: 0.0, maximum: 1.0, default: 0.0 },
        ];
        ir.physics = Some(rig("In", "Out", 100.0));
        let mut m = GenericCubismModel::load(ir, "test");
        assert!(m.idle.is_empty() && m.motion.is_none());
        frames(&mut m, 600);
        assert!((value(&m, "Out") - SETTLED).abs() < 2e-3, "{}", value(&m, "Out"));
    }

    #[test]
    fn a_model_without_a_rig_is_unaffected() {
        let mut with = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        let mut without = with_motions(vec![hold_both("Idle", 0.9)]);
        frames(&mut with, 10);
        frames(&mut without, 10);
        assert_eq!(value(&without, "ParamEyeLOpen"), 0.9);
        assert!(value(&with, "ParamEyeLOpen") != 0.9);
        assert!(without.physics.is_none());
    }

    #[test]
    fn switching_motions_does_not_reset_the_physics() {
        let motions = vec![hold_both("A", 0.9), hold_both("B", 0.2)];
        let physics = || rig("ParamAngleX", "ParamEyeLOpen", 100.0);
        let mut m = with_physics(motions.clone(), physics());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        frames(&mut m, 600);
        let settled = value(&m, "ParamEyeLOpen");
        m.play_animation("B", PlayOptions::looping()).expect("known");
        frames(&mut m, 1);
        assert!((value(&m, "ParamEyeLOpen") - settled).abs() < 1e-3, "the chain carries over");
        // A model that had only just started is nowhere near that after one frame.
        let mut fresh = with_physics(motions, physics());
        fresh.play_animation("B", PlayOptions::looping()).expect("known");
        frames(&mut fresh, 1);
        assert!((value(&fresh, "ParamEyeLOpen") - settled).abs() > 0.05);
    }

    #[test]
    fn a_paused_frame_keeps_the_physics_output_instead_of_snapping_to_the_motion() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        frames(&mut m, 20);
        let moving = value(&m, "ParamEyeLOpen");
        m.update(Duration::ZERO).expect("update");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving);
    }

    #[test]
    fn pose_at_with_physics_is_a_function_of_the_timestamp_alone() {
        let motions = vec![sweep("S", "ParamAngleX", 40.0), hold_both("H", 0.9)];
        let physics = || rig("ParamAngleX", "ParamEyeLOpen", 100.0);
        let mut fresh = with_physics(motions.clone(), physics());
        fresh.pose_at("S", 1.0).expect("known");
        let once = value(&fresh, "ParamEyeLOpen");
        fresh.pose_at("S", 1.0).expect("known");
        assert_eq!(value(&fresh, "ParamEyeLOpen"), once, "same time twice");

        let mut used = with_physics(motions.clone(), physics());
        used.play_animation("H", PlayOptions::looping()).expect("known");
        frames(&mut used, 123);
        used.pose_at("S", 1.0).expect("known");
        assert_eq!(used.values, fresh.values);
        let (mut a, mut b) = (RenderList::new(), RenderList::new());
        used.emit(&mut a);
        fresh.emit(&mut b);
        for (x, y) in a.meshes().iter().zip(b.meshes()) {
            assert_eq!(x.vertices, y.vertices);
        }

        // And it really is simulated: not the motion's own value, and a later
        // time differs from an earlier one.
        let mut plain = with_motions(motions);
        plain.pose_at("S", 1.0).expect("known");
        let rest = plain.moc().parameter("ParamEyeLOpen").expect("p").default;
        assert_eq!(value(&plain, "ParamEyeLOpen"), rest);
        assert!(once > 0.0 && once != rest);
        fresh.pose_at("S", 0.25).expect("known");
        assert!(value(&fresh, "ParamEyeLOpen") != once);
        // Time zero shows the first frame of playback, finite.
        fresh.pose_at("S", 0.0).expect("known");
        assert!(value(&fresh, "ParamEyeLOpen").is_finite());
    }

    #[test]
    fn starting_or_stopping_a_motion_does_not_pop_the_physics_outputs() {
        let motions = vec![hold_both("A", 0.9), hold_both("B", 0.7)];
        let mut m = with_physics(motions, rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.play_animation("A", PlayOptions::looping()).expect("known");
        frames(&mut m, 120);
        let moving = value(&m, "ParamEyeLOpen");
        assert!(moving < 0.5);
        m.play_animation("B", PlayOptions::looping()).expect("known");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving, "not the new curve's 0.7");
        m.stop_animation("B");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving, "not the default");
    }

    #[test]
    fn pose_at_replays_only_a_bounded_window_so_a_huge_motion_returns_promptly() {
        let mut long = hold_both("L", 0.9);
        long.duration = 1.0e9;
        long.curves[0].keys[1].time = 1.0e9;
        long.curves[1].keys[1].time = 1.0e9;
        let mut m = with_physics(vec![long.clone()], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        let start = std::time::Instant::now();
        m.pose_at("L", 9.0e8).expect("known");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(value(&m, "ParamEyeLOpen").is_finite());

        // A long but representable motion: the window is replayed and has settled.
        let mut mid = hold_both("M", 0.9);
        mid.duration = 1000.0;
        mid.curves[0].keys[1].time = 1000.0;
        mid.curves[1].keys[1].time = 1000.0;
        let mut m = with_physics(vec![mid], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.pose_at("M", 900.0).expect("known");
        assert!(
            (value(&m, "ParamEyeLOpen") - SETTLED).abs() < 2e-3,
            "settled inside the window: {}",
            value(&m, "ParamEyeLOpen")
        );

        // Inside the window the replay still starts from rest: half a second is
        // 30 frames, nowhere near settled.
        let mut posed = with_physics(vec![long], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        posed.pose_at("L", 0.5).expect("known");
        assert!((value(&posed, "ParamEyeLOpen") - SETTLED).abs() > 1e-3);
    }

    #[test]
    fn unresolved_physics_targets_are_reported() {
        let mut m = with_physics(vec![], rig("NoSuchInput", "NoSuchOutput", 100.0));
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        let text: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(text.len(), 2, "{text:?}");
        assert!(text.iter().any(|t| t.contains("NoSuchInput")) && text.iter().any(|t| t.contains("NoSuchOutput")));
        // The rig still loads and does nothing harmful.
        frames(&mut m, 5);
        assert!(m.values.iter().all(|v| v.is_finite()));
    }
}
