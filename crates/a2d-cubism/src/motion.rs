//! Evaluating a model's own motions.
//!
//! Lives here rather than in `a2d-runtime` because CLAUDE.md §5 keeps Spine and
//! Cubism from sharing animation machinery below `AnimatedModel`.
//!
//! Curves are sampled as authored: cubic Hermite between the keys, with no
//! resampling to a fixed step, so the easing the author drew is the easing that
//! plays. Fades are stored in the IR but not applied here.

use a2d_core::ir::cubism::{CubismIr, CurveKey};

/// Samples a curve at `t` seconds.
///
/// Holds the first value before the first key and the last value after the last
/// key. A segment leaving a `stepped` key holds that key's value until the next
/// key. An empty curve has nothing to say and returns `None`, so the parameter
/// keeps whatever it had rather than being forced to zero.
pub fn sample(keys: &[CurveKey], t: f32) -> Option<f32> {
    let first = keys.first()?;
    let last = keys.last()?;
    // Index of the first key strictly after `t`. A NaN time compares false
    // everywhere and so lands on the first key, which is a safe answer.
    let next = keys.partition_point(|k| k.time <= t);
    if next == 0 {
        return Some(first.value);
    }
    if next >= keys.len() {
        return Some(last.value);
    }
    let (k0, k1) = (&keys[next - 1], &keys[next]);
    if k0.stepped {
        return Some(k0.value);
    }
    let span = k1.time - k0.time;
    // Finite keys can still be far enough apart that the span overflows, which
    // would turn the division below into 0 * inf. Treat it like a zero span.
    if !span.is_finite() || span <= f32::EPSILON {
        return Some(k1.value);
    }
    let s = (t - k0.time) / span;
    let (s2, s3) = (s * s, s * s * s);
    // Hermite basis. The slopes are per second, so they scale by the span to
    // become the per-segment tangents the basis expects.
    let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
    let h10 = s3 - 2.0 * s2 + s;
    let h01 = -2.0 * s3 + 3.0 * s2;
    let h11 = s3 - s2;
    let v = h00 * k0.value + h10 * span * k0.out_slope + h01 * k1.value + h11 * span * k1.in_slope;
    // Huge opposite tangent terms can still overflow to inf - inf. A finite key
    // value is a better answer than NaN, which `Parameter::clamp` lets through.
    Some(if v.is_finite() { v } else { k0.value })
}

/// Maps a playhead onto a motion of `duration` seconds.
///
/// Looping wraps (also for a negative playhead, which backwards playback makes);
/// otherwise the playhead is clamped to the motion. A zero or negative duration
/// has nowhere to go and gives `0.0`.
pub fn wrap_time(t: f32, duration: f32, looping: bool) -> f32 {
    if duration.is_nan() || duration <= 0.0 || !t.is_finite() {
        return 0.0;
    }
    if looping {
        t.rem_euclid(duration)
    } else {
        t.clamp(0.0, duration)
    }
}

/// A motion being played: its curves bound to parameter indices once, plus a
/// playhead.
#[derive(Debug, Clone)]
pub struct MotionPlayer {
    motion: usize,
    /// `(curve index, parameter index)` for every curve that resolved.
    bindings: Vec<(usize, usize)>,
    time: f32,
    looping: bool,
    speed: f32,
    finished: bool,
}

impl MotionPlayer {
    /// Binds motion `motion` of `ir`. `None` if there is no such motion.
    pub fn new(ir: &CubismIr, motion: usize, looping: bool, speed: f32) -> Option<MotionPlayer> {
        let m = ir.motions.get(motion)?;
        let mut bindings = Vec::new();
        for (ci, c) in m.curves.iter().enumerate() {
            // A curve that resolves to nothing is simply not bound; it is
            // reported through `CubismIr::unresolved_motion_targets`.
            if let Some(pi) = ir.motion_target(&c.target) {
                bindings.push((ci, pi));
            }
        }
        let speed = if speed.is_finite() { speed } else { 1.0 };
        // Backwards playback starts at the end, looping or not; starting at 0 would
        // count the very first step as a finished pass.
        let start = if speed < 0.0 { m.duration.max(0.0) } else { 0.0 };
        Some(MotionPlayer { motion, bindings, time: start, looping, speed, finished: false })
    }

    pub fn motion_index(&self) -> usize {
        self.motion
    }

    /// Playhead within the motion, in seconds. Used by tests.
    #[cfg(test)]
    pub fn playhead(&self) -> f32 {
        self.time
    }

    /// Whether a non-looping motion has reached its end.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Moves the playhead by `dt` seconds of wall time, scaled by the speed.
    ///
    /// Returns `Some(leftover)` when a pass ended during this step: a looping
    /// motion wrapped, or a non-looping one reached its end. `leftover` is the
    /// wall-clock time that was past the end, so a queued motion taking over
    /// can start that far in rather than dropping it. A step of zero or less
    /// time never ends a pass.
    pub fn advance(&mut self, ir: &CubismIr, dt: f32) -> Option<f32> {
        let Some(m) = ir.motions.get(self.motion) else {
            self.finished = true;
            return Some(0.0);
        };
        if dt <= 0.0 || dt.is_nan() {
            // No time passed: leave the playhead alone. Wrapping it here would
            // turn a backwards motion's start at `duration` into 0.
            return None;
        }
        let raw = self.time + dt * self.speed;
        self.time = wrap_time(raw, m.duration, self.looping);
        let past = if self.speed > 0.0 {
            raw - m.duration
        } else if self.speed < 0.0 {
            -raw
        } else {
            -1.0
        };
        let ran_off = past >= 0.0;
        if !self.looping {
            // Running off either end of a motion that does not loop ends it.
            self.finished = ran_off;
        }
        ran_off.then(|| if self.speed != 0.0 { past / self.speed.abs() } else { 0.0 })
    }

    /// Writes this motion's value at the playhead into `values`, clamped to
    /// each parameter's range. Curves with no keys leave their parameter alone.
    pub fn apply(&self, ir: &CubismIr, values: &mut [f32]) {
        let Some(m) = ir.motions.get(self.motion) else { return };
        for &(ci, pi) in &self.bindings {
            let (Some(curve), Some(param)) = (m.curves.get(ci), ir.parameters.get(pi)) else { continue };
            let Some(v) = sample(&curve.keys, self.time) else { continue };
            if let Some(slot) = values.get_mut(pi) {
                *slot = param.clamp(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(time: f32, value: f32, in_slope: f32, out_slope: f32) -> CurveKey {
        CurveKey { time, value, in_slope, out_slope, stepped: false }
    }

    #[test]
    fn a_linear_segment_is_exact_at_the_midpoint() {
        // Chord from (0, 0) to (2, 4) is slope 2; the midpoint is 2.
        let keys = [key(0.0, 0.0, 2.0, 2.0), key(2.0, 4.0, 2.0, 2.0)];
        assert!((sample(&keys, 1.0).unwrap() - 2.0).abs() < 1e-6);
        assert!((sample(&keys, 0.5).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_hermite_segment_matches_a_hand_computed_cubic() {
        // v0 = 0, v1 = 1, span 1, m0 = m1 = 0 is smoothstep: 3s^2 - 2s^3.
        // At s = 0.25: 3/16 - 2/64 = 0.15625.
        let keys = [key(0.0, 0.0, 0.0, 0.0), key(1.0, 1.0, 0.0, 0.0)];
        assert!((sample(&keys, 0.25).unwrap() - 0.15625).abs() < 1e-6);
        // v0 = 1, v1 = 2, span 2, m0 = 1, m1 = -1, s = 0.5:
        // h00 = h01 = 0.5, h10 = 0.125, h11 = -0.125.
        // 0.5*1 + 0.125*2*1 + 0.5*2 + (-0.125)*2*(-1) = 0.5 + 0.25 + 1 + 0.25 = 2.0
        let keys = [key(0.0, 1.0, 0.0, 1.0), key(2.0, 2.0, -1.0, 0.0)];
        assert!((sample(&keys, 1.0).unwrap() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_span_that_overflows_does_not_produce_nan() {
        // Both times are finite but their difference is inf.
        let keys = [key(-3e38, 1.0, 0.0, 0.0), key(3e38, 2.0, 0.0, 0.0)];
        let v = sample(&keys, 0.0).unwrap();
        assert!(v.is_finite(), "{v}");
    }

    #[test]
    fn opposite_huge_tangents_do_not_produce_nan() {
        // h10 * span * out_slope and h11 * span * in_slope overflow with
        // opposite signs, so their sum would be inf - inf.
        let keys = [key(0.0, 1.0, 0.0, 3e38), key(4000.0, 2.0, 3e38, 0.0)];
        let v = sample(&keys, 2000.0).unwrap();
        assert!(v.is_finite(), "{v}");
        assert_eq!(v, 1.0, "falls back to the segment's starting value");
    }

    #[test]
    fn a_stepped_segment_holds_until_the_next_key() {
        let mut k0 = key(0.0, 3.0, 0.0, 0.0);
        k0.stepped = true;
        let keys = [k0, key(1.0, 7.0, 0.0, 0.0), key(2.0, 9.0, 0.0, 0.0)];
        assert_eq!(sample(&keys, 0.0), Some(3.0));
        assert_eq!(sample(&keys, 0.999), Some(3.0));
        assert_eq!(sample(&keys, 1.0), Some(7.0));
    }

    #[test]
    fn it_holds_before_the_first_key_and_after_the_last() {
        let keys = [key(1.0, 5.0, 0.0, 0.0), key(2.0, 8.0, 0.0, 0.0)];
        assert_eq!(sample(&keys, -3.0), Some(5.0));
        assert_eq!(sample(&keys, 0.5), Some(5.0));
        assert_eq!(sample(&keys, 2.0), Some(8.0));
        assert_eq!(sample(&keys, 99.0), Some(8.0));
    }

    #[test]
    fn a_single_key_is_a_constant_and_an_empty_curve_is_nothing() {
        let keys = [key(0.5, 4.0, 9.0, 9.0)];
        assert_eq!(sample(&keys, 0.0), Some(4.0));
        assert_eq!(sample(&keys, 100.0), Some(4.0));
        assert_eq!(sample(&[], 1.0), None);
    }

    #[test]
    fn a_nan_time_does_not_panic() {
        let keys = [key(0.0, 1.0, 0.0, 0.0), key(1.0, 2.0, 0.0, 0.0)];
        assert_eq!(sample(&keys, f32::NAN), Some(1.0));
    }

    #[test]
    fn looping_wraps_and_non_looping_clamps() {
        assert_eq!(wrap_time(2.0, 2.0, true), 0.0);
        assert!((wrap_time(2.5, 2.0, true) - 0.5).abs() < 1e-6);
        assert!((wrap_time(-0.5, 2.0, true) - 1.5).abs() < 1e-6);
        assert_eq!(wrap_time(2.0, 2.0, false), 2.0);
        assert_eq!(wrap_time(5.0, 2.0, false), 2.0);
        assert_eq!(wrap_time(-1.0, 2.0, false), 0.0);
        assert_eq!(wrap_time(1.0, 0.0, true), 0.0);
    }
}
