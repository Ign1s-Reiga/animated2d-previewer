//! The one interface the desktop viewer talks to.
//!
//! Both `GenericSpineModel` and `GenericCubismModel` implement this. Everything
//! below it — bones vs. parameters, meshes vs. drawables — stays behind the
//! concrete type. Spec §5 explicitly forbids forcing the two ecosystems into a
//! shared low-level deformation model, so this trait stays high level.

use std::time::Duration;

use crate::error::RuntimeError;
use crate::math::Aabb;
use crate::render::{HitAreaId, RenderList};

/// How to start an animation.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayOptions {
    /// Track index. Higher tracks compose over lower ones.
    pub track: u32,
    pub looping: bool,
    /// Crossfade duration from whatever is currently on the track.
    pub mix_duration: Duration,
    /// Playback rate multiplier. Negative values play backwards.
    pub speed: f32,
    /// Delay before the animation starts, measured from the call.
    pub delay: Duration,
    /// Append after what is already queued on the track instead of replacing it.
    ///
    /// A queued entry takes over at the end of the current pass of the entry
    /// ahead of it: for a once entry when it finishes, for a looping entry at
    /// its next loop wrap. [`PlayOptions::handover_at`] moves that moment
    /// earlier.
    ///
    /// The end of a pass follows the playing direction. At a negative speed
    /// the playhead starts at the end of the animation and runs towards 0: a
    /// once entry ends when it reaches 0 and a looping entry wraps there. At
    /// speed 0 a pass never ends, so nothing queued behind it takes over.
    pub queued: bool,
    /// For a `queued` entry: the takeover (the start of the crossfade) happens
    /// when the playhead of the entry ahead of it reaches this time, measured
    /// within its current pass. `None` is the end of the pass. Ignored for an
    /// entry that is not queued. A runtime that cannot honour it (a playhead
    /// running backwards, or a time past the end of the pass) falls back to the
    /// end of the pass.
    pub handover_at: Option<Duration>,
}

impl Default for PlayOptions {
    fn default() -> Self {
        PlayOptions {
            track: 0,
            looping: true,
            mix_duration: Duration::ZERO,
            speed: 1.0,
            delay: Duration::ZERO,
            queued: false,
            handover_at: None,
        }
    }
}

impl PlayOptions {
    /// Loops on track 0 with no crossfade. The default for an idle.
    pub fn looping() -> Self {
        PlayOptions::default()
    }

    /// Plays through once and then leaves the track empty.
    pub fn once() -> Self {
        PlayOptions { looping: false, ..PlayOptions::default() }
    }

    pub fn with_track(mut self, track: u32) -> Self {
        self.track = track;
        self
    }

    pub fn with_mix(mut self, mix: Duration) -> Self {
        self.mix_duration = mix;
        self
    }

    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Appends after what is already on the track instead of replacing it.
    ///
    /// The entry takes over at the end of the current pass of the entry ahead
    /// of it: for a once entry when it finishes, for a looping entry at its
    /// next loop wrap, or at [`PlayOptions::with_handover_at`] if given.
    pub fn queued(mut self) -> Self {
        self.queued = true;
        self
    }

    /// For a queued entry, start the takeover when the entry ahead reaches
    /// `at` into its current pass instead of at its end.
    pub fn with_handover_at(mut self, at: Duration) -> Self {
        self.handover_at = Some(at);
        self
    }
}

/// What the viewer needs to list an animation in a selector.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationInfo {
    pub name: String,
    /// Length of one pass, in seconds.
    pub duration: f32,
    /// What the source says follows this animation, if it says anything.
    pub then: Option<Handover>,
}

/// A scripted return from one animation to another.
///
/// After the animation this belongs to, play `animation`. The crossfade into it
/// starts when the outgoing animation's playhead reaches `at` seconds into its
/// pass, and lasts `mix` seconds. The source decides all three; nothing here
/// says where it came from, so either runtime can carry one.
#[derive(Debug, Clone, PartialEq)]
pub struct Handover {
    /// The animation to play next.
    pub animation: String,
    /// Seconds into the outgoing animation's pass at which the crossfade begins.
    pub at: f32,
    /// Seconds the crossfade lasts.
    pub mix: f32,
}

/// What the viewer needs to list an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionInfo {
    pub name: String,
}

/// Shared runtime interface (spec §5).
///
/// `load` is deliberately not a trait method: it is a constructor on each
/// concrete type, so a model never exists in a partially initialised state.
/// Picks a sensible animation to open a model with.
///
/// A viewer opening a character wants it standing there, not swinging a
/// weapon. Rigs do not agree on what to call that pose, so the common names are
/// tried in turn before falling back to whatever comes first:
///
/// * `idle`, then any name containing it;
/// * `stand`, then any name containing it -- one real rig's animations are
///   `Hitaction_1`, `Hitaction_2`, `Stand` and `Stop`, and without this it
///   opens mid-attack with the camera fitted to a swing twice the character's
///   height.
///
/// * `normal` and `wait`: both exact names first (`normal` before `wait`), then
///   names with `normal` as a whole part, then `wait` as a whole part. A part
///   is a run of letters, so names split on `_`, `-`, `.`, spaces and digits:
///   `Live_normal` and `pose_wait_01` match; `abnormal`, `await` and
///   `waitress` do not. Cubism models commonly ship mood motions
///   (`<prefix>_happy`, `_normal`, `_shy`) and no idle, where the first by name
///   is an arbitrary mood rather than the resting face;
/// * otherwise the first animation.
///
/// `stop` is deliberately not in the list: it reads as a standing pose but is
/// a zero-length one on every rig seen, so it has nothing to play.
pub fn preferred_animation(all: &[AnimationInfo]) -> Option<&str> {
    let exact = |want: &str| all.iter().find(|a| a.name.eq_ignore_ascii_case(want));
    let loose = |want: &str| all.iter().find(|a| a.name.to_ascii_lowercase().contains(want));
    let part = |want: &str| {
        all.iter().find(|a| a.name.split(|c: char| !c.is_ascii_alphabetic()).any(|p| p.eq_ignore_ascii_case(want)))
    };
    exact("idle")
        .or_else(|| loose("idle"))
        .or_else(|| exact("stand"))
        .or_else(|| loose("stand"))
        .or_else(|| exact("normal"))
        .or_else(|| exact("wait"))
        .or_else(|| part("normal"))
        .or_else(|| part("wait"))
        .or_else(|| all.first())
        .map(|a| a.name.as_str())
}

/// `render` is deliberately split into [`AnimatedModel::emit`] plus a renderer,
/// so that no GPU type reaches this crate.
pub trait AnimatedModel {
    /// Advances animation state by `dt`. Evaluation is delta-time based and
    /// independent of rendering frame rate (spec §12).
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError>;

    /// Appends this model's current frame to `out`.
    fn emit(&self, out: &mut RenderList);

    /// Starts `name`. A `queued` entry waits behind the one ahead of it and
    /// takes over at the end of that entry's current pass (when a once entry
    /// finishes, at a looping entry's next wrap), or at
    /// [`PlayOptions::handover_at`] if given; see [`PlayOptions::queued`].
    fn play_animation(&mut self, name: &str, opts: PlayOptions) -> Result<(), RuntimeError>;

    /// Stops `name` wherever it is playing. Unknown names are a no-op, since
    /// stopping something that is not running is not an error.
    fn stop_animation(&mut self, name: &str);

    fn set_expression(&mut self, name: &str) -> Result<(), RuntimeError>;

    fn animations(&self) -> &[AnimationInfo];

    fn expressions(&self) -> &[ExpressionInfo];

    /// Bounds of the current pose, in model space.
    fn bounds(&self) -> Aabb;

    fn hit_test(&self, x: f32, y: f32) -> Option<HitAreaId>;

    /// True when the model declares named hit areas that [`AnimatedModel::hit_test`]
    /// can return.
    ///
    /// The viewer uses it to choose between "only a hit area reacts to a click"
    /// (true) and "anywhere on the character reacts" (false). Default `false`,
    /// for a model with no hit areas.
    fn has_hit_areas(&self) -> bool {
        false
    }

    /// Releases any retained resources. Idempotent.
    fn dispose(&mut self);

    /// Name the viewer should show. Defaults to the package display name.
    fn display_name(&self) -> &str;

    /// A sensible animation to start with; see [`preferred_animation`].
    ///
    /// The rule is the same for every runtime family, so it lives here rather
    /// than being restated per model.
    fn default_animation(&self) -> Option<&str> {
        preferred_animation(self.animations())
    }

    /// The animation that most recently took over track 0: it changes when a
    /// crossfade into a new animation starts, including a queued takeover, and
    /// is `None` when nothing plays. Default `None`, for a model that does not
    /// track it.
    fn current_animation(&self) -> Option<&str> {
        None
    }

    /// Scales the whole model. A negative component mirrors it.
    ///
    /// Default is to ignore it, for a model that cannot be mirrored meaningfully.
    fn set_scale(&mut self, _scale: crate::Vec2) {}

    /// Points the model at `target`, a point in **model space** (the space of
    /// [`AnimatedModel::bounds`] and [`AnimatedModel::hit_test`]), for a model
    /// that can follow a pointer. `None` releases it: it eases back to its own
    /// rest target with its own smoothing.
    ///
    /// A mirrored model (a negative x from [`AnimatedModel::set_scale`])
    /// interprets the target in its own unmirrored space: each family handles
    /// that, the host never does.
    ///
    /// Default is to ignore it, for a model with nothing that follows.
    fn set_look_target(&mut self, _target: Option<crate::Vec2>) {}

    /// Poses at an absolute time within one animation, without advancing state.
    ///
    /// This is what visual regression tests and frame export need: a frame that
    /// depends on the timestamp alone, not on how it was reached. A model whose
    /// timelines cannot be seeked says so rather than posing something else,
    /// which is why this has a default rather than being required.
    fn pose_at(&mut self, animation: &str, _time: f32) -> Result<(), RuntimeError> {
        Err(RuntimeError::UnknownAnimation(format!("{animation:?}: this model cannot be posed at a timestamp")))
    }

    /// Folds any degradations the model noticed while posing into a report.
    ///
    /// Default is silence, for models that have nothing to add.
    fn absorb_degradations(&self, _report: &mut crate::LoadReport) {}

    /// Whether the model has an animation with this name.
    fn has_animation(&self, name: &str) -> bool {
        self.animations().iter().any(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
    fn named(names: &[&str]) -> Vec<AnimationInfo> {
        names.iter().map(|n| AnimationInfo { name: (*n).to_string(), duration: 1.0, then: None }).collect()
    }

    #[test]
    fn an_idle_animation_wins_however_it_is_capitalised() {
        assert_eq!(preferred_animation(&named(&["Attack", "IDLE", "Stand"])), Some("IDLE"));
        assert_eq!(preferred_animation(&named(&["Attack", "idle_loop", "Stand"])), Some("idle_loop"));
    }

    #[test]
    fn a_standing_animation_is_taken_when_there_is_no_idle() {
        // A real rig: without this the viewer opens mid-attack, with the camera
        // fitted to a swing twice the character's height.
        assert_eq!(preferred_animation(&named(&["Hitaction_1", "Hitaction_2", "Stand", "Stop"])), Some("Stand"));
    }

    #[test]
    fn idle_is_preferred_over_standing() {
        assert_eq!(preferred_animation(&named(&["Stand", "Idle"])), Some("Idle"));
    }

    #[test]
    fn stop_is_not_mistaken_for_a_standing_pose() {
        // It reads like one but is zero-length on every rig seen, so there
        // would be nothing to play.
        assert_eq!(preferred_animation(&named(&["Hitaction_1", "Stop"])), Some("Hitaction_1"));
    }

    #[test]
    fn stand_beats_normal_and_idle_beats_both() {
        assert_eq!(preferred_animation(&named(&["pose_normal", "pose_stand"])), Some("pose_stand"));
        assert_eq!(preferred_animation(&named(&["pose_normal", "pose_stand", "pose_idle"])), Some("pose_idle"));
    }

    #[test]
    fn normal_beats_wait_and_wait_beats_first() {
        assert_eq!(preferred_animation(&named(&["pose_happy", "pose_wait", "pose_normal"])), Some("pose_normal"));
        assert_eq!(preferred_animation(&named(&["pose_happy", "pose_shy", "pose_wait"])), Some("pose_wait"));
    }

    #[test]
    fn exact_names_beat_substrings_and_case_is_ignored() {
        assert_eq!(preferred_animation(&named(&["pose_normal", "NORMAL"])), Some("NORMAL"));
        assert_eq!(preferred_animation(&named(&["pose_wait", "Wait"])), Some("Wait"));
        assert_eq!(preferred_animation(&named(&["pose_happy", "Pose_NORMAL"])), Some("Pose_NORMAL"));
    }

    #[test]
    fn both_exact_names_beat_either_partial_match() {
        assert_eq!(preferred_animation(&named(&["wait", "normal_attack"])), Some("wait"));
        assert_eq!(preferred_animation(&named(&["wait", "normal"])), Some("normal"));
    }

    #[test]
    fn normal_and_wait_match_whole_name_parts_only() {
        for name in ["x_abnormal", "await", "waitress", "normality"] {
            assert_eq!(preferred_animation(&named(&["pose_happy", name])), Some("pose_happy"), "{name}");
        }
        for name in ["Live_normal", "pose_wait_01", "pose-normal2", "a.wait"] {
            assert_eq!(preferred_animation(&named(&["pose_happy", name])), Some(name), "{name}");
        }
    }

    #[test]
    fn no_match_falls_back_to_the_first_animation() {
        assert_eq!(preferred_animation(&named(&["pose_happy", "pose_shy", "pose_smile"])), Some("pose_happy"));
    }

    #[test]
    fn a_model_with_no_animations_has_no_default() {
        assert_eq!(preferred_animation(&[]), None);
    }

    use super::*;

    #[test]
    fn default_play_options_loop_on_track_zero_at_normal_speed() {
        let o = PlayOptions::default();
        assert!(o.looping);
        assert_eq!(o.track, 0);
        assert_eq!(o.speed, 1.0);
        assert_eq!(o.mix_duration, Duration::ZERO);
        assert!(!o.queued);
    }

    #[test]
    fn once_does_not_loop() {
        assert!(!PlayOptions::once().looping);
    }

    #[test]
    fn builders_compose() {
        let o = PlayOptions::once()
            .with_track(2)
            .with_mix(Duration::from_millis(200))
            .with_speed(0.5)
            .with_delay(Duration::from_millis(50))
            .queued();
        assert_eq!(o.track, 2);
        assert_eq!(o.mix_duration, Duration::from_millis(200));
        assert_eq!(o.speed, 0.5);
        assert_eq!(o.delay, Duration::from_millis(50));
        assert!(o.queued);
        assert!(!o.looping);
        assert_eq!(o.handover_at, None);
    }

    #[test]
    fn with_handover_at_sets_the_takeover_time() {
        let o = PlayOptions::once().queued().with_handover_at(Duration::from_millis(750));
        assert_eq!(o.handover_at, Some(Duration::from_millis(750)));
        assert!(o.queued);
    }

    #[test]
    fn current_animation_defaults_to_none() {
        struct Bare;
        impl AnimatedModel for Bare {
            fn update(&mut self, _: Duration) -> Result<(), RuntimeError> {
                Ok(())
            }
            fn emit(&self, _: &mut RenderList) {}
            fn play_animation(&mut self, _: &str, _: PlayOptions) -> Result<(), RuntimeError> {
                Ok(())
            }
            fn stop_animation(&mut self, _: &str) {}
            fn set_expression(&mut self, _: &str) -> Result<(), RuntimeError> {
                Ok(())
            }
            fn animations(&self) -> &[AnimationInfo] {
                &[]
            }
            fn expressions(&self) -> &[ExpressionInfo] {
                &[]
            }
            fn bounds(&self) -> Aabb {
                Aabb::EMPTY
            }
            fn hit_test(&self, _: f32, _: f32) -> Option<HitAreaId> {
                None
            }
            fn dispose(&mut self) {}
            fn display_name(&self) -> &str {
                "bare"
            }
        }
        assert_eq!(Bare.current_animation(), None);
    }
}
