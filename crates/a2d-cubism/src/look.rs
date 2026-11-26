//! The look-at rig at playback: a smoothed position that nudges parameters.
//!
//! [`LookState`] holds a rig's smoothed position, the velocity of that smoothing
//! and the point the host last asked the model to look at. Each update moves the
//! position toward a goal and the model then writes the rig's parameters from it
//! (see `GenericCubismModel`): after the motion or idle wrote its values and
//! before physics reads them.
//!
//! # The goal
//!
//! The goal is `point - center`, per component (see [`LookRig`] for the space and
//! the units). The point is the driven target with its z at `rest_target.2`, or
//! the rest target when nothing drives the rig. A rig starts *at zero*, as the
//! SDK's look controller starts its smoothed value, and eases to the goal with
//! the smoothing below (about half a second at a damping of 0.15 s), so the
//! first frames of playback carry no look offset. Releasing the target eases
//! back to the rest goal. [`LookState::rest`] puts the position at the settled
//! rest goal instead: the deterministic still pose (`pose_at`) uses it, because a
//! pose must be a function of the timestamp alone and not of how long the model
//! has been running.
//!
//! # The smoothing
//!
//! [`smooth_damp`] is the critically damped spring Unity publishes as
//! `Vector3.SmoothDamp`, which in turn is the one from *Game
//! Programming Gems 4*, chapter 1.10 ("Critically Damped Ease-In/Ease-Out
//! Smoothing", Thomas Lowe). With `omega = 2 / smooth_time` and
//! `x = omega * dt` it replaces the exact exponential decay with the cubic
//! `exp = 1 / (1 + x + 0.48 x^2 + 0.235 x^3)`:
//!
//! ```text
//! change   = current - target
//! temp     = (velocity + omega * change) * dt
//! velocity = (velocity - omega * temp) * exp
//! output   = target + (change + temp) * exp
//! ```
//!
//! for each of the three components, and then a single check on the vector as
//! Unity's `Vector3` version makes it: if `dot(goal - current, output - goal)`
//! is positive the step overshot, and the output is the goal and the velocity
//! zero for the whole vector (not per component). The maximum speed is
//! unbounded. The smoothing time is the
//! rig's damping, floored at `1e-4` s as Unity floors it, so a damping of zero
//! is a jump to within a few parts in a hundred thousand rather than a division
//! by zero.

use a2d_core::ir::cubism::{LookAxis, LookBlend, LookRig, Parameter};
use a2d_core::Vec2;

/// The smallest smoothing time, in seconds, as in Unity's `SmoothDamp`.
const MIN_SMOOTH_TIME: f32 = 1e-4;

/// One step of a critically damped follow of `target` from `current`, moving
/// `velocity` along: Unity's `Vector3.SmoothDamp` with an unbounded speed. A
/// step of zero (or less, or not finite) changes nothing.
pub(crate) fn smooth_damp(
    current: [f32; 3],
    target: [f32; 3],
    velocity: &mut [f32; 3],
    smooth_time: f32,
    dt: f32,
) -> [f32; 3] {
    let finite = |v: &[f32; 3]| v.iter().all(|c| c.is_finite());
    if !(dt > 0.0 && dt.is_finite()) || !finite(&current) || !finite(&target) {
        return current;
    }
    let smooth_time = smooth_time.max(MIN_SMOOTH_TIME);
    let omega = 2.0 / smooth_time;
    let x = omega * dt;
    let exp = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let mut output = [0.0; 3];
    for i in 0..3 {
        let change = current[i] - target[i];
        let temp = (velocity[i] + omega * change) * dt;
        velocity[i] = (velocity[i] - omega * temp) * exp;
        output[i] = target[i] + (change + temp) * exp;
    }
    // The step overshot the goal as a vector: land on it and stop, all three.
    let dot: f32 = (0..3).map(|i| (target[i] - current[i]) * (output[i] - target[i])).sum();
    if dot > 0.0 || !finite(&output) || !finite(velocity) {
        *velocity = [0.0; 3];
        return target;
    }
    output
}

fn axis_index(axis: LookAxis) -> usize {
    match axis {
        LookAxis::X => 0,
        LookAxis::Y => 1,
        LookAxis::Z => 2,
    }
}

/// A rig resolved against its model, and the state of its follow.
#[derive(Debug, Clone)]
pub(crate) struct LookState {
    blend: LookBlend,
    damping: f32,
    rest_target: [f32; 3],
    center: [f32; 3],
    /// The driven parameters: index into the model's parameters, the component
    /// of the position that feeds it, and its factor.
    slots: Vec<(usize, usize, f32)>,
    position: [f32; 3],
    velocity: [f32; 3],
    /// What the host asked to look at, in model space.
    target: Option<(f32, f32)>,
}

impl LookState {
    /// The state for `rig` on a model with `parameters`, with the smoothed
    /// position at zero and still (it eases in from there); `None` for a
    /// rig that cannot be used with them (see [`LookRig::problem`]).
    pub fn new(rig: &LookRig, parameters: &[Parameter]) -> Option<LookState> {
        if rig.problem(parameters).is_some() {
            return None;
        }
        let slots = rig
            .parameters
            .iter()
            .filter_map(|p| {
                let index = parameters.iter().position(|m| m.id == p.parameter)?;
                Some((index, axis_index(p.axis), p.factor))
            })
            .collect();
        Some(LookState {
            blend: rig.blend,
            damping: rig.damping,
            rest_target: [rig.rest_target.0, rig.rest_target.1, rig.rest_target.2],
            center: [rig.center.0, rig.center.1, rig.center.2],
            slots,
            position: [0.0; 3],
            velocity: [0.0; 3],
            target: None,
        })
    }

    /// Points the rig at `target`, or releases it. A target that is not finite
    /// releases it too.
    pub fn set_target(&mut self, target: Option<Vec2>) {
        self.target = target.filter(|t| t.x.is_finite() && t.y.is_finite()).map(|t| (t.x, t.y));
    }

    /// Where the position is heading: the driven point (z at the rest target's)
    /// or the rest target, less the centre.
    fn goal(&self) -> [f32; 3] {
        let point = match self.target {
            Some((x, y)) => [x, y, self.rest_target[2]],
            None => self.rest_target,
        };
        [point[0] - self.center[0], point[1] - self.center[1], point[2] - self.center[2]]
    }

    /// Puts the position at the settled rest goal, still, and lets go of the
    /// target (not the load state: that is zero, see [`LookState::new`]).
    pub fn rest(&mut self) {
        self.target = None;
        self.velocity = [0.0; 3];
        self.position = self.goal();
    }

    /// Moves the position `dt` seconds toward the goal.
    pub fn advance(&mut self, dt: f32) {
        self.position = smooth_damp(self.position, self.goal(), &mut self.velocity, self.damping, dt);
    }

    /// Whether there are parameters to drive.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Writes the rig's parameters into `values`, each combined with what is
    /// there and then kept in its range the way a motion's output is: wrapped for
    /// a repeating parameter, clamped otherwise.
    ///
    /// The SDK generation these rigs come from adds without clamping at this
    /// point. Whether the official Core clamps the value before deforming is not
    /// established (it is closed source and `docs/cubism-moc3.md` only records that
    /// *our* evaluation clamps), so the range is kept here as before.
    ///
    /// This reads what the motion (or idle) wrote this frame and is applied on
    /// top of it, so it must run on freshly written values, once per frame: the
    /// model rebuilds every parameter from its default each update, and an
    /// unkeyed one is a default again, so nothing accumulates.
    pub fn apply(&self, values: &mut [f32], parameters: &[Parameter], repeating: &[bool]) {
        for &(index, axis, factor) in &self.slots {
            let (Some(slot), Some(param)) = (values.get_mut(index), parameters.get(index)) else { continue };
            let driven = self.position[axis] * factor;
            let v = match self.blend {
                LookBlend::Additive => *slot + driven,
                LookBlend::Multiply => *slot * driven,
                LookBlend::Override => driven,
            };
            if !v.is_finite() {
                continue;
            }
            *slot = if repeating.get(index).copied().unwrap_or(false) { param.wrap(v) } else { param.clamp(v) };
        }
    }

    #[cfg(test)]
    pub fn position(&self) -> [f32; 3] {
        self.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::cubism::LookParameter;

    fn close(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }

    /// One axis of [`smooth_damp`] with the other two at rest on their goals.
    fn damp1(current: f32, target: f32, velocity: &mut f32, smooth_time: f32, dt: f32) -> f32 {
        let mut v = [*velocity, 0.0, 0.0];
        let out = smooth_damp([current, 0.0, 0.0], [target, 0.0, 0.0], &mut v, smooth_time, dt);
        *velocity = v[0];
        out[0]
    }

    #[test]
    fn smooth_damp_first_step_matches_the_hand_derived_numbers() {
        // smooth 0.2 s, dt 0.1 s: omega = 10, x = 1, exp = 1 / 2.715 = 0.368324.
        // change = -1, temp = (0 + 10 * -1) * 0.1 = -1,
        // velocity = (0 + 10) * exp = 3.683241, output = 1 + (-1 - 1) * exp = 0.263352.
        let mut v = 0.0;
        let out = damp1(0.0, 1.0, &mut v, 0.2, 0.1);
        close(out, 0.263_352, 1e-5);
        close(v, 3.683_241, 1e-5);
        // Second step: change = -0.736648, temp = (3.683241 - 7.36648) * 0.1 = -0.368324,
        // velocity = (3.683241 + 3.68324) * exp = 2.713254,
        // output = 1 + (-0.736648 - 0.368324) * exp = 0.593012.
        let out = damp1(out, 1.0, &mut v, 0.2, 0.1);
        close(out, 0.593_012, 1e-4);
        close(v, 2.713_254, 1e-4);
    }

    #[test]
    fn smooth_damp_at_sixty_hertz_with_the_rigs_damping() {
        // smooth 0.15 s, dt 1/60: omega = 13.3333, x = 0.222222,
        // exp = 1 / 1.2485047 = 0.8009582, temp = -0.222222,
        // velocity = 2.962963 * exp = 2.373209, output = 1 - 1.222222 * exp = 0.0210511.
        let mut v = 0.0;
        let out = damp1(0.0, 1.0, &mut v, 0.15, 1.0 / 60.0);
        close(out, 0.021_051_1, 1e-5);
        close(v, 2.373_209, 1e-5);
    }

    #[test]
    fn a_component_that_crosses_its_goal_alone_does_not_stop_the_vector() {
        // smooth 0.15 s, dt 1/60: exp = 0.8009582, omega = 13.33333.
        // x: change -0.05, temp = (4 - 0.666667) / 60 = 0.0555556,
        //    velocity = (4 - 0.740741) * exp = 2.610530,
        //    output = 0.5 + (-0.05 + 0.0555556) * exp = 0.5044498 (past 0.5).
        // y: from 0 toward 1: 0.0210511, velocity 2.373209.
        // dot((0.05, 1), (0.0044498, -0.9789489)) < 0, so no snap.
        let mut v = [4.0, 0.0, 0.0];
        let out = smooth_damp([0.45, 0.0, 0.0], [0.5, 1.0, 0.0], &mut v, 0.15, 1.0 / 60.0);
        close(out[0], 0.504_449_8, 1e-5);
        close(out[1], 0.021_051_1, 1e-5);
        close(v[0], 2.610_53, 1e-4);
        close(v[1], 2.373_209, 1e-5);
    }

    #[test]
    fn an_overshoot_of_the_vector_snaps_every_component_and_zeroes_the_velocity() {
        // x: change -0.05, temp = (6 - 0.666667) / 60 = 0.0888889,
        //    output = 0.5 + 0.0388889 * exp = 0.5311463 (past 0.5).
        // y: change -0.01, temp = -0.0022222, output = 1 - 0.0122222 * exp = 0.9902098.
        // dot((0.05, 0.01), (0.0311463, -0.0097902)) = 0.0015573 - 0.0000979 > 0.
        let mut v = [6.0, 0.0, 0.0];
        let out = smooth_damp([0.45, 0.99, 0.0], [0.5, 1.0, 0.0], &mut v, 0.15, 1.0 / 60.0);
        assert_eq!(out, [0.5, 1.0, 0.0]);
        assert_eq!(v, [0.0; 3]);
    }

    #[test]
    fn smooth_damp_does_not_overshoot_the_target() {
        // 0.9 -> 1 already moving toward it at 10: the raw step lands on 1.2947.
        let mut v = 10.0;
        let out = damp1(0.9, 1.0, &mut v, 0.2, 0.1);
        assert_eq!(out, 1.0);
        assert_eq!(v, 0.0);
    }

    #[test]
    fn smooth_damp_with_no_time_or_a_bad_step_stays_put() {
        let mut v = 3.0;
        assert_eq!(damp1(0.5, 1.0, &mut v, 0.2, 0.0), 0.5);
        assert_eq!(damp1(0.5, 1.0, &mut v, 0.2, f32::NAN), 0.5);
        assert_eq!(v, 3.0);
    }

    #[test]
    fn a_damping_of_zero_jumps() {
        let mut v = 0.0;
        let out = damp1(0.0, 1.0, &mut v, 0.0, 1.0 / 60.0);
        close(out, 1.0, 1e-3);
        // And a long step lands on it to float precision.
        let mut v = 0.0;
        close(damp1(0.0, 1.0, &mut v, 0.0, 0.1), 1.0, 1e-5);
    }

    #[test]
    fn it_converges_and_settles() {
        let mut v = 0.0;
        let mut x = 0.0;
        for _ in 0..600 {
            x = damp1(x, 1.0, &mut v, 0.15, 1.0 / 60.0);
        }
        close(x, 1.0, 1e-4);
        close(v, 0.0, 1e-3);
    }

    fn params() -> Vec<Parameter> {
        vec![
            Parameter { id: "A".into(), minimum: -30.0, maximum: 30.0, default: 0.0 },
            Parameter { id: "B".into(), minimum: 0.0, maximum: 1.0, default: 1.0 },
        ]
    }

    fn rig(blend: LookBlend, rest: (f32, f32, f32), center: (f32, f32, f32)) -> LookRig {
        LookRig {
            blend,
            damping: 0.15,
            rest_target: rest,
            center,
            parameters: vec![
                LookParameter { parameter: "A".into(), axis: LookAxis::X, factor: 10.0 },
                LookParameter { parameter: "B".into(), axis: LookAxis::Z, factor: 0.5 },
            ],
        }
    }

    #[test]
    fn a_rig_starts_at_zero_and_rest_puts_it_at_the_goal_less_the_centre() {
        let mut s =
            LookState::new(&rig(LookBlend::Additive, (0.5, 0.25, 1.0), (0.1, 0.0, 0.5)), &params()).expect("usable");
        assert_eq!(s.position(), [0.0; 3]);
        // No contribution at t = 0: A = 0 * 10, B = 0 * 0.5 added to 2 and 0.8.
        let p = params();
        let mut values = vec![2.0, 0.8];
        s.apply(&mut values, &p, &[false, false]);
        assert_eq!(values, vec![2.0, 0.8]);
        s.rest();
        assert_eq!(s.position(), [0.4, 0.25, 0.5]);
    }

    #[test]
    fn the_ease_in_follows_smooth_damp_toward_the_rest_goal() {
        // Goal x = 0.4 (rest 0.5 less centre 0.1), damping 0.15, dt 1/60:
        // exp = 0.8009582, omega = 13.33333, x = 0.222222.
        // Step 1: change -0.4, temp = -0.4 * 13.33333 / 60 = -0.0888889,
        //   velocity = (0 + 0.0888889 * 13.33333) * exp = 0.949283,
        //   position = 0.4 + (-0.4 - 0.0888889) * exp = 0.0084204.
        // Step 2: change = -0.3915795, temp = (0.949283 - 5.221061) / 60 = -0.0711963,
        //   position = 0.4 + (-0.3915795 - 0.0711963) * exp = 0.4 - 0.4627758 * 0.8009582 = 0.0293359.
        let mut s =
            LookState::new(&rig(LookBlend::Additive, (0.5, 0.0, 0.0), (0.1, 0.0, 0.0)), &params()).expect("usable");
        s.advance(1.0 / 60.0);
        close(s.position()[0], 0.008_420_4, 1e-5);
        s.advance(1.0 / 60.0);
        close(s.position()[0], 0.029_335_9, 1e-5);
        // About half a second in, it is most of the way; much later, there.
        for _ in 0..28 {
            s.advance(1.0 / 60.0);
        }
        assert!(s.position()[0] > 0.35 && s.position()[0] < 0.4, "{}", s.position()[0]);
        for _ in 0..600 {
            s.advance(1.0 / 60.0);
        }
        close(s.position()[0], 0.4, 1e-4);
    }

    #[test]
    fn a_2d_target_leaves_z_at_the_rest_targets() {
        let mut s =
            LookState::new(&rig(LookBlend::Additive, (0.0, 0.0, 0.75), (0.0, 0.0, 0.25)), &params()).expect("usable");
        s.set_target(Some(Vec2::new(0.3, -0.2)));
        for _ in 0..1000 {
            s.advance(0.1);
        }
        let p = s.position();
        close(p[0], 0.3, 1e-4);
        close(p[1], -0.2, 1e-4);
        close(p[2], 0.5, 1e-6);
    }

    #[test]
    fn each_blend_combines_with_the_value_the_parameter_has() {
        let p = params();
        let at = |blend| {
            let mut s = LookState::new(&rig(blend, (0.4, 0.0, 1.0), (0.0, 0.0, 0.0)), &p).expect("usable");
            s.rest();
            let mut values = vec![2.0, 0.8];
            s.apply(&mut values, &p, &[false, false]);
            values
        };
        // A: position x 0.4 * factor 10 = 4. B: position z 1.0 * factor 0.5 = 0.5.
        let add = at(LookBlend::Additive);
        close(add[0], 6.0, 1e-6);
        close(add[1], 1.0, 1e-6); // 0.8 + 0.5 = 1.3, clamped to the range
        let mul = at(LookBlend::Multiply);
        close(mul[0], 8.0, 1e-6);
        close(mul[1], 0.4, 1e-6);
        let over = at(LookBlend::Override);
        close(over[0], 4.0, 1e-6);
        close(over[1], 0.5, 1e-6);
    }

    #[test]
    fn a_repeating_parameter_wraps_instead_of_clamping() {
        let p = params();
        let mut s = LookState::new(&rig(LookBlend::Additive, (1.0, 0.0, 0.0), (0.0, 0.0, 0.0)), &p).expect("usable");
        s.rest();
        let mut values = vec![25.0, 0.0];
        s.apply(&mut values, &p, &[true, false]);
        close(values[0], -25.0, 1e-4); // 35 on [-30, 30)
        let mut values = vec![25.0, 0.0];
        s.apply(&mut values, &p, &[false, false]);
        close(values[0], 30.0, 1e-6);
    }

    #[test]
    fn an_unusable_rig_builds_no_state() {
        let mut bad = rig(LookBlend::Additive, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
        bad.parameters[0].parameter = "Missing".into();
        assert!(LookState::new(&bad, &params()).is_none());
    }
}
