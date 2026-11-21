//! Checks deformer composition against a real model, where only a real model
//! can say whether the chain is the one the format means.
//!
//! The synthetic tests in `a2d-cubism` pin the composition rule by
//! hand-computed geometry. What they cannot show is that the rule survives a
//! rig with dozens of nested warps and rotations, and that the *premise* of the
//! rule holds in real data. Three checks do that, and they are not equally
//! independent of the evaluator:
//!
//! * **Evidence.** A rigid drawable -- one keyform, hung directly off a
//!   rotation deformer -- is an exact similarity of its own texture layout in
//!   the file. That is a property of the data alone, and it is what says a
//!   rotation must not deform what hangs below it: at least 85% of such
//!   drawables must measure as similar before any evaluation happens.
//! * **Regression guards.** Under the rule, the posed mesh keeps those
//!   proportions at rest, and over a motion changes size only within the
//!   product of the rotation scales above it. Both hold *by construction* of
//!   the rule, so they cannot show it is right; they catch a change that stops
//!   applying it. They would have failed on the rule they replaced.
//! * **Glue at rest.** Glued pairs were one surface when drawn, so they
//!   coincide at rest. How many do, and how far apart the rest are, is a
//!   measurement of the evaluator against the data.
//!
//! Glue is cleared for all three, because a glue moves vertices after the
//! chain and would hide a chain defect or add its own distortion.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_deform -- --ignored --nocapture
//! ```

use a2d_core::ir::cubism::{CubismIr, DeformerKind};
use a2d_core::{AnimatedModel, LoadReport};
use a2d_cubism::{CubismEval, GenericCubismModel};

fn load() -> Option<CubismIr> {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return None };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    Some(package.model.as_cubism().expect("a Cubism package").clone())
}

/// The model with its glue removed: what the deformer chains alone produce.
fn without_glue(ir: &CubismIr) -> CubismIr {
    let mut ir = ir.clone();
    ir.glues.clear();
    ir
}

/// Drawables with one keyform hung directly off a rotation deformer.
fn rigid_candidates(ir: &CubismIr) -> Vec<usize> {
    ir.drawables
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            d.keyform_count == 1
                && d.vertex_count() >= 3
                && d.parent_deformer
                    .and_then(|p| ir.deformers.get(p as usize))
                    .is_some_and(|p| matches!(p.kind, DeformerKind::Rotation(_)))
        })
        .map(|(i, _)| i)
        .collect()
}

/// A drawable's keyform coordinates, in its parent deformer's own space.
fn local_points(ir: &CubismIr, drawable: usize) -> Vec<(f32, f32)> {
    let d = &ir.drawables[drawable];
    ir.keyforms
        .drawable(d.keyform_begin as usize, d.vertex_count())
        .map(|f| f.chunks_exact(2).map(|p| (p[0], p[1])).collect())
        .unwrap_or_default()
}

/// How lopsided the best-fit linear map from `from` to `to` is: the ratio of
/// its larger singular value to its smaller one. One means a similarity, a
/// mirror included. `None` when the layout has no area to fit.
fn anisotropy(from: &[(f32, f32)], to: &[(f32, f32)]) -> Option<f64> {
    let n = from.len().min(to.len()) as f64;
    if n < 3.0 {
        return None;
    }
    let centre = |p: &[(f32, f32)]| {
        let (sx, sy) = p.iter().fold((0.0f64, 0.0f64), |a, q| (a.0 + q.0 as f64, a.1 + q.1 as f64));
        (sx / n, sy / n)
    };
    let (cf, ct) = (centre(from), centre(to));
    // Normal equations for M = (Tᵀ F)(Fᵀ F)⁻¹ on centred coordinates.
    let (mut ff, mut tf) = ([[0.0f64; 2]; 2], [[0.0f64; 2]; 2]);
    for (f, t) in from.iter().zip(to) {
        let f = [f.0 as f64 - cf.0, f.1 as f64 - cf.1];
        let t = [t.0 as f64 - ct.0, t.1 as f64 - ct.1];
        for i in 0..2 {
            for j in 0..2 {
                ff[i][j] += f[i] * f[j];
                tf[i][j] += t[i] * f[j];
            }
        }
    }
    let det_ff = ff[0][0] * ff[1][1] - ff[0][1] * ff[1][0];
    if det_ff.abs() < 1e-12 {
        return None;
    }
    let inv = [[ff[1][1] / det_ff, -ff[0][1] / det_ff], [-ff[1][0] / det_ff, ff[0][0] / det_ff]];
    let mut m = [[0.0f64; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            m[i][j] = tf[i][0] * inv[0][j] + tf[i][1] * inv[1][j];
        }
    }
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let trace = m.iter().flatten().map(|v| v * v).sum::<f64>();
    let disc = (trace * trace - 4.0 * det * det).max(0.0).sqrt();
    let (big, small) = ((trace + disc) / 2.0, (trace - disc) / 2.0);
    (small > 1e-18).then(|| (big / small).sqrt())
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn rigid_meshes_keep_their_texture_proportions() {
    let Some(ir) = load() else { return };
    let ir = without_glue(&ir);
    let rigid = rigid_candidates(&ir);
    let pose = ir.pose(&[]);

    let mut checked = 0usize;
    let mut similar = 0usize;
    let mut worst = (0.0f64, 0usize);
    for &d in &rigid {
        let drawable = &ir.drawables[d];
        let (local, posed) = (local_points(&ir, d), &pose.drawables[d]);
        let (Some(was), Some(is)) = (anisotropy(&drawable.uvs, &local), anisotropy(&drawable.uvs, posed)) else {
            continue;
        };
        checked += 1;
        if (was - 1.0).abs() < 1e-3 {
            similar += 1;
        }
        let off = (is - was).abs();
        if off > worst.0 {
            worst = (off, d);
        }
    }
    let similar_fraction = similar as f64 / checked.max(1) as f64;
    println!("{similar} of {checked} start out similar to their texture ({similar_fraction:.3})");
    println!(
        "{} rigid candidates, {checked} measurable; worst anisotropy change {:e} (drawable {})",
        rigid.len(),
        worst.0,
        worst.1
    );
    assert!(checked > 0, "no rigid drawable could be measured, so nothing was checked");
    // Measured at 0.91, 0.98 and 1.00 on three real models; the rest are
    // meshes whose layout is not a pure similarity of their texture region.
    assert!(similar_fraction >= 0.85, "only {similar} of {checked} rigid meshes are similar to their texture");
    assert!(pose.is_stable(), "unstable drawables: {:?}", pose.unstable);
    assert!(worst.0 < 1e-3, "drawable {} changed its texture proportions by {}", worst.1, worst.0);
}

/// The lowest and highest product of rotation scales the chain above a
/// drawable's parent can blend to, or `None` when a scale is not positive.
fn scale_bounds(ir: &CubismIr, drawable: usize) -> Option<(f32, f32)> {
    let (mut lo, mut hi) = (1.0f32, 1.0f32);
    let mut at = ir.drawables[drawable].parent_deformer;
    let mut steps = 0;
    while let Some(i) = at {
        let deformer = ir.deformers.get(i as usize)?;
        if let DeformerKind::Rotation(r) = deformer.kind {
            let shape = ir.rotation_deformers.get(r as usize)?;
            let begin = shape.keyform_begin as usize;
            let keys = ir.rotation_keyforms.get(begin..begin + shape.keyform_count as usize)?;
            let (min, max) = keys.iter().fold((f32::MAX, f32::MIN), |a, k| (a.0.min(k.scale), a.1.max(k.scale)));
            if min <= 0.0 {
                return None;
            }
            lo *= min;
            hi *= max;
        }
        at = deformer.parent;
        steps += 1;
        if steps > ir.deformers.len() {
            return None;
        }
    }
    Some((lo, hi))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn rigid_meshes_stay_rigid_and_change_size_only_within_their_rotation_scales() {
    let Some(ir) = load() else { return };
    let ir = without_glue(&ir);
    let rigid = rigid_candidates(&ir);
    let mut model = GenericCubismModel::load(ir.clone(), "deform check");

    let mut frames = 0usize;
    let mut measured = 0usize;
    let mut worst_spread = (1.0f64, 0usize, 0.0f32);
    let mut worst_bound = (0.0f64, 0usize, 0.0f32);
    for motion in &ir.motions {
        let steps = (motion.duration * 15.0).ceil() as usize;
        for i in 0..=steps {
            let t = (i as f32 / 15.0).min(motion.duration);
            model.pose_at(&motion.name, t).expect("the motion should pose");
            let pose = model.pose();
            frames += 1;
            for &d in &rigid {
                let Some((lo, hi)) = scale_bounds(&ir, d) else { continue };
                let local = local_points(&ir, d);
                let drawable = &ir.drawables[d];
                let mut ratios = Vec::new();
                for tri in drawable.indices.chunks_exact(3) {
                    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                        let (a, b) = (tri[a] as usize, tri[b] as usize);
                        let (Some(la), Some(lb), Some(pa), Some(pb)) =
                            (local.get(a), local.get(b), pose.drawables[d].get(a), pose.drawables[d].get(b))
                        else {
                            continue;
                        };
                        let before = (((la.0 - lb.0) as f64).powi(2) + ((la.1 - lb.1) as f64).powi(2)).sqrt();
                        let after = (((pa.0 - pb.0) as f64).powi(2) + ((pa.1 - pb.1) as f64).powi(2)).sqrt();
                        // Edges too short to measure only add rounding.
                        if before > 1e-3 {
                            ratios.push(after / before);
                        }
                    }
                }
                if ratios.is_empty() {
                    continue;
                }
                measured += 1;
                ratios.sort_by(|a, b| a.total_cmp(b));
                let (low, high) = (ratios[ratios.len() / 10], ratios[ratios.len() * 9 / 10]);
                if high / low > worst_spread.0 {
                    worst_spread = (high / low, d, t);
                }
                let median = ratios[ratios.len() / 2];
                let off = ((lo as f64 - median) / lo as f64).max((median - hi as f64) / hi as f64).max(0.0);
                if off > worst_bound.0 {
                    worst_bound = (off, d, t);
                }
            }
        }
    }
    println!(
        "{} rigid candidates over {frames} frames; worst p90/p10 edge ratio {:.6} (drawable {} at {:.2}s); \
         worst excursion past the rotation scale bounds {:e} (drawable {} at {:.2}s)",
        rigid.len(),
        worst_spread.0,
        worst_spread.1,
        worst_spread.2,
        worst_bound.0,
        worst_bound.1,
        worst_bound.2
    );
    assert!(frames > 0 && measured > 0, "no motion frame measured a rigid mesh ({frames} frames)");
    assert!(worst_spread.0 < 1.01, "drawable {} stretched unevenly: {}", worst_spread.1, worst_spread.0);
    assert!(worst_bound.0 < 1e-3, "drawable {} changed size beyond its rotation scales", worst_bound.1);
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn most_glued_pairs_coincide_at_rest_before_the_glue_runs() {
    let Some(ir) = load() else { return };
    if ir.glues.is_empty() {
        return;
    }
    let pose = without_glue(&ir).pose(&[]);
    let mut closed = 0usize;
    let mut widest = 0.0f32;
    let mut gaps = Vec::new();
    for (index, glue) in ir.glues.iter().enumerate() {
        let mut gap = 0.0f32;
        for pair in &glue.pairs {
            let a = pose.drawables[glue.mesh_a as usize][pair.vertex_a as usize];
            let b = pose.drawables[glue.mesh_b as usize][pair.vertex_b as usize];
            gap = gap.max(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt());
        }
        if gap < 1e-5 {
            closed += 1;
        } else {
            println!("glue {index} open at rest by {gap}");
        }
        widest = widest.max(gap);
        gaps.push(gap);
    }
    let fraction = closed as f64 / ir.glues.len() as f64;
    // Gaps are in canvas units, the unit `pose` reports: the canvas is one
    // unit wide, and a little under two tall on the models measured.
    let wide = gaps.iter().filter(|g| **g > 0.02).count();
    println!(
        "canvas {} x {} units; {wide} glues open by more than 0.02",
        ir.canvas.size.0 / ir.canvas.pixels_per_unit,
        ir.canvas.size.1 / ir.canvas.pixels_per_unit
    );
    println!("{closed} of {} glues closed at rest ({fraction:.3}); widest rest gap {widest}", ir.glues.len());
    // Measured on three real models (canvas 1 unit wide): closed fractions of
    // 0.81, 0.86 and 1.00; exactly one glue open by more than 0.02 on one of
    // them (0.295) and none on the other two; the next-widest anywhere is 0.018.
    // So the bounds are a floor on the closed fraction, at most one wide-open
    // glue, and a widest gap no more than about 20% past the measured one. A
    // few glues really are open at rest, so this is not exactness.
    assert!(fraction >= 0.75, "only {closed} of {} glues close at rest", ir.glues.len());
    assert!(wide <= 1, "{wide} glues are open by more than 0.02 canvas units at rest");
    assert!(widest < 0.35, "a glued pair is {widest} canvas units apart at rest");
}
