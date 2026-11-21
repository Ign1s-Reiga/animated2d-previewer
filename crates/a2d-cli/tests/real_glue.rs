//! Checks that glue holds a real model's split meshes together while it moves.
//!
//! The synthetic tests in `a2d-cubism` pin the application rule by hand-computed
//! geometry. What only a real model can say is that the decoded tables are the
//! ones the format writes, which shows up as every glued pair of vertices
//! staying coincident across a motion. Without glue the same pairs drift apart
//! by about a hundredth of a canvas unit, which is a visible crack.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_glue -- --ignored --nocapture
//! ```

use a2d_core::{AnimatedModel, LoadReport};
use a2d_cubism::GenericCubismModel;

/// The widest distance between any glued pair of vertices in the model's
/// current pose.
fn widest_gap(model: &GenericCubismModel) -> f32 {
    let pose = model.pose();
    let mut worst = 0.0f32;
    for glue in &model.moc().glues {
        for pair in &glue.pairs {
            let a = pose.drawables[glue.mesh_a as usize][pair.vertex_a as usize];
            let b = pose.drawables[glue.mesh_b as usize][pair.vertex_b as usize];
            worst = worst.max(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt());
        }
    }
    worst
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn glued_vertices_of_a_real_model_stay_together_through_every_motion() {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    let ir = package.model.as_cubism().expect("a Cubism package").clone();
    println!("{} glues, {} motions", ir.glues.len(), ir.motions.len());
    // A model that declares glue ids must have had them decoded, not just named.
    assert_eq!(ir.glues.len(), ir.glue_ids.len());

    let mut model = GenericCubismModel::load(ir.clone(), "glue check");
    let mut worst = 0.0f32;
    for motion in &ir.motions {
        let steps = (motion.duration * 10.0).ceil() as usize;
        for i in 0..=steps {
            let t = (i as f32 / 10.0).min(motion.duration);
            model.pose_at(&motion.name, t).expect("the motion should pose");
            worst = worst.max(widest_gap(&model));
        }
    }
    println!("widest glued gap over every motion: {worst:e}");
    assert!(worst < 1e-5, "a glued pair came apart by {worst}");
}
