//! Animation timelines.
//!
//! Every timeline stores its own interpolation *per animated component*. Spine
//! 3.x shares one curve across a keyframe's components and 4.x gives each its
//! own; normalising to the richer 4.x shape lets 3.x decoders duplicate a curve
//! and keeps the runtime version-blind (spec §7).

use crate::ir::ids::{
    AttachmentId, BoneId, EventId, IkConstraintId, PathConstraintId, SkinId, SlotId, TransformConstraintId,
};
use crate::math::{Interpolation, Rgb, Rgba, Vec2};

/// Which components of a two-component timeline are actually animated.
///
/// Spine 4.x can key a single axis (`translatex`, `scaley`, `shearx`), leaving
/// the other axis under whatever the setup pose or a lower track supplies.
/// Spine 3.x always keys both, so its decoder always emits [`Axes::Both`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Axes {
    #[default]
    Both,
    X,
    Y,
}

impl Axes {
    #[inline]
    pub fn has_x(self) -> bool {
        matches!(self, Axes::Both | Axes::X)
    }

    #[inline]
    pub fn has_y(self) -> bool {
        matches!(self, Axes::Both | Axes::Y)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Axes::Both => "xy",
            Axes::X => "x",
            Axes::Y => "y",
        }
    }
}

/// Which channels of a colour timeline are animated.
///
/// Spine 4.x splits colour into `rgba`, `rgb` and `alpha` timelines; an `rgb`
/// timeline must leave the slot's alpha alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChannels {
    #[default]
    Rgba,
    Rgb,
}

impl ColorChannels {
    #[inline]
    pub fn has_alpha(self) -> bool {
        matches!(self, ColorChannels::Rgba)
    }

    /// Number of independently-curved components, which is what the curve
    /// reader needs to know.
    #[inline]
    pub fn component_count(self) -> usize {
        match self {
            ColorChannels::Rgba => 4,
            ColorChannels::Rgb => 3,
        }
    }
}

/// A keyframe holding one scalar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarKey {
    pub time: f32,
    pub value: f32,
    pub interp: Interpolation,
}

/// A keyframe holding two independently-curved scalars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2Key {
    pub time: f32,
    pub value: Vec2,
    pub interp_x: Interpolation,
    pub interp_y: Interpolation,
}

impl Vec2Key {
    /// Builds a key whose components share one curve, as Spine 3.x stores them.
    pub fn shared(time: f32, value: Vec2, interp: Interpolation) -> Self {
        Vec2Key { time, value, interp_x: interp, interp_y: interp }
    }
}

/// A keyframe holding an RGBA colour, one curve per channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorKey {
    pub time: f32,
    pub value: Rgba,
    /// Curves for r, g, b, a in that order.
    pub interp: [Interpolation; 4],
}

impl ColorKey {
    pub fn shared(time: f32, value: Rgba, interp: Interpolation) -> Self {
        ColorKey { time, value, interp: [interp; 4] }
    }
}

/// A keyframe for Spine two-colour tinting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoColorKey {
    pub time: f32,
    pub light: Rgba,
    pub dark: Rgb,
    /// Curves for light r, g, b, a.
    pub interp_light: [Interpolation; 4],
    /// Curves for dark r, g, b.
    pub interp_dark: [Interpolation; 3],
}

impl TwoColorKey {
    pub fn shared(time: f32, light: Rgba, dark: Rgb, interp: Interpolation) -> Self {
        TwoColorKey { time, light, dark, interp_light: [interp; 4], interp_dark: [interp; 3] }
    }
}

/// Switches which attachment a slot shows. Always stepped.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentKey {
    pub time: f32,
    /// Placeholder name, or `None` to hide the slot.
    pub name: Option<String>,
}

/// A mesh deformation keyframe.
///
/// Values are offsets added to the attachment's setup vertices. They are stored
/// sparsely: `values` applies starting at float index `offset`, and everything
/// outside that window is zero. Both Spine dialects write this shape.
#[derive(Debug, Clone, PartialEq)]
pub struct DeformKey {
    pub time: f32,
    pub offset: u32,
    pub values: Vec<f32>,
    pub interp: Interpolation,
}

impl DeformKey {
    /// Reads the offset for float index `i`, treating the sparse window as zero
    /// outside its bounds.
    #[inline]
    pub fn value_at(&self, i: usize) -> f32 {
        let start = self.offset as usize;
        if i < start {
            return 0.0;
        }
        self.values.get(i - start).copied().unwrap_or(0.0)
    }
}

/// A draw-order keyframe.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawOrderKey {
    pub time: f32,
    /// Slots in draw order. `None` restores the setup-pose order.
    pub order: Option<Vec<SlotId>>,
}

/// A fired event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventKey {
    pub time: f32,
    pub event: EventId,
    pub int_value: i32,
    pub float_value: f32,
    /// Overrides the event's default string when present.
    pub string_value: Option<String>,
    pub volume: f32,
    pub balance: f32,
}

/// An IK constraint mix keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IkKey {
    pub time: f32,
    pub mix: f32,
    pub softness: f32,
    pub bend_positive: bool,
    pub compress: bool,
    pub stretch: bool,
    pub interp: Interpolation,
}

/// A transform constraint mix keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformKey {
    pub time: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub mix_scale_x: f32,
    pub mix_scale_y: f32,
    pub mix_shear_y: f32,
    pub interp: Interpolation,
}

/// A path constraint mix keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathMixKey {
    pub time: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub interp: Interpolation,
}

/// Declares an event's default payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EventData {
    pub name: String,
    pub int_value: i32,
    pub float_value: f32,
    pub string_value: String,
    /// Audio path, for events that trigger sound. The viewer does not play
    /// audio; this is preserved so it is not lost on re-export.
    pub audio_path: Option<String>,
    pub volume: f32,
    pub balance: f32,
}

/// One animated channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Timeline {
    BoneRotate {
        bone: BoneId,
        keys: Vec<ScalarKey>,
    },
    BoneTranslate {
        bone: BoneId,
        axes: Axes,
        keys: Vec<Vec2Key>,
    },
    BoneScale {
        bone: BoneId,
        axes: Axes,
        keys: Vec<Vec2Key>,
    },
    BoneShear {
        bone: BoneId,
        axes: Axes,
        keys: Vec<Vec2Key>,
    },
    SlotColor {
        slot: SlotId,
        channels: ColorChannels,
        keys: Vec<ColorKey>,
    },
    SlotTwoColor {
        slot: SlotId,
        channels: ColorChannels,
        keys: Vec<TwoColorKey>,
    },
    /// Spine 4.x can key alpha on its own, without the RGB channels.
    SlotAlpha {
        slot: SlotId,
        keys: Vec<ScalarKey>,
    },
    SlotAttachment {
        slot: SlotId,
        keys: Vec<AttachmentKey>,
    },
    Deform {
        slot: SlotId,
        skin: SkinId,
        attachment: AttachmentId,
        keys: Vec<DeformKey>,
    },
    DrawOrder {
        keys: Vec<DrawOrderKey>,
    },
    Event {
        keys: Vec<EventKey>,
    },
    IkConstraint {
        constraint: IkConstraintId,
        keys: Vec<IkKey>,
    },
    TransformConstraint {
        constraint: TransformConstraintId,
        keys: Vec<TransformKey>,
    },
    PathPosition {
        constraint: PathConstraintId,
        keys: Vec<ScalarKey>,
    },
    PathSpacing {
        constraint: PathConstraintId,
        keys: Vec<ScalarKey>,
    },
    PathMix {
        constraint: PathConstraintId,
        keys: Vec<PathMixKey>,
    },
}

impl Timeline {
    /// Stable name for reports and `inspect` output.
    pub fn type_name(&self) -> &'static str {
        match self {
            Timeline::BoneRotate { .. } => "bone rotate",
            Timeline::BoneTranslate { .. } => "bone translate",
            Timeline::BoneScale { .. } => "bone scale",
            Timeline::BoneShear { .. } => "bone shear",
            Timeline::SlotColor { .. } => "slot color",
            Timeline::SlotTwoColor { .. } => "slot two-color",
            Timeline::SlotAlpha { .. } => "slot alpha",
            Timeline::SlotAttachment { .. } => "attachment",
            Timeline::Deform { .. } => "deform",
            Timeline::DrawOrder { .. } => "draw order",
            Timeline::Event { .. } => "event",
            Timeline::IkConstraint { .. } => "ik constraint",
            Timeline::TransformConstraint { .. } => "transform constraint",
            Timeline::PathPosition { .. } => "path position",
            Timeline::PathSpacing { .. } => "path spacing",
            Timeline::PathMix { .. } => "path mix",
        }
    }

    /// Time of the last keyframe, or 0 when the timeline is empty.
    pub fn last_time(&self) -> f32 {
        fn last<T>(keys: &[T], time: impl Fn(&T) -> f32) -> f32 {
            keys.last().map(time).unwrap_or(0.0)
        }
        match self {
            Timeline::BoneRotate { keys, .. } => last(keys, |k| k.time),
            Timeline::BoneTranslate { keys, .. }
            | Timeline::BoneScale { keys, .. }
            | Timeline::BoneShear { keys, .. } => last(keys, |k| k.time),
            Timeline::SlotColor { keys, .. } => last(keys, |k| k.time),
            Timeline::SlotTwoColor { keys, .. } => last(keys, |k| k.time),
            Timeline::SlotAlpha { keys, .. } => last(keys, |k| k.time),
            Timeline::SlotAttachment { keys, .. } => last(keys, |k| k.time),
            Timeline::Deform { keys, .. } => last(keys, |k| k.time),
            Timeline::DrawOrder { keys } => last(keys, |k| k.time),
            Timeline::Event { keys } => last(keys, |k| k.time),
            Timeline::IkConstraint { keys, .. } => last(keys, |k| k.time),
            Timeline::TransformConstraint { keys, .. } => last(keys, |k| k.time),
            Timeline::PathPosition { keys, .. } | Timeline::PathSpacing { keys, .. } => last(keys, |k| k.time),
            Timeline::PathMix { keys, .. } => last(keys, |k| k.time),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Timeline::BoneRotate { keys, .. } => keys.is_empty(),
            Timeline::BoneTranslate { keys, .. }
            | Timeline::BoneScale { keys, .. }
            | Timeline::BoneShear { keys, .. } => keys.is_empty(),
            Timeline::SlotColor { keys, .. } => keys.is_empty(),
            Timeline::SlotTwoColor { keys, .. } => keys.is_empty(),
            Timeline::SlotAlpha { keys, .. } => keys.is_empty(),
            Timeline::SlotAttachment { keys, .. } => keys.is_empty(),
            Timeline::Deform { keys, .. } => keys.is_empty(),
            Timeline::DrawOrder { keys } => keys.is_empty(),
            Timeline::Event { keys } => keys.is_empty(),
            Timeline::IkConstraint { keys, .. } => keys.is_empty(),
            Timeline::TransformConstraint { keys, .. } => keys.is_empty(),
            Timeline::PathPosition { keys, .. } | Timeline::PathSpacing { keys, .. } => keys.is_empty(),
            Timeline::PathMix { keys, .. } => keys.is_empty(),
        }
    }
}

/// A named animation: a bag of timelines plus its length.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub name: String,
    /// Length of one pass, in seconds.
    pub duration: f32,
    pub timelines: Vec<Timeline>,
}

impl Animation {
    pub fn new(name: impl Into<String>) -> Self {
        Animation { name: name.into(), duration: 0.0, timelines: Vec::new() }
    }

    /// Recomputes `duration` as the latest keyframe across all timelines.
    ///
    /// Source files record a duration, but exports do occasionally disagree
    /// with their own keyframes; decoders take the larger of the two.
    pub fn max_key_time(&self) -> f32 {
        self.timelines.iter().map(Timeline::last_time).fold(0.0, f32::max)
    }
}

/// Finds the index of the last keyframe at or before `time`.
///
/// Returns `None` when `time` precedes the first key. Keys are required to be
/// sorted by time; decoders sort them, so this can binary search.
pub fn search_keys<T>(keys: &[T], time: f32, key_time: impl Fn(&T) -> f32) -> Option<usize> {
    if keys.is_empty() || time < key_time(&keys[0]) {
        return None;
    }
    // Upper bound: first index with key_time > time.
    let (mut lo, mut hi) = (0usize, keys.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if key_time(&keys[mid]) <= time {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Some(lo - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_curve_constructors_duplicate_the_curve() {
        let k = Vec2Key::shared(0.0, Vec2::ONE, Interpolation::Stepped);
        assert_eq!(k.interp_x, Interpolation::Stepped);
        assert_eq!(k.interp_y, Interpolation::Stepped);

        let c = ColorKey::shared(0.0, Rgba::WHITE, Interpolation::Stepped);
        assert!(c.interp.iter().all(|i| *i == Interpolation::Stepped));

        let t = TwoColorKey::shared(0.0, Rgba::WHITE, Rgb::BLACK, Interpolation::Linear);
        assert!(t.interp_light.iter().all(|i| *i == Interpolation::Linear));
        assert!(t.interp_dark.iter().all(|i| *i == Interpolation::Linear));
    }

    #[test]
    fn sparse_deform_reads_zero_outside_its_window() {
        let k = DeformKey { time: 0.0, offset: 4, values: vec![1.0, 2.0], interp: Interpolation::Linear };
        assert_eq!(k.value_at(0), 0.0);
        assert_eq!(k.value_at(3), 0.0);
        assert_eq!(k.value_at(4), 1.0);
        assert_eq!(k.value_at(5), 2.0);
        assert_eq!(k.value_at(6), 0.0);
        assert_eq!(k.value_at(1000), 0.0);
    }

    fn scalar_keys(times: &[f32]) -> Vec<ScalarKey> {
        times.iter().map(|&t| ScalarKey { time: t, value: t * 10.0, interp: Interpolation::Linear }).collect()
    }

    #[test]
    fn key_search_finds_the_key_at_or_before_the_time() {
        let keys = scalar_keys(&[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(search_keys(&keys, 0.0, |k| k.time), Some(0));
        assert_eq!(search_keys(&keys, 0.5, |k| k.time), Some(0));
        assert_eq!(search_keys(&keys, 1.0, |k| k.time), Some(1));
        assert_eq!(search_keys(&keys, 2.999, |k| k.time), Some(2));
        assert_eq!(search_keys(&keys, 3.0, |k| k.time), Some(3));
    }

    #[test]
    fn key_search_past_the_end_returns_the_last_key() {
        let keys = scalar_keys(&[0.0, 1.0]);
        assert_eq!(search_keys(&keys, 99.0, |k| k.time), Some(1));
    }

    #[test]
    fn key_search_before_the_first_key_returns_none() {
        let keys = scalar_keys(&[1.0, 2.0]);
        assert_eq!(search_keys(&keys, 0.5, |k| k.time), None);
    }

    #[test]
    fn key_search_on_empty_timeline_returns_none() {
        let keys: Vec<ScalarKey> = vec![];
        assert_eq!(search_keys(&keys, 0.0, |k| k.time), None);
    }

    #[test]
    fn key_search_with_duplicate_times_picks_the_last_of_the_run() {
        let keys = scalar_keys(&[0.0, 1.0, 1.0, 1.0, 2.0]);
        assert_eq!(search_keys(&keys, 1.0, |k| k.time), Some(3));
    }

    #[test]
    fn key_search_matches_a_linear_scan_over_many_sizes() {
        for n in 0..40usize {
            let times: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();
            let keys = scalar_keys(&times);
            for step in 0..(n * 2 + 4) {
                let t = step as f32 * 0.25 - 0.25;
                let expected = keys.iter().rposition(|k| k.time <= t);
                assert_eq!(search_keys(&keys, t, |k| k.time), expected, "n={n} t={t}");
            }
        }
    }

    #[test]
    fn animation_duration_comes_from_the_latest_key() {
        let mut a = Animation::new("idle");
        a.timelines.push(Timeline::BoneRotate { bone: BoneId(0), keys: scalar_keys(&[0.0, 1.5]) });
        a.timelines.push(Timeline::DrawOrder { keys: vec![DrawOrderKey { time: 2.25, order: None }] });
        assert_eq!(a.max_key_time(), 2.25);
    }

    #[test]
    fn empty_animation_has_zero_duration() {
        assert_eq!(Animation::new("empty").max_key_time(), 0.0);
    }

    #[test]
    fn empty_timelines_report_empty_and_zero_last_time() {
        let t = Timeline::BoneRotate { bone: BoneId(0), keys: vec![] };
        assert!(t.is_empty());
        assert_eq!(t.last_time(), 0.0);
    }
}
