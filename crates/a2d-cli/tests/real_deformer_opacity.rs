//! Checks deformer opacity against a real model.
//!
//! Cubism hides an alternate piece by fading the deformer it hangs off, not
//! the piece itself. A model that ignores that draws the hidden piece, and
//! glue, which stitches vertex pairs of two meshes, then welds it to a visible
//! neighbour. The synthetic tests in `a2d-cubism` pin the composition rule by
//! hand-computed products; what only real data can say is that the rule
//! explains the symptom.
//!
//! * `deformer_opacity_hides_more_than_drawable_opacity` proves the real model
//!   uses the mechanism at all: at rest, more drawables are invisible once the
//!   deformer product is applied than by their own keyform opacity alone. It
//!   does not prove the product is the *right* rule (multiplication against a
//!   minimum, say), only that the decoded section matters. The "own opacity
//!   alone" pose is the same evaluator with the deformer opacity tracks
//!   removed, so the two counts differ only by the rule under test.
//! * `glued_seams_stay_close_while_both_meshes_show` proves the rule removes
//!   the visible symptom: over every motion, whenever both meshes of a glue
//!   are shown, the pairs the glue would join are already close before glue
//!   acts. Without the rule the worst such gap on the models measured was
//!   0.6 canvas units, with it 0.09. Glue is cleared before posing, so glue
//!   cannot be what satisfies it, and it fails if the rule is dropped.
//!
//! Neither is a comparison with the official runtime; that is still missing.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_deformer_opacity -- --ignored --nocapture
//! ```

use a2d_core::ir::cubism::CubismIr;
use a2d_core::{AnimatedModel, LoadReport};
use a2d_cubism::{CubismEval, GenericCubismModel};

/// Below this a mesh is treated as not shown, a little above the renderer's own
/// cut-off of `1/512` so a fade-out tail does not count as visible.
const SHOWN: f32 = 0.01;
/// The widest pre-glue gap, in canvas units, allowed between a glued pair while
/// both meshes show. Measured on three real models: 0.042, 0.012 and 0.089;
/// without deformer opacity 0.323 and 0.599 on two of them.
const MAX_VISIBLE_GAP: f32 = 0.1;

fn load() -> Option<CubismIr> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    Some(package.model.as_cubism().expect("a Cubism package").clone())
}

fn gap(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn deformer_opacity_hides_more_than_drawable_opacity() {
    let Some(ir) = load() else { return };
    assert!(!ir.warp_keyform_opacities.is_empty(), "the model carries no warp opacity track");

    let hidden = |ir: &CubismIr| ir.pose(&[]).opacities.iter().filter(|o| **o <= 1.0 / 512.0).count();
    let with_deformers = hidden(&ir);
    let mut own_only = ir.clone();
    own_only.warp_keyform_opacities.clear();
    for k in &mut own_only.rotation_keyforms {
        k.opacity = 1.0;
    }
    let by_own = hidden(&own_only);
    println!(
        "at rest {with_deformers} of {} drawables hidden; by their own opacity alone {by_own}",
        ir.drawables.len()
    );
    assert!(with_deformers > by_own, "deformer opacity hid nothing the drawables did not hide themselves");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn glued_seams_stay_close_while_both_meshes_show() {
    let Some(ir) = load() else { return };
    let mut raw_ir = ir.clone();
    raw_ir.glues.clear();
    let mut raw = GenericCubismModel::load(raw_ir, "opacity check, no glue");

    let (mut worst, mut samples) = (0.0f32, 0usize);
    for motion in &ir.motions {
        let steps = (motion.duration * 10.0).ceil() as usize;
        for i in 0..=steps {
            let t = (i as f32 / 10.0).min(motion.duration);
            raw.pose_at(&motion.name, t).expect("the motion should pose");
            let pose = raw.pose();
            for glue in &ir.glues {
                let (a, b) = (glue.mesh_a as usize, glue.mesh_b as usize);
                if pose.opacities[a] <= SHOWN
                    || pose.opacities[b] <= SHOWN
                    || pose.unstable.contains(&a)
                    || pose.unstable.contains(&b)
                {
                    continue;
                }
                for pair in &glue.pairs {
                    let g = gap(pose.drawables[a][pair.vertex_a as usize], pose.drawables[b][pair.vertex_b as usize]);
                    worst = worst.max(g);
                    samples += 1;
                }
            }
        }
    }
    println!("{samples} shown pair samples; worst pre-glue gap {worst}");
    assert!(samples > 0, "no glued pair was ever shown, so nothing was checked");
    assert!(worst < MAX_VISIBLE_GAP, "a pair of shown meshes is {worst} apart before glue");
}
