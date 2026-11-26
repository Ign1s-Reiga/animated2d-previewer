//! Recovering a physics rig from a `CubismPhysicsController` behaviour.
//!
//! The Cubism SDK's C# classes keep what the editor's `.physics3.json` held as
//! Unity serialized fields, and there is no `.physics3.json` in the bundle. With
//! the type tree stripped, the field order is the only schema. It is taken from
//! the SDK's public source (`Framework/Physics/*.cs`), and the one layout read
//! here is the one that fitted three real models to the byte (see
//! `docs/cubism-physics.md` for the evidence).
//!
//! ```text
//! rig        i32 groupCount · group[] · f32 gravity.x · f32 gravity.y
//!            · f32 wind.x · f32 wind.y
//! group      i32 inputCount · input[] · i32 outputCount · output[]
//!            · i32 particleCount · particle[] · normalization
//! input      string source · f32 x2 translationScale · f32 angleScale
//!            · f32 weight · i32 component · bool reflect
//! output     string destination · i32 particle · f32 x2 translationScale
//!            · f32 angleScale · f32 weight · i32 component · bool reflect
//! particle   f32 x6: initial position x, y · mobility · delay · acceleration
//!            · radius
//! normalization  f32 x6: position maximum, minimum, default
//!            · angle maximum, minimum, default
//! ```
//!
//! A `bool` is one byte padded to four. Strings are length-prefixed and padded.
//!
//! Newer SDK builds add a `string` name at the start of every group and an
//! `f32` frame rate after the wind. Those layouts are known from the SDK's
//! history but have not been seen in a real bundle, so they are not read: a body
//! that fits only them is refused, loudly, rather than misread as this one.

use a2d_core::ir::cubism::{
    NormRange, Physics, PhysicsComponent, PhysicsGroup, PhysicsInput, PhysicsNormalization, PhysicsOutput,
    PhysicsParticle,
};
use a2d_core::{DecodeError, LoadReport};
use a2d_unity::Reader;

/// What a group's fixed fields need at the least, so a corrupt count is refused
/// before anything is allocated: three counts and the six normalization floats.
const MIN_GROUP_BYTES: usize = 12 + 24;
/// A string length, six floats and ints, and a padded flag.
const MIN_INPUT_BYTES: usize = 4 + 8 + 4 + 4 + 4 + 4;
/// A string length, the particle index, the same again as an input.
const MIN_OUTPUT_BYTES: usize = 4 + 4 + 8 + 4 + 4 + 4 + 4;
const PARTICLE_BYTES: usize = 24;

/// An input as serialized.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawInput {
    pub source: String,
    pub weight: f32,
    pub component: i32,
    pub reflect: bool,
}

/// An output as serialized.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawOutput {
    pub destination: String,
    pub particle: i32,
    /// Used when the component is a translation.
    pub translation_scale: (f32, f32),
    /// Used when the component is an angle.
    pub angle_scale: f32,
    pub weight: f32,
    pub component: i32,
    pub reflect: bool,
}

/// A group as serialized.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawGroup {
    pub inputs: Vec<RawInput>,
    pub outputs: Vec<RawOutput>,
    pub particles: Vec<PhysicsParticle>,
    /// Position maximum, minimum, default, then angle maximum, minimum, default.
    pub normalization: [f32; 6],
}

/// The whole rig as serialized, before it becomes IR.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawRig {
    pub groups: Vec<RawGroup>,
    pub gravity: (f32, f32),
    pub wind: (f32, f32),
}

fn flag(r: &mut Reader<'_>) -> Result<bool, DecodeError> {
    let at = r.position() as u64;
    let b = r.u8()?;
    r.align(4)?;
    match b {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DecodeError::corrupt_at(format!("a flag byte is {other}, not 0 or 1"), at)),
    }
}

fn pair(r: &mut Reader<'_>) -> Result<(f32, f32), DecodeError> {
    Ok((r.f32()?, r.f32()?))
}

fn read_group(r: &mut Reader<'_>) -> Result<RawGroup, DecodeError> {
    let n = r.count(MIN_INPUT_BYTES, "a physics group's inputs")?;
    let mut inputs = Vec::with_capacity(n);
    for _ in 0..n {
        let source = r.string()?;
        // The two translation/angle scales of an input are not used by the
        // simulation and are zero in every model seen; they are stepped over.
        r.f32()?;
        r.f32()?;
        r.f32()?;
        let weight = r.f32()?;
        let component = r.i32()?;
        let reflect = flag(r)?;
        inputs.push(RawInput { source, weight, component, reflect });
    }

    let n = r.count(MIN_OUTPUT_BYTES, "a physics group's outputs")?;
    let mut outputs = Vec::with_capacity(n);
    for _ in 0..n {
        let destination = r.string()?;
        let particle = r.i32()?;
        let translation_scale = pair(r)?;
        let angle_scale = r.f32()?;
        let weight = r.f32()?;
        let component = r.i32()?;
        let reflect = flag(r)?;
        outputs.push(RawOutput { destination, particle, translation_scale, angle_scale, weight, component, reflect });
    }

    let n = r.count(PARTICLE_BYTES, "a physics group's particles")?;
    let mut particles = Vec::with_capacity(n);
    for _ in 0..n {
        // The initial position is stepped over: every SDK recomputes it from
        // the radii when the rig starts.
        pair(r)?;
        let (mobility, delay, acceleration, radius) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
        particles.push(PhysicsParticle { mobility, delay, acceleration, radius });
    }

    let mut normalization = [0.0f32; 6];
    for v in &mut normalization {
        *v = r.f32()?;
    }
    Ok(RawGroup { inputs, outputs, particles, normalization })
}

/// Decodes a body positioned after the behaviour header and `m_Name`.
///
/// The layout must consume the body exactly. Anything else is a build of the
/// SDK that serializes the rig differently, and is an error, not a guess.
pub(super) fn decode(body: &Reader<'_>) -> Result<RawRig, DecodeError> {
    let mut r = body.clone();
    let result = (|| {
        let n = r.count(MIN_GROUP_BYTES, "the physics rig's groups")?;
        let mut groups = Vec::with_capacity(n);
        for _ in 0..n {
            groups.push(read_group(&mut r)?);
        }
        let gravity = pair(&mut r)?;
        let wind = pair(&mut r)?;
        if r.remaining() != 0 {
            return Err(DecodeError::corrupt(format!("{} bytes are left after the last field", r.remaining())));
        }
        Ok(RawRig { groups, gravity, wind })
    })();
    result.map_err(|error: DecodeError| DecodeError::Reconstruction {
        game: "unity_cubism".into(),
        message: format!(
            "the physics controller does not fit the one layout known ({error}); this Cubism SDK build \
             serializes it differently (newer builds name each group and add a frame rate)"
        ),
    })
}

fn component(raw: i32) -> Option<PhysicsComponent> {
    match raw {
        0 => Some(PhysicsComponent::X),
        1 => Some(PhysicsComponent::Y),
        2 => Some(PhysicsComponent::Angle),
        _ => None,
    }
}

/// Converts decoded rig data to IR. Never produces anything the package writer
/// would refuse; whatever has to be dropped is reported, naming the group by
/// its place in the source.
///
/// `None` when nothing usable is left.
pub(super) fn to_physics(raw: &RawRig, report: &mut LoadReport) -> Option<Physics> {
    let mut physics = Physics { gravity: raw.gravity, wind: raw.wind, groups: Vec::new() };
    if !physics.forces_finite() {
        report.note(format!(
            "physics: gravity {:?} or wind {:?} is not finite; physics is not recovered",
            raw.gravity, raw.wind
        ));
        return None;
    }

    for (g, raw_group) in raw.groups.iter().enumerate() {
        physics.groups.extend(to_group(g, raw_group, report));
    }
    if physics.groups.is_empty() {
        report.note(format!(
            "physics: no usable group among the {} in the source; physics is not recovered",
            raw.groups.len()
        ));
        return None;
    }
    Some(physics)
}

fn to_group(g: usize, raw: &RawGroup, report: &mut LoadReport) -> Option<PhysicsGroup> {
    let mut inputs = Vec::with_capacity(raw.inputs.len());
    for i in &raw.inputs {
        let Some(component) = component(i.component) else {
            report.note(format!(
                "source physics group {g}: input {:?} has component {}, not 0, 1 or 2; the group is dropped",
                i.source, i.component
            ));
            return None;
        };
        inputs.push(PhysicsInput { source: i.source.clone(), component, weight: i.weight, reflect: i.reflect });
    }

    let mut outputs = Vec::with_capacity(raw.outputs.len());
    for (index, o) in raw.outputs.iter().enumerate() {
        let Some(component) = component(o.component) else {
            report.note(format!(
                "source physics group {g}: output {:?} has component {}, not 0, 1 or 2; the group is dropped",
                o.destination, o.component
            ));
            return None;
        };
        // The root particle has no predecessor to be measured against, so only
        // particles 1.. can drive an output. The SDK generation these rigs come
        // from stops at the first output it cannot use, so the ones after it
        // never ran either; they are dropped with it.
        //
        // That entry is information, not a note: in every SDK revision whose
        // layout `decode` accepts, an output whose particle index is past the
        // end of its chain ends that group's output loop and leaves other
        // groups alone, which is exactly what is done here, so nothing the
        // source ran is lost. Established by reading those revisions' behaviour
        // (no source is reproduced here). Every other drop stays a note.
        let particle = u32::try_from(o.particle).unwrap_or(0);
        let candidate = PhysicsOutput {
            destination: o.destination.clone(),
            particle,
            component,
            scale: 0.0,
            weight: o.weight,
            reflect: o.reflect,
        };
        if !candidate.reads_particle_in(raw.particles.len()) {
            let dropped = raw.outputs.len() - index;
            report.info(format!(
                "source physics group {g}: output {:?} reads particle {} of a chain of {}; it and the {} output(s) \
                 after it are dropped, as the source stops at the first unusable output",
                o.destination,
                o.particle,
                raw.particles.len(),
                dropped - 1
            ));
            break;
        }
        let scale = match component {
            PhysicsComponent::X => o.translation_scale.0,
            PhysicsComponent::Y => o.translation_scale.1,
            PhysicsComponent::Angle => o.angle_scale,
        };
        outputs.push(PhysicsOutput { scale, ..candidate });
    }

    let n = raw.normalization;
    // `NormRange::new` orders its ends with `min`/`max`, which would quietly
    // swallow a NaN, so finiteness is settled first.
    if n.iter().any(|v| !v.is_finite()) {
        report.note(format!("source physics group {g}: a normalization value is not finite; the group is dropped"));
        return None;
    }
    let group = PhysicsGroup {
        inputs,
        outputs,
        particles: raw.particles.clone(),
        normalization: PhysicsNormalization {
            // Serialized as maximum, minimum, default.
            position: NormRange::new(n[1], n[0], n[2]),
            angle: NormRange::new(n[4], n[3], n[5]),
        },
    };
    if let Some(problem) = group.problem() {
        report.note(format!("source physics group {g}: {problem}; the group is dropped"));
        return None;
    }
    Some(group)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use a2d_unity::Endian;

    fn put_string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as i32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn put_f32(out: &mut Vec<u8>, v: f32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn put_i32(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn put_flag(out: &mut Vec<u8>, v: bool) {
        out.push(u8::from(v));
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    /// Writes `rig` in the known layout. `named` instead writes the newer one,
    /// with a name per group, to prove it is refused.
    pub(in super::super) fn body(rig: &RawRig, named: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_i32(&mut out, rig.groups.len() as i32);
        for g in &rig.groups {
            if named {
                put_string(&mut out, "Group");
            }
            put_i32(&mut out, g.inputs.len() as i32);
            for i in &g.inputs {
                put_string(&mut out, &i.source);
                for v in [0.0, 0.0, 0.0, i.weight] {
                    put_f32(&mut out, v);
                }
                put_i32(&mut out, i.component);
                put_flag(&mut out, i.reflect);
            }
            put_i32(&mut out, g.outputs.len() as i32);
            for o in &g.outputs {
                put_string(&mut out, &o.destination);
                put_i32(&mut out, o.particle);
                for v in [o.translation_scale.0, o.translation_scale.1, o.angle_scale, o.weight] {
                    put_f32(&mut out, v);
                }
                put_i32(&mut out, o.component);
                put_flag(&mut out, o.reflect);
            }
            put_i32(&mut out, g.particles.len() as i32);
            let mut y = 0.0;
            for p in &g.particles {
                y += p.radius;
                for v in [0.0, y, p.mobility, p.delay, p.acceleration, p.radius] {
                    put_f32(&mut out, v);
                }
            }
            for v in g.normalization {
                put_f32(&mut out, v);
            }
        }
        for v in [rig.gravity.0, rig.gravity.1, rig.wind.0, rig.wind.1] {
            put_f32(&mut out, v);
        }
        out
    }

    fn particle(radius: f32) -> PhysicsParticle {
        PhysicsParticle { mobility: 0.9, delay: 0.8, acceleration: 1.2, radius }
    }

    pub(in super::super) fn sample() -> RawRig {
        RawRig {
            groups: vec![
                RawGroup {
                    inputs: vec![
                        RawInput { source: "ParamAngleX".into(), weight: 60.0, component: 0, reflect: false },
                        RawInput { source: "ParamAngleZ".into(), weight: 40.0, component: 2, reflect: true },
                    ],
                    outputs: vec![
                        RawOutput {
                            destination: "ParamHairA".into(),
                            particle: 1,
                            translation_scale: (0.0, 0.0),
                            angle_scale: 30.0,
                            weight: 100.0,
                            component: 2,
                            reflect: false,
                        },
                        RawOutput {
                            destination: "ParamHairB".into(),
                            particle: 2,
                            translation_scale: (0.5, 0.25),
                            angle_scale: 7.0,
                            weight: 25.0,
                            component: 1,
                            reflect: true,
                        },
                    ],
                    particles: vec![particle(0.0), particle(10.0), particle(12.0)],
                    // Maximum before minimum, as serialized.
                    normalization: [20.0, -20.0, 0.0, 40.0, -40.0, 2.0],
                },
                RawGroup {
                    inputs: vec![],
                    outputs: vec![],
                    particles: vec![particle(0.0)],
                    normalization: [1.0, -1.0, 0.0, 1.0, -1.0, 0.0],
                },
            ],
            gravity: (0.0, -1.0),
            wind: (0.5, 0.0),
        }
    }

    fn decode_bytes(bytes: &[u8]) -> Result<RawRig, DecodeError> {
        decode(&Reader::new(bytes, Endian::Little))
    }

    #[test]
    fn a_hand_built_body_decodes_to_the_rig_that_wrote_it() {
        let want = sample();
        assert_eq!(decode_bytes(&body(&want, false)).unwrap(), want);
    }

    #[test]
    fn trailing_bytes_fit_no_layout() {
        let mut bytes = body(&sample(), false);
        bytes.extend_from_slice(&[0; 4]);
        let err = decode_bytes(&bytes).unwrap_err();
        assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err:?}");
        assert!(err.to_string().contains("left after the last field"), "{err}");
    }

    #[test]
    fn a_truncated_body_is_an_error_at_every_length() {
        let bytes = body(&sample(), false);
        for cut in 0..bytes.len() {
            assert!(decode_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn the_newer_layout_with_a_name_per_group_is_refused_not_misread() {
        let err = decode_bytes(&body(&sample(), true)).unwrap_err();
        assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err:?}");
        assert!(err.to_string().contains("serializes it differently"), "{err}");
    }

    #[test]
    fn an_implausible_count_is_refused_before_allocating() {
        let mut bytes = body(&sample(), false);
        bytes[..4].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(decode_bytes(&bytes).unwrap_err().to_string().contains("declares"));
        bytes[..4].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(decode_bytes(&bytes).unwrap_err().to_string().contains("negative"));
    }

    #[test]
    fn a_flag_byte_that_is_not_zero_or_one_is_refused() {
        let mut rig = sample();
        rig.groups[0].inputs[0].reflect = true;
        let mut bytes = body(&rig, false);
        // The first flag byte follows count, string (4 + 12), four floats, and the component.
        let at = 4 + 4 + (4 + 12) + 16 + 4;
        assert_eq!(bytes[at], 1);
        bytes[at] = 2;
        assert!(decode_bytes(&bytes).unwrap_err().to_string().contains("not 0 or 1"));
    }

    #[test]
    fn conversion_picks_the_scale_by_component_and_orders_the_ranges() {
        let mut report = LoadReport::new();
        let physics = to_physics(&sample(), &mut report).unwrap();
        assert!(report.is_empty(), "{report}");
        assert_eq!((physics.gravity, physics.wind), ((0.0, -1.0), (0.5, 0.0)));
        assert_eq!(physics.groups.len(), 2);

        let g = &physics.groups[0];
        assert_eq!(g.inputs[0].component, PhysicsComponent::X);
        assert_eq!(g.inputs[1].component, PhysicsComponent::Angle);
        assert!(g.inputs[1].reflect && !g.inputs[0].reflect);
        assert_eq!((g.inputs[0].weight, g.inputs[1].weight), (60.0, 40.0));
        // An angle output takes the angle scale, a Y output the Y translation scale.
        assert_eq!((g.outputs[0].component, g.outputs[0].scale), (PhysicsComponent::Angle, 30.0));
        assert_eq!((g.outputs[1].component, g.outputs[1].scale), (PhysicsComponent::Y, 0.25));
        assert_eq!((g.outputs[1].particle, g.outputs[1].weight, g.outputs[1].reflect), (2, 25.0, true));
        assert_eq!(g.particles.len(), 3);
        assert_eq!(g.particles[1], particle(10.0));
        assert_eq!(g.normalization.position, NormRange { minimum: -20.0, maximum: 20.0, default: 0.0 });
        assert_eq!(g.normalization.angle, NormRange { minimum: -40.0, maximum: 40.0, default: 2.0 });
    }

    #[test]
    fn a_range_serialized_the_wrong_way_round_is_put_in_order() {
        let mut rig = sample();
        rig.groups[0].normalization = [-5.0, 5.0, 1.0, -8.0, 8.0, 0.0];
        let physics = to_physics(&rig, &mut LoadReport::new()).unwrap();
        let n = physics.groups[0].normalization;
        assert_eq!((n.position.minimum, n.position.maximum), (-5.0, 5.0));
        assert_eq!((n.angle.minimum, n.angle.maximum), (-8.0, 8.0));
    }

    #[test]
    fn an_output_naming_a_particle_outside_the_chain_is_dropped_with_the_ones_after_it() {
        let mut rig = sample();
        // Index 3 of a three-particle chain, index 0 (the root) and a negative one.
        for bad in [3, 0, -1] {
            rig.groups[0].outputs[0].particle = bad;
            let mut report = LoadReport::new();
            let physics = to_physics(&rig, &mut report).unwrap();
            // The good output after the bad one is dropped too: the source stops there.
            assert!(physics.groups[0].outputs.is_empty(), "particle {bad}");
            let text = report.to_string();
            assert!(text.contains("ParamHairA") && text.contains("it and the 1 output(s) after it"), "{text}");
            assert_eq!(report.len(), 1);
            // Information: the source stops there too, so nothing it ran is lost.
            assert!(matches!(report.warnings()[0], a2d_core::Degradation::Info(_)), "{report}");
        }
        // A bad output after good ones keeps the good ones.
        rig.groups[0].outputs[0].particle = 1;
        rig.groups[0].outputs[1].particle = 3;
        let mut report = LoadReport::new();
        let physics = to_physics(&rig, &mut report).unwrap();
        assert_eq!(physics.groups[0].outputs.len(), 1);
        assert!(report.to_string().contains("it and the 0 output(s)"), "{report}");
    }

    #[test]
    fn import_notes_say_source_groups_and_the_ir_says_its_own_after_a_drop() {
        let mut rig = sample();
        rig.groups[0].particles[1].radius = f32::NAN;
        rig.groups[1].inputs.push(RawInput {
            source: "ParamNoSuch".into(),
            weight: 10.0,
            component: 0,
            reflect: false,
        });
        let mut report = LoadReport::new();
        let physics = to_physics(&rig, &mut report).unwrap();
        assert!(report.to_string().contains("source physics group 0"), "{report}");
        // The surviving group is source group 1 but IR group 0.
        let ir = a2d_core::ir::cubism::CubismIr { physics: Some(physics), ..Default::default() };
        let text = ir.unresolved_physics_targets()[0].to_string();
        assert!(text.contains("physics group 0") && text.contains("ParamNoSuch"), "{text}");
    }

    #[test]
    fn a_group_with_non_finite_or_out_of_range_values_is_dropped_and_the_rest_kept() {
        type Damage = fn(&mut RawGroup);
        let cases: [(&str, Damage); 6] = [
            ("not finite", |g| g.particles[1].radius = f32::NAN),
            ("not finite", |g| g.normalization[3] = f32::INFINITY),
            ("not finite", |g| g.outputs[0].angle_scale = f32::NAN),
            ("outside 0..=100", |g| g.inputs[0].weight = 101.0),
            ("outside 0..=100", |g| g.outputs[0].weight = -1.0),
            ("is negative", |g| g.particles[1].radius = -1.0),
        ];
        for (what, damage) in cases {
            let mut rig = sample();
            damage(&mut rig.groups[0]);
            let mut report = LoadReport::new();
            let physics = to_physics(&rig, &mut report).unwrap();
            assert_eq!(physics.groups.len(), 1, "{what}: only the healthy group is kept");
            let text = report.to_string();
            assert!(text.contains("physics group 0") && text.contains(what) && text.contains("dropped"), "{text}");
            assert!(physics.problem().is_none());
        }
    }

    #[test]
    fn other_physics_drops_stay_notes() {
        use a2d_core::Degradation;
        let mut rig = sample();
        rig.groups[0].outputs[0].component = 3;
        let mut report = LoadReport::new();
        to_physics(&rig, &mut report).unwrap();
        assert!(matches!(report.warnings(), [Degradation::Note(_)]), "{report}");

        let mut rig = sample();
        rig.groups[0].particles[1].radius = f32::NAN;
        let mut report = LoadReport::new();
        to_physics(&rig, &mut report).unwrap();
        assert!(report.warnings().iter().all(|w| matches!(w, Degradation::Note(_))), "{report}");
        assert!(!report.is_empty());
    }

    #[test]
    fn an_unknown_component_drops_its_group() {
        let mut rig = sample();
        rig.groups[0].inputs[0].component = 3;
        let mut report = LoadReport::new();
        assert_eq!(to_physics(&rig, &mut report).unwrap().groups.len(), 1);
        assert!(report.to_string().contains("not 0, 1 or 2"));

        let mut rig = sample();
        rig.groups[0].outputs[0].component = -1;
        assert_eq!(to_physics(&rig, &mut LoadReport::new()).unwrap().groups.len(), 1);
    }

    #[test]
    fn a_rig_with_nothing_usable_yields_no_physics() {
        let mut rig = sample();
        rig.gravity.1 = f32::NAN;
        let mut report = LoadReport::new();
        assert!(to_physics(&rig, &mut report).is_none());
        assert!(report.to_string().contains("gravity"));

        let mut rig = sample();
        rig.groups.truncate(1);
        rig.groups[0].inputs[0].weight = f32::NAN;
        let mut report = LoadReport::new();
        assert!(to_physics(&rig, &mut report).is_none());
        assert_eq!(report.len(), 2, "the group and then the empty result are each reported");

        let empty = RawRig { groups: vec![], ..sample() };
        assert!(to_physics(&empty, &mut LoadReport::new()).is_none());
    }

    #[test]
    fn a_group_with_no_outputs_is_kept() {
        // It does nothing, but dropping it would hide that the source had it.
        let physics = to_physics(&sample(), &mut LoadReport::new()).unwrap();
        assert!(physics.groups[1].outputs.is_empty());
    }
}
