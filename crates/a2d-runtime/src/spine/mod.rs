//! The Generic Spine runtime.

pub mod apply;
pub mod emit;
pub mod pose;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use a2d_core::ir::ids::SlotId;
use a2d_core::ir::spine::SpineIr;
use a2d_core::{
    Aabb, AnimatedModel, AnimationInfo, ExpressionInfo, HitAreaId, LoadReport, PlayOptions, RenderList, RuntimeError,
    Vec2,
};

pub use apply::{FiredEvent, MixBlend};
pub use pose::SkeletonPose;
pub use state::AnimationState;

/// A playable Spine model.
///
/// Holds a shared reference to the decoded [`SpineIr`] plus this instance's own
/// mutable pose and animation state, so several characters can share one
/// decode. The spec's single `GenericSpineModel` is split this way for the same
/// reason Spine itself splits `SkeletonData` from `Skeleton`.
#[derive(Debug)]
pub struct GenericSpineModel {
    ir: Arc<SpineIr>,
    pose: SkeletonPose,
    state: AnimationState,
    animations: Vec<AnimationInfo>,
    /// Spine has no expression concept; skins are the closest analogue and are
    /// surfaced through the shared trait so the viewer needs only one control.
    expressions: Vec<ExpressionInfo>,
    display_name: String,
    /// Whole-model alpha, multiplied into every emitted mesh.
    pub alpha: f32,
    disposed: bool,
}

impl GenericSpineModel {
    /// Builds a playable model from decoded IR.
    pub fn load(ir: Arc<SpineIr>, display_name: impl Into<String>) -> Self {
        let animations =
            ir.animations.iter().map(|a| AnimationInfo { name: a.name.clone(), duration: a.duration }).collect();
        let expressions = ir.skins.iter().map(|s| ExpressionInfo { name: s.name.clone() }).collect();
        let pose = SkeletonPose::new(ir.clone());
        GenericSpineModel {
            ir,
            pose,
            state: AnimationState::new(),
            animations,
            expressions,
            display_name: display_name.into(),
            alpha: 1.0,
            disposed: false,
        }
    }

    pub fn ir(&self) -> &Arc<SpineIr> {
        &self.ir
    }

    pub fn pose(&self) -> &SkeletonPose {
        &self.pose
    }

    pub fn pose_mut(&mut self) -> &mut SkeletonPose {
        &mut self.pose
    }

    pub fn state(&self) -> &AnimationState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AnimationState {
        &mut self.state
    }

    /// Events fired by the most recent [`AnimatedModel::update`].
    pub fn events(&self) -> &[FiredEvent] {
        &self.state.events
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.state.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.state.paused
    }

    /// Position offset applied to the whole skeleton.
    pub fn set_position(&mut self, position: Vec2) {
        self.pose.position = position;
    }

    /// Scale applied to the whole skeleton. Negative components flip it.
    pub fn set_scale(&mut self, scale: Vec2) {
        self.pose.scale = scale;
    }

    /// A sensible animation to start with: `idle` if present, else the first.
    pub fn default_animation(&self) -> Option<&str> {
        self.animations
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case("idle"))
            .or_else(|| self.animations.iter().find(|a| a.name.to_ascii_lowercase().contains("idle")))
            .or_else(|| self.animations.first())
            .map(|a| a.name.as_str())
    }

    /// Poses the skeleton at an exact time without advancing any track.
    ///
    /// Visual regression tests render fixed timestamps (spec §17.3); this is how
    /// they get a reproducible frame regardless of how `dt` would have been cut.
    /// The scrub does not loop, so `time == duration` poses the final frame
    /// rather than wrapping back to the first.
    pub fn pose_at(&mut self, animation: &str, time: f32) -> Result<(), RuntimeError> {
        let index = self.animation_index(animation)?;
        self.state.clear_all();
        self.state.set_animation(0, index, false, 1.0, 0.0, 0.0);
        // `seek` rather than `update`: advancing a non-looping animation to its
        // own duration would retire the track before the pose was applied.
        self.state.seek(0, time);
        self.state.apply(&mut self.pose);
        Ok(())
    }

    /// Degradations discovered while posing, for the caller's report.
    pub fn absorb_degradations(&self, report: &mut LoadReport) {
        self.pose.absorb_degradations(report);
    }

    fn animation_index(&self, name: &str) -> Result<usize, RuntimeError> {
        self.ir
            .animations
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| RuntimeError::UnknownAnimation(name.to_string()))
    }
}

impl AnimatedModel for GenericSpineModel {
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError> {
        if self.disposed {
            return Err(RuntimeError::InvalidState("model has been disposed".into()));
        }
        self.state.update(&self.ir, dt.as_secs_f32());
        self.state.apply(&mut self.pose);
        Ok(())
    }

    fn emit(&self, out: &mut RenderList) {
        if self.disposed {
            return;
        }
        emit::emit(&self.pose, self.alpha, out);
    }

    fn play_animation(&mut self, name: &str, opts: PlayOptions) -> Result<(), RuntimeError> {
        let index = self.animation_index(name)?;
        let track = opts.track as usize;
        let speed = opts.speed;
        let delay = opts.delay.as_secs_f32();
        let mix = opts.mix_duration.as_secs_f32();
        if opts.queued {
            self.state.add_animation(track, index, opts.looping, speed, delay, mix);
        } else {
            self.state.set_animation(track, index, opts.looping, speed, delay, mix);
        }
        Ok(())
    }

    fn stop_animation(&mut self, name: &str) {
        // Stopping something that is not playing is not an error.
        if let Ok(index) = self.animation_index(name) {
            self.state.stop_animation(index);
        }
    }

    fn set_expression(&mut self, name: &str) -> Result<(), RuntimeError> {
        let skin = self.ir.skin_by_name(name).ok_or_else(|| RuntimeError::UnknownExpression(name.to_string()))?;
        self.pose.set_skin(skin);
        Ok(())
    }

    fn animations(&self) -> &[AnimationInfo] {
        &self.animations
    }

    fn expressions(&self) -> &[ExpressionInfo] {
        &self.expressions
    }

    fn bounds(&self) -> Aabb {
        emit::pose_bounds(&self.pose)
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<HitAreaId> {
        let point = Vec2::new(x, y);
        // Front to back, so the topmost hit area wins.
        for slot_id in self.pose.draw_order.iter().rev() {
            let Some(polygon) = emit::bounding_box_polygon(&self.pose, *slot_id) else { continue };
            if emit::point_in_polygon(&polygon, point) {
                let name = self
                    .pose
                    .slots
                    .get(slot_id.index())
                    .and_then(|s| s.attachment)
                    .and_then(|a| self.ir.attachment(a))
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| self.slot_name(*slot_id));
                return Some(HitAreaId(name));
            }
        }
        None
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.state.clear_all();
        self.disposed = true;
    }

    fn set_scale(&mut self, scale: a2d_core::Vec2) {
        GenericSpineModel::set_scale(self, scale);
    }

    fn pose_at(&mut self, animation: &str, time: f32) -> Result<(), RuntimeError> {
        GenericSpineModel::pose_at(self, animation, time)
    }

    fn absorb_degradations(&self, report: &mut a2d_core::LoadReport) {
        GenericSpineModel::absorb_degradations(self, report);
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl GenericSpineModel {
    fn slot_name(&self, slot: SlotId) -> String {
        self.ir.slot(slot).map(|s| s.name.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, AttachmentId, BoneId};
    use a2d_core::ir::spine::{
        Animation, Attachment, AttachmentKind, Bone, BoundingBoxAttachment, RegionAttachment, ScalarKey, Skin,
        SkinEntry, Slot, Timeline, VertexData,
    };
    use a2d_core::{Interpolation, Rgba};

    /// Trig-derived coordinates carry the same rounding the reference runtime
    /// has, so geometry is compared with a tolerance rather than exactly.
    fn assert_close2(a: Vec2, b: Vec2) {
        assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3, "{a:?} != {b:?}");
    }

    fn model() -> GenericSpineModel {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None), Bone::new("torso", Some(BoneId(0)))],
            slots: vec![
                Slot { setup_attachment: Some("body".into()), ..Slot::new("body", BoneId(1)) },
                Slot { setup_attachment: Some("hit".into()), ..Slot::new("hitbox", BoneId(0)) },
            ],
            skins: vec![Skin::new("default"), Skin::new("blue")],
            attachments: vec![
                Attachment {
                    name: "body".into(),
                    kind: AttachmentKind::Region(RegionAttachment {
                        path: "body".into(),
                        region: Some(a2d_core::ir::ids::AtlasRegionId(0)),
                        position: Vec2::ZERO,
                        rotation: 0.0,
                        scale: Vec2::ONE,
                        size: Vec2::new(20.0, 40.0),
                        color: Rgba::WHITE,
                        sequence: None,
                    }),
                },
                Attachment {
                    name: "hit".into(),
                    kind: AttachmentKind::BoundingBox(BoundingBoxAttachment {
                        vertices: VertexData::Rigid(vec![
                            Vec2::new(-10.0, -10.0),
                            Vec2::new(10.0, -10.0),
                            Vec2::new(10.0, 10.0),
                            Vec2::new(-10.0, 10.0),
                        ]),
                        color: Rgba::WHITE,
                    }),
                },
            ],
            animations: vec![
                Animation {
                    name: "idle".into(),
                    duration: 1.0,
                    timelines: vec![Timeline::BoneRotate {
                        bone: BoneId(1),
                        keys: vec![
                            ScalarKey { time: 0.0, value: 0.0, interp: Interpolation::Linear },
                            ScalarKey { time: 1.0, value: 90.0, interp: Interpolation::Linear },
                        ],
                    }],
                },
                Animation::new("wave"),
            ],
            atlas: Atlas {
                pages: vec![AtlasPage { size: Some((64, 64)), ..AtlasPage::new("p.png") }],
                regions: vec![AtlasRegion {
                    name: "body".into(),
                    page: AtlasPageId(0),
                    xy: (0, 0),
                    size: (20, 40),
                    rotate_deg: 0,
                    offset: (0, 0),
                    original_size: (20, 40),
                    index: -1,
                    splits: None,
                    pads: None,
                }],
            },
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "hit".into(), attachment: AttachmentId(1) });
        ir.skins[1].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        GenericSpineModel::load(Arc::new(ir), "Test")
    }

    #[test]
    fn animations_are_listed_with_their_durations() {
        let m = model();
        let names: Vec<&str> = m.animations().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["idle", "wave"]);
        assert_eq!(m.animations()[0].duration, 1.0);
        assert!(m.has_animation("idle"));
        assert!(!m.has_animation("run"));
    }

    #[test]
    fn skins_are_surfaced_as_expressions() {
        let m = model();
        let names: Vec<&str> = m.expressions().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["default", "blue"]);
    }

    #[test]
    fn setting_an_unknown_expression_is_an_error() {
        let mut m = model();
        assert!(m.set_expression("blue").is_ok());
        assert!(matches!(m.set_expression("ghost"), Err(RuntimeError::UnknownExpression(_))));
    }

    #[test]
    fn playing_an_unknown_animation_is_an_error() {
        let mut m = model();
        let err = m.play_animation("nope", PlayOptions::default()).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownAnimation(_)));
    }

    #[test]
    fn updating_advances_the_pose() {
        let mut m = model();
        m.play_animation("idle", PlayOptions::looping()).unwrap();
        m.update(Duration::from_millis(500)).unwrap();
        let rotation = m.pose().bones[1].local.rotation;
        assert!((rotation - 45.0).abs() < 1e-3, "got {rotation}");
    }

    #[test]
    fn stopping_an_animation_that_is_not_playing_is_silent() {
        let mut m = model();
        m.stop_animation("wave");
        m.stop_animation("does-not-exist");
    }

    #[test]
    fn emitting_produces_drawable_geometry() {
        let mut m = model();
        m.play_animation("idle", PlayOptions::looping()).unwrap();
        m.update(Duration::from_millis(100)).unwrap();
        let mut list = RenderList::new();
        m.emit(&mut list);
        assert_eq!(list.meshes().len(), 1);
        assert!(list.meshes()[0].is_well_formed());
    }

    #[test]
    fn bounds_cover_the_visible_geometry() {
        let m = model();
        let b = m.bounds();
        assert!(!b.is_empty());
        // The bounding box attachment spans 20x20 and the region 20x40.
        assert_close2(b.size(), Vec2::new(20.0, 40.0));
    }

    #[test]
    fn hit_testing_finds_the_bounding_box_attachment() {
        let m = model();
        assert_eq!(m.hit_test(0.0, 0.0), Some(HitAreaId("hit".into())));
        assert_eq!(m.hit_test(100.0, 100.0), None);
    }

    #[test]
    fn posing_at_a_fixed_time_is_reproducible() {
        let mut a = model();
        let mut b = model();
        a.pose_at("idle", 0.25).unwrap();
        b.pose_at("idle", 0.25).unwrap();
        assert_eq!(a.pose().bones[1].local.rotation, b.pose().bones[1].local.rotation);
        assert!((a.pose().bones[1].local.rotation - 22.5).abs() < 1e-3);
    }

    #[test]
    fn posing_at_the_four_regression_timestamps_gives_four_distinct_poses() {
        let mut m = model();
        let mut seen = Vec::new();
        for t in [0.0f32, 0.25, 0.5, 1.0] {
            m.pose_at("idle", t).unwrap();
            seen.push(m.pose().bones[1].local.rotation);
        }
        assert_eq!(seen, vec![0.0, 22.5, 45.0, 90.0]);
    }

    #[test]
    fn the_default_animation_prefers_one_named_idle() {
        assert_eq!(model().default_animation(), Some("idle"));
    }

    #[test]
    fn pausing_freezes_playback() {
        let mut m = model();
        m.play_animation("idle", PlayOptions::looping()).unwrap();
        m.update(Duration::from_millis(200)).unwrap();
        let frozen = m.pose().bones[1].local.rotation;
        m.set_paused(true);
        m.update(Duration::from_millis(500)).unwrap();
        assert_eq!(m.pose().bones[1].local.rotation, frozen);
        assert!(m.is_paused());
    }

    #[test]
    fn disposing_stops_playback_and_emits_nothing() {
        let mut m = model();
        m.play_animation("idle", PlayOptions::looping()).unwrap();
        m.dispose();
        let mut list = RenderList::new();
        m.emit(&mut list);
        assert!(list.is_empty());
        assert!(matches!(m.update(Duration::ZERO), Err(RuntimeError::InvalidState(_))));
    }

    #[test]
    fn disposing_twice_is_harmless() {
        let mut m = model();
        m.dispose();
        m.dispose();
    }

    #[test]
    fn model_scale_and_position_move_the_emitted_geometry() {
        let mut m = model();
        m.set_position(Vec2::new(100.0, 0.0));
        m.set_scale(Vec2::new(2.0, 2.0));
        m.update(Duration::ZERO).unwrap();
        let b = m.bounds();
        assert_close2(b.center(), Vec2::new(100.0, 0.0));
        assert_close2(b.size(), Vec2::new(40.0, 80.0));
    }

    #[test]
    fn model_alpha_multiplies_the_emitted_tint() {
        let mut m = model();
        m.alpha = 0.5;
        m.update(Duration::ZERO).unwrap();
        let mut list = RenderList::new();
        m.emit(&mut list);
        assert!((list.meshes()[0].color.a - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_queued_animation_follows_a_one_shot() {
        let mut m = model();
        m.play_animation("idle", PlayOptions::once()).unwrap();
        m.play_animation("wave", PlayOptions::looping().queued()).unwrap();
        for _ in 0..12 {
            m.update(Duration::from_millis(100)).unwrap();
        }
        let playing = m.state().track(0).unwrap().current().unwrap().animation;
        assert_eq!(m.ir().animations[playing].name, "wave");
    }
}
