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
    pub queued: bool,
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

    pub fn queued(mut self) -> Self {
        self.queued = true;
        self
    }
}

/// What the viewer needs to list an animation in a selector.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationInfo {
    pub name: String,
    /// Length of one pass, in seconds.
    pub duration: f32,
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
/// `render` is deliberately split into [`AnimatedModel::emit`] plus a renderer,
/// so that no GPU type reaches this crate.
pub trait AnimatedModel {
    /// Advances animation state by `dt`. Evaluation is delta-time based and
    /// independent of rendering frame rate (spec §12).
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError>;

    /// Appends this model's current frame to `out`.
    fn emit(&self, out: &mut RenderList);

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

    /// Releases any retained resources. Idempotent.
    fn dispose(&mut self);

    /// Name the viewer should show. Defaults to the package display name.
    fn display_name(&self) -> &str;

    /// Whether the model has an animation with this name.
    fn has_animation(&self, name: &str) -> bool {
        self.animations().iter().any(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
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
    }
}
