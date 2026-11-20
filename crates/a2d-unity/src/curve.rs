//! Unity's `AnimationCurve`, as it sits inside a serialized `MonoBehaviour`.
//!
//! # Layout (UNVERIFIED against a real bundle)
//!
//! Recalled from the serialization used by Unity 2018.1 and later as other
//! open readers describe it; no real bundle in hand has been used to confirm it
//! (CLAUDE.md §16: this records what the claim rests on). All little-endian:
//!
//! ```text
//! i32 keyCount
//! keyCount x Keyframe   (28 bytes)
//!     f32 time · f32 value · f32 inSlope · f32 outSlope
//!     i32 weightedMode · f32 inWeight · f32 outWeight
//! i32 m_PreInfinity · i32 m_PostInfinity · i32 m_RotationOrder
//! ```
//!
//! Older Unity versions omit the weighted fields (20-byte keys). That layout is
//! not read: a reader that guessed between the two would corrupt silently, so a
//! mismatch is left to the caller's exact-length check to expose.
//!
//! An infinite tangent is how Unity marks a constant (stepped) segment; this
//! reader returns slopes untouched and leaves that interpretation to callers.

/// One key of an [`AnimationCurve`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    pub time: f32,
    pub value: f32,
    pub in_slope: f32,
    pub out_slope: f32,
    /// Unity's `WeightedMode`: 0 none, 1 in, 2 out, 3 both.
    pub weighted_mode: i32,
    pub in_weight: f32,
    pub out_weight: f32,
}

/// A keyed curve, as serialized.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationCurve {
    pub keys: Vec<Keyframe>,
    pub pre_infinity: i32,
    pub post_infinity: i32,
    pub rotation_order: i32,
}

/// Serialized size of one [`Keyframe`].
pub(crate) const KEYFRAME_BYTES: usize = 28;
/// Smallest serialized curve: an empty key list and three `i32`s.
pub(crate) const MIN_CURVE_BYTES: usize = 4 + 12;
