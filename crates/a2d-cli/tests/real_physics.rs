//! Recovers the physics rig of a real Unity bundle.
//!
//! The synthetic tests prove the decoder and its test writer agree; only a real
//! bundle says the serialized layout is the one the engine writes. The layout
//! reads a body only if it consumes it to the byte, so recovering a rig at all
//! is already strong evidence. What is asserted here is what any rig must
//! satisfy, not counts that belong to one model.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_physics -- --ignored --nocapture
//! ```

use a2d_core::ir::cubism::CubismIr;
use a2d_core::LoadReport;

#[path = "support/mod.rs"]
mod support;

use a2d_pack::{Package, PackageModel};

fn import() -> Option<(Package, LoadReport)> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let mut report = LoadReport::new();
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should reconstruct");
    Some((package, report))
}

fn model(package: &Package) -> &CubismIr {
    package.model.as_cubism().expect("a Cubism package")
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn the_physics_rig_of_a_real_bundle_is_recovered_whole_and_coherent() {
    let Some((package, report)) = import() else { return };
    let ir = model(&package);
    println!("{report}");

    let physics = ir.physics.as_ref().expect("the bundle's physics controller should be recovered");
    let inputs: usize = physics.groups.iter().map(|g| g.inputs.len()).sum();
    let outputs: usize = physics.groups.iter().map(|g| g.outputs.len()).sum();
    let particles: usize = physics.groups.iter().map(|g| g.particles.len()).sum();
    println!("physics: {} groups, {inputs} inputs, {outputs} outputs, {particles} particles", physics.groups.len());

    assert!(!physics.groups.is_empty());
    assert!(inputs > 0 && outputs > 0, "a rig that reads or writes nothing does nothing");
    assert!(physics.problem().is_none(), "{:?}", physics.problem());
    for (g, group) in physics.groups.iter().enumerate() {
        assert!(!group.particles.is_empty(), "group {g} has no particles");
        for o in &group.outputs {
            assert!((1..group.particles.len() as u32).contains(&o.particle), "group {g} output {:?}", o.destination);
        }
    }

    // Every id the rig names is a parameter of the model.
    assert!(ir.unresolved_physics_targets().is_empty(), "{:?}", ir.unresolved_physics_targets());

    // Nothing the importer reports about physics may be a decode failure: a
    // layout change must not hide behind a model that merely loads without it.
    let text = report.to_string();
    assert!(!text.contains("physics controller"), "{text}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn recovered_physics_survives_a_package_byte_for_byte_and_validates() {
    let Some((package, _)) = import() else { return };
    let first = package.encode_model().expect("the model should encode");
    let PackageModel::Cubism(back) = Package::decode_model(&first).expect("the model should decode") else {
        panic!("a Cubism package must decode as one")
    };
    assert_eq!(back.physics, model(&package).physics);
    assert_eq!(Package::from_cubism(back, "x", "y").encode_model().expect("re-encode"), first);

    // Expected: notes recorded at import and the repeat notes `validate`
    // re-derives for inferred looping parameters. Anything else is a real problem.
    let problems = support::unexpected_validate_warnings(&package);
    assert!(problems.is_empty(), "validate reported: {problems:#?}");
}

// ---- simulation ----

use a2d_core::{AnimatedModel, RenderList};
use a2d_cubism::{GenericCubismModel, PhysicsState};
use std::time::Duration;

/// Everything the model would draw, flattened.
fn drawn(model: &GenericCubismModel) -> Vec<f32> {
    let mut list = RenderList::new();
    model.emit(&mut list);
    list.meshes().iter().flat_map(|m| m.vertices.iter().flat_map(|v| [v.x, v.y])).collect()
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn ten_seconds_of_playback_stays_finite_and_physics_moves_something() {
    let Some((package, _)) = import() else { return };
    let ir = model(&package).clone();
    let mut bare_ir = ir.clone();
    bare_ir.physics = None;
    let mut with = GenericCubismModel::load(ir.clone(), "physics");
    let mut without = GenericCubismModel::load(bare_ir, "no physics");

    let mut differing_frames = 0;
    for frame in 0..600 {
        with.update(Duration::from_secs_f64(1.0 / 60.0)).expect("update");
        without.update(Duration::from_secs_f64(1.0 / 60.0)).expect("update");
        for p in &ir.parameters {
            let v = with.parameter_value(&p.id).expect("declared");
            assert!(v.is_finite(), "frame {frame}: {} is {v}", p.id);
            assert!(v >= p.minimum.min(p.maximum) && v <= p.minimum.max(p.maximum), "{} out of range: {v}", p.id);
        }
        let (a, b) = (drawn(&with), drawn(&without));
        assert!(a.iter().all(|v| v.is_finite()), "frame {frame}: non-finite geometry");
        if a != b {
            differing_frames += 1;
        }
    }
    println!("physics changed the drawing on {differing_frames} of 600 frames");
    assert!(differing_frames > 0, "physics changed nothing at all");

    // Some output parameter must differ from the motion-only run.
    let physics = ir.physics.as_ref().expect("recovered");
    let changed = physics
        .groups
        .iter()
        .flat_map(|g| &g.outputs)
        .filter(|o| with.parameter_value(&o.destination) != without.parameter_value(&o.destination))
        .count();
    println!("{changed} output parameters differ from the motion-only value after 10 s");
    assert!(changed > 0);

    // `pose_at` stays a function of the timestamp.
    let name = ir.motions.first().map(|m| m.name.clone());
    if let Some(name) = name {
        with.pose_at(&name, 0.7).expect("pose");
        let first = drawn(&with);
        with.update(Duration::from_millis(33)).expect("update");
        with.pose_at(&name, 0.7).expect("pose");
        assert_eq!(drawn(&with), first);
    }
}

/// The sign this algorithm predicts for a swing, and why.
///
/// Take a chain hanging straight down, `(0, 1)` being down in physics space, and
/// look at the first segment, measured from down. Every input is negated unless
/// reflected, so with `s_in = -1` for an ordinary input and `+1` for a
/// reflected one, raising a parameter above its midpoint adds `s_in * n` with
/// `n > 0`.
///
/// * An **angle** input adds `A = s_in * n` degrees. Gravity turns to
///   `(sin A, cos A)` and the chain swings to hang along it, which turns the
///   first segment by `-A` radians. The first swing is therefore
///   `-s_in * sign`.
/// * An **X** input moves the root by `T.x = s_in * n`. The next particle is
///   left behind at `(0, r)`, so relative to the root it sits at
///   `(-T.x, r)`, tilted the other way, and the first segment turns by
///   `+atan(T.x / r)`. The first swing is `+s_in * sign`: opposite to an angle
///   input with the same flags, which is the hair lagging behind a moving head
///   rather than following a tilting one.
///
/// A non-reflected output keeps that sign and a reflected one flips it; a
/// negative output scale flips it too. For each group whose inputs from the
/// driven parameter agree on that sign, drive the parameter from its default to
/// its maximum in 0.3 s and require the largest excursion from the undriven run
/// in the following 0.6 s to have the predicted sign. This is the only guard
/// against a mirrored sway until the cross-implementation comparison exists.
#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn driving_the_most_used_input_swings_first_segment_outputs_the_predicted_way() {
    use a2d_core::ir::cubism::PhysicsComponent;
    use std::collections::HashMap;

    let Some((package, _)) = import() else { return };
    let ir = model(&package);
    let physics = ir.physics.as_ref().expect("recovered");

    // The parameter feeding the most inputs, among groups with an angle output
    // on particle 1: the prediction below covers the first segment only, and a
    // small rig may read only deeper particles (one of six real models has no
    // particle-1 output at all), where this test has nothing to predict.
    let groups: Vec<_> = physics
        .groups
        .iter()
        .filter(|g| g.outputs.iter().any(|o| o.particle == 1 && o.component == PhysicsComponent::Angle))
        .collect();
    if groups.is_empty() {
        println!("skipped: no physics group has an angle output on particle 1, so the first-segment prediction does not apply");
        return;
    }
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for i in groups.iter().flat_map(|g| &g.inputs) {
        *uses.entry(i.source.as_str()).or_default() += 1;
    }
    let (driven, count) = uses
        .iter()
        .max_by_key(|(id, n)| (**n, std::cmp::Reverse(**id)))
        .map(|(a, b)| (*a, *b))
        .expect("groups with a particle-1 output have inputs");
    let param = ir.parameter(driven).expect("resolves");
    let index = ir.motion_target(driven).expect("resolves");
    println!("driving {driven} ({count} inputs), range {}..{}", param.minimum, param.maximum);

    // Largest excursion of every parameter from the undriven run, signed.
    let mut driven_state = PhysicsState::new(physics, &ir.parameters).expect("a rig");
    let mut rest_state = PhysicsState::new(physics, &ir.parameters).expect("a rig");
    let mut peak = vec![0.0f32; ir.parameters.len()];
    for frame in 0..54 {
        let t = (frame as f32 / 60.0 / 0.3).min(1.0);
        let mut up: Vec<f32> = ir.parameters.iter().map(|p| p.default).collect();
        up[index] = param.default + (param.maximum - param.default) * t;
        let mut rest: Vec<f32> = ir.parameters.iter().map(|p| p.default).collect();
        driven_state.step(1.0 / 60.0, &mut up);
        rest_state.step(1.0 / 60.0, &mut rest);
        for (k, p) in peak.iter_mut().enumerate() {
            let delta = up[k] - rest[k];
            if delta.abs() > p.abs() {
                *p = delta;
            }
        }
    }

    let (mut checked, mut wrong) = (0, 0);
    for (g, group) in physics.groups.iter().enumerate() {
        // +1 or -1 per driven input: the sign of the first swing it asks for.
        let factors: Vec<f32> = group
            .inputs
            .iter()
            .filter(|i| i.source == driven && i.weight > 0.0)
            .filter_map(|i| {
                let s_in = if i.reflect { 1.0 } else { -1.0 };
                match i.component {
                    PhysicsComponent::Angle => Some(-s_in),
                    PhysicsComponent::X => Some(s_in),
                    PhysicsComponent::Y => None,
                }
            })
            .collect();
        let Some(&factor) = factors.first() else { continue };
        if factors.iter().any(|f| *f != factor)
            || factors.len() != group.inputs.iter().filter(|i| i.source == driven && i.weight > 0.0).count()
        {
            continue;
        }
        for o in group.outputs.iter().filter(|o| o.particle == 1 && o.component == PhysicsComponent::Angle) {
            let out = ir.motion_target(&o.destination).expect("resolves");
            let delta = peak[out];
            let p = &ir.parameters[out];
            let saturated = |v: f32| v <= p.minimum || v >= p.maximum;
            // A saturated or negligible swing says nothing about the sign.
            if delta.abs() < 1e-4 || saturated(p.default + delta) {
                continue;
            }
            let s_out = if o.reflect { -1.0 } else { 1.0 };
            let expected = factor * s_out * o.scale.signum();
            checked += 1;
            if delta.signum() != expected {
                wrong += 1;
                println!("group {g} -> {}: excursion {delta}, expected sign {expected}", o.destination);
            }
        }
    }
    println!("{checked} first-segment outputs checked, {wrong} against the predicted sign");
    assert!(checked > 0, "no group has a checkable input from {driven}");
    assert_eq!(wrong, 0);
}
