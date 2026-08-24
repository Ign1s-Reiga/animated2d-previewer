//! Keyframe interpolation shared by every timeline in the IR.
//!
//! Source formats disagree on how Bezier segments are stored: Spine 3.x writes
//! four raw control values, Spine 4.x writes a pre-sampled table. Decoders
//! normalise both into [`Interpolation::Bezier`] control points, and the runtime
//! solves them exactly. That keeps version quirks inside `formats/`.

/// How a keyframe blends into the next one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Interpolation {
    /// Constant-rate blend.
    #[default]
    Linear,
    /// Hold the current value until the next keyframe time is reached.
    Stepped,
    /// Cubic Bezier easing in normalised keyframe space.
    Bezier(Bezier),
}

impl Interpolation {
    /// Maps a normalised time `t` in `0..=1` between two keyframes to a
    /// normalised blend factor.
    #[inline]
    pub fn apply(self, t: f32) -> f32 {
        match self {
            Interpolation::Linear => t,
            Interpolation::Stepped => 0.0,
            Interpolation::Bezier(b) => b.evaluate(t),
        }
    }
}

/// Cubic Bezier easing curve with implicit endpoints `(0,0)` and `(1,1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bezier {
    pub cx1: f32,
    pub cy1: f32,
    pub cx2: f32,
    pub cy2: f32,
}

impl Bezier {
    /// Newton refinement steps. Eight is well past convergence for the control
    /// point ranges authoring tools produce, and keeps evaluation branch-free.
    const NEWTON_ITERATIONS: usize = 8;
    const NEWTON_EPSILON: f32 = 1e-6;

    #[inline]
    pub const fn new(cx1: f32, cy1: f32, cx2: f32, cy2: f32) -> Self {
        Bezier { cx1, cy1, cx2, cy2 }
    }

    /// Evaluates the eased value for a normalised input time.
    pub fn evaluate(self, t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        cubic(self.cy1, self.cy2, self.solve_parameter(t))
    }

    /// Finds the curve parameter `u` such that `x(u) == t`.
    fn solve_parameter(self, t: f32) -> f32 {
        // Newton-Raphson first; it converges in two or three steps for the
        // monotone curves authoring tools emit.
        let mut u = t;
        for _ in 0..Self::NEWTON_ITERATIONS {
            let x = cubic(self.cx1, self.cx2, u) - t;
            if x.abs() < Self::NEWTON_EPSILON {
                return u;
            }
            let dx = cubic_derivative(self.cx1, self.cx2, u);
            if dx.abs() < 1e-6 {
                break;
            }
            u -= x / dx;
            if !(0.0..=1.0).contains(&u) {
                break;
            }
        }

        // Bisection fallback. Handles non-monotone or degenerate control points,
        // which malformed assets do produce, without looping forever.
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let mut u = t.clamp(0.0, 1.0);
        for _ in 0..32 {
            let x = cubic(self.cx1, self.cx2, u);
            if (x - t).abs() < Self::NEWTON_EPSILON {
                break;
            }
            if x < t {
                lo = u;
            } else {
                hi = u;
            }
            u = (lo + hi) * 0.5;
        }
        u
    }
}

/// Cubic Bezier with endpoints pinned to 0 and 1.
#[inline]
fn cubic(c1: f32, c2: f32, u: f32) -> f32 {
    let inv = 1.0 - u;
    3.0 * inv * inv * u * c1 + 3.0 * inv * u * u * c2 + u * u * u
}

#[inline]
fn cubic_derivative(c1: f32, c2: f32, u: f32) -> f32 {
    let inv = 1.0 - u;
    3.0 * inv * inv * c1 + 6.0 * inv * u * (c2 - c1) + 3.0 * u * u * (1.0 - c2)
}

/// Linear interpolation between two scalars.
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_the_identity() {
        for t in [0.0f32, 0.25, 0.5, 1.0] {
            assert_eq!(Interpolation::Linear.apply(t), t);
        }
    }

    #[test]
    fn stepped_holds_until_the_next_key() {
        assert_eq!(Interpolation::Stepped.apply(0.0), 0.0);
        assert_eq!(Interpolation::Stepped.apply(0.999), 0.0);
    }

    #[test]
    fn bezier_with_linear_control_points_is_linear() {
        let b = Bezier::new(1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0);
        for t in [0.1f32, 0.25, 0.5, 0.75, 0.9] {
            assert!((b.evaluate(t) - t).abs() < 1e-3, "t={t} -> {}", b.evaluate(t));
        }
    }

    #[test]
    fn bezier_endpoints_are_pinned() {
        let b = Bezier::new(0.9, 0.0, 0.1, 1.0);
        assert_eq!(b.evaluate(0.0), 0.0);
        assert_eq!(b.evaluate(1.0), 1.0);
        assert_eq!(b.evaluate(-0.5), 0.0);
        assert_eq!(b.evaluate(2.0), 1.0);
    }

    #[test]
    fn ease_in_curve_lags_behind_linear() {
        // Classic ease-in: slow start, fast finish.
        let b = Bezier::new(0.42, 0.0, 1.0, 1.0);
        assert!(b.evaluate(0.25) < 0.25);
        assert!(b.evaluate(0.5) < 0.5);
    }

    #[test]
    fn ease_out_curve_leads_linear() {
        let b = Bezier::new(0.0, 0.0, 0.58, 1.0);
        assert!(b.evaluate(0.25) > 0.25);
    }

    #[test]
    fn bezier_is_monotone_for_well_formed_control_points() {
        let b = Bezier::new(0.42, 0.0, 0.58, 1.0);
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = b.evaluate(i as f32 / 100.0);
            assert!(v >= prev - 1e-4, "not monotone at {i}: {prev} -> {v}");
            prev = v;
        }
    }

    #[test]
    fn degenerate_control_points_terminate_and_stay_in_range() {
        // A flat-x curve has no unique solution; it must still return a usable value.
        let b = Bezier::new(0.0, 0.5, 0.0, 0.5);
        for i in 0..=20 {
            let v = b.evaluate(i as f32 / 20.0);
            assert!((0.0..=1.0).contains(&v) && v.is_finite(), "{v}");
        }
    }

    #[test]
    fn scalar_lerp_hits_endpoints() {
        assert_eq!(lerp(2.0, 10.0, 0.0), 2.0);
        assert_eq!(lerp(2.0, 10.0, 1.0), 10.0);
        assert_eq!(lerp(2.0, 10.0, 0.5), 6.0);
    }
}
