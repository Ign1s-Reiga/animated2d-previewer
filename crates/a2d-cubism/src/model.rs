//! [`GenericCubismModel`]: a MOC3 behind the shared [`AnimatedModel`] interface.
//!
//! This is what lets one viewer show both runtime families (spec §5). The
//! renderer never learns which it is drawing; it receives triangles either way.
//!
//! # What this can and cannot do yet
//!
//! It poses and draws. It does **not** play the model's own motions: those live
//! in the bundle as Unity `AnimationClip`s in the compressed muscle-clip form,
//! which is not decoded, so [`AnimatedModel::animations`] is empty and
//! [`AnimatedModel::play_animation`] refuses by name rather than silently doing
//! nothing.
//!
//! What [`AnimatedModel::update`] does instead is drive a small idle of its
//! own, from Cubism's conventional parameter names. That is **synthetic
//! motion**, not the character's: it exists so a viewer shows something alive
//! and so the deformation path is exercised continuously rather than only at
//! rest. It is skipped entirely for any parameter the model does not declare.

use std::time::Duration;

use a2d_core::{
    Aabb, AnimatedModel, AnimationInfo, ExpressionInfo, HitAreaId, PlayOptions, RenderList, RuntimeError, TextureId,
};

use crate::eval::Pose;
use crate::moc3::Moc3;

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

/// A Cubism model, posed and drawable.
pub struct GenericCubismModel {
    moc: Moc3,
    name: String,
    /// One value per parameter, in the model's own order.
    values: Vec<f32>,
    pose: Pose,
    elapsed: f32,
    /// Whether the idle drives anything, so a model without the conventional
    /// parameters is left alone rather than jittered.
    idle: Vec<(usize, f32, f32, f32, f32)>,
    animations: Vec<AnimationInfo>,
    expressions: Vec<ExpressionInfo>,
    texture: TextureId,
}

impl GenericCubismModel {
    /// Wraps a parsed model.
    pub fn load(moc: Moc3, display_name: impl Into<String>) -> GenericCubismModel {
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

        let pose = moc.pose(&values);
        GenericCubismModel {
            moc,
            name: display_name.into(),
            values,
            pose,
            elapsed: 0.0,
            idle,
            animations: Vec::new(),
            expressions: Vec::new(),
            texture: TextureId(0),
        }
    }

    pub fn moc(&self) -> &Moc3 {
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
    pub fn set_parameter(&mut self, id: &str, value: f32) -> bool {
        let Some(index) = self.moc.parameters.iter().position(|p| p.id == id) else { return false };
        self.values[index] = self.moc.parameters[index].clamp(value);
        true
    }

    /// Which texture page the drawables sample.
    pub fn set_texture(&mut self, texture: TextureId) {
        self.texture = texture;
    }

    /// Drawables whose deformer chain produced nothing usable.
    pub fn unstable(&self) -> &[usize] {
        &self.pose.unstable
    }

    fn repose(&mut self) {
        self.pose = self.moc.pose(&self.values);
    }
}

impl AnimatedModel for GenericCubismModel {
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError> {
        // A long step means the host was blocked or asleep; carrying it through
        // would jump the idle rather than advance it.
        self.elapsed += dt.as_secs_f32().min(0.1);
        if self.idle.is_empty() {
            return Ok(());
        }
        for (index, period, span, rest, _) in &self.idle {
            let phase = self.elapsed / period * std::f32::consts::TAU;
            let value = rest + phase.sin() * span;
            if let Some(p) = self.moc.parameters.get(*index) {
                self.values[*index] = p.clamp(value);
            }
        }
        self.repose();
        Ok(())
    }

    fn emit(&self, out: &mut RenderList) {
        self.moc.emit(&self.pose, self.texture, out);
    }

    fn play_animation(&mut self, name: &str, _opts: PlayOptions) -> Result<(), RuntimeError> {
        Err(RuntimeError::UnknownAnimation(format!(
            "{name:?}: this model's motions are Unity animation clips, which are not decoded yet"
        )))
    }

    fn stop_animation(&mut self, _name: &str) {}

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
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_motions_are_refused_by_name_rather_than_ignored() {
        // Silently doing nothing would look like a model with no idle rather
        // than like a decoder that has not got there yet.
        let mut m = model();
        let err = m.play_animation("idle", PlayOptions::looping()).unwrap_err();
        assert!(err.to_string().contains("not decoded yet"), "{err}");
        assert!(m.animations().is_empty());
    }

    #[test]
    fn a_long_step_does_not_jump_the_idle() {
        let mut m = model();
        m.update(Duration::from_secs(600)).expect("update should not fail");
        assert!(m.elapsed <= 0.1, "a blocked host must not teleport the idle: {}", m.elapsed);
    }
}
