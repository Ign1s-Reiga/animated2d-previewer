//! Checks the loop seam of every motion of a real model.
//!
//! A looping motion wraps from its last frame to its first, and a viewer shows
//! that as a pop unless the two frames are the same pose. Cubism lets an author
//! mark a parameter as *repeating*, so a motion can turn it through several
//! whole ranges and still close; clamping such a parameter instead freezes it
//! at an end and snaps it back at the wrap. The flag is stored in the MOC3
//! (section 54) and read into `CubismIr::parameter_repeats`; this test is what
//! says the flagged set closes the seam on a real model.
//!
//! For every motion, physics off (it is a simulation and has its own
//! tests), the pose at the end is compared with the pose at the start:
//!
//! * no drawable's opacity changes by 0.01 or more, wrapped or not;
//! * no drawable showing at both ends moves more than the bound below.
//!
//! The bound is not zero. Wrapping a parameter into `[min, max)` puts a value
//! at the maximum onto the minimum, but the keyform grid still treats the two
//! as different positions, so a drawable whose keyforms differ between the two
//! ends of a repeating parameter moves by that difference when a curve crosses
//! the join. Whether the editor's Core treats the keyform grid the same way is
//! unverified (the editor's manual says the rightmost key is not recognised, so
//! the maximum *is* the minimum). The test measures the residual directly -- the largest
//! move of any visible drawable between a repeating parameter at its minimum
//! and at its maximum, everything else at rest -- and allows that much, plus
//! 1e-4. A model that flags nothing has no such allowance and must close to
//! within 1e-3 canvas units, the size of the largest motion in a single
//! frame at 60 Hz.
//!
//! Measured before wrapping existed: a seam move of 0.0357 canvas units over
//! eleven drawables and an opacity pop of 0.70 on the one model that has
//! repeating parameters; every other model already closed to 0.0.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_motion_seam -- --ignored --nocapture
//! ```

use a2d_core::ir::cubism::CubismIr;
use a2d_core::{AnimatedModel, LoadReport};
use a2d_cubism::{CubismEval, GenericCubismModel, Pose};

const VISIBLE: f32 = 0.01;

fn load() -> Option<CubismIr> {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return None };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    Some(package.model.as_cubism().expect("a Cubism package").clone())
}

/// `(largest vertex move of a drawable visible in both poses, largest opacity change)`.
fn difference(a: &Pose, b: &Pose) -> (f32, f32) {
    let mut moved = 0.0f32;
    let mut opacity = 0.0f32;
    for (d, (pa, pb)) in a.drawables.iter().zip(&b.drawables).enumerate() {
        let (oa, ob) = (a.opacities[d], b.opacities[d]);
        opacity = opacity.max((oa - ob).abs());
        if oa > VISIBLE && ob > VISIBLE {
            for (p, q) in pa.iter().zip(pb) {
                moved = moved.max(((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt());
            }
        }
    }
    (moved, opacity)
}

/// The most a drawable that shows at both ends moves between `parameter` at its
/// minimum and at its maximum, the rest of the model at defaults.
fn join_residual(ir: &CubismIr, parameter: usize) -> f32 {
    let mut values: Vec<f32> = ir.parameters.iter().map(|p| p.default).collect();
    values[parameter] = ir.parameters[parameter].minimum;
    let low = ir.pose(&values);
    values[parameter] = ir.parameters[parameter].maximum;
    difference(&low, &ir.pose(&values)).0
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_motion_closes_its_loop_seam() {
    let Some(mut ir) = load() else { return };
    ir.physics = None;

    let repeating = ir.repeating_parameters();
    for d in ir.repeat_degradations() {
        println!("{d}");
    }
    let allowance = repeating.iter().map(|&p| join_residual(&ir, p)).fold(0.0f32, f32::max) + 1e-4;
    let bound = if repeating.is_empty() { 1e-3 } else { allowance };
    println!(
        "{} repeating parameters: {:?}; seam move bound {bound:.5}",
        repeating.len(),
        repeating.iter().map(|&p| ir.parameters[p].id.as_str()).collect::<Vec<_>>()
    );

    let motions: Vec<(String, f32)> = ir.motions.iter().map(|m| (m.name.clone(), m.duration)).collect();
    assert!(!motions.is_empty(), "the bundle holds no motion");
    let mut model = GenericCubismModel::load(ir, "seam");
    let mut worst_move = 0.0f32;
    for (name, duration) in &motions {
        model.pose_at(name, 0.0).expect("the motion should pose");
        let start = model.pose().clone();
        model.pose_at(name, *duration).expect("the motion should pose");
        let end = model.pose().clone();
        let (moved, opacity) = difference(&end, &start);
        println!("  {name:<32} {duration:7.3}s  seam move {moved:.5}  opacity change {opacity:.5}");
        assert!(opacity < 0.01, "{name}: a drawable's opacity jumps by {opacity} at the loop wrap");
        assert!(moved <= bound, "{name}: a drawable moves {moved} canvas units at the loop wrap (bound {bound})");
        worst_move = worst_move.max(moved);
    }
    println!("worst seam move {worst_move:.5}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_inferred_parameter_is_flagged() {
    let Some(ir) = load() else { return };
    let flags = ir.parameter_repeats.clone().expect("a real MOC3 carries the repeat section");
    assert_eq!(flags.len(), ir.parameters.len());
    // What the motions alone would suggest must be a subset of what the source
    // marks: a parameter the motions push far outside its range but the source
    // does not mark would be one the inference and the editor disagree about.
    let mut without = ir.clone();
    without.parameter_repeats = None;
    let inferred = without.repeating_parameters();
    for &p in &inferred {
        assert!(flags[p], "{} is inferred to repeat but the source does not mark it", ir.parameters[p].id);
    }
    assert_eq!(ir.repeating_parameters(), (0..flags.len()).filter(|&i| flags[i]).collect::<Vec<_>>());
    // The two agree, so nothing is reported as a disagreement.
    assert!(ir.repeat_degradations().is_empty(), "{:?}", ir.repeat_degradations());
    println!("{} flagged, {} inferred", ir.repeating_parameters().len(), inferred.len());
}
