//! [`GenericCubismModel`]: a MOC3 behind the shared [`AnimatedModel`] interface.
//!
//! This is what lets one viewer show both runtime families (spec §5). The
//! renderer never learns which it is drawing; it receives triangles either way.
//!
//! # What this can and cannot do yet
//!
//! It poses, draws, and plays the motions the model is given
//! ([`CubismIr::motions`]): [`AnimatedModel::animations`] lists them and
//! [`AnimatedModel::play_animation`] starts one by name, and
//! [`AnimatedModel::pose_at`] poses the model at a timestamp. A model built
//! straight from a MOC3 has none; one built from an imported package does.
//! Part-opacity targets are not driven; only parameter curves are.
//!
//! # Crossfades
//!
//! Replacing a playing motion with a non-zero [`PlayOptions::mix_duration`]
//! crossfades: the outgoing motion keeps playing (a looping one keeps looping,
//! a once-motion holds its last frame) while a weight `0.5 - 0.5 cos(pi t)`
//! of the elapsed fraction `t` of the mix moves from 0 to 1. Each side is
//! evaluated alone from the parameter defaults and every parameter becomes
//! `a + (b - a) * w`, so a parameter only one side drives fades to or from its
//! default, and a repeating parameter blends along the shortest arc. A mix of
//! zero is a cut. A loop wrap does not fade. The blend happens before physics,
//! which therefore sees continuous inputs. Replacing a motion again before the
//! fade is done fades from the pose the model was showing (a snapshot of the
//! blended values), so nothing jumps.
//!
//! A queued motion with a handover time ([`PlayOptions::handover_at`]) starts
//! its crossfade when the outgoing motion's playhead crosses that time; the
//! outgoing motion then plays out the rest of its pass under the fade (it does
//! not loop and clamps at its end), and the time past the crossing is carried
//! into both the incoming playhead and the fade.
//!
//! The fade times a motion may carry (`Motion::fade_in` and the per-curve
//! overrides in the IR) are stored but not applied: the assets this was built
//! against have none, so the length of a fade is the host's choice.
//!
//! A model's physics rig, when it has one, runs after the motion (or idle) has
//! written its values and before the pose is computed, every update. Its state
//! survives a change of motion. [`AnimatedModel::pose_at`] replays it from rest
//! so that a pose stays a function of the timestamp alone.
//!
//! When no motion is playing, [`AnimatedModel::update`] drives a small idle of
//! its own, from Cubism's conventional parameter names. That is **synthetic
//! motion**, not the character's: it exists so a viewer shows something alive
//! and so the deformation path is exercised continuously rather than only at
//! rest. It is skipped entirely for any parameter the model does not declare,
//! and while a motion plays, so the two never fight over a parameter.

use std::time::Duration;

use a2d_core::{
    preferred_animation, Aabb, AnimatedModel, AnimationInfo, ExpressionInfo, HitAreaId, PlayOptions, RenderList,
    RuntimeError, TextureId,
};

use crate::emit::CubismEmit;
use crate::eval::{CubismEval, Pose};
use crate::look::LookState;
use crate::moc3::CubismIr;
use crate::motion::{wrap_time, MotionPlayer};
use crate::physics::PhysicsState;

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

/// The frame rate [`AnimatedModel::pose_at`] replays physics at.
const POSE_AT_FPS: f32 = 60.0;

/// How much of the motion before the requested time `pose_at` replays the
/// physics over, in seconds.
const POSE_AT_WARM_UP: f32 = 10.0;

/// A queued motion and how to start it.
#[derive(Debug, Clone, Copy)]
struct Pending {
    index: usize,
    looping: bool,
    speed: f32,
    /// Crossfade length in seconds when it takes over; zero cuts.
    mix: f32,
    /// Seconds into the outgoing motion's pass at which it takes over, instead
    /// of at the end of the pass.
    handover_at: Option<f32>,
}

/// What the outgoing side of a crossfade shows.
#[derive(Debug, Clone)]
enum FadeFrom {
    /// A motion that keeps advancing under the fade.
    Player(MotionPlayer),
    /// The values the model showed when a second switch interrupted a fade.
    Frozen(Vec<f32>),
}

/// A crossfade in progress.
#[derive(Debug, Clone)]
struct Fade {
    from: FadeFrom,
    elapsed: f32,
    duration: f32,
    /// Per parameter, the signed distance `b - a` (a multiple of the range
    /// away from the plain difference) chosen last frame for a repeating
    /// parameter. The arc is kept continuous from frame to frame: it is the
    /// shortest at the start, and afterwards the one nearest the previous.
    arcs: Vec<Option<f32>>,
}

impl Fade {
    /// The incoming side's weight: a sine ease from 0 to 1.
    fn weight(&self) -> f32 {
        let t = if self.duration > 0.0 { (self.elapsed / self.duration).clamp(0.0, 1.0) } else { 1.0 };
        0.5 - 0.5 * (std::f32::consts::PI * t).cos()
    }

    fn is_done(&self) -> bool {
        self.elapsed >= self.duration
    }
}

/// Whether a point lies in the axis-aligned box around `points`, edges
/// included. Non-finite vertices are ignored; no finite vertex means no hit.
fn in_bounding_box(points: &[(f32, f32)], x: f32, y: f32) -> bool {
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut any = false;
    for (px, py) in points.iter().filter(|(a, b)| a.is_finite() && b.is_finite()) {
        lo_x = lo_x.min(*px);
        hi_x = hi_x.max(*px);
        lo_y = lo_y.min(*py);
        hi_y = hi_y.max(*py);
        any = true;
    }
    any && x >= lo_x && x <= hi_x && y >= lo_y && y <= hi_y
}

/// Whether a point lies in any triangle of the mesh, edges included, whichever
/// way the triangle winds. Out-of-range indices and degenerate triangles are
/// skipped.
fn in_triangles(points: &[(f32, f32)], indices: &[u16], x: f32, y: f32) -> bool {
    indices.chunks_exact(3).any(|tri| {
        let (Some(a), Some(b), Some(c)) =
            (points.get(tri[0] as usize), points.get(tri[1] as usize), points.get(tri[2] as usize))
        else {
            return false;
        };
        let side = |p: &(f32, f32), q: &(f32, f32)| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area == 0.0 || !area.is_finite() {
            return false;
        }
        let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
        // Inside (edges included) when no two of the signs oppose each other.
        let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(negative && positive) && d1.is_finite() && d2.is_finite() && d3.is_finite()
    })
}

/// A Cubism model, posed and drawable.
pub struct GenericCubismModel {
    /// The normalized model. A MOC3 is one way to obtain it and a package is
    /// another, and nothing here can tell which it was.
    moc: CubismIr,
    name: String,
    /// One value per parameter, in the model's own order.
    values: Vec<f32>,
    pose: Pose,
    elapsed: f32,
    /// Whether the idle drives anything, so a model without the conventional
    /// parameters is left alone rather than jittered.
    idle: Vec<(usize, f32, f32, f32, f32)>,
    animations: Vec<AnimationInfo>,
    /// The motion currently playing, if any. While set it owns the parameters it
    /// drives and the synthetic idle stays off.
    motion: Option<MotionPlayer>,
    /// A motion queued behind the current one. It takes over when the current
    /// motion finishes, or at the next loop wrap of a looping one. A newer
    /// queued call replaces it.
    pending: Option<Pending>,
    /// The crossfade from a replaced motion into `motion`, while one is running.
    fading: Option<Fade>,
    /// Per parameter: whether motions wrap it instead of clamping it.
    repeating: Vec<bool>,
    expressions: Vec<ExpressionInfo>,
    texture: TextureId,
    /// The physics simulation, if the model has a rig with anything to simulate.
    physics: Option<PhysicsState>,
    /// The look-at rig and its smoothing, when the model has a usable one.
    look: Option<LookState>,
    /// The presentation scale from [`AnimatedModel::set_scale`]; a negative
    /// component mirrors. Applied to the emitted geometry and the bounds only.
    scale: a2d_core::Vec2,
    /// The look target as the host gave it (post-scale model space), kept so a
    /// later scale change can re-derive the un-scaled target the rig follows.
    look_target: Option<a2d_core::Vec2>,
}

impl GenericCubismModel {
    /// Wraps a parsed model.
    pub fn load(moc: impl Into<CubismIr>, display_name: impl Into<String>) -> GenericCubismModel {
        let moc = moc.into();
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

        let animations: Vec<AnimationInfo> = moc
            .motions
            .iter()
            .map(|m| AnimationInfo {
                name: m.name.clone(),
                duration: m.duration,
                then: moc.handovers.iter().find(|h| h.from == m.name).map(|h| h.handover.clone()),
            })
            .collect();
        let pose = moc.pose(&values);
        let mut repeating = vec![false; moc.parameters.len()];
        for index in moc.repeating_parameters() {
            if let Some(slot) = repeating.get_mut(index) {
                *slot = true;
            }
        }
        let physics = moc.physics.as_ref().and_then(|p| PhysicsState::new(p, &moc.parameters));
        let look =
            moc.look.as_ref().and_then(|rig| LookState::new(rig, &moc.parameters)).filter(|state| !state.is_empty());
        let mut model = GenericCubismModel {
            moc,
            name: display_name.into(),
            values,
            pose,
            elapsed: 0.0,
            idle,
            animations,
            motion: None,
            pending: None,
            fading: None,
            repeating,
            expressions: Vec::new(),
            texture: TextureId(0),
            physics,
            look,
            scale: a2d_core::Vec2::ONE,
            look_target: None,
        };
        // Open on the same kind of animation the Spine side does, rather than a
        // second rule for what a standing pose is called.
        if let Some(index) = model.default_name().and_then(|name| model.animations.iter().position(|a| a.name == name))
        {
            model.start_motion(index, true, 1.0, 0.0);
        } else if model.look.is_some() {
            // Apply the rig at its starting (zero) position, as the first update
            // would, so the blend (an override of zero, say) is the same from
            // frame 0 rather than bare defaults until then.
            model.step_physics(0.0);
            model.repose();
        }
        model
    }

    /// The source's default motion when it names one of the motions, else the
    /// shared name heuristic.
    fn default_name(&self) -> Option<&str> {
        self.moc
            .default_motion
            .as_deref()
            .filter(|name| self.animations.iter().any(|a| a.name == *name))
            .or_else(|| preferred_animation(&self.animations))
    }

    /// Puts every parameter back to its default.
    ///
    /// Whatever drives a parameter drives it afresh each update, so a parameter
    /// nothing drives any more (a finished or replaced motion) returns to rest
    /// instead of freezing at its last value.
    fn reset_values(&mut self) {
        for (v, p) in self.values.iter_mut().zip(&self.moc.parameters) {
            *v = p.default;
        }
    }

    /// Starts motion `index`, crossfading from what plays now over `mix` seconds
    /// when there is something to fade from. A zero (or non-finite) `mix`, or no
    /// motion playing, cuts.
    fn start_motion(&mut self, index: usize, looping: bool, speed: f32, mix: f32) {
        let incoming = MotionPlayer::new(&self.moc, index, looping, speed);
        let from = if mix > 0.0 && mix.is_finite() && incoming.is_some() { self.fade_source() } else { None };
        self.fading = from.map(|from| Fade { from, elapsed: 0.0, duration: mix, arcs: Vec::new() });
        self.motion = incoming;
        // Pose the first frame now, so the model does not open at rest for one
        // update before the motion takes over. A fade starts at weight 0, so this
        // frame is the outgoing pose and nothing jumps.
        self.write_motion_values();
        // Physics outputs stay where the chain is rather than showing the new
        // motion's raw values for a frame (a paused step writes without moving).
        self.step_physics(0.0);
        self.repose();
    }

    /// What a fade starting now would fade away from: the motion playing, or, if
    /// that is itself mid-fade, the blended values on show.
    fn fade_source(&mut self) -> Option<FadeFrom> {
        let player = self.motion.as_ref()?;
        if self.fading.is_some() {
            Some(FadeFrom::Frozen(self.motion_values()))
        } else {
            Some(FadeFrom::Player(player.clone()))
        }
    }

    /// The parameter values the current motion (and the fade into it, if any)
    /// asks for, from the defaults, before physics. Defaults when no motion plays.
    fn motion_values(&mut self) -> Vec<f32> {
        let mut b: Vec<f32> = self.moc.parameters.iter().map(|p| p.default).collect();
        let Some(player) = &self.motion else { return b };
        player.apply(&self.moc, &mut b);
        let Some(mut fade) = self.fading.take() else { return b };
        let w = fade.weight();
        let a = match &fade.from {
            FadeFrom::Player(p) => {
                let mut a: Vec<f32> = self.moc.parameters.iter().map(|p| p.default).collect();
                p.apply(&self.moc, &mut a);
                a
            }
            FadeFrom::Frozen(v) => v.clone(),
        };
        fade.arcs.resize(self.moc.parameters.len(), None);
        for (i, (param, out)) in self.moc.parameters.iter().zip(b.iter_mut()).enumerate() {
            let Some(&from) = a.get(i) else { continue };
            *out = if self.repeating.get(i).copied().unwrap_or(false) {
                let span = param.maximum - param.minimum;
                let mut d = *out - from;
                if span.is_finite() && span > 0.0 && d.is_finite() {
                    // The shortest way round at the start, then whichever way
                    // round is nearest last frame's, so a side sweeping past the
                    // other's opposite point does not flip the arc mid-fade.
                    let reference = fade.arcs[i].unwrap_or(0.0);
                    d -= span * ((d - reference) / span).round();
                    fade.arcs[i] = Some(d);
                }
                param.wrap(from + d * w)
            } else {
                from + (*out - from) * w
            };
        }
        self.fading = Some(fade);
        b
    }

    /// Resets every parameter and writes the motion's (blended) values.
    fn write_motion_values(&mut self) {
        let values = self.motion_values();
        // Zipped rather than copied: after `dispose` there are no values to write.
        for (slot, v) in self.values.iter_mut().zip(values) {
            *slot = v;
        }
    }

    pub fn moc(&self) -> &CubismIr {
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
    ///
    /// The value lasts only until the next [`AnimatedModel::update`], which
    /// rebuilds every parameter from its default and whatever is driving it.
    pub fn set_parameter(&mut self, id: &str, value: f32) -> bool {
        let Some(index) = self.moc.parameters.iter().position(|p| p.id == id) else { return false };
        self.values[index] = self.moc.parameters[index].clamp(value);
        true
    }

    /// A parameter's current value by name, as the last update left it.
    pub fn parameter_value(&self, id: &str) -> Option<f32> {
        let index = self.moc.parameters.iter().position(|p| p.id == id)?;
        self.values.get(index).copied()
    }

    /// Which texture page the drawables sample.
    pub fn set_texture(&mut self, texture: TextureId) {
        self.texture = texture;
    }

    /// Drawables whose deformer chain produced nothing usable.
    pub fn unstable(&self) -> &[usize] {
        &self.pose.unstable
    }

    /// Writes the look rig's parameters over the values the motion or idle just
    /// wrote. Does nothing for a model without a usable rig.
    fn apply_look(&mut self) {
        if let Some(look) = &self.look {
            look.apply(&mut self.values, &self.moc.parameters, &self.repeating);
        }
    }

    /// Applies the look rig, then advances the physics by `dt` seconds over the
    /// values the motion or idle just wrote. The look goes first so physics reads
    /// the head angle the viewer will see. Every caller has just rebuilt the
    /// values from their defaults, which is what stops the look offset building
    /// up from one frame to the next. The physics does nothing for a model
    /// without a rig.
    fn step_physics(&mut self, dt: f32) {
        self.apply_look();
        if let Some(physics) = &mut self.physics {
            physics.step(dt, &mut self.values);
        }
    }

    /// Hands the rig the stored look target, un-scaled into model space.
    fn forward_look_target(&mut self) {
        let Some(look) = &mut self.look else { return };
        let (sx, sy) = (self.scale.x, self.scale.y);
        let own = match self.look_target {
            Some(t) if sx != 0.0 && sy != 0.0 => Some(a2d_core::Vec2::new(t.x / sx, t.y / sy)),
            _ => None,
        };
        look.set_target(own);
    }

    fn repose(&mut self) {
        self.pose = self.moc.pose(&self.values);
    }
}

impl AnimatedModel for GenericCubismModel {
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError> {
        // A long step means the host was blocked or asleep; carrying it through
        // would jump the idle rather than advance it.
        let step = dt.as_secs_f32().min(0.1);
        self.elapsed += step;
        if let Some(look) = &mut self.look {
            look.advance(step);
        }
        let mut leftover = None;
        // A queued entry with a handover time takes over when the playhead
        // crosses it, part-way through this step; the crossing leaves the
        // outgoing player un-advanced here and is resolved below.
        let crossing = match (&self.motion, &self.pending) {
            (Some(player), Some(next)) => {
                next.handover_at.and_then(|at| player.crossing(&self.moc, step, at).map(|over| (at, over)))
            }
            _ => None,
        };
        if crossing.is_none() {
            if let Some(player) = &mut self.motion {
                leftover = player.advance(&self.moc, step);
            }
        }
        if let Some(fade) = &mut self.fading {
            fade.elapsed += step;
            if let FadeFrom::Player(outgoing) = &mut fade.from {
                outgoing.advance(&self.moc, step);
            }
        }
        if let Some((at, over)) = crossing {
            if let Some(next) = self.pending.take() {
                // The outgoing motion plays on from `at` as a non-looping player
                // that clamps at its end, so the fade runs under the rest of it.
                if let Some(player) = &mut self.motion {
                    player.detach_at(&self.moc, at);
                    player.advance(&self.moc, over);
                }
                self.start_motion(next.index, next.looping, next.speed, next.mix);
                // Carry the time past `at` into the new motion and the fade, so
                // the result does not depend on where the step boundaries fell.
                if let Some(player) = &mut self.motion {
                    player.advance(&self.moc, over);
                }
                if let Some(fade) = &mut self.fading {
                    fade.elapsed += over;
                }
            }
        }
        if let Some(over) = leftover {
            if let Some(next) = self.pending.take() {
                // Fade from where the old motion ended, rather than from its
                // wrapped-around start.
                if let Some(player) = &mut self.motion {
                    player.hold_end(&self.moc);
                }
                self.start_motion(next.index, next.looping, next.speed, next.mix);
                // Carry the time that was past the old pass's end into the new
                // one, so the handover does not stall for the overshoot.
                if let Some(player) = &mut self.motion {
                    player.advance(&self.moc, over);
                }
                if let Some(fade) = &mut self.fading {
                    fade.elapsed += over;
                }
            }
        }
        if self.motion.as_ref().is_some_and(|p| p.is_finished()) {
            // A motion that does not loop leaves the track empty (see
            // `PlayOptions::once`) once its final frame has been shown.
            self.write_motion_values();
            self.motion = None;
            self.fading = None;
            self.step_physics(step);
            self.repose();
            return Ok(());
        }

        if self.motion.is_none() && self.idle.is_empty() && self.physics.is_none() && self.look.is_none() {
            // Nothing drives anything: only repose if something was left off rest.
            if self.values.iter().zip(&self.moc.parameters).any(|(v, p)| *v != p.default) {
                self.reset_values();
                self.repose();
            }
            return Ok(());
        }
        if self.motion.is_some() {
            // The motion owns its parameters, and the idle stays off so the two
            // never fight over one.
            self.write_motion_values();
            if self.fading.as_ref().is_some_and(Fade::is_done) {
                self.fading = None;
            }
        } else {
            self.reset_values();
            for (index, period, span, rest, _) in &self.idle {
                let phase = self.elapsed / period * std::f32::consts::TAU;
                let value = rest + phase.sin() * span;
                if let Some(p) = self.moc.parameters.get(*index) {
                    self.values[*index] = p.clamp(value);
                }
            }
        }
        self.step_physics(step);
        self.repose();
        Ok(())
    }

    fn emit(&self, out: &mut RenderList) {
        self.moc.emit_scaled(&self.pose, self.texture, self.scale, out);
    }

    /// Scales the emitted geometry about the model-space origin, the way the
    /// Spine runtime scales its skeleton: a negative component mirrors, and the
    /// magnitude scales. Unlike Spine, which stores whatever it is given, a
    /// non-finite scale is ignored here and the previous one stays. The scale
    /// takes effect at the next emit (and bounds query), not retroactively on a
    /// list already filled.
    ///
    /// This is a presentation transform applied at emit, not to the
    /// parameters, so physics, motions and the look rig's own state are
    /// untouched: a mirrored model's physics sees gravity and wind exactly as an
    /// unmirrored one does, which is also what the official runtime does for a
    /// mirror applied to the drawn model. Only the look target is adjusted, so
    /// that the model looks at the point the host meant in the space it sees.
    fn set_scale(&mut self, scale: a2d_core::Vec2) {
        if !(scale.x.is_finite() && scale.y.is_finite()) {
            return;
        }
        self.scale = scale;
        self.forward_look_target();
    }

    /// Only `looping`, `speed`, `mix_duration` and `queued` are honoured: there
    /// is one track, and delay is not implemented.
    ///
    /// A non-zero `mix_duration` crossfades from the motion playing now (see the
    /// module documentation); zero, or nothing playing, cuts.
    ///
    /// A queued call while a motion plays waits behind it, starting when a
    /// non-looping motion finishes or a looping one next wraps, and replaces any
    /// older queued entry; its own `mix_duration` is the crossfade from the end
    /// of the motion it waited for. With nothing playing it starts at once. A
    /// call that is not queued discards whatever was waiting.
    ///
    /// [`PlayOptions::handover_at`] makes a queued entry take over when the
    /// outgoing motion's playhead reaches that time within its pass, with the
    /// outgoing motion playing on (and clamping at its end) under the fade. It
    /// falls back to the end-of-pass rule for a backwards or stationary
    /// outgoing motion, for a time outside `[0, duration)`, and when the
    /// playhead is already past it.
    fn play_animation(&mut self, name: &str, opts: PlayOptions) -> Result<(), RuntimeError> {
        let index = self
            .animations
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| RuntimeError::UnknownAnimation(name.to_string()))?;
        if opts.queued && self.motion.is_some() {
            self.pending = Some(Pending {
                index,
                looping: opts.looping,
                speed: opts.speed,
                mix: opts.mix_duration.as_secs_f32(),
                handover_at: opts.handover_at.map(|d| d.as_secs_f32()),
            });
        } else {
            self.pending = None;
            self.start_motion(index, opts.looping, opts.speed, opts.mix_duration.as_secs_f32());
        }
        Ok(())
    }

    /// Poses the model `time` seconds into `animation`, from the parameter
    /// defaults, so the result does not depend on what played before.
    ///
    /// `time` is clamped to the motion (not wrapped), matching the Spine side:
    /// asking for the end shows the final frame. Afterwards the motion is the
    /// current one, looping, with its playhead at `time`; anything queued is
    /// dropped, as is any crossfade: the pose is the motion alone at full weight.
    /// The synthetic idle is not applied. A look rig is posed at its settled rest
    /// target, not at its load-time zero: playback eases in from zero, but a still
    /// pose must depend on the timestamp alone, not on how long the model has run.
    /// The host's driven target and the rig's smoothing in flight are left as
    /// they were.
    fn pose_at(&mut self, animation: &str, time: f32) -> Result<(), RuntimeError> {
        let index = self
            .animations
            .iter()
            .position(|a| a.name == animation)
            .ok_or_else(|| RuntimeError::UnknownAnimation(animation.to_string()))?;
        self.pending = None;
        self.fading = None;
        // The look rig sits at its rest target for the pose, so it is a function
        // of the timestamp alone; the host's driven target and the smoothing in
        // flight are put back afterwards.
        let saved_look = self.look.clone();
        if let Some(look) = &mut self.look {
            look.rest();
        }
        self.motion = MotionPlayer::new(&self.moc, index, true, 1.0);
        let duration = self.moc.motions.get(index).map_or(0.0, |m| m.duration);
        let target = wrap_time(time, duration, false);
        // Physics is replayed from rest in fixed frames, sampling the motion at
        // each, so the pose depends on the timestamp alone. Only the last
        // `POSE_AT_WARM_UP` seconds are replayed, which bounds the cost however
        // long the motion is; a chain has all but settled by then. At least one
        // frame, so time zero shows the first frame of playback rather than a
        // rig that has never moved.
        let start = (target - POSE_AT_WARM_UP).max(0.0);
        let frames = if self.physics.is_some() { (((target - start) * POSE_AT_FPS).ceil() as u32).max(1) } else { 0 };
        if let Some(physics) = &mut self.physics {
            physics.reset();
        }
        for frame in 1..=frames {
            let at = (start + frame as f32 / POSE_AT_FPS).min(target);
            if let Some(player) = &mut self.motion {
                player.seek(&self.moc, at);
            }
            self.reset_values();
            if let Some(player) = &self.motion {
                player.apply(&self.moc, &mut self.values);
            }
            self.step_physics(1.0 / POSE_AT_FPS);
        }
        if let Some(player) = &mut self.motion {
            player.seek(&self.moc, target);
        }
        if frames == 0 {
            self.reset_values();
            if let Some(player) = &self.motion {
                player.apply(&self.moc, &mut self.values);
            }
            self.apply_look();
        }
        self.look = saved_look;
        self.repose();
        Ok(())
    }

    fn stop_animation(&mut self, name: &str) {
        let is = |i: usize| self.animations.get(i).is_some_and(|a| a.name == name);
        let drop_pending = self.pending.is_some_and(|p| is(p.index));
        let drop_outgoing =
            self.fading.as_ref().is_some_and(|f| matches!(&f.from, FadeFrom::Player(p) if is(p.motion_index())));
        let drop_current = self.motion.as_ref().is_some_and(|p| is(p.motion_index()));
        if drop_pending {
            self.pending = None;
        }
        if drop_current {
            self.motion = None;
            self.fading = None;
            self.reset_values();
            self.step_physics(0.0);
            self.repose();
            // What was queued behind it takes over at once, as on the Spine
            // side. Nothing is playing to fade from, so it is a cut whatever mix
            // it asked for.
            if let Some(next) = self.pending.take() {
                self.start_motion(next.index, next.looping, next.speed, 0.0);
            }
        } else if drop_outgoing {
            // Stopping the motion being faded out ends the fade: what is left is
            // the incoming motion alone.
            self.fading = None;
            self.write_motion_values();
            self.step_physics(0.0);
            self.repose();
        }
    }

    /// Drives the look-at rig toward `target` (model space); see the module
    /// documentation. Without a rig this does nothing.
    ///
    /// The target is in the space the host sees, after
    /// [`AnimatedModel::set_scale`]: it is divided by the scale to get back to
    /// the model's own space (a mirror negates x), so a point to the viewer's
    /// right is looked at on the right whether or not the model is flipped.
    /// The rest target is the model's own and is not mirrored. A zero scale
    /// component has no inverse, so the target is released.
    fn set_look_target(&mut self, target: Option<a2d_core::Vec2>) {
        self.look_target = target;
        self.forward_look_target();
    }

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
        // It is scaled like the geometry, so a mirrored model's frame is what
        // is drawn (a corner pair either way round extends to the same box).
        let canvas = self.moc.canvas;
        if canvas.pixels_per_unit > 0.0 {
            let (w, h) = (canvas.size.0 / canvas.pixels_per_unit, canvas.size.1 / canvas.pixels_per_unit);
            let s = self.scale;
            out.extend(a2d_core::Vec2::new(-w * 0.5 * s.x, -h * 0.5 * s.y));
            out.extend(a2d_core::Vec2::new(w * 0.5 * s.x, h * 0.5 * s.y));
        }
        out
    }

    /// The topmost hit area under a point, in the same space as [`bounds`].
    ///
    /// The point is divided by the presentation scale back into the model's own
    /// space (which also undoes a mirror) and tested against each hit area's
    /// drawable as currently posed, front to back in paint order. Opacity is
    /// ignored on purpose: collision meshes are usually transparent.
    ///
    /// [`bounds`]: AnimatedModel::bounds
    fn hit_test(&self, x: f32, y: f32) -> Option<HitAreaId> {
        if self.moc.hit_areas.is_empty() {
            return None;
        }
        let (sx, sy) = (self.scale.x, self.scale.y);
        if !(sx.is_finite() && sy.is_finite() && sx != 0.0 && sy != 0.0) {
            return None;
        }
        let (px, py) = (x / sx, y / sy);
        if !(px.is_finite() && py.is_finite()) {
            return None;
        }
        let order =
            crate::emit::paint_sequence_of(self.moc.drawables.len(), &self.pose.draw_orders, &self.moc.draw_order);
        for &index in order.iter().rev() {
            for area in self.moc.hit_areas.iter().filter(|a| a.drawable as usize == index) {
                let (Some(drawable), Some(points)) = (self.moc.drawables.get(index), self.pose.drawables.get(index))
                else {
                    continue;
                };
                let hit = match area.precision {
                    a2d_core::ir::cubism::HitPrecision::BoundingBox => in_bounding_box(points, px, py),
                    a2d_core::ir::cubism::HitPrecision::Triangles => in_triangles(points, &drawable.indices, px, py),
                };
                if hit {
                    let name = if area.name.is_empty() { drawable.id.clone() } else { area.name.clone() };
                    return Some(HitAreaId(name));
                }
            }
        }
        None
    }

    fn dispose(&mut self) {
        self.pose = Pose::default();
        self.values.clear();
        self.idle.clear();
        self.motion = None;
        self.fading = None;
        self.pending = None;
        self.physics = None;
        self.look = None;
    }

    fn display_name(&self) -> &str {
        &self.name
    }

    fn default_animation(&self) -> Option<&str> {
        self.default_name()
    }

    fn current_animation(&self) -> Option<&str> {
        let player = self.motion.as_ref()?;
        self.animations.get(player.motion_index()).map(|a| a.name.as_str())
    }

    fn absorb_degradations(&self, report: &mut a2d_core::LoadReport) {
        // A curve aimed at a parameter the model lacks is not applied; say so
        // rather than letting the motion play with a part of it missing.
        for degradation in self.moc.unresolved_motion_targets() {
            report.warn(degradation);
        }
        // The same for a physics input or output: the rig runs without it.
        for degradation in self.moc.unresolved_physics_targets() {
            report.warn(degradation);
        }
        // A look rig the model cannot use is left off; say why.
        if let Some(problem) = self.moc.look.as_ref().and_then(|rig| rig.problem(&self.moc.parameters)) {
            report.warn(a2d_core::Degradation::Note(format!("look-at rig not applied: {problem}")));
        }
        // Where the motions suggest a parameter repeats but the source says it
        // does not (or, with no stored flags, where one is inferred), name it so
        // a wrong call is visible.
        for degradation in self.moc.repeat_degradations() {
            report.warn(degradation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Moc3;

    fn model() -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        GenericCubismModel::load(Moc3::parse(&bytes).expect("should parse"), "test")
    }

    #[test]
    fn handovers_are_no_longer_reported_because_playback_applies_them() {
        let m = model();
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert!(!report.to_string().contains("handover"), "{report}");
        let mut ir = m.moc().clone();
        ir.handovers.push(a2d_core::ir::cubism::MotionHandover {
            from: "a".into(),
            handover: a2d_core::Handover { animation: "b".into(), at: 1.0, mix: 0.25 },
        });
        let m = GenericCubismModel::load(ir, "test");
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert!(!report.to_string().contains("handover"), "{report}");
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
    fn a_long_step_does_not_jump_the_idle() {
        let mut m = model();
        m.update(Duration::from_secs(600)).expect("update should not fail");
        assert!(m.elapsed <= 0.1, "a blocked host must not teleport the idle: {}", m.elapsed);
    }

    use a2d_core::ir::cubism::{CurveKey, Motion, MotionCurve};

    fn k(time: f32, value: f32, slope: f32) -> CurveKey {
        CurveKey { time, value, in_slope: slope, out_slope: slope, stepped: false }
    }

    fn with_motions(motions: Vec<Motion>) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        // No stored flags, so repeating parameters are inferred from the motions
        // (the path of a package written before the flags were kept). The flagged
        // path is covered by `flagged_model`.
        ir.parameter_repeats = None;
        ir.motions = motions;
        GenericCubismModel::load(ir, "test")
    }

    /// A linear motion driving `target` from 0 to `to` over two seconds.
    fn sweep(name: &str, target: &str, to: f32) -> Motion {
        Motion {
            name: name.into(),
            duration: 2.0,
            curves: vec![MotionCurve {
                target: target.into(),
                keys: vec![k(0.0, 0.0, to / 2.0), k(2.0, to, to / 2.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    fn value(m: &GenericCubismModel, id: &str) -> f32 {
        let index = m.moc().parameters.iter().position(|p| p.id == id).expect("present");
        m.values[index]
    }

    fn max_of(m: &GenericCubismModel, id: &str) -> f32 {
        m.moc().parameter(id).expect("present").maximum
    }

    #[test]
    fn an_unknown_animation_is_an_error() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        let err = m.play_animation("Nope", PlayOptions::looping()).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownAnimation(_)), "{err}");
        assert!(model().animations().is_empty());
    }

    #[test]
    fn motions_are_listed_and_the_preferred_one_starts_on_load() {
        let m = with_motions(vec![sweep("Wave", "ParamAngleX", 5.0), sweep("Idle", "ParamAngleX", 20.0)]);
        let names: Vec<_> = m.animations().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Wave", "Idle"]);
        assert_eq!(m.animations()[1].duration, 2.0);
        assert_eq!(m.motion.as_ref().map(|p| p.motion_index()), Some(1), "Idle is preferred over Wave");
    }

    #[test]
    fn an_update_applies_the_motion_to_its_parameter() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        m.play_animation("Idle", PlayOptions::looping()).expect("known");
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        // One second in, linear 0 -> 20 over two seconds is 10, unless the
        // parameter's own range is narrower.
        let want = 10.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn speed_scales_the_playhead() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 20.0)]);
        m.play_animation("Idle", PlayOptions::looping().with_speed(2.0)).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        // 0.2 s of motion time is 2.0.
        let want = 2.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3);
    }

    #[test]
    fn a_value_outside_the_range_is_clamped() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 1.0e6)]);
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamAngleX"), max_of(&m, "ParamAngleX"));
    }

    #[test]
    fn an_unresolved_target_is_reported_and_not_applied() {
        let mut m = with_motions(vec![sweep("Idle", "ParamNoSuchThing", 20.0)]);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert_eq!(report.len(), 1);
        let text = report.warnings()[0].to_string();
        assert!(text.contains("ParamNoSuchThing") && text.contains("Idle"), "{text}");
        // The motion is playing, so the idle is off and nothing else may move.
        let before = m.values.clone();
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(m.values, before);
    }

    #[test]
    fn the_synthetic_idle_stays_off_while_a_motion_plays() {
        // The motion holds ParamAngleX constant; the idle would swing it.
        let constant = Motion {
            name: "Idle".into(),
            duration: 2.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, 3.0, 0.0), k(2.0, 3.0, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let mut m = with_motions(vec![constant]);
        for _ in 0..30 {
            m.update(Duration::from_millis(100)).expect("update");
            assert!((value(&m, "ParamAngleX") - 3.0).abs() < 1e-6);
        }
        // Once stopped, the idle takes the parameter back.
        m.stop_animation("Idle");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamAngleX") - 3.0).abs() > 1e-3);
    }

    #[test]
    fn a_motion_that_does_not_loop_ends_on_its_last_frame() {
        let mut m = with_motions(vec![sweep("Idle", "ParamAngleX", 1.0)]);
        m.stop_animation("Other");
        assert!(m.motion.is_some(), "stopping an unknown name is a no-op");
        m.play_animation("Idle", PlayOptions::once()).expect("known");
        // Stop at the update that ends it: after that the idle owns the
        // parameter again.
        let mut steps = 0;
        while m.motion.is_some() && steps < 100 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        assert!(m.motion.is_none());
        let want = 1.0f32.min(max_of(&m, "ParamAngleX"));
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3);
    }

    #[test]
    fn a_model_without_motions_has_none_playing() {
        let m = model();
        assert!(m.motion.is_none());
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert!(report.is_empty());
    }

    /// Three parameters: the test model's two, plus one the idle never drives.
    fn with_three(motions: Vec<Motion>) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameters.push(crate::moc3::Parameter {
            id: "ParamExtra".into(),
            minimum: 0.0,
            maximum: 10.0,
            default: 2.0,
        });
        ir.motions = motions;
        GenericCubismModel::load(ir, "test")
    }

    fn hold(name: &str, target: &str, v: f32) -> Motion {
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: target.into(),
                keys: vec![k(0.0, v, 0.0), k(1.0, v, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    #[test]
    fn switching_motions_returns_the_old_ones_parameters_to_rest() {
        let mut m = with_three(vec![hold("A", "ParamEyeLOpen", 0.25), hold("B", "ParamExtra", 7.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamEyeLOpen") - 0.25).abs() < 1e-6);
        m.play_animation("B", PlayOptions::looping()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert!((value(&m, "ParamExtra") - 7.0).abs() < 1e-6);
        let eye_default = m.moc().parameter("ParamEyeLOpen").expect("present").default;
        assert_eq!(value(&m, "ParamEyeLOpen"), eye_default, "A's parameter must not stay frozen");
    }

    #[test]
    fn a_finished_one_shot_returns_its_parameters_to_their_defaults() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 7.0)]);
        m.play_animation("A", PlayOptions::once()).expect("known");
        let mut steps = 0;
        while m.motion.is_some() && steps < 100 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 2.0);
    }

    #[test]
    fn stopping_a_motion_returns_its_parameters_to_their_defaults() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 7.0)]);
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 7.0);
        m.stop_animation("A");
        assert_eq!(value(&m, "ParamExtra"), 2.0);
    }

    #[test]
    fn a_queued_idle_waits_for_the_variation_to_finish() {
        // The viewer's sequence: a variation once, then the idle queued in the
        // same frame.
        let mut m = with_three(vec![hold("Idle", "ParamExtra", 5.0), hold("Wave", "ParamExtra", 9.0)]);
        m.play_animation("Wave", PlayOptions::once()).expect("known");
        m.play_animation("Idle", PlayOptions::looping().queued()).expect("known");
        m.update(Duration::from_millis(100)).expect("update");
        assert_eq!(value(&m, "ParamExtra"), 9.0, "the variation must be seen first");
        for _ in 0..8 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 9.0, "still inside its one second");
        for _ in 0..3 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 5.0, "then the idle takes over");
        assert!(m.pending.is_none());
    }

    #[test]
    fn a_queued_call_with_nothing_playing_starts_at_once_and_a_plain_call_clears_the_queue() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 5.0), hold("B", "ParamExtra", 9.0)]);
        m.stop_animation("A");
        assert!(m.motion.is_none());
        m.play_animation("A", PlayOptions::once().queued()).expect("known");
        assert!(m.motion.is_some() && m.pending.is_none());
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        assert!(m.pending.is_some());
        m.play_animation("B", PlayOptions::looping()).expect("known");
        assert!(m.pending.is_none());
    }

    #[test]
    fn a_queued_motion_waits_out_a_full_backwards_pass() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 5.0), hold("B", "ParamExtra", 9.0)]);
        m.play_animation("A", PlayOptions::looping().with_speed(-1.0)).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        m.update(Duration::ZERO).expect("update");
        assert!(m.pending.is_some(), "a zero step hands nothing over");
        // Nine 0.1 s steps is 0.9 s of a 1 s pass: still A.
        for _ in 0..9 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 5.0);
        for _ in 0..2 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        assert_eq!(value(&m, "ParamExtra"), 9.0);
    }

    #[test]
    fn a_handover_carries_the_overshoot_into_the_new_motion() {
        let mut a = hold("A", "ParamExtra", 5.0);
        a.duration = 0.95;
        let mut m = with_three(vec![a, hold("B", "ParamExtra", 9.0)]);
        m.play_animation("A", PlayOptions::once()).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        for _ in 0..10 {
            m.update(Duration::from_millis(100)).expect("update");
        }
        let player = m.motion.as_ref().expect("B is playing");
        assert_eq!(player.motion_index(), 1);
        // Ten steps is 1.0 s; A ended at 0.95 s, so B is 0.05 s in.
        assert!((player.playhead() - 0.05).abs() < 1e-3, "{}", player.playhead());
    }

    // ---- handovers, the default motion and the current animation ----

    fn handover(from: &str, to: &str, at: f32, mix: f32) -> a2d_core::ir::cubism::MotionHandover {
        a2d_core::ir::cubism::MotionHandover {
            from: from.into(),
            handover: a2d_core::Handover { animation: to.into(), at, mix },
        }
    }

    fn with_source(
        motions: Vec<Motion>,
        default_motion: Option<&str>,
        handovers: Vec<a2d_core::ir::cubism::MotionHandover>,
    ) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameter_repeats = None;
        ir.parameters.push(crate::moc3::Parameter {
            id: "ParamExtra".into(),
            minimum: 0.0,
            maximum: 10.0,
            default: 2.0,
        });
        ir.motions = motions;
        ir.default_motion = default_motion.map(str::to_string);
        ir.handovers = handovers;
        GenericCubismModel::load(ir, "test")
    }

    /// One second, `ParamExtra` rising linearly from 0 to 10.
    fn ramp(name: &str) -> Motion {
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamExtra".into(),
                keys: vec![k(0.0, 0.0, 10.0), k(1.0, 10.0, 10.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    /// A model playing `A` (the ramp) once with `B` (a hold at 4) queued behind
    /// it to take over at `at` over `mix`.
    fn queued_ramp(at: Option<f32>, mix: f32) -> GenericCubismModel {
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::once()).expect("known");
        let mut opts = PlayOptions::looping().with_mix(Duration::from_secs_f32(mix)).queued();
        if let Some(at) = at {
            opts = opts.with_handover_at(Duration::from_secs_f32(at));
        }
        m.play_animation("B", opts).expect("known");
        m
    }

    fn steps(m: &mut GenericCubismModel, n: usize, dt: Duration) {
        for _ in 0..n {
            m.update(dt).expect("update");
        }
    }

    /// 1/16 s, exact in f32; an update caps a step at 0.1 s.
    const STEP: Duration = Duration::from_micros(62_500);

    #[test]
    fn then_comes_from_the_handovers() {
        let m = with_source(
            vec![hold("A", "ParamExtra", 1.0), hold("B", "ParamExtra", 2.0)],
            None,
            vec![handover("A", "B", 0.75, 0.25)],
        );
        let a = &m.animations()[0];
        assert_eq!(a.then, Some(a2d_core::Handover { animation: "B".into(), at: 0.75, mix: 0.25 }));
        assert_eq!(m.animations()[1].then, None);
    }

    #[test]
    fn the_default_animation_is_the_sources_default_motion_when_it_names_one() {
        let motions =
            || vec![hold("A", "ParamExtra", 1.0), hold("Idle", "ParamExtra", 2.0), hold("C", "ParamExtra", 3.0)];
        let m = with_source(motions(), Some("C"), Vec::new());
        assert_eq!(m.default_animation(), Some("C"));
        assert_eq!(m.current_animation(), Some("C"), "it also opens on it");
        // Absent, or naming nothing the model has: the name heuristic.
        assert_eq!(with_source(motions(), None, Vec::new()).default_animation(), Some("Idle"));
        let m = with_source(motions(), Some("Gone"), Vec::new());
        assert_eq!(m.default_animation(), Some("Idle"));
        assert_eq!(m.current_animation(), Some("Idle"));
    }

    #[test]
    fn the_current_animation_follows_play_stop_and_the_end_of_a_once_motion() {
        let mut m = with_source(vec![hold("A", "ParamExtra", 1.0), hold("B", "ParamExtra", 2.0)], None, Vec::new());
        assert_eq!(m.current_animation(), Some("A"));
        m.play_animation("B", PlayOptions::once()).expect("known");
        assert_eq!(m.current_animation(), Some("B"));
        steps(&mut m, 18, STEP);
        assert_eq!(m.current_animation(), None, "a finished once motion leaves the track empty");
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.stop_animation("A");
        assert_eq!(m.current_animation(), None);
        assert_eq!(model().current_animation(), None);
    }

    #[test]
    fn a_queued_takeover_at_the_end_of_a_pass_changes_the_current_animation_then() {
        let mut m = queued_ramp(None, 0.0);
        steps(&mut m, 14, STEP);
        assert_eq!(m.current_animation(), Some("A"), "0.875 s into a one second pass");
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        assert_eq!(value(&m, "ParamExtra"), 4.0);
    }

    #[test]
    fn a_queued_entry_with_a_handover_time_starts_its_crossfade_exactly_there() {
        // A rises 0 -> 10 over one second (10 per second); B holds 4; the fade is
        // one second long and eased by w = 0.5 - 0.5 cos(pi t). Steps are 1/16 s,
        // exact in f32, and the handover is at 0.5 s.
        let mut m = queued_ramp(Some(0.5), 1.0);
        steps(&mut m, 6, STEP);
        // 0.375 s: before it. A alone.
        assert_eq!(m.current_animation(), Some("A"));
        assert!((value(&m, "ParamExtra") - 3.75).abs() < 1e-5);
        steps(&mut m, 2, STEP);
        // 0.5 s: the step lands on it. B has taken over, at weight 0, so A shows: 5.
        assert_eq!(m.current_animation(), Some("B"));
        assert!((value(&m, "ParamExtra") - 5.0).abs() < 1e-5, "{}", value(&m, "ParamExtra"));
        steps(&mut m, 2, STEP);
        // 0.625 s: fade 0.125 s in, t = 0.125, w = 0.5 - 0.5 cos(pi/8) = 0.0380602.
        // A is at 6.25, so 6.25 + (4 - 6.25) * 0.0380602 = 6.1643645.
        assert!((value(&m, "ParamExtra") - 6.164_364_5).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
    }

    #[test]
    fn the_outgoing_motion_keeps_moving_during_the_fade_and_clamps_at_its_end() {
        let mut m = queued_ramp(Some(0.5), 1.0);
        steps(&mut m, 18, STEP);
        // 1.125 s: 0.625 s into the fade, t = 0.625, w = 0.5 + 0.5 cos(3pi/8) =
        // 0.6913417. A has clamped at its end (10) rather than looping back to
        // 0.125 s -> 1.25: 10 + (4 - 10) * 0.6913417 = 5.8519498.
        assert!(m.fading.is_some());
        assert!((value(&m, "ParamExtra") - 5.851_95).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        // The fade ends one second after it began, at 1.5 s: B alone.
        steps(&mut m, 6, STEP);
        assert!(m.fading.is_none());
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-5);
    }

    #[test]
    fn the_overshoot_is_carried_so_step_size_does_not_matter() {
        // The handover at 0.53 s falls inside a step whichever size is used.
        let run = |dt: Duration, n: usize| {
            let mut m = queued_ramp(Some(0.53), 1.0);
            steps(&mut m, n, dt);
            m
        };
        let big = Duration::from_secs_f64(1.0 / 16.0);
        let small = Duration::from_secs_f64(1.0 / 64.0);
        for (n_big, n_small) in [(12, 48), (16, 64), (20, 80)] {
            let (a, b) = (run(big, n_big), run(small, n_small));
            assert_eq!(a.current_animation(), Some("B"));
            assert!(
                (value(&a, "ParamExtra") - value(&b, "ParamExtra")).abs() < 1e-5,
                "{n_big}: {} vs {}",
                value(&a, "ParamExtra"),
                value(&b, "ParamExtra")
            );
            let (pa, pb) = (a.motion.as_ref().expect("B"), b.motion.as_ref().expect("B"));
            assert!((pa.playhead() - pb.playhead()).abs() < 1e-5, "the incoming playhead carries it too");
        }
        // And against the formula at 1.0 s: 0.47 s into the fade, A at its end.
        let m = run(big, 16);
        let w = 0.5 - 0.5 * (std::f32::consts::PI * 0.47).cos();
        assert!((value(&m, "ParamExtra") - (10.0 + (4.0 - 10.0) * w)).abs() < 1e-4);
        assert!((m.motion.as_ref().expect("B").playhead() - 0.47).abs() < 1e-5);
    }

    #[test]
    fn without_a_handover_time_the_takeover_waits_for_the_end_of_the_pass() {
        let mut m = queued_ramp(None, 0.5);
        steps(&mut m, 8, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 8, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        // The outgoing side is held at its last frame: 10 at weight 0.
        assert!((value(&m, "ParamExtra") - 10.0).abs() < 1e-5);
    }

    #[test]
    fn a_queued_entry_behind_a_looping_one_takes_over_at_the_loop_wrap() {
        let mut m = with_source(vec![hold("A", "ParamExtra", 5.0), hold("B", "ParamExtra", 9.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        steps(&mut m, 14, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        assert_eq!(value(&m, "ParamExtra"), 9.0);
    }

    #[test]
    fn a_handover_time_works_behind_a_looping_entry_in_its_current_pass() {
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", PlayOptions::looping().queued().with_handover_at(Duration::from_millis(500)))
            .expect("known");
        steps(&mut m, 8, STEP);
        assert_eq!(m.current_animation(), Some("B"));
    }

    #[test]
    fn a_handover_time_already_passed_waits_for_the_end_of_the_pass() {
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        steps(&mut m, 12, STEP);
        m.play_animation("B", PlayOptions::looping().queued().with_handover_at(Duration::from_millis(500)))
            .expect("known");
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("A"), "0.875 s: the 0.5 s mark is behind it");
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"), "taken over at the wrap");
    }

    #[test]
    fn stopping_the_current_motion_promotes_the_queued_one() {
        let mut m = queued_ramp(None, 1.0);
        steps(&mut m, 2, STEP);
        m.stop_animation("A");
        assert_eq!(m.current_animation(), Some("B"));
        assert!(m.pending.is_none() && m.fading.is_none(), "a cut: there is nothing left to fade from");
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-5);
        // Stopping the queued entry instead leaves the current one alone.
        let mut m = queued_ramp(None, 1.0);
        m.stop_animation("B");
        assert!(m.pending.is_none());
        assert_eq!(m.current_animation(), Some("A"));
    }

    #[test]
    fn a_handover_time_of_zero_takes_over_on_the_first_update() {
        let mut m = queued_ramp(Some(0.0), 1.0);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 1, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        // Fade and incoming playhead start one step in; A is at 0.0625 * 10.
        assert!((m.motion.as_ref().expect("B").playhead() - 0.0625).abs() < 1e-6);
        assert!((m.fading.as_ref().expect("fading").elapsed - 0.0625).abs() < 1e-6);
    }

    #[test]
    fn a_handover_crossing_scales_the_overshoot_by_the_outgoing_speed() {
        // A at speed 2, handover at 0.55 s. Four steps of 1/16 s reach 0.5; the
        // fifth would reach 0.625, so 0.075 s of motion time (0.0375 s of wall
        // time) lies past the mark.
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::once().with_speed(2.0)).expect("known");
        m.play_animation(
            "B",
            PlayOptions::looping()
                .with_mix(Duration::from_secs(1))
                .queued()
                .with_handover_at(Duration::from_secs_f32(0.55)),
        )
        .expect("known");
        steps(&mut m, 4, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 1, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        let over = 0.0375;
        assert!((m.motion.as_ref().expect("B").playhead() - over).abs() < 1e-5);
        let fade = m.fading.as_ref().expect("fading");
        assert!((fade.elapsed - over).abs() < 1e-5);
        let FadeFrom::Player(outgoing) = &fade.from else { panic!("outgoing keeps playing") };
        assert!((outgoing.playhead() - 0.625).abs() < 1e-5, "{}", outgoing.playhead());
        // A shows 6.25; B 4; weight 0.5 - 0.5 cos(pi * 0.0375).
        let w = 0.5 - 0.5 * (std::f32::consts::PI * over).cos();
        assert!((value(&m, "ParamExtra") - (6.25 + (4.0 - 6.25) * w)).abs() < 1e-4);
    }

    #[test]
    fn a_stationary_outgoing_motion_never_hands_over() {
        for at in [None, Some(0.5)] {
            let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
            m.play_animation("A", PlayOptions::looping().with_speed(0.0)).expect("known");
            let mut opts = PlayOptions::looping().queued();
            if let Some(at) = at {
                opts = opts.with_handover_at(Duration::from_secs_f32(at));
            }
            m.play_animation("B", opts).expect("known");
            steps(&mut m, 64, STEP);
            assert_eq!(m.current_animation(), Some("A"));
            assert!(m.pending.is_some());
        }
    }

    #[test]
    fn a_backwards_looping_entry_hands_over_when_it_wraps_at_zero() {
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::looping().with_speed(-1.0)).expect("known");
        m.play_animation("B", PlayOptions::looping().queued()).expect("known");
        steps(&mut m, 15, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 1, STEP);
        assert_eq!(m.current_animation(), Some("B"));
    }

    #[test]
    fn a_handover_time_past_the_pass_or_a_backwards_playhead_falls_back_to_the_end_of_the_pass() {
        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::once()).expect("known");
        m.play_animation("B", PlayOptions::looping().queued().with_handover_at(Duration::from_secs(5))).expect("known");
        steps(&mut m, 14, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"));

        let mut m = with_source(vec![ramp("A"), hold("B", "ParamExtra", 4.0)], None, Vec::new());
        m.play_animation("A", PlayOptions::once().with_speed(-1.0)).expect("known");
        m.play_animation("B", PlayOptions::looping().queued().with_handover_at(Duration::from_millis(500)))
            .expect("known");
        steps(&mut m, 14, STEP);
        assert_eq!(m.current_animation(), Some("A"), "0.5 s is crossed going backwards, and ignored");
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"));
    }

    #[test]
    fn pose_at_samples_the_motion_at_a_timestamp() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        m.pose_at("S", 1.0).expect("known");
        // Linear 0 -> 8 over two seconds, one second in.
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        m.pose_at("S", 0.25).expect("known");
        assert!((value(&m, "ParamExtra") - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_motion_that_moves_a_draw_order_changes_what_is_painted_first() {
        // Drawable 0's draw order is 400 up to the middle key and 600 at the
        // last, so as the motion drives the first parameter from 0 to 30 it
        // crosses the 500 the other drawable holds. The list names drawable 1
        // first, so the tie and everything after paints 1 behind 0.
        let bytes = crate::moc3::tests::Builder::new()
            .drawables(2)
            .tie_order(&[1, 0])
            .draw_orders(&[400.0, 400.0, 600.0, 500.0, 500.0, 500.0])
            .build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.motions = vec![sweep("S", "ParamAngleX", 30.0)];
        let mut m = GenericCubismModel::load(ir, "test");
        let painted = |m: &GenericCubismModel| {
            let mut list = RenderList::new();
            m.emit(&mut list);
            list.meshes().iter().map(|mesh| (mesh.uvs[0].x / 0.125).round() as usize).collect::<Vec<_>>()
        };
        m.pose_at("S", 0.0).expect("known");
        assert_eq!(painted(&m), [0, 1], "drawable 0 at 400 is behind");
        m.pose_at("S", 2.0).expect("known");
        assert_eq!(painted(&m), [1, 0], "drawable 0 at 600 is in front");
    }

    #[test]
    fn pose_at_does_not_depend_on_what_happened_before() {
        let motions = vec![sweep("S", "ParamExtra", 8.0), hold("H", "ParamEyeLOpen", 0.25)];
        let mut fresh = with_three(motions.clone());
        fresh.pose_at("S", 0.5).expect("known");

        let mut used = with_three(motions);
        used.play_animation("H", PlayOptions::once()).expect("known");
        used.play_animation("S", PlayOptions::looping().queued()).expect("known");
        for _ in 0..7 {
            used.update(Duration::from_millis(130)).expect("update");
        }
        used.set_parameter("ParamAngleX", 5.0);
        used.pose_at("S", 0.5).expect("known");

        assert_eq!(used.values, fresh.values);
        assert_eq!(used.pose().drawables.len(), fresh.pose().drawables.len());
        let (mut a, mut b) = (RenderList::new(), RenderList::new());
        used.emit(&mut a);
        fresh.emit(&mut b);
        assert_eq!(a.meshes().len(), b.meshes().len());
        for (x, y) in a.meshes().iter().zip(b.meshes()) {
            assert_eq!(x.vertices, y.vertices);
        }
        // The other motion's parameter is back at rest, not left where it was.
        let eye_default = used.moc().parameter("ParamEyeLOpen").expect("present").default;
        assert_eq!(value(&used, "ParamEyeLOpen"), eye_default);
    }

    #[test]
    fn pose_at_clamps_beyond_the_ends() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        m.pose_at("S", 99.0).expect("known");
        assert!((value(&m, "ParamExtra") - 8.0).abs() < 1e-4, "the final frame, not a wrap to the start");
        m.pose_at("S", -3.0).expect("known");
        assert_eq!(value(&m, "ParamExtra"), 0.0);
        m.pose_at("S", f32::NAN).expect("known");
        assert_eq!(value(&m, "ParamExtra"), 0.0);
    }

    #[test]
    fn pose_at_an_unknown_animation_is_an_error() {
        let mut m = with_three(vec![sweep("S", "ParamExtra", 8.0)]);
        let err = m.pose_at("Nope", 0.0).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownAnimation(_)), "{err}");
        assert!(matches!(model().pose_at("S", 0.0), Err(RuntimeError::UnknownAnimation(_))));
    }

    // ---- physics ----

    use a2d_core::ir::cubism::{
        NormRange, Physics, PhysicsComponent, PhysicsGroup, PhysicsInput, PhysicsNormalization, PhysicsOutput,
        PhysicsParticle,
    };

    /// A rig where `input` (+-30) swings a two-particle chain whose first segment
    /// writes `output`. Held at its maximum the input gives an angle of -10
    /// degrees, and the chain settles on an output of +0.1745 (10 degrees in
    /// radians).
    fn rig(input: &str, output: &str, weight: f32) -> Physics {
        Physics {
            gravity: (0.0, -1.0),
            wind: (0.0, 0.0),
            groups: vec![PhysicsGroup {
                inputs: vec![PhysicsInput {
                    source: input.into(),
                    component: PhysicsComponent::Angle,
                    weight: 100.0,
                    reflect: false,
                }],
                outputs: vec![PhysicsOutput {
                    destination: output.into(),
                    particle: 1,
                    component: PhysicsComponent::Angle,
                    scale: 1.0,
                    weight,
                    reflect: false,
                }],
                particles: vec![PhysicsParticle { mobility: 1.0, delay: 1.0, acceleration: 1.0, radius: 10.0 }; 2],
                normalization: PhysicsNormalization {
                    position: NormRange::new(-5.0, 5.0, 0.0),
                    angle: NormRange::new(-10.0, 10.0, 0.0),
                },
            }],
        }
    }

    fn with_physics(motions: Vec<Motion>, physics: Physics) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameter_repeats = None;
        ir.motions = motions;
        ir.physics = Some(physics);
        GenericCubismModel::load(ir, "test")
    }

    const SETTLED: f32 = 0.174_532_93;

    fn frames(m: &mut GenericCubismModel, n: usize) {
        for _ in 0..n {
            m.update(Duration::from_millis(16)).expect("update");
        }
    }

    /// A motion holding `ParamAngleX` at its maximum and `ParamEyeLOpen` at `eye`.
    fn hold_both(name: &str, eye: f32) -> Motion {
        let curve = |target: &str, v: f32| MotionCurve {
            target: target.into(),
            keys: vec![k(0.0, v, 0.0), k(1.0, v, 0.0)],
            ..MotionCurve::default()
        };
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![curve("ParamAngleX", 30.0), curve("ParamEyeLOpen", eye)],
            ..Motion::default()
        }
    }

    #[test]
    fn physics_runs_after_the_motion_and_overwrites_its_curve_at_full_weight() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.update(Duration::from_millis(16)).expect("update");
        let first = value(&m, "ParamEyeLOpen");
        assert!(first < 0.5, "the curve's 0.9 must be replaced by the chain's output, got {first}");
        frames(&mut m, 600);
        assert!((value(&m, "ParamEyeLOpen") - SETTLED).abs() < 2e-3, "{}", value(&m, "ParamEyeLOpen"));
        assert!(m.unstable().is_empty());
    }

    #[test]
    fn a_partial_weight_blends_physics_over_the_motions_curve() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 25.0));
        frames(&mut m, 600);
        // 75 % of the curve's 0.9 and 25 % of the settled output.
        let want = 0.75 * 0.9 + 0.25 * SETTLED;
        assert!((value(&m, "ParamEyeLOpen") - want).abs() < 2e-3, "{}", value(&m, "ParamEyeLOpen"));
    }

    #[test]
    fn physics_keeps_running_after_a_one_shot_motion_ends() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.play_animation("Idle", PlayOptions::once()).expect("known");
        let mut steps = 0;
        while m.motion.is_some() && steps < 200 {
            m.update(Duration::from_millis(100)).expect("update");
            steps += 1;
        }
        assert!(m.motion.is_none());
        // The finished frame still shows the held input, so the chain still swings.
        let eye = value(&m, "ParamEyeLOpen");
        assert!(eye != 0.9 && eye != 1.0 && eye > 0.0, "{eye}");
        frames(&mut m, 5);
        assert!(value(&m, "ParamEyeLOpen") != eye, "the chain is still settling");
    }

    #[test]
    fn physics_runs_even_when_nothing_else_drives_the_model() {
        // No motion, and parameter names the idle does not know: only the physics
        // has anything to say. The input rests at 30, its maximum.
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameters = vec![
            crate::moc3::Parameter { id: "In".into(), minimum: -30.0, maximum: 30.0, default: 30.0 },
            crate::moc3::Parameter { id: "Out".into(), minimum: 0.0, maximum: 1.0, default: 0.0 },
        ];
        ir.physics = Some(rig("In", "Out", 100.0));
        let mut m = GenericCubismModel::load(ir, "test");
        assert!(m.idle.is_empty() && m.motion.is_none());
        frames(&mut m, 600);
        assert!((value(&m, "Out") - SETTLED).abs() < 2e-3, "{}", value(&m, "Out"));
    }

    #[test]
    fn a_model_without_a_rig_is_unaffected() {
        let mut with = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        let mut without = with_motions(vec![hold_both("Idle", 0.9)]);
        frames(&mut with, 10);
        frames(&mut without, 10);
        assert_eq!(value(&without, "ParamEyeLOpen"), 0.9);
        assert!(value(&with, "ParamEyeLOpen") != 0.9);
        assert!(without.physics.is_none());
    }

    #[test]
    fn switching_motions_does_not_reset_the_physics() {
        let motions = vec![hold_both("A", 0.9), hold_both("B", 0.2)];
        let physics = || rig("ParamAngleX", "ParamEyeLOpen", 100.0);
        let mut m = with_physics(motions.clone(), physics());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        frames(&mut m, 600);
        let settled = value(&m, "ParamEyeLOpen");
        m.play_animation("B", PlayOptions::looping()).expect("known");
        frames(&mut m, 1);
        assert!((value(&m, "ParamEyeLOpen") - settled).abs() < 1e-3, "the chain carries over");
        // A model that had only just started is nowhere near that after one frame.
        let mut fresh = with_physics(motions, physics());
        fresh.play_animation("B", PlayOptions::looping()).expect("known");
        frames(&mut fresh, 1);
        assert!((value(&fresh, "ParamEyeLOpen") - settled).abs() > 0.05);
    }

    #[test]
    fn a_paused_frame_keeps_the_physics_output_instead_of_snapping_to_the_motion() {
        let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        frames(&mut m, 20);
        let moving = value(&m, "ParamEyeLOpen");
        m.update(Duration::ZERO).expect("update");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving);
    }

    #[test]
    fn pose_at_with_physics_is_a_function_of_the_timestamp_alone() {
        let motions = vec![sweep("S", "ParamAngleX", 40.0), hold_both("H", 0.9)];
        let physics = || rig("ParamAngleX", "ParamEyeLOpen", 100.0);
        let mut fresh = with_physics(motions.clone(), physics());
        fresh.pose_at("S", 1.0).expect("known");
        let once = value(&fresh, "ParamEyeLOpen");
        fresh.pose_at("S", 1.0).expect("known");
        assert_eq!(value(&fresh, "ParamEyeLOpen"), once, "same time twice");

        let mut used = with_physics(motions.clone(), physics());
        used.play_animation("H", PlayOptions::looping()).expect("known");
        frames(&mut used, 123);
        used.pose_at("S", 1.0).expect("known");
        assert_eq!(used.values, fresh.values);
        let (mut a, mut b) = (RenderList::new(), RenderList::new());
        used.emit(&mut a);
        fresh.emit(&mut b);
        for (x, y) in a.meshes().iter().zip(b.meshes()) {
            assert_eq!(x.vertices, y.vertices);
        }

        // And it really is simulated: not the motion's own value, and a later
        // time differs from an earlier one.
        let mut plain = with_motions(motions);
        plain.pose_at("S", 1.0).expect("known");
        let rest = plain.moc().parameter("ParamEyeLOpen").expect("p").default;
        assert_eq!(value(&plain, "ParamEyeLOpen"), rest);
        assert!(once > 0.0 && once != rest);
        fresh.pose_at("S", 0.25).expect("known");
        assert!(value(&fresh, "ParamEyeLOpen") != once);
        // Time zero shows the first frame of playback, finite.
        fresh.pose_at("S", 0.0).expect("known");
        assert!(value(&fresh, "ParamEyeLOpen").is_finite());
    }

    #[test]
    fn starting_or_stopping_a_motion_does_not_pop_the_physics_outputs() {
        let motions = vec![hold_both("A", 0.9), hold_both("B", 0.7)];
        let mut m = with_physics(motions, rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.play_animation("A", PlayOptions::looping()).expect("known");
        frames(&mut m, 120);
        let moving = value(&m, "ParamEyeLOpen");
        assert!(moving < 0.5);
        m.play_animation("B", PlayOptions::looping()).expect("known");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving, "not the new curve's 0.7");
        m.stop_animation("B");
        assert_eq!(value(&m, "ParamEyeLOpen"), moving, "not the default");
    }

    #[test]
    fn pose_at_replays_only_a_bounded_window_so_a_huge_motion_returns_promptly() {
        let mut long = hold_both("L", 0.9);
        long.duration = 1.0e9;
        long.curves[0].keys[1].time = 1.0e9;
        long.curves[1].keys[1].time = 1.0e9;
        let mut m = with_physics(vec![long.clone()], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        let start = std::time::Instant::now();
        m.pose_at("L", 9.0e8).expect("known");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(value(&m, "ParamEyeLOpen").is_finite());

        // A long but representable motion: the window is replayed and has settled.
        let mut mid = hold_both("M", 0.9);
        mid.duration = 1000.0;
        mid.curves[0].keys[1].time = 1000.0;
        mid.curves[1].keys[1].time = 1000.0;
        let mut m = with_physics(vec![mid], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        m.pose_at("M", 900.0).expect("known");
        assert!(
            (value(&m, "ParamEyeLOpen") - SETTLED).abs() < 2e-3,
            "settled inside the window: {}",
            value(&m, "ParamEyeLOpen")
        );

        // Inside the window the replay still starts from rest: half a second is
        // 30 frames, nowhere near settled.
        let mut posed = with_physics(vec![long], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        posed.pose_at("L", 0.5).expect("known");
        assert!((value(&posed, "ParamEyeLOpen") - SETTLED).abs() > 1e-3);
    }

    #[test]
    fn unresolved_physics_targets_are_reported() {
        let mut m = with_physics(vec![], rig("NoSuchInput", "NoSuchOutput", 100.0));
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        let text: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(text.len(), 2, "{text:?}");
        assert!(text.iter().any(|t| t.contains("NoSuchInput")) && text.iter().any(|t| t.contains("NoSuchOutput")));
        // The rig still loads and does nothing harmful.
        frames(&mut m, 5);
        assert!(m.values.iter().all(|v| v.is_finite()));
    }

    // ---- repeating parameters ----

    /// `ParamAngleX` ([-30, 30]) turning linearly 0 -> 180 over 4 s, plus a
    /// motion holding it at 360 (six turns), which marks it as repeating.
    fn spinner(physics: Option<Physics>) -> GenericCubismModel {
        let spin = Motion {
            name: "Spin".into(),
            duration: 4.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, 0.0, 45.0), k(4.0, 180.0, 45.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let rest = Motion {
            name: "Rest".into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, 360.0, 0.0), k(1.0, 360.0, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        match physics {
            Some(p) => with_physics(vec![spin, rest], p),
            None => with_motions(vec![spin, rest]),
        }
    }

    #[test]
    fn a_repeating_parameter_is_posed_wrapped_and_pose_at_stays_pure() {
        let mut fresh = spinner(None);
        fresh.pose_at("Spin", 1.0).expect("pose");
        assert!((value(&fresh, "ParamAngleX") - -15.0).abs() < 1e-3);
        let mut used = spinner(None);
        used.pose_at("Spin", 3.0).expect("pose");
        used.play_animation("Rest", PlayOptions::looping()).expect("play");
        frames(&mut used, 10);
        used.pose_at("Spin", 1.0).expect("pose");
        assert_eq!(used.values, fresh.values);
        assert_eq!(used.pose, fresh.pose);
    }

    #[test]
    fn physics_reads_the_wrapped_value_of_a_repeating_parameter() {
        // 360 is six whole turns, so the model is posed at 0. A rig reading it
        // must behave exactly as one reading a held 0, not one reading a
        // clamped 30.
        let mut wrapped = spinner(Some(rig("ParamAngleX", "ParamEyeLOpen", 100.0)));
        wrapped.play_animation("Rest", PlayOptions::looping()).expect("play");
        let mut zero = with_physics(vec![hold_at("Rest", 0.0)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
        zero.play_animation("Rest", PlayOptions::looping()).expect("play");
        frames(&mut wrapped, 60);
        frames(&mut zero, 60);
        assert_eq!(value(&wrapped, "ParamAngleX"), 0.0);
        assert!((value(&wrapped, "ParamEyeLOpen") - value(&zero, "ParamEyeLOpen")).abs() < 1e-6);
    }

    /// A motion holding `ParamAngleX` at `v`.
    fn hold_at(name: &str, v: f32) -> Motion {
        Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, v, 0.0), k(1.0, v, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }
    }

    /// A model from a MOC3 whose section 54 flags `flagged`, playing the given
    /// motions: the stored flags decide what repeats.
    fn flagged_model(flagged: &[usize], motions: Vec<Motion>) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().repeating(flagged).build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.motions = motions;
        GenericCubismModel::load(ir, "test")
    }

    #[test]
    fn a_stored_flag_wraps_a_parameter_whose_motions_give_no_evidence() {
        // 45 on [-30, 30] with a key inside the range: nothing to infer from.
        let sweep45 = || Motion {
            name: "Sweep".into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![k(0.0, 0.0, 0.0), k(1.0, 45.0, 0.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let mut flagged = flagged_model(&[0], vec![sweep45()]);
        flagged.pose_at("Sweep", 1.0).expect("pose");
        assert!((value(&flagged, "ParamAngleX") - -15.0).abs() < 1e-3, "{}", value(&flagged, "ParamAngleX"));
        let mut plain = flagged_model(&[], vec![sweep45()]);
        plain.pose_at("Sweep", 1.0).expect("pose");
        assert_eq!(value(&plain, "ParamAngleX"), 30.0);
    }

    #[test]
    fn a_cleared_flag_clamps_what_the_motions_would_have_made_repeat_and_says_so_once() {
        let mut m = flagged_model(&[], vec![hold_at("Rest", 360.0)]);
        m.pose_at("Rest", 0.0).expect("pose");
        assert_eq!(value(&m, "ParamAngleX"), 30.0);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        let text: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].contains("ParamAngleX") && text[0].contains("does not mark it repeating"), "{text:?}");
        // Marked in the source, the same motion agrees with the flag and is silent.
        let m = flagged_model(&[0], vec![hold_at("Rest", 360.0)]);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        assert!(report.is_empty(), "{:?}", report.warnings());
    }

    #[test]
    fn repeating_parameters_are_reported_once_each() {
        let m = spinner(None);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        let text: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].contains("ParamAngleX") && text[0].contains("repeating"), "{text:?}");
    }

    // ---- crossfades ----

    fn mix(seconds: f32) -> PlayOptions {
        PlayOptions::looping().with_mix(Duration::from_secs_f32(seconds))
    }

    /// Advances by `seconds` in sub-steps of at most 1/16 s (an exact binary
    /// fraction), because `update` caps a single step at 0.1 s.
    fn step(m: &mut GenericCubismModel, seconds: f32) {
        let n = (seconds * 16.0).ceil().max(1.0) as u32;
        for _ in 0..n {
            m.update(Duration::from_secs_f32(seconds / n as f32)).expect("update");
        }
    }

    #[test]
    fn a_crossfade_follows_the_sine_weight() {
        // w = 0.5 - 0.5 cos(pi t): 0.146447, 0.5, 0.853553, 1 at t = 0.25 .. 1.
        // value = 0 + (8 - 0) * w.
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        step(&mut m, 0.1);
        m.play_animation("B", mix(1.0)).expect("known");
        for want in [1.171_573, 4.0, 6.828_427, 8.0] {
            step(&mut m, 0.25);
            assert!((value(&m, "ParamExtra") - want).abs() < 1e-4, "{} vs {want}", value(&m, "ParamExtra"));
        }
        assert!(m.fading.is_none(), "the fade is dropped once it reaches 1");
        step(&mut m, 0.25);
        assert_eq!(value(&m, "ParamExtra"), 8.0);
    }

    #[test]
    fn a_parameter_only_the_outgoing_motion_drives_fades_to_its_default() {
        let a = Motion {
            curves: vec![
                hold("", "ParamEyeLOpen", 0.25).curves.remove(0),
                hold("", "ParamExtra", 0.0).curves.remove(0),
            ],
            ..hold("A", "ParamExtra", 0.0)
        };
        let mut m = with_three(vec![a, hold("B", "ParamExtra", 8.0)]);
        let d = m.moc().parameter("ParamEyeLOpen").expect("present").default;
        m.play_animation("A", PlayOptions::looping()).expect("known");
        step(&mut m, 0.1);
        assert!((value(&m, "ParamEyeLOpen") - 0.25).abs() < 1e-6);
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        let w = 0.146_446_6;
        let want = 0.25 + (d - 0.25) * w;
        assert!((value(&m, "ParamEyeLOpen") - want).abs() < 1e-5, "{} vs {want}", value(&m, "ParamEyeLOpen"));
        step(&mut m, 0.75);
        assert!((value(&m, "ParamEyeLOpen") - d).abs() < 1e-6);
    }

    #[test]
    fn the_first_frame_after_a_switch_shows_no_jump() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        step(&mut m, 0.1);
        let (values, pose) = (m.values.clone(), m.pose.clone());
        m.play_animation("B", mix(0.5)).expect("known");
        assert_eq!(m.values, values);
        assert_eq!(m.pose, pose);
    }

    #[test]
    fn a_zero_mix_is_a_cut() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        step(&mut m, 0.1);
        m.play_animation("B", PlayOptions::looping()).expect("known");
        assert!(m.fading.is_none());
        assert_eq!(value(&m, "ParamExtra"), 8.0);
    }

    #[test]
    fn nothing_playing_means_nothing_to_fade_from() {
        let mut m = with_three(vec![hold("B", "ParamExtra", 8.0)]);
        m.motion = None;
        m.play_animation("B", mix(1.0)).expect("known");
        assert!(m.fading.is_none());
        assert_eq!(value(&m, "ParamExtra"), 8.0);
    }

    #[test]
    fn the_outgoing_motion_keeps_advancing_under_the_fade() {
        // A sweeps ParamExtra 0 -> 8 at 4 per second; B leaves it alone, so its
        // side is the default 2. A frozen outgoing motion would give
        // 0 + (2 - 0) * w = 0.2929 instead.
        let mut m = with_three(vec![sweep("A", "ParamExtra", 8.0), hold("B", "ParamAngleX", 10.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        // a = 4 * 0.25 = 1, b = 2.
        let want = 1.0 + (2.0 - 1.0) * 0.146_446_6;
        assert!((value(&m, "ParamExtra") - want).abs() < 1e-5, "{}", value(&m, "ParamExtra"));
    }

    #[test]
    fn a_once_motion_holds_its_last_frame_while_it_fades_out() {
        let mut m = with_three(vec![sweep("A", "ParamExtra", 8.0), hold("B", "ParamAngleX", 10.0)]);
        m.play_animation("A", PlayOptions::once()).expect("known");
        step(&mut m, 0.1);
        for _ in 0..18 {
            step(&mut m, 0.1);
        }
        m.play_animation("B", mix(2.0)).expect("known");
        // A ends 0.1 s into the fade and holds 8; B does not drive ParamExtra
        // (default 2). At 1 s of 2 s, w = 0.5: 8 + (2 - 8) * 0.5 = 5.
        for _ in 0..10 {
            step(&mut m, 0.1);
        }
        assert!((value(&m, "ParamExtra") - 5.0).abs() < 1e-3, "{}", value(&m, "ParamExtra"));
    }

    #[test]
    fn a_queued_handover_crossfades_from_the_end_of_the_first_motion() {
        let mut m = with_three(vec![hold("Wave", "ParamExtra", 0.0), hold("Idle", "ParamExtra", 8.0)]);
        m.play_animation("Wave", PlayOptions::once()).expect("known");
        m.play_animation("Idle", mix(0.5).queued()).expect("known");
        // Wave is one second long; until it ends nothing fades.
        for _ in 0..3 {
            step(&mut m, 0.25);
            assert_eq!(value(&m, "ParamExtra"), 0.0);
            assert!(m.fading.is_none());
        }
        step(&mut m, 0.25);
        assert_eq!(value(&m, "ParamExtra"), 0.0, "the handover frame is still the outgoing pose");
        // 0.125 s into a 0.5 s mix: w = 0.146447.
        step(&mut m, 0.125);
        assert!((value(&m, "ParamExtra") - 8.0 * 0.146_446_6).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        step(&mut m, 0.5);
        assert_eq!(value(&m, "ParamExtra"), 8.0);
        assert!(m.fading.is_none());
    }

    #[test]
    fn repeating_parameters_blend_along_the_shortest_arc() {
        // 29 and -29 are two units apart round the loop of [-30, 30], so half way
        // is the seam, not 0.
        let motions = || vec![hold_at("A", 29.0), hold_at("B", -29.0), hold_at("Marker", 360.0)];
        let mut m = with_motions(motions());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.5);
        let v = value(&m, "ParamAngleX");
        assert!((v.abs() - 30.0).abs() < 1e-3, "{v}");
        // A quarter of the way in (w = 0.146447) it is 29 + 2 * w.
        let mut m = with_motions(motions());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        let want = 29.0 + 2.0 * 0.146_446_6;
        assert!((value(&m, "ParamAngleX") - want).abs() < 1e-3, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn pose_at_mid_fade_equals_pose_at_on_a_fresh_model() {
        let motions = || vec![hold("A", "ParamExtra", 0.0), sweep("B", "ParamExtra", 8.0)];
        let mut used = with_three(motions());
        used.play_animation("A", PlayOptions::looping()).expect("known");
        used.play_animation("B", mix(1.0)).expect("known");
        step(&mut used, 0.25);
        assert!(used.fading.is_some());
        used.pose_at("B", 1.0).expect("pose");
        assert!(used.fading.is_none(), "pose_at drops the fade");
        let mut fresh = with_three(motions());
        fresh.pose_at("B", 1.0).expect("pose");
        assert_eq!(used.values, fresh.values);
        assert_eq!(used.pose, fresh.pose);
        step(&mut used, 0.25);
        assert!(used.fading.is_none());
    }

    #[test]
    fn a_loop_wrap_after_a_completed_crossfade_does_not_fade() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), sweep("S", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("S", mix(0.5)).expect("known");
        step(&mut m, 0.5);
        assert!(m.fading.is_none(), "the real crossfade is complete");
        step(&mut m, 1.25);
        // 1.75 s into S: 4 * 1.75 = 7.
        assert!((value(&m, "ParamExtra") - 7.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        step(&mut m, 0.5);
        // 2.25 s wraps to 0.25 s: 1.0, with no fade and no pull toward the default 2.
        assert!(m.fading.is_none());
        assert!((value(&m, "ParamExtra") - 1.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
    }

    #[test]
    fn a_looping_handover_with_an_overshooting_step_fades_from_the_end_of_the_pass() {
        // A loops 0 -> 8 over 2 s; B holds 0. One 0.1 s step from 1.9375 s ends
        // the pass 0.0375 s ago.
        let mut m = with_three(vec![sweep("A", "ParamExtra", 8.0), hold("B", "ParamExtra", 0.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0).queued()).expect("known");
        step(&mut m, 1.9375);
        assert!(m.fading.is_none());
        m.update(Duration::from_secs_f32(0.1)).expect("update");
        let over = 0.0375;
        let fade = m.fading.as_ref().expect("the handover started a fade");
        assert!((fade.elapsed - over).abs() < 1e-3, "the overshoot counts toward the fade: {}", fade.elapsed);
        assert!((m.motion.as_ref().expect("B plays").playhead() - over).abs() < 1e-3);
        // The outgoing side holds the end of the pass (8), not its wrapped start
        // (0.15); w = 0.5 - 0.5 cos(pi * 0.0375) = 0.003466.
        let want = 8.0 - 8.0 * 0.003_466;
        assert!((value(&m, "ParamExtra") - want).abs() < 1e-3, "{} vs {want}", value(&m, "ParamExtra"));
    }

    #[test]
    fn a_one_shot_that_ends_mid_fade_shows_its_last_blended_frame_then_the_idle() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", PlayOptions::once().with_mix(Duration::from_secs(2))).expect("known");
        step(&mut m, 1.0);
        // B ended at 1 s of a 2 s fade: the last frame is the blend at w = 0.5.
        assert!(m.motion.is_none() && m.fading.is_none());
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        step(&mut m, 0.0625);
        assert_eq!(value(&m, "ParamExtra"), 2.0, "then the parameter is back at rest, as after any one-shot");
    }

    #[test]
    fn playing_after_dispose_does_not_panic() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.dispose();
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        m.pose_at("A", 0.5).expect("pose");
    }

    #[test]
    fn a_sweep_past_the_incoming_sides_opposite_point_does_not_flip_the_arc() {
        // A turns 28 -> 32 over 2 s on [-30, 30] (32 wraps to -28), crossing the
        // seam at 1 s, while B holds 0 whose opposite point is that seam. Taking
        // the shortest arc afresh each frame would flip it there: at w = 0.5 the
        // output would jump from 14.95 to -14.95. Keeping the arc continuous moves
        // it by the sweep step plus the weight step times the span only.
        let motions = || {
            let spin = Motion {
                name: "A".into(),
                duration: 2.0,
                curves: vec![MotionCurve {
                    target: "ParamAngleX".into(),
                    keys: vec![k(0.0, 28.0, 2.0), k(2.0, 32.0, 2.0)],
                    ..MotionCurve::default()
                }],
                ..Motion::default()
            };
            vec![spin, hold_at("B", 0.0), hold_at("Marker", 360.0)]
        };
        let mut m = with_motions(motions());
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(2.0)).expect("known");
        let dt = 1.0 / 16.0;
        let bound = 2.0 * dt + 60.0 * (std::f32::consts::FRAC_PI_2 / 2.0) * dt + 1e-3;
        let circular = |x: f32, y: f32| {
            let d = (x - y).rem_euclid(60.0);
            d.min(60.0 - d)
        };
        let mut last = value(&m, "ParamAngleX");
        for _ in 0..32 {
            step(&mut m, dt);
            let now = value(&m, "ParamAngleX");
            assert!(circular(now, last) <= bound, "{last} -> {now}");
            last = now;
        }
        assert!(value(&m, "ParamAngleX").abs() < 1e-3, "ends at B's 0");
    }

    #[test]
    fn physics_sees_the_blended_inputs_with_no_step_during_a_fade() {
        // The input goes from -30 to 30 in 0.3 s at 60 Hz. The weight's steepest
        // slope is pi / 2 per mix, so a frame may move it by at most
        // 60 * (pi / 2 / 0.3) * (1 / 60) = 5.236, against 60 for a cut. The rig's
        // output must equal a fresh simulation fed the hand-blended input
        // sequence -30 + 60 * w(k dt).
        let physics = rig("ParamAngleX", "ParamEyeLOpen", 100.0);
        let mut m = with_physics(vec![hold_at("A", -30.0), hold_at("B", 30.0)], physics);
        let (x, eye) = (
            m.moc().parameters.iter().position(|p| p.id == "ParamAngleX").expect("present"),
            m.moc().parameters.iter().position(|p| p.id == "ParamEyeLOpen").expect("present"),
        );
        let mut reference =
            PhysicsState::new(m.moc().physics.as_ref().expect("rig"), &m.moc().parameters).expect("a rig to simulate");
        let defaults: Vec<f32> = m.moc().parameters.iter().map(|p| p.default).collect();
        let mut feed = |input: f32, dt: f32| {
            let mut v = defaults.clone();
            v[x] = input;
            reference.step(dt, &mut v);
            v[eye]
        };
        m.play_animation("A", PlayOptions::looping()).expect("known");
        feed(-30.0, 0.0);
        let warm = Duration::from_millis(16).as_secs_f32();
        for _ in 0..30 {
            m.update(Duration::from_millis(16)).expect("update");
            feed(-30.0, warm);
        }
        m.play_animation("B", mix(0.3)).expect("known");
        let mut want_eye = feed(-30.0, 0.0);
        assert!((m.values[eye] - want_eye).abs() < 1e-5);
        let dt = Duration::from_secs_f32(1.0 / 60.0).as_secs_f32();
        let mut elapsed = 0.0f32;
        let mut last = value(&m, "ParamAngleX");
        let mut worst = 0.0f32;
        for _ in 0..30 {
            m.update(Duration::from_secs_f32(1.0 / 60.0)).expect("update");
            elapsed += dt;
            let t = (elapsed / 0.3).min(1.0);
            let w = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
            want_eye = feed(-30.0 + 60.0 * w, dt);
            assert!(
                (m.values[eye] - want_eye).abs() < 1e-4,
                "physics output {} vs hand-blended simulation {want_eye}",
                m.values[eye]
            );
            let now = value(&m, "ParamAngleX");
            worst = worst.max((now - last).abs());
            last = now;
        }
        assert!(worst < 5.3, "{worst}");
        assert!(worst > 1.0, "the fade must actually move it: {worst}");
        assert!((last - 30.0).abs() < 1e-3);
    }

    #[test]
    fn switching_again_mid_fade_fades_from_what_was_on_show() {
        let mut m =
            with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0), hold("C", "ParamExtra", 6.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.5);
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-4);
        m.play_animation("C", mix(1.0)).expect("known");
        assert!((value(&m, "ParamExtra") - 4.0).abs() < 1e-4, "no jump on the second switch");
        step(&mut m, 0.5);
        // 4 + (6 - 4) * 0.5
        assert!((value(&m, "ParamExtra") - 5.0).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        step(&mut m, 0.5);
        assert!((value(&m, "ParamExtra") - 6.0).abs() < 1e-4);
        assert!(m.fading.is_none());
    }

    #[test]
    fn stopping_mid_fade_clears_it() {
        let mut m = with_three(vec![hold("A", "ParamExtra", 0.0), hold("B", "ParamExtra", 8.0)]);
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        m.stop_animation("B");
        assert!(m.fading.is_none() && m.motion.is_none());
        assert_eq!(value(&m, "ParamExtra"), 2.0);
        // Stopping the outgoing motion instead cuts to the incoming one.
        m.play_animation("A", PlayOptions::looping()).expect("known");
        m.play_animation("B", mix(1.0)).expect("known");
        step(&mut m, 0.25);
        m.stop_animation("A");
        assert!(m.fading.is_none() && m.motion.is_some());
        step(&mut m, 0.1);
        assert_eq!(value(&m, "ParamExtra"), 8.0);
    }

    // ---- look-at rig ----

    use a2d_core::ir::cubism::{LookAxis, LookBlend, LookParameter, LookRig};

    fn look_rig(
        blend: LookBlend,
        rest: (f32, f32, f32),
        center: (f32, f32, f32),
        drives: &[(&str, LookAxis, f32)],
    ) -> LookRig {
        LookRig {
            blend,
            damping: 0.15,
            rest_target: rest,
            center,
            parameters: drives
                .iter()
                .map(|(p, axis, factor)| LookParameter { parameter: (*p).into(), axis: *axis, factor: *factor })
                .collect(),
        }
    }

    fn with_look(motions: Vec<Motion>, rig: LookRig) -> GenericCubismModel {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameter_repeats = None;
        ir.motions = motions;
        ir.look = Some(rig);
        GenericCubismModel::load(ir, "test")
    }

    fn angle_rig(blend: LookBlend, rest: (f32, f32, f32)) -> LookRig {
        look_rig(blend, rest, (0.0, 0.0, 0.0), &[("ParamAngleX", LookAxis::X, 10.0)])
    }

    fn tick(m: &mut GenericCubismModel) {
        m.update(Duration::from_millis(16)).expect("update");
    }

    /// Runs long enough (6 s) for the look rig's ease-in to have settled.
    fn settle(m: &mut GenericCubismModel) {
        frames(m, 375);
    }

    #[test]
    fn the_rest_offset_applies_by_default_in_each_blend() {
        // Position x 0.4 * factor 10 = 4 against a keyed 5.
        for (blend, want) in [(LookBlend::Additive, 9.0), (LookBlend::Multiply, 20.0), (LookBlend::Override, 4.0)] {
            let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], angle_rig(blend, (0.4, 0.0, 0.0)));
            settle(&mut m);
            assert!((value(&m, "ParamAngleX") - want).abs() < 1e-4, "{blend:?}: {}", value(&m, "ParamAngleX"));
        }
    }

    #[test]
    fn playback_eases_the_look_in_from_zero() {
        // Rest goal x 0.4, factor 10 -> +4 on a keyed 5 once settled. The first
        // 16 ms step is compared with the SmoothDamp formula (hand-computed
        // in look.rs's tests); at t = 0 there is no contribution.
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], angle_rig(LookBlend::Additive, (0.4, 0.0, 0.0)));
        // t = 0: no contribution at all.
        assert_eq!(value(&m, "ParamAngleX"), 5.0);
        tick(&mut m);
        let mut v = [0.0; 3];
        let want = crate::look::smooth_damp([0.0; 3], [0.4, 0.0, 0.0], &mut v, 0.15, 0.016)[0] * 10.0;
        assert!(want > 0.0 && want < 0.2, "{want}");
        assert!((value(&m, "ParamAngleX") - (5.0 + want)).abs() < 1e-5, "{}", value(&m, "ParamAngleX"));
        // Half a second in it is most of the way, and it never overshoots.
        let mut max = value(&m, "ParamAngleX");
        for _ in 0..30 {
            tick(&mut m);
            max = max.max(value(&m, "ParamAngleX"));
        }
        let mid = value(&m, "ParamAngleX");
        assert!(mid > 8.0 && mid < 9.0, "{mid}");
        for _ in 0..345 {
            tick(&mut m);
            max = max.max(value(&m, "ParamAngleX"));
        }
        assert!(max <= 9.0 + 1e-5, "overshot: {max}");
        assert!((value(&m, "ParamAngleX") - 9.0).abs() < 1e-3);
    }

    #[test]
    fn pose_at_shows_the_settled_rest_whatever_the_playback_age() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], angle_rig(LookBlend::Additive, (0.4, 0.0, 0.0)));
        m.pose_at("Idle", 0.0).expect("pose");
        assert!((value(&m, "ParamAngleX") - 9.0).abs() < 1e-5, "fresh model: {}", value(&m, "ParamAngleX"));
        let fresh = m.pose().clone();
        tick(&mut m);
        let easing = value(&m, "ParamAngleX");
        assert!(easing < 9.0);
        m.pose_at("Idle", 0.0).expect("pose");
        assert!((value(&m, "ParamAngleX") - 9.0).abs() < 1e-5);
        assert_eq!(*m.pose(), fresh);
        // The ease-in in flight is put back: playback continues from where it was.
        tick(&mut m);
        assert!(value(&m, "ParamAngleX") > easing && value(&m, "ParamAngleX") < 9.0);
    }

    #[test]
    fn replaying_a_motion_does_not_restart_the_ease_in() {
        // The SDK's controller lives beside the animator: a new clip does not reset it.
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], angle_rig(LookBlend::Additive, (0.4, 0.0, 0.0)));
        settle(&mut m);
        m.play_animation("Idle", PlayOptions::default()).expect("play");
        tick(&mut m);
        assert!((value(&m, "ParamAngleX") - 9.0).abs() < 1e-3);
    }

    #[test]
    fn releasing_the_target_eases_back_to_rest_not_to_zero() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], angle_rig(LookBlend::Additive, (0.2, 0.0, 0.0)));
        m.set_look_target(Some(a2d_core::Vec2::new(1.0, 0.0)));
        settle(&mut m);
        m.set_look_target(None);
        settle(&mut m);
        assert!((value(&m, "ParamAngleX") - 2.0).abs() < 1e-3);
    }

    #[test]
    fn a_model_loaded_from_the_same_ir_eases_in_afresh() {
        let bytes = crate::moc3::tests::Builder::new().build();
        let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
        ir.parameter_repeats = None;
        ir.motions = vec![hold("Idle", "ParamAngleX", 0.0)];
        ir.look = Some(angle_rig(LookBlend::Additive, (0.4, 0.0, 0.0)));
        let mut first = GenericCubismModel::load(ir.clone(), "test");
        settle(&mut first);
        assert!((value(&first, "ParamAngleX") - 4.0).abs() < 1e-3);
        let mut second = GenericCubismModel::load(ir, "test");
        assert_eq!(value(&second, "ParamAngleX"), 0.0);
        tick(&mut second);
        assert!(value(&second, "ParamAngleX") < 0.2);
    }

    #[test]
    fn the_result_is_clamped_to_the_parameters_range() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], angle_rig(LookBlend::Additive, (10.0, 0.0, 0.0)));
        settle(&mut m);
        assert_eq!(value(&m, "ParamAngleX"), 30.0);
    }

    #[test]
    fn the_centre_is_subtracted_from_the_point() {
        let rig =
            look_rig(LookBlend::Additive, (0.5, 0.0, 0.0), (0.1, 0.0, 0.0), &[("ParamAngleX", LookAxis::X, 10.0)]);
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], rig);
        settle(&mut m);
        assert!((value(&m, "ParamAngleX") - 4.0).abs() < 1e-4, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn a_2d_target_leaves_z_at_the_rest_targets() {
        let rig =
            look_rig(LookBlend::Additive, (0.0, 0.0, 0.3), (0.0, 0.0, 0.0), &[("ParamAngleX", LookAxis::Z, 10.0)]);
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], rig);
        m.set_look_target(Some(a2d_core::Vec2::new(0.9, 0.9)));
        frames(&mut m, 300);
        assert!((value(&m, "ParamAngleX") - 3.0).abs() < 1e-4, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn a_driven_target_is_followed_and_released_back_to_rest() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], angle_rig(LookBlend::Additive, (0.2, 0.0, 0.0)));
        settle(&mut m);
        assert!((value(&m, "ParamAngleX") - 2.0).abs() < 1e-4);
        m.set_look_target(Some(a2d_core::Vec2::new(1.0, 0.0)));
        tick(&mut m);
        let first = value(&m, "ParamAngleX");
        assert!(first > 2.0 && first < 10.0, "it eases, not jumps: {first}");
        frames(&mut m, 300);
        assert!((value(&m, "ParamAngleX") - 10.0).abs() < 1e-3);
        m.set_look_target(None);
        tick(&mut m);
        let back = value(&m, "ParamAngleX");
        assert!(back < 10.0 && back > 2.0, "it eases back, not jumps: {back}");
        frames(&mut m, 600);
        assert!((value(&m, "ParamAngleX") - 2.0).abs() < 1e-3);
    }

    #[test]
    fn the_offset_does_not_accumulate_on_a_parameter_nothing_keys() {
        // The eye parameter defaults to 1 on 0..1.2; the rig adds 0.5 * 0.1.
        let rig =
            look_rig(LookBlend::Additive, (0.0, 0.0, 0.5), (0.0, 0.0, 0.0), &[("ParamEyeLOpen", LookAxis::Z, 0.1)]);
        for motions in [vec![hold("Idle", "ParamAngleX", 0.0)], Vec::new()] {
            let mut m = with_look(motions, rig.clone());
            frames(&mut m, 500);
            assert!((value(&m, "ParamEyeLOpen") - 1.05).abs() < 1e-5, "{}", value(&m, "ParamEyeLOpen"));
        }
    }

    #[test]
    fn the_offset_is_in_place_before_physics_reads_its_inputs() {
        // The motion holds ParamAngleX at 0; the rig pushes it to its maximum.
        // A chain fed by ParamAngleX settles at the angle of a maximal input only
        // if physics sees the pushed value.
        let build = |look: Option<LookRig>| {
            let bytes = crate::moc3::tests::Builder::new().build();
            let mut ir: CubismIr = Moc3::parse(&bytes).expect("should parse").into();
            ir.parameter_repeats = None;
            ir.motions = vec![hold("Idle", "ParamAngleX", 0.0)];
            ir.physics = Some(rig("ParamAngleX", "ParamEyeLOpen", 100.0));
            ir.look = look;
            GenericCubismModel::load(ir, "test")
        };
        let mut with = build(Some(angle_rig(LookBlend::Additive, (100.0, 0.0, 0.0))));
        frames(&mut with, 600);
        assert!((value(&with, "ParamEyeLOpen") - SETTLED).abs() < 2e-3, "{}", value(&with, "ParamEyeLOpen"));
        let mut without = build(None);
        frames(&mut without, 600);
        assert!(value(&without, "ParamEyeLOpen").abs() < 2e-3, "{}", value(&without, "ParamEyeLOpen"));
    }

    #[test]
    fn pose_at_applies_the_rest_offset_and_ignores_a_driven_target() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], angle_rig(LookBlend::Additive, (0.2, 0.0, 0.0)));
        m.set_look_target(Some(a2d_core::Vec2::new(1.0, 0.0)));
        frames(&mut m, 300);
        m.pose_at("Idle", 0.5).expect("pose");
        assert!((value(&m, "ParamAngleX") - 2.0).abs() < 1e-4, "{}", value(&m, "ParamAngleX"));
        // The driven target and the smoothing survive it: the next update shows
        // the followed position (10), not the rest offset.
        tick(&mut m);
        assert!((value(&m, "ParamAngleX") - 10.0).abs() < 1e-3, "{}", value(&m, "ParamAngleX"));
    }

    #[test]
    fn a_model_with_a_rig_and_no_motion_shows_the_offset_at_load() {
        let rig = look_rig(
            LookBlend::Override,
            (0.0, 0.0, 0.5),
            (0.0, 0.0, 0.0),
            &[("ParamEyeLOpen", LookAxis::Z, 1.0), ("ParamAngleX", LookAxis::Z, 10.0)],
        );
        let mut m = with_look(Vec::new(), rig);
        let at_load = m.values.clone();
        let pose_at_load = m.pose().clone();
        // The rig starts at zero like the SDK's: an override of zero, not the rest offset yet.
        assert!(value(&m, "ParamEyeLOpen").abs() < 1e-6);
        m.update(Duration::ZERO).expect("update");
        assert_eq!(m.values, at_load);
        assert_eq!(*m.pose(), pose_at_load);
        settle(&mut m);
        assert!((value(&m, "ParamEyeLOpen") - 0.5).abs() < 1e-4);
    }

    #[test]
    fn the_look_is_applied_once_across_a_handover_crossing() {
        // A ramps ParamExtra by 10 per second, B holds 4, the handover is at 0.5 s
        // over a one second fade (see the handover tests above). A look of
        // +0.5 on ParamExtra (position x 1 * factor 0.5) is added after the blend,
        // once, so every expected value is the handover test's plus 0.5.
        let mut m = queued_ramp(Some(0.5), 1.0);
        let rig = look_rig(LookBlend::Additive, (1.0, 0.0, 0.0), (0.0, 0.0, 0.0), &[("ParamExtra", LookAxis::X, 0.5)]);
        m.look = LookState::new(&rig, &m.moc.parameters);
        if let Some(look) = &mut m.look {
            look.rest(); // settled, so the expected values are the handover test's plus 0.5
        }
        steps(&mut m, 6, STEP);
        assert_eq!(m.current_animation(), Some("A"));
        assert!((value(&m, "ParamExtra") - 4.25).abs() < 1e-5, "{}", value(&m, "ParamExtra"));
        steps(&mut m, 2, STEP);
        assert_eq!(m.current_animation(), Some("B"));
        assert!((value(&m, "ParamExtra") - 5.5).abs() < 1e-5, "{}", value(&m, "ParamExtra"));
        steps(&mut m, 2, STEP);
        assert!((value(&m, "ParamExtra") - 6.664_364_5).abs() < 1e-4, "{}", value(&m, "ParamExtra"));
        // Once the fade is over, B alone: 4 + 0.5.
        steps(&mut m, 20, STEP);
        assert!((value(&m, "ParamExtra") - 4.5).abs() < 1e-5, "{}", value(&m, "ParamExtra"));
    }

    #[test]
    fn a_motionless_model_with_a_rig_still_applies_it() {
        // No motion and no idle: the early "nothing drives anything" return must
        // not skip the look.
        let rig =
            look_rig(LookBlend::Override, (0.0, 0.0, 0.5), (0.0, 0.0, 0.0), &[("ParamEyeLOpen", LookAxis::Z, 1.0)]);
        let mut m = with_look(Vec::new(), rig);
        m.idle.clear();
        settle(&mut m);
        assert!((value(&m, "ParamEyeLOpen") - 0.5).abs() < 1e-4);
    }

    #[test]
    fn a_rig_naming_a_missing_parameter_is_left_off_and_reported() {
        let rig = look_rig(LookBlend::Additive, (1.0, 0.0, 0.0), (0.0, 0.0, 0.0), &[("NoSuch", LookAxis::X, 1.0)]);
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 5.0)], rig);
        tick(&mut m);
        assert_eq!(value(&m, "ParamAngleX"), 5.0);
        let mut report = a2d_core::LoadReport::new();
        m.absorb_degradations(&mut report);
        let text: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert!(text.iter().any(|t| t.contains("look-at rig not applied") && t.contains("NoSuch")), "{text:?}");
    }

    #[test]
    fn a_model_without_a_rig_ignores_a_look_target() {
        let mut m = with_motions(vec![hold("Idle", "ParamAngleX", 5.0)]);
        m.set_look_target(Some(a2d_core::Vec2::new(3.0, 3.0)));
        tick(&mut m);
        assert_eq!(value(&m, "ParamAngleX"), 5.0);
    }

    fn emitted(m: &GenericCubismModel) -> RenderList {
        let mut list = RenderList::new();
        m.emit(&mut list);
        list
    }

    #[test]
    fn a_mirror_flips_every_emitted_vertex_about_the_origin() {
        let bytes = crate::moc3::tests::Builder::new().masked().build();
        let mut m = GenericCubismModel::load(Moc3::parse(&bytes).expect("should parse"), "masked");
        let plain = emitted(&m);
        m.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
        let flipped = emitted(&m);
        assert_eq!(plain.meshes().len(), flipped.meshes().len());
        let mut checked = 0;
        for (a, b) in plain.meshes().iter().zip(flipped.meshes()) {
            assert_eq!(a.indices, b.indices);
            assert_eq!(a.uvs, b.uvs, "the texture follows the geometry");
            for (p, q) in a.vertices.iter().zip(&b.vertices) {
                assert_eq!((q.x, q.y), (-p.x, p.y));
                checked += 1;
            }
        }
        assert!(checked > 0);
        assert!(!plain.masks().is_empty(), "the fixture must carry a mask or this half proves nothing");
        assert_eq!(plain.masks().len(), flipped.masks().len());
        for (a, b) in plain.masks().iter().zip(flipped.masks()) {
            for (p, q) in a.vertices.iter().zip(&b.vertices) {
                assert_eq!((q.x, q.y), (-p.x, p.y), "clip shapes mirror with the meshes");
            }
        }
    }

    #[test]
    fn the_magnitude_scales_and_unit_scale_restores() {
        let mut m = model();
        let plain = emitted(&m);
        m.set_scale(a2d_core::Vec2::new(-2.0, 0.5));
        let scaled = emitted(&m);
        for (a, b) in plain.meshes().iter().zip(scaled.meshes()) {
            for (p, q) in a.vertices.iter().zip(&b.vertices) {
                assert_eq!((q.x, q.y), (-2.0 * p.x, 0.5 * p.y));
            }
        }
        m.set_scale(a2d_core::Vec2::new(f32::NAN, 1.0));
        assert_eq!(emitted(&m).meshes()[0].vertices, scaled.meshes()[0].vertices, "a non-finite scale is ignored");
        m.set_scale(a2d_core::Vec2::ONE);
        for (a, b) in plain.meshes().iter().zip(emitted(&m).meshes()) {
            assert_eq!(a.vertices, b.vertices);
        }
    }

    #[test]
    fn bounds_follow_the_scale_and_stay_ordered() {
        let mut m = model();
        let b = m.bounds();
        m.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
        let mirrored = m.bounds();
        assert_eq!((mirrored.min.x, mirrored.max.x), (-b.max.x, -b.min.x));
        assert_eq!((mirrored.min.y, mirrored.max.y), (b.min.y, b.max.y));
        m.set_scale(a2d_core::Vec2::new(-2.0, 3.0));
        let big = m.bounds();
        assert_eq!((big.min.x, big.max.x), (-2.0 * b.max.x, -2.0 * b.min.x));
        assert_eq!((big.min.y, big.max.y), (3.0 * b.min.y, 3.0 * b.max.y));
        assert!(big.min.x <= big.max.x && big.min.y <= big.max.y);
    }

    #[test]
    fn a_target_on_the_right_of_a_mirrored_model_is_the_mirrored_point() {
        let rig = || angle_rig(LookBlend::Additive, (0.2, 0.0, 0.0));
        let settle = |scale: f32, x: f32| {
            let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], rig());
            m.set_scale(a2d_core::Vec2::new(scale, 1.0));
            m.set_look_target(Some(a2d_core::Vec2::new(x, 0.0)));
            frames(&mut m, 400);
            value(&m, "ParamAngleX")
        };
        // Unmirrored, a point at x = 1 gives 10.
        assert!((settle(1.0, 1.0) - 10.0).abs() < 1e-3);
        // Mirrored, the viewer's right is the model's left: the same point on
        // screen is x = -1 in the model's own space, giving -10.
        assert!((settle(-1.0, 1.0) - -10.0).abs() < 1e-3);
        // And the mirrored model reaches the unmirrored value at the mirrored point.
        assert!((settle(-1.0, -1.0) - 10.0).abs() < 1e-3);
        // The rest target is the model's own: mirroring does not move it.
        let rest = |scale: f32| {
            let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], rig());
            m.set_scale(a2d_core::Vec2::new(scale, 1.0));
            tick(&mut m);
            value(&m, "ParamAngleX")
        };
        assert!((rest(-1.0) - rest(1.0)).abs() < 1e-6);
    }

    #[test]
    fn a_scale_set_after_the_target_still_unmirrors_it() {
        let mut m = with_look(vec![hold("Idle", "ParamAngleX", 0.0)], angle_rig(LookBlend::Additive, (0.0, 0.0, 0.0)));
        m.set_look_target(Some(a2d_core::Vec2::new(1.0, 0.0)));
        m.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
        frames(&mut m, 400);
        assert!((value(&m, "ParamAngleX") - -10.0).abs() < 1e-3);
        m.set_scale(a2d_core::Vec2::new(0.0, 1.0));
        frames(&mut m, 600);
        assert!(value(&m, "ParamAngleX").abs() < 1e-3, "a zero scale has no inverse: the target is released");
    }

    #[test]
    fn mirroring_does_not_touch_the_physics() {
        let run = |mirror: bool| {
            let mut m = with_physics(vec![hold_both("Idle", 0.9)], rig("ParamAngleX", "ParamEyeLOpen", 100.0));
            if mirror {
                m.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
            }
            let mut seen = Vec::new();
            for _ in 0..120 {
                m.update(Duration::from_millis(16)).expect("update");
                seen.push(value(&m, "ParamEyeLOpen"));
            }
            seen
        };
        assert_eq!(run(false), run(true));
    }
}

#[cfg(test)]
mod hit_tests {
    use super::*;
    use a2d_core::ir::cubism::{Drawable, HitArea, HitPrecision};

    type Shape = (Vec<(f32, f32)>, Vec<u16>);

    fn drawable(id: &str, indices: Vec<u16>) -> Drawable {
        Drawable {
            id: id.into(),
            parent_deformer: None,
            uvs: Vec::new(),
            indices,
            keyform_begin: 0,
            keyform_count: 0,
            keyform_binding: 0,
            masks: Vec::new(),
            flags: 0,
            part: None,
            texture: 0,
        }
    }

    fn area(name: &str, drawable: u32, precision: HitPrecision) -> HitArea {
        HitArea { name: name.into(), drawable, precision }
    }

    /// A model whose drawables are posed exactly as given. Drawable `i` is
    /// described by the vertices and indices at `shapes[i]`.
    fn rig(shapes: &[Shape], hit_areas: Vec<HitArea>) -> GenericCubismModel {
        let drawables: Vec<Drawable> =
            shapes.iter().enumerate().map(|(i, (_, idx))| drawable(&format!("D{i}"), idx.clone())).collect();
        let ir = CubismIr { drawables, hit_areas, ..CubismIr::default() };
        let mut m = GenericCubismModel::load(ir, "hit");
        m.pose = Pose {
            drawables: shapes.iter().map(|(p, _)| p.clone()).collect(),
            opacities: vec![0.0; shapes.len()],
            draw_orders: vec![500.0; shapes.len()],
            unstable: Vec::new(),
        };
        m
    }

    fn quad(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape {
        (vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)], vec![0, 1, 2, 0, 2, 3])
    }

    fn hit(name: &str) -> Option<HitAreaId> {
        Some(HitAreaId(name.into()))
    }

    #[test]
    fn a_quad_hit_area_hits_inside_and_on_its_edge_but_not_outside() {
        // Fully transparent (opacity 0 in `rig`) and still hit.
        let m = rig(&[quad(-1.0, -1.0, 1.0, 1.0)], vec![area("Head", 0, HitPrecision::Triangles)]);
        assert_eq!(m.hit_test(0.0, 0.0), hit("Head"));
        assert_eq!(m.hit_test(1.0, 0.5), hit("Head"));
        assert_eq!(m.hit_test(-1.0, -1.0), hit("Head"));
        assert_eq!(m.hit_test(1.01, 0.0), None);
        assert_eq!(m.hit_test(0.0, -1.5), None);
    }

    #[test]
    fn a_mirrored_scale_maps_the_point_back_into_model_space() {
        let mut m = rig(&[quad(0.0, 0.0, 2.0, 2.0)], vec![area("Head", 0, HitPrecision::BoundingBox)]);
        m.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
        assert_eq!(m.hit_test(-1.0, 1.0), hit("Head"));
        assert_eq!(m.hit_test(1.0, 1.0), None);
    }

    #[test]
    fn a_non_uniform_scale_is_divided_out_per_axis() {
        let mut m = rig(&[quad(0.0, 0.0, 2.0, 2.0)], vec![area("Head", 0, HitPrecision::BoundingBox)]);
        m.set_scale(a2d_core::Vec2::new(2.0, 0.5));
        // The box is drawn at x 0..4, y 0..1.
        assert_eq!(m.hit_test(3.5, 0.9), hit("Head"));
        assert_eq!(m.hit_test(3.5, 1.1), None);
        assert_eq!(m.hit_test(4.1, 0.5), None);
    }

    #[test]
    fn bounding_box_and_triangles_differ_on_a_triangle() {
        let tri = (vec![(0.0, 0.0), (2.0, 0.0), (0.0, 2.0)], vec![0, 1, 2]);
        let boxed = rig(std::slice::from_ref(&tri), vec![area("A", 0, HitPrecision::BoundingBox)]);
        let exact = rig(&[tri], vec![area("A", 0, HitPrecision::Triangles)]);
        // Inside the AABB, past the hypotenuse.
        assert_eq!(boxed.hit_test(1.8, 1.8), hit("A"));
        assert_eq!(exact.hit_test(1.8, 1.8), None);
        assert_eq!(exact.hit_test(0.5, 0.5), hit("A"));
        // On the hypotenuse itself.
        assert_eq!(exact.hit_test(1.0, 1.0), hit("A"));
    }

    #[test]
    fn triangles_accept_either_winding() {
        let cw = (vec![(0.0, 0.0), (0.0, 2.0), (2.0, 0.0)], vec![0, 1, 2]);
        let m = rig(&[cw], vec![area("A", 0, HitPrecision::Triangles)]);
        assert_eq!(m.hit_test(0.5, 0.5), hit("A"));
        assert_eq!(m.hit_test(1.8, 1.8), None);
    }

    #[test]
    fn the_front_hit_area_in_paint_order_wins() {
        let shapes = [quad(0.0, 0.0, 2.0, 2.0), quad(1.0, 1.0, 3.0, 3.0)];
        let areas = vec![area("Back", 0, HitPrecision::BoundingBox), area("Front", 1, HitPrecision::BoundingBox)];
        let mut m = rig(&shapes, areas);
        // Equal draw orders and no tie list: the later index is painted last.
        assert_eq!(m.hit_test(1.5, 1.5), hit("Front"));
        assert_eq!(m.hit_test(0.5, 0.5), hit("Back"));
        // Raising drawable 0's draw order puts it in front.
        m.pose.draw_orders = vec![600.0, 500.0];
        assert_eq!(m.hit_test(1.5, 1.5), hit("Back"));
    }

    #[test]
    fn an_empty_name_falls_back_to_the_drawable_id() {
        let m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], vec![area("", 0, HitPrecision::BoundingBox)]);
        assert_eq!(m.hit_test(0.5, 0.5), hit("D0"));
    }

    #[test]
    fn no_hit_areas_means_no_hit() {
        let m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], Vec::new());
        assert_eq!(m.hit_test(0.5, 0.5), None);
    }

    #[test]
    fn an_out_of_range_drawable_index_is_a_miss_not_a_panic() {
        let m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], vec![area("Gone", 7, HitPrecision::Triangles)]);
        assert_eq!(m.hit_test(0.5, 0.5), None);
    }

    #[test]
    fn a_zero_scale_is_a_miss() {
        let mut m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], vec![area("A", 0, HitPrecision::BoundingBox)]);
        m.set_scale(a2d_core::Vec2::new(0.0, 1.0));
        assert_eq!(m.hit_test(0.5, 0.5), None);
    }

    #[test]
    fn setting_a_nan_scale_keeps_the_previous_scale() {
        let mut m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], vec![area("A", 0, HitPrecision::BoundingBox)]);
        m.set_scale(a2d_core::Vec2::new(2.0, 2.0));
        m.set_scale(a2d_core::Vec2::new(1.0, f32::NAN));
        // Still drawn at 2x: the box is 0..2.
        assert_eq!(m.hit_test(1.5, 1.5), hit("A"));
        assert_eq!(m.hit_test(2.5, 1.5), None);
    }

    #[test]
    fn a_non_finite_scale_field_is_a_miss_not_a_panic() {
        // `set_scale` refuses such a value, so the guard is reached by writing the field.
        let mut m = rig(&[quad(0.0, 0.0, 1.0, 1.0)], vec![area("A", 0, HitPrecision::BoundingBox)]);
        for scale in [a2d_core::Vec2::new(f32::NAN, 1.0), a2d_core::Vec2::new(1.0, f32::INFINITY)] {
            m.scale = scale;
            assert_eq!(m.hit_test(0.5, 0.5), None);
        }
    }
}
