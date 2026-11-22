//! Simulating a model's physics rig during playback.
//!
//! A rig is a set of pendulum chains. Each chain reads some parameters (the
//! head turning, say), swings accordingly, and writes the swing back into other
//! parameters (a strand of hair), which the deformers then draw. The rig itself
//! is [`Physics`] in the IR; this module holds the *state* it needs between
//! frames and the step that advances it.
//!
//! The rules implemented here are written down, with what established them and
//! what is still unverified, in `docs/cubism-physics.md`. In short, per group
//! and per step:
//!
//! 1. every input is clamped to its parameter's range, mapped through a
//!    piecewise-linear normalization pivoting on the range's **midpoint**,
//!    negated unless reflected, weighted, and added to a translation or an angle;
//! 2. the translation is rotated by the (negated) angle;
//! 3. the chain's root is moved to the translation and each later particle
//!    follows the one before it, pulled along the direction the angle makes,
//!    then pulled back to its fixed distance;
//! 4. each output measures one segment of the chain as an angle (or an offset),
//!    scales it, clamps it to its parameter and blends it into the parameter.
//!
//! Everything here is deterministic and allocation-free per step.

use a2d_core::ir::cubism::{
    NormRange, Parameter, Physics, PhysicsComponent, PhysicsGroup, PhysicsInput, PhysicsOutput,
};

/// Air resistance: the share of a change of gravity direction a particle
/// follows in one step is `1 / AIR_RESISTANCE`.
const AIR_RESISTANCE: f32 = 5.0;
/// Weights are percentages.
const MAX_WEIGHT: f32 = 100.0;
/// A particle whose horizontal offset is below this fraction of the position
/// normalization's maximum is snapped to the vertical, so a chain at rest does
/// not creep.
const MOVEMENT_THRESHOLD: f32 = 0.001;
/// The frame rate the delay values were authored against.
const AUTHORED_FPS: f32 = 30.0;
/// The longest step simulated in one go. A longer frame is split into equal
/// steps no longer than this, which is a deliberate difference from the source
/// engine (see `docs/cubism-physics.md`).
const MAX_STEP: f32 = 1.0 / 30.0;
/// The most time one call will simulate. A frame this long means the host was
/// blocked; the model clamps far below it already, and this keeps a direct
/// caller from asking for thousands of steps.
const MAX_ADVANCE: f32 = 1.0;
/// Vectors no longer than this have no direction.
const NO_DIRECTION: f32 = 1.0e-5;

/// A two-component vector, local to the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct V(f32, f32);

impl std::ops::Add for V {
    type Output = V;
    fn add(self, o: V) -> V {
        V(self.0 + o.0, self.1 + o.1)
    }
}
impl std::ops::Sub for V {
    type Output = V;
    fn sub(self, o: V) -> V {
        V(self.0 - o.0, self.1 - o.1)
    }
}
impl std::ops::Mul<f32> for V {
    type Output = V;
    fn mul(self, k: f32) -> V {
        V(self.0 * k, self.1 * k)
    }
}

impl V {
    fn is_finite(self) -> bool {
        self.0.is_finite() && self.1.is_finite()
    }

    /// Unit length, or zero for a vector with no usable direction.
    fn unit(self) -> V {
        let len = (self.0 * self.0 + self.1 * self.1).sqrt();
        if len > NO_DIRECTION {
            V(self.0 / len, self.1 / len)
        } else {
            V(0.0, 0.0)
        }
    }
}

/// Rotates `v` by `radians`, **in place** in the sense the source engines do:
/// the new `y` is computed from the already-replaced `x`, so this is not a true
/// rotation. It is reproduced because the output depends on it; a test pins it.
fn rotate_in_place(v: V, radians: f32) -> V {
    let (s, c) = radians.sin_cos();
    let x = c * v.0 - v.1 * s;
    V(x, s * x + c * v.1)
}

/// The signed angle that takes direction `from` to direction `to`, in
/// `[-pi, pi]`.
fn angle_between(from: V, to: V) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut d = to.1.atan2(to.0) - from.1.atan2(from.0);
    // Both operands are in [-pi, pi], so one correction each way suffices. A
    // loop here would never end on a non-finite input.
    if d < -PI {
        d += TAU;
    } else if d > PI {
        d -= TAU;
    }
    d
}

/// Maps an input parameter value onto the group's normalization range.
///
/// `value` is clamped to `[lo, hi]` first. The pivot is the **midpoint** of the
/// parameter's range, not its default: the value at the midpoint lands on
/// `range.default`, the maximum on `range.maximum` and the minimum on
/// `range.minimum`, linearly in between. Unless `reflect` is set the result is
/// negated. A parameter with no width has nothing to map and gives 0.
fn normalize(value: f32, lo: f32, hi: f32, range: NormRange, reflect: bool) -> f32 {
    let value = value.max(lo).min(hi);
    let mid = lo + (hi - lo) / 2.0;
    let d = value - mid;
    // Zero belongs to the upper side, as in the source: it lands on
    // `range.default` there, or on 0 if the range has no width.
    let mapped = if d >= 0.0 {
        let span = hi - mid;
        if span != 0.0 {
            d * ((range.maximum - range.default) / span) + range.default
        } else {
            0.0
        }
    } else {
        let span = lo - mid;
        if span != 0.0 {
            d * ((range.minimum - range.default) / span) + range.default
        } else {
            0.0
        }
    };
    if reflect {
        mapped
    } else {
        -mapped
    }
}

/// A parameter read by a group, resolved once.
#[derive(Debug, Clone)]
struct BoundInput {
    param: usize,
    lo: f32,
    hi: f32,
    component: PhysicsComponent,
    /// `weight / 100`.
    weight: f32,
    reflect: bool,
}

/// A parameter written by a group, resolved once.
#[derive(Debug, Clone)]
struct BoundOutput {
    param: usize,
    lo: f32,
    hi: f32,
    /// Index into the chain; at least 1 and inside it.
    particle: usize,
    component: PhysicsComponent,
    scale: f32,
    /// `weight / 100`.
    weight: f32,
    reflect: bool,
}

/// One point of a chain: what the rig fixes, and what the simulation moves.
#[derive(Debug, Clone, Copy)]
struct Particle {
    mobility: f32,
    delay: f32,
    acceleration: f32,
    radius: f32,
    position: V,
    velocity: V,
    /// The direction gravity had at this particle's previous step.
    last_gravity: V,
}

/// One chain with everything resolved against the model's parameters.
#[derive(Debug, Clone)]
struct Group {
    inputs: Vec<BoundInput>,
    outputs: Vec<BoundOutput>,
    chain: Vec<Particle>,
    angle_range: NormRange,
    position_range: NormRange,
    /// The reference direction an output measures the first segment against,
    /// and the direction particles start with: the gravity with `y` flipped.
    reference: V,
    /// Whether this chain has taken a step since it was last reset; until then
    /// it has nothing to say and writes no outputs.
    live: bool,
    /// Output values awaiting a finiteness check, so a group either writes all of
    /// its outputs or none.
    pending: Vec<f32>,
}

impl Group {
    fn reset(&mut self) {
        let mut previous = V(0.0, 0.0);
        for (i, p) in self.chain.iter_mut().enumerate() {
            // The root sits at the origin; each particle hangs one radius below
            // the one before (physics space has `y` pointing down).
            p.position = if i == 0 { V(0.0, 0.0) } else { V(previous.0, previous.1 + p.radius) };
            p.velocity = V(0.0, 0.0);
            p.last_gravity = self.reference;
            previous = p.position;
        }
        self.live = false;
    }

    /// The translation and angle (degrees) the inputs ask for, or `None` if a
    /// parameter holds something that is not a number.
    fn read_inputs(&self, values: &[f32]) -> Option<(V, f32)> {
        let mut translation = V(0.0, 0.0);
        let mut angle = 0.0f32;
        for input in &self.inputs {
            let value = *values.get(input.param)?;
            if !value.is_finite() {
                return None;
            }
            let range = match input.component {
                PhysicsComponent::Angle => self.angle_range,
                _ => self.position_range,
            };
            let n = normalize(value, input.lo, input.hi, range, input.reflect) * input.weight;
            match input.component {
                PhysicsComponent::X => translation.0 += n,
                PhysicsComponent::Y => translation.1 += n,
                PhysicsComponent::Angle => angle += n,
            }
        }
        // The translation is turned against the angle, with the engines' shared
        // in-place quirk.
        let translation = rotate_in_place(translation, (-angle).to_radians());
        Some((translation, angle))
    }

    /// Moves the chain by one step of `dt` seconds.
    fn step_chain(&mut self, translation: V, angle: f32, wind: V, dt: f32) {
        let threshold = MOVEMENT_THRESHOLD * self.position_range.maximum;
        if let Some(root) = self.chain.first_mut() {
            root.position = translation;
        }
        let gravity = V(angle.to_radians().sin(), angle.to_radians().cos()).unit();
        for i in 1..self.chain.len() {
            let parent = self.chain[i - 1].position;
            let p = &mut self.chain[i];
            let force = gravity * p.acceleration + wind;
            let last = p.position;
            // Delays were authored for 30 frames a second.
            let delay = p.delay * dt * AUTHORED_FPS;

            // Swing the hanging direction by a fraction of how far gravity
            // has turned since the last step.
            let turn = angle_between(p.last_gravity, gravity) / AIR_RESISTANCE;
            let mut position = parent + rotate_in_place(last - parent, turn);
            position = position + p.velocity * delay + force * (delay * delay);
            // The link keeps its length: only the direction survives.
            position = parent + (position - parent).unit() * p.radius;
            if position.0.abs() < threshold {
                position.0 = 0.0;
            }
            if delay != 0.0 {
                p.velocity = (position - last) * (1.0 / delay) * p.mobility;
            }
            p.position = position;
            p.last_gravity = gravity;
        }
    }

    fn chain_is_finite(&self) -> bool {
        self.chain.iter().all(|p| p.position.is_finite() && p.velocity.is_finite())
    }

    /// Writes the outputs from the chain's current state; all or nothing.
    fn write_outputs(&mut self, values: &mut [f32]) {
        self.pending.clear();
        for o in &self.outputs {
            let (Some(p), Some(q)) = (self.chain.get(o.particle), self.chain.get(o.particle - 1)) else {
                self.pending.push(f32::NAN);
                continue;
            };
            let segment = p.position - q.position;
            let raw = match o.component {
                PhysicsComponent::X => segment.0,
                PhysicsComponent::Y => segment.1,
                PhysicsComponent::Angle => {
                    // Against the segment before it, or against the reference
                    // direction for the first one.
                    let before = if o.particle >= 2 {
                        match self.chain.get(o.particle - 2) {
                            Some(r) => q.position - r.position,
                            None => self.reference,
                        }
                    } else {
                        self.reference
                    };
                    angle_between(before, segment)
                }
            };
            let signed = if o.reflect { -raw } else { raw };
            self.pending.push(signed * o.scale);
        }
        if !self.pending.iter().all(|v| v.is_finite()) {
            // Nothing from this group reaches the parameters.
            return;
        }
        for (o, &v) in self.outputs.iter().zip(&self.pending) {
            let Some(slot) = values.get_mut(o.param) else { continue };
            let clamped = if v < o.lo {
                o.lo
            } else if v > o.hi {
                o.hi
            } else {
                v
            };
            // Whatever the parameter holds now is what the weight blends against:
            // the motion's value, or an earlier group's output.
            let blended = if o.weight >= 1.0 { clamped } else { *slot * (1.0 - o.weight) + clamped * o.weight };
            if blended.is_finite() {
                *slot = blended;
            }
        }
    }
}

/// The running state of a model's physics: every group's particles.
///
/// Built once from the rig and the model's parameter table, which resolves every
/// parameter id to an index; an input or output naming no parameter is skipped
/// here (it is reported separately, by `CubismIr::unresolved_physics_targets`).
#[derive(Debug, Clone)]
pub struct PhysicsState {
    wind: V,
    groups: Vec<Group>,
}

impl PhysicsState {
    /// Resolves `physics` against `parameters`, with every chain hanging at rest.
    ///
    /// Returns `None` if there is nothing to simulate: no groups, or none with a
    /// chain.
    pub fn new(physics: &Physics, parameters: &[Parameter]) -> Option<PhysicsState> {
        let groups: Vec<Group> = physics.groups.iter().filter_map(|g| bind_group(g, physics, parameters)).collect();
        if groups.is_empty() {
            return None;
        }
        let mut state = PhysicsState { wind: V(physics.wind.0, physics.wind.1), groups };
        state.reset();
        Some(state)
    }

    /// Puts every chain back to hanging from its root along gravity, at rest.
    pub fn reset(&mut self) {
        for g in &mut self.groups {
            g.reset();
        }
    }

    /// Advances the simulation by `dt` seconds and writes the outputs into
    /// `values`, indexed like the parameter table this was built from.
    ///
    /// Call it after whatever else drives the parameters (the motion): outputs
    /// replace or blend into what the parameters hold.
    ///
    /// * A step of zero, negative or non-finite length moves nothing: it is a
    ///   paused frame. A chain that has already moved still writes its outputs,
    ///   from where it stopped, so a paused frame does not snap the parameters
    ///   back to the motion's values; one that has never moved writes nothing.
    /// * One call with the frame's own `dt` is one step, as the source engine
    ///   does. A `dt` longer than 1/30 s is split into equal steps of at most
    ///   that, with the inputs held constant; outputs are written once, after
    ///   the last. Time beyond one second is dropped.
    /// * If a group's inputs or its chain turn out not to be finite numbers, that
    ///   group is reset and writes nothing this step, so no non-finite value
    ///   ever reaches a parameter.
    pub fn step(&mut self, dt: f32, values: &mut [f32]) {
        let advance = dt.is_finite() && dt > 0.0;
        let (count, each) = if advance {
            let dt = dt.min(MAX_ADVANCE);
            // A hair of tolerance so a frame of exactly 1/30 s is one step.
            let count = (dt / MAX_STEP - 1.0e-3).ceil().max(1.0);
            (count as u32, dt / count)
        } else {
            (0, 0.0)
        };
        let wind = self.wind;
        for g in &mut self.groups {
            if advance {
                let Some((translation, angle)) = g.read_inputs(values) else {
                    g.reset();
                    continue;
                };
                for _ in 0..count {
                    g.step_chain(translation, angle, wind, each);
                }
                if !(translation.is_finite() && angle.is_finite() && g.chain_is_finite()) {
                    g.reset();
                    continue;
                }
                g.live = true;
            }
            if g.live {
                g.write_outputs(values);
            }
        }
    }
}

fn bind_group(group: &PhysicsGroup, physics: &Physics, parameters: &[Parameter]) -> Option<Group> {
    if group.particles.is_empty() {
        return None;
    }
    let find = |id: &str| parameters.iter().position(|p| p.id == id);
    let range = |i: usize| {
        let p = &parameters[i];
        (p.minimum.min(p.maximum), p.minimum.max(p.maximum))
    };
    let inputs = group
        .inputs
        .iter()
        .filter_map(|i: &PhysicsInput| {
            let param = find(&i.source)?;
            let (lo, hi) = range(param);
            Some(BoundInput {
                param,
                lo,
                hi,
                component: i.component,
                weight: i.weight / MAX_WEIGHT,
                reflect: i.reflect,
            })
        })
        .collect();
    let outputs: Vec<BoundOutput> = group
        .outputs
        .iter()
        .filter(|o| o.reads_particle_in(group.particles.len()))
        .filter_map(|o: &PhysicsOutput| {
            let param = find(&o.destination)?;
            let (lo, hi) = range(param);
            Some(BoundOutput {
                param,
                lo,
                hi,
                particle: o.particle as usize,
                component: o.component,
                scale: o.scale,
                weight: o.weight / MAX_WEIGHT,
                reflect: o.reflect,
            })
        })
        .collect();
    let chain = group
        .particles
        .iter()
        .map(|p| Particle {
            mobility: p.mobility,
            delay: p.delay,
            acceleration: p.acceleration,
            radius: p.radius,
            position: V(0.0, 0.0),
            velocity: V(0.0, 0.0),
            last_gravity: V(0.0, 0.0),
        })
        .collect();
    Some(Group {
        pending: Vec::with_capacity(outputs.len()),
        inputs,
        outputs,
        chain,
        angle_range: group.normalization.angle,
        position_range: group.normalization.position,
        reference: V(physics.gravity.0, -physics.gravity.1),
        live: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::cubism::{PhysicsNormalization, PhysicsParticle};

    const ANGLE_IN: usize = 0;
    const OUT: usize = 2;
    const OUT2: usize = 3;

    fn params() -> Vec<Parameter> {
        let p = |id: &str, lo: f32, hi: f32| Parameter { id: id.into(), minimum: lo, maximum: hi, default: 0.0 };
        vec![p("AngleIn", -30.0, 30.0), p("XIn", -10.0, 10.0), p("Out", -1000.0, 1000.0), p("Out2", -1000.0, 1000.0)]
    }

    fn particle() -> PhysicsParticle {
        PhysicsParticle { mobility: 1.0, delay: 1.0, acceleration: 1.0, radius: 10.0 }
    }

    /// A chain of `n` particles (the root's radius is irrelevant), angle input on
    /// `AngleIn` normalizing +-30 to +-10 degrees.
    fn group(n: usize) -> PhysicsGroup {
        PhysicsGroup {
            inputs: vec![PhysicsInput {
                source: "AngleIn".into(),
                component: PhysicsComponent::Angle,
                weight: 100.0,
                reflect: false,
            }],
            outputs: vec![PhysicsOutput {
                destination: "Out".into(),
                particle: 1,
                component: PhysicsComponent::Angle,
                scale: 1.0,
                weight: 100.0,
                reflect: false,
            }],
            particles: vec![particle(); n],
            normalization: PhysicsNormalization {
                position: NormRange::new(-5.0, 5.0, 0.0),
                angle: NormRange::new(-10.0, 10.0, 0.0),
            },
        }
    }

    fn rig(g: PhysicsGroup) -> Physics {
        Physics { gravity: (0.0, -1.0), wind: (0.0, 0.0), groups: vec![g] }
    }

    fn state(g: PhysicsGroup) -> PhysicsState {
        PhysicsState::new(&rig(g), &params()).expect("a chain was given")
    }

    fn close(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() <= tol, "{a} vs {b} (tolerance {tol})");
    }

    fn pos(s: &PhysicsState, particle: usize) -> V {
        s.groups[0].chain[particle].position
    }

    // ---- normalization ----

    #[test]
    fn normalization_pivots_on_the_range_midpoint_and_has_a_slope_per_side() {
        // A parameter spanning 0..10 (midpoint 5) onto -4..8 with 1 at the
        // midpoint: the upper side has slope (8 - 1) / 5, the lower (-4 - 1) / -5.
        let r = NormRange::new(-4.0, 8.0, 1.0);
        // Reflected, so no sign flip: the raw map.
        close(normalize(5.0, 0.0, 10.0, r, true), 1.0, 1e-6);
        close(normalize(10.0, 0.0, 10.0, r, true), 8.0, 1e-6);
        close(normalize(0.0, 0.0, 10.0, r, true), -4.0, 1e-6);
        // Halfway up: 1 + 2.5 * 7/5 = 4.5; halfway down: 1 + (-2.5) * (-5 / -5) = -1.5.
        close(normalize(7.5, 0.0, 10.0, r, true), 4.5, 1e-6);
        close(normalize(2.5, 0.0, 10.0, r, true), -1.5, 1e-6);
    }

    #[test]
    fn normalization_is_negated_unless_reflected() {
        let r = NormRange::new(-10.0, 10.0, 0.0);
        close(normalize(30.0, -30.0, 30.0, r, false), -10.0, 1e-6);
        close(normalize(30.0, -30.0, 30.0, r, true), 10.0, 1e-6);
        close(normalize(-15.0, -30.0, 30.0, r, false), 5.0, 1e-6);
        close(normalize(-15.0, -30.0, 30.0, r, true), -5.0, 1e-6);
    }

    #[test]
    fn normalization_clamps_the_value_and_survives_a_range_with_no_width() {
        let r = NormRange::new(-10.0, 10.0, 0.0);
        close(normalize(1.0e6, -30.0, 30.0, r, true), 10.0, 1e-6);
        close(normalize(-1.0e6, -30.0, 30.0, r, true), -10.0, 1e-6);
        let r = NormRange::new(-10.0, 10.0, 2.0);
        assert_eq!(normalize(3.0, 3.0, 3.0, r, true), 0.0, "nothing to map, and not the default 2");
    }

    #[test]
    fn a_weight_scales_what_an_input_adds() {
        let mut g = group(2);
        g.inputs[0].weight = 25.0;
        let mut s = state(g);
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        s.step(1.0 / 30.0, &mut v);
        // Angle = -10 degrees * 0.25, not reflected: the chain hangs along
        // (sin -2.5deg, cos -2.5deg), so its angle output after one step is
        // positive and about a quarter of the full-weight one.
        let mut full = state(group(2));
        let mut v2 = vec![30.0, 0.0, 0.0, 0.0];
        full.step(1.0 / 30.0, &mut v2);
        assert!(v[OUT] > 0.0 && v[OUT] < v2[OUT]);
        close(v[OUT] / v2[OUT], 0.25, 0.01);
    }

    // ---- the rotation quirk ----

    #[test]
    fn the_input_rotation_uses_the_already_rotated_x() {
        // (1, 2) turned by 0.5 rad. cos = 0.8775826, sin = 0.4794255.
        // x' = 0.8775826 - 2 * 0.4794255 = -0.0812684
        // y' uses x' (the quirk): -0.0812684 * 0.4794255 + 2 * 0.8775826 = 1.7162030.
        // A true rotation would give y' = 0.4794255 + 1.7551652 = 2.2345907.
        let r = rotate_in_place(V(1.0, 2.0), 0.5);
        close(r.0, -0.0812684, 1e-6);
        close(r.1, 1.716203, 1e-5);
    }

    #[test]
    fn the_quirk_reaches_the_chain_root_when_translation_and_angle_are_both_set() {
        // X input at its maximum (10 of +-10 -> position range +-5, negated): -5.
        // Angle input at its maximum: -10 degrees, so the translation is turned by
        // +10 degrees (cos 0.9848078, sin 0.1736482):
        //   x' = -5 * 0.9848078 = -4.9240388
        //   y' = x' * 0.1736482 = -0.8550504   (a true rotation: -5 * 0.1736482 = -0.8682409)
        let mut g = group(2);
        g.inputs.push(PhysicsInput {
            source: "XIn".into(),
            component: PhysicsComponent::X,
            weight: 100.0,
            reflect: false,
        });
        let mut s = state(g);
        let mut v = vec![30.0, 10.0, 0.0, 0.0];
        s.step(1.0 / 30.0, &mut v);
        let root = pos(&s, 0);
        close(root.0, -4.924_039, 1e-4);
        close(root.1, -0.855_05, 1e-4);
    }

    // ---- the chain ----

    #[test]
    fn one_pendulum_step_matches_the_hand_computed_values() {
        // Two particles, radius 10, acceleration 1, delay 1, mobility 1. One angle
        // input at its maximum gives A = -10 degrees; dt = 1/30 makes delay 1.
        //   gravity g = (sin -10deg, cos -10deg) = (-0.173648, 0.984808)
        //   last gravity (0, 1): turn = +10deg / 5 = 0.0349066 rad
        //   hanging direction (0, 10) turned (with the quirk) = (-0.348995, 9.981728)
        //   plus force g * 1 * 1^2: (-0.522643, 10.966536), pulled back to length 10:
        //   particle 1 = (-0.476040, 9.988663)
        //   output angle from (0, 1) to it = +0.047622 rad.
        let mut s = state(group(2));
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        s.step(1.0 / 30.0, &mut v);
        let p1 = pos(&s, 1);
        close(p1.0, -0.476040, 1e-5);
        close(p1.1, 9.988663, 1e-5);
        close(v[OUT], 0.047622, 1e-5);
        // Velocity = (new - last) / delay * mobility = (-0.476040, -0.011337).
        let vel = s.groups[0].chain[1].velocity;
        close(vel.0, -0.476040, 1e-5);
        close(vel.1, -0.011337, 1e-5);
    }

    #[test]
    fn a_second_step_uses_the_velocity_and_the_last_gravity() {
        // Input back at its midpoint (A = 0, g = (0, 1)); last gravity is still
        // (-0.173648, 0.984808): turn = -10deg / 5 = -0.0349066.
        //   direction (-0.476040, 9.988663) turned: (-0.127151, 9.987016)
        //   plus velocity * 1: (-0.603191, 9.975679), plus force (0, 1): (-0.603191, 10.975679)
        //   pulled back to length 10: (-0.548742, 9.984933); output angle +0.054902.
        let mut s = state(group(2));
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        s.step(1.0 / 30.0, &mut v);
        let mut v = vec![0.0, 0.0, 0.0, 0.0];
        s.step(1.0 / 30.0, &mut v);
        let p1 = pos(&s, 1);
        close(p1.0, -0.548742, 1e-5);
        close(p1.1, 9.984933, 1e-5);
        close(v[OUT], 0.054902, 1e-5);
    }

    #[test]
    fn at_the_midpoint_a_settled_chain_outputs_exactly_zero() {
        let mut s = state(group(3));
        for _ in 0..200 {
            let mut v = vec![0.0; 4];
            s.step(1.0 / 60.0, &mut v);
            assert_eq!(v[OUT], 0.0);
        }
    }

    #[test]
    fn the_first_segment_is_measured_against_gravity_and_later_ones_against_their_parent() {
        // Held at A = -10 degrees until settled, every link hangs along the
        // gravity direction: the first segment is turned 10 degrees from straight
        // down (0.174533 rad), the second is parallel to the first (0).
        let mut g = group(3);
        g.outputs.push(PhysicsOutput {
            destination: "Out2".into(),
            particle: 2,
            component: PhysicsComponent::Angle,
            scale: 1.0,
            weight: 100.0,
            reflect: false,
        });
        let mut s = state(g);
        let mut v = vec![0.0; 4];
        for _ in 0..1500 {
            v = vec![30.0, 0.0, 0.0, 0.0];
            s.step(1.0 / 60.0, &mut v);
        }
        close(v[OUT], 10.0f32.to_radians(), 1e-3);
        close(v[OUT2], 0.0, 1e-3);
    }

    // ---- outputs ----

    fn settled(g: PhysicsGroup, input: f32, values: [f32; 4]) -> Vec<f32> {
        let mut s = state(g);
        let mut v = values.to_vec();
        for _ in 0..600 {
            v = values.to_vec();
            v[ANGLE_IN] = input;
            s.step(1.0 / 60.0, &mut v);
        }
        v
    }

    #[test]
    fn an_output_is_reflected_scaled_and_clamped() {
        let base = settled(group(2), 30.0, [0.0; 4])[OUT];
        close(base, 10.0f32.to_radians(), 1e-3);

        let mut g = group(2);
        g.outputs[0].reflect = true;
        close(settled(g, 30.0, [0.0; 4])[OUT], -base, 1e-3);

        let mut g = group(2);
        g.outputs[0].scale = 20.0;
        close(settled(g, 30.0, [0.0; 4])[OUT], base * 20.0, 1e-2);

        // A parameter that only reaches +-1 caps the scaled value.
        let mut g = group(2);
        g.outputs[0].scale = 20.0;
        let mut p = params();
        p[OUT].minimum = -1.0;
        p[OUT].maximum = 1.0;
        let mut s = PhysicsState::new(&rig(g), &p).expect("chain");
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        for _ in 0..600 {
            v = vec![30.0, 0.0, 0.0, 0.0];
            s.step(1.0 / 60.0, &mut v);
        }
        assert_eq!(v[OUT], 1.0);
    }

    #[test]
    fn a_partial_weight_blends_toward_physics_over_the_motions_value() {
        let full = settled(group(2), 30.0, [0.0; 4])[OUT];
        let mut g = group(2);
        g.outputs[0].weight = 25.0;
        // The parameter holds 1.0 (a motion's value) before physics runs.
        let blended = settled(g, 30.0, [0.0, 0.0, 1.0, 0.0])[OUT];
        close(blended, 0.75 * 1.0 + 0.25 * full, 1e-3);
    }

    #[test]
    fn a_full_weight_overwrites_the_motions_value() {
        let v = settled(group(2), 30.0, [0.0, 0.0, 123.0, 0.0]);
        close(v[OUT], 10.0f32.to_radians(), 1e-3);
    }

    #[test]
    fn x_and_y_outputs_use_their_own_scales() {
        // Settled at A = -10 degrees the first link is (r sin a, r cos a) =
        // (-1.736482, 9.848078) for r = 10.
        let mut g = group(2);
        g.outputs[0].component = PhysicsComponent::X;
        g.outputs[0].scale = 2.0;
        let x = settled(g, 30.0, [0.0; 4])[OUT];
        close(x, -1.736482 * 2.0, 2e-2);
        let mut g = group(2);
        g.outputs[0].component = PhysicsComponent::Y;
        g.outputs[0].scale = 0.5;
        let y = settled(g, 30.0, [0.0; 4])[OUT];
        close(y, 9.848078 * 0.5, 2e-2);
    }

    // ---- time ----

    #[test]
    fn a_paused_step_changes_nothing_and_does_not_forget_the_outputs() {
        let mut s = state(group(2));
        // Never moved: writes nothing at all.
        let mut v = vec![30.0, 0.0, 7.0, 0.0];
        s.step(0.0, &mut v);
        assert_eq!(v, [30.0, 0.0, 7.0, 0.0]);
        for dt in [-1.0, f32::NAN, f32::INFINITY] {
            s.step(dt, &mut v);
            assert_eq!(v, [30.0, 0.0, 7.0, 0.0], "dt {dt}");
        }

        s.step(1.0 / 60.0, &mut v);
        let (after, particles) = (v.clone(), pos(&s, 1));
        assert_ne!(after[OUT], 7.0);
        // Paused: the particles stay and the parameter is written from them, so
        // a frame rebuilt from the motion does not snap back.
        let mut rebuilt = vec![30.0, 0.0, 7.0, 0.0];
        s.step(0.0, &mut rebuilt);
        assert_eq!(pos(&s, 1), particles);
        assert_eq!(rebuilt, after);
    }

    #[test]
    fn a_long_frame_is_split_into_equal_steps_of_at_most_a_thirtieth() {
        let one = |dt: f32| {
            let mut s = state(group(2));
            let mut v = vec![30.0, 0.0, 0.0, 0.0];
            s.step(dt, &mut v);
            (pos(&s, 1), v[OUT])
        };
        // 0.1 s is three steps of 0.1/3 s.
        let mut by_hand = state(group(2));
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        for _ in 0..3 {
            by_hand.step(0.1 / 3.0, &mut v);
        }
        let (p, out) = one(0.1);
        assert_eq!(p, pos(&by_hand, 1));
        assert_eq!(out, v[OUT]);
        // A frame of exactly a thirtieth is a single step, so it differs from two halves.
        let mut halves = state(group(2));
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        halves.step(1.0 / 60.0, &mut v);
        halves.step(1.0 / 60.0, &mut v);
        assert_ne!(one(1.0 / 30.0).0, pos(&halves, 1));
        // A frame that blocked for hours stays bounded and finite.
        let (p, out) = one(1.0e9);
        assert!(p.is_finite() && out.is_finite());
    }

    // ---- robustness ----

    #[test]
    fn a_non_finite_input_resets_the_group_and_leaves_its_outputs_alone() {
        let mut s = state(group(2));
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        for _ in 0..10 {
            s.step(1.0 / 60.0, &mut v);
        }
        assert!(pos(&s, 1).0 != 0.0, "the chain had swung");

        let mut bad = vec![f32::NAN, 0.0, 5.0, 0.0];
        s.step(1.0 / 60.0, &mut bad);
        assert_eq!(bad[OUT], 5.0, "the output is untouched");
        assert_eq!(pos(&s, 1), V(0.0, 10.0), "the chain is back at rest");
        assert_eq!(s.groups[0].chain[1].velocity, V(0.0, 0.0));

        // And a later finite frame works again.
        let mut ok = vec![30.0, 0.0, 5.0, 0.0];
        s.step(1.0 / 60.0, &mut ok);
        assert!(ok[OUT].is_finite() && ok[OUT] != 5.0);
    }

    #[test]
    fn a_chain_that_blows_up_is_reset_and_never_reaches_the_parameters() {
        let mut g = group(2);
        g.particles[1].acceleration = f32::INFINITY;
        let mut s = state(g);
        let mut v = vec![30.0, 0.0, 5.0, 0.0];
        s.step(1.0 / 60.0, &mut v);
        assert_eq!(v, [30.0, 0.0, 5.0, 0.0]);
        assert!(pos(&s, 1).is_finite());
    }

    #[test]
    fn unresolved_ids_are_skipped_and_an_empty_rig_gives_no_state() {
        let mut g = group(2);
        g.inputs.push(PhysicsInput {
            source: "Nope".into(),
            component: PhysicsComponent::X,
            weight: 100.0,
            reflect: false,
        });
        g.outputs.push(PhysicsOutput {
            destination: "AlsoNope".into(),
            particle: 1,
            component: PhysicsComponent::Angle,
            scale: 1.0,
            weight: 100.0,
            reflect: false,
        });
        let s = state(g);
        assert_eq!(s.groups[0].inputs.len(), 1);
        assert_eq!(s.groups[0].outputs.len(), 1);
        assert!(PhysicsState::new(&Physics::default(), &params()).is_none());
    }

    #[test]
    fn reset_hangs_the_chain_from_the_root_along_gravity() {
        let mut g = group(3);
        g.particles[1].radius = 4.0;
        g.particles[2].radius = 6.0;
        let mut s = state(g);
        let mut v = vec![30.0, 0.0, 0.0, 0.0];
        for _ in 0..5 {
            s.step(1.0 / 60.0, &mut v);
        }
        s.reset();
        assert_eq!(pos(&s, 0), V(0.0, 0.0));
        assert_eq!(pos(&s, 1), V(0.0, 4.0));
        assert_eq!(pos(&s, 2), V(0.0, 10.0));
        assert_eq!(s.groups[0].chain[2].last_gravity, V(0.0, 1.0));
    }
}
