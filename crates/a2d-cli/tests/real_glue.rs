//! Checks that glue behaves on a real model as its weights say it should.
//!
//! The synthetic tests in `a2d-cubism` pin the application rule by hand-computed
//! geometry. What only a real model can say is that the decoded tables are the
//! ones the format writes. Not every pair is meant to close: the Cubism Editor
//! lets an artist partly or fully release glued vertices, so a pair's weights
//! may sum below one, and a vertex can be in two glues. So the model is posed
//! twice at each sampled time, with and without glue, and each pair is judged
//! by what its weights say it should do:
//!
//! * weight sum one, vertices in a single glue: the gap is closed;
//! * weight sum below one, vertices in a single glue: the gap is exactly
//!   `1 - sum` of the gap without glue;
//! * a vertex in several glues, or a mesh that is unstable: printed, not
//!   asserted.
//!
//! Those checks hold of `apply_glue` for any weights, so the data itself is
//! also checked: weights lie in `[0, 1]`, glued pairs coincide at rest, and
//! most weight sums are one.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_glue -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use a2d_core::{AnimatedModel, LoadReport};
use a2d_cubism::GenericCubismModel;

type Key = (u32, u32);

/// How far apart a glued pair may sit at rest and still count as coincident.
/// Measured on three real models: the widest single-glue pair is 0.0053, 0.0127
/// and 0.295 canvas units, with 0, 0 and 8 of 817 pairs over this tolerance;
/// the 0.295 ones are known rest gaps that glue exists to close.
const REST_TOLERANCE: f32 = 2e-2;
/// The share of single-glue pairs allowed to be farther apart than that.
const MAX_REST_APART_SHARE: f32 = 0.05;
/// The share of single-glue pairs whose weights must sum to one.
const MIN_FULL_SHARE: f32 = 0.9;

fn gap(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

#[derive(Default)]
struct Tally {
    pairs: usize,
    worst: f32,
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn glued_pairs_of_a_real_model_close_by_exactly_their_weights() {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    let ir = package.model.as_cubism().expect("a Cubism package").clone();
    println!("{} glues, {} motions", ir.glues.len(), ir.motions.len());
    // A model that declares glue ids must have had them decoded, not just named.
    assert_eq!(ir.glues.len(), ir.glue_ids.len());
    // The data claims, which hold whatever apply_glue does: a weight is a
    // fraction, and a pair is stitched where the two meshes already coincide.
    for glue in &ir.glues {
        for pair in &glue.pairs {
            for w in [pair.weight_a, pair.weight_b] {
                assert!((0.0..=1.0).contains(&w), "a glue weight of {w} is outside [0, 1]");
            }
        }
    }

    // Glues whose intensity is keyed (any keyform not one) are skipped: the
    // closing rule below would need the intensity blended through the binding.
    let keyed: Vec<bool> = ir
        .glues
        .iter()
        .map(|g| {
            let begin = g.keyform_begin as usize;
            let end = begin + g.keyform_count as usize;
            ir.glue_intensities.get(begin..end).is_none_or(|k| k.iter().any(|i| *i != 1.0))
        })
        .collect();
    println!(
        "{} of {} glues have a keyed intensity and are skipped",
        keyed.iter().filter(|k| **k).count(),
        keyed.len()
    );

    // How many glues name each vertex. A vertex in several glues has no
    // single closing rule, so those pairs are reported and not asserted.
    let mut uses: HashMap<Key, usize> = HashMap::new();
    for glue in &ir.glues {
        for pair in &glue.pairs {
            *uses.entry((glue.mesh_a, pair.vertex_a)).or_default() += 1;
            *uses.entry((glue.mesh_b, pair.vertex_b)).or_default() += 1;
        }
    }

    let mut raw_ir = ir.clone();
    raw_ir.glues.clear();
    let mut glued = GenericCubismModel::load(ir.clone(), "glue check");
    let mut raw = GenericCubismModel::load(raw_ir, "glue check, no glue");

    // Claim (ii): single-glue pairs coincide at rest before glue. This is what
    // says the vertex indices and the mesh order were decoded right.
    let (mut single_pairs, mut single_full, mut rest_far, mut rest_worst) = (0usize, 0usize, 0usize, 0.0f32);
    {
        let rp = raw.pose();
        for (gi, glue) in ir.glues.iter().enumerate() {
            if keyed[gi] {
                continue;
            }
            for pair in &glue.pairs {
                if uses[&(glue.mesh_a, pair.vertex_a)] != 1 || uses[&(glue.mesh_b, pair.vertex_b)] != 1 {
                    continue;
                }
                single_pairs += 1;
                if (pair.weight_a + pair.weight_b - 1.0).abs() < 1e-4 {
                    single_full += 1;
                }
                let g = gap(
                    rp.drawables[glue.mesh_a as usize][pair.vertex_a as usize],
                    rp.drawables[glue.mesh_b as usize][pair.vertex_b as usize],
                );
                rest_worst = rest_worst.max(g);
                if g > REST_TOLERANCE {
                    rest_far += 1;
                }
            }
        }
    }
    let rest_far_share = rest_far as f32 / single_pairs.max(1) as f32;
    let full_share = single_full as f32 / single_pairs.max(1) as f32;
    println!(
        "single-glue pairs {single_pairs}: {rest_far} apart at rest by over {REST_TOLERANCE} (share {rest_far_share:.4}, widest {rest_worst}); weight sum one on {full_share:.4}"
    );
    assert!(single_pairs > 0, "no single-glue pair to check");
    assert!(rest_far_share <= MAX_REST_APART_SHARE, "too many glued pairs are apart at rest: {rest_far_share}");
    assert!(full_share >= MIN_FULL_SHARE, "too few pairs have weights summing to one: {full_share}");

    let (mut closed, mut partial, mut shared, mut unstable, mut crossed) =
        (Tally::default(), Tally::default(), Tally::default(), Tally::default(), Tally::default());
    for motion in &ir.motions {
        let steps = (motion.duration * 10.0).ceil() as usize;
        for i in 0..=steps {
            let t = (i as f32 / 10.0).min(motion.duration);
            glued.pose_at(&motion.name, t).expect("the motion should pose");
            raw.pose_at(&motion.name, t).expect("the motion should pose");
            let (gp, rp) = (glued.pose(), raw.pose());
            for (gi, glue) in ir.glues.iter().enumerate() {
                if keyed[gi] {
                    continue;
                }
                let (a, b) = (glue.mesh_a as usize, glue.mesh_b as usize);
                for pair in &glue.pairs {
                    let (va, vb) = (pair.vertex_a as usize, pair.vertex_b as usize);
                    let after = gap(gp.drawables[a][va], gp.drawables[b][vb]);
                    let before = gap(rp.drawables[a][va], rp.drawables[b][vb]);
                    let sum = pair.weight_a + pair.weight_b;
                    let single = uses[&(glue.mesh_a, pair.vertex_a)] == 1 && uses[&(glue.mesh_b, pair.vertex_b)] == 1;
                    // apply_glue skips a glue whose mesh is unstable, so a
                    // deformer problem is not blamed on glue.
                    if gp.unstable.contains(&a) || gp.unstable.contains(&b) {
                        unstable.pairs += 1;
                        unstable.worst = unstable.worst.max(after);
                    } else if !single {
                        shared.pairs += 1;
                        shared.worst = shared.worst.max(after);
                    } else if (sum - 1.0).abs() < 1e-4 {
                        closed.pairs += 1;
                        closed.worst = closed.worst.max(after);
                        assert!(after < 1e-5, "a fully weighted pair stayed {after} apart");
                    } else {
                        // Keeps |1 - sum| of its gap, whichever side of one.
                        let off = (after - (1.0 - sum).abs() * before).abs();
                        let tally = if sum < 1.0 { &mut partial } else { &mut crossed };
                        tally.pairs += 1;
                        tally.worst = tally.worst.max(off);
                        assert!(off < 1e-5, "a pair of weight sum {sum} kept {after} of {before}");
                    }
                }
            }
        }
    }
    println!("(a) closing pair samples {}: widest gap {:e}", closed.pairs, closed.worst);
    println!(
        "(b) partly released pair samples {}: worst error against (1 - sum) * raw gap {:e}",
        partial.pairs, partial.worst
    );
    println!("(c) shared-vertex pair samples {} (not asserted): widest gap {:e}", shared.pairs, shared.worst);
    println!("(d) pair samples on an unstable mesh {} (not asserted): widest gap {:e}", unstable.pairs, unstable.worst);
    // Weights are within [0, 1] above, so a sum over one is only possible as
    // two large weights; none has been seen, and one would be news.
    assert_eq!(crossed.pairs, 0, "a pair with weights summing above one");
    assert!(closed.pairs > 0, "no pair of the model was expected to close, so nothing was checked");
}
