//! Checks that switching between the motions of a real model crossfades
//! instead of cutting.
//!
//! For every ordered pair of distinct motions `(A, B)`, physics off (it is a
//! simulation with its own tests), the model plays `A` to 40% of its length,
//! then switches to `B` with a 0.25 s mix, stepping at 60 Hz. The same switch
//! is made with a mix of zero for comparison. The largest frame-to-frame move
//! of any drawable visible in both frames is measured over the switch and the
//! 15 frames of the fade.
//!
//! What may a frame of the fade move? With `v = a + (b - a) * w`, a frame
//! changes `v` by `da * (1 - w) + db * w + (b - a) * dw`. The first two terms
//! are convex mixes of what `A` and `B` move in a frame anyway, so at most
//! the larger of their ordinary per-frame moves (`ordinary`, measured with
//! `pose_at` one frame apart over the whole of each motion, so no loop seam
//! is included). The third is the gap between the two poses times the
//! weight's steepest step, `pi / 2 / 0.25 s / 60 Hz = 0.1047` of it; `gap` is
//! the largest vertex distance between "A alone" and "B alone" over the fade.
//! Vertex positions are not linear in the parameters (a rotation deformer
//! turns a vertex along an arc, not a chord, and the mid-fade pose is not on
//! the line between the two end poses), and a first run measured up to 1.8 times
//! the linear estimate on one model, so the bound allows 2.5 times:
//!
//! ```text
//! worst fade frame <= 2.5 * (ordinary + 0.1047 * gap)
//! ```
//!
//! A cut moves the whole gap in one frame, so the same measurement of a cut
//! is printed next to it; the test also requires the fade to beat the cut by
//! at least a factor of two on every pair whose cut moves more than five times
//! the ordinary move (the bound alone could be met by a cut when the gap is
//! small), which is the point of the feature. On a model with repeating parameters it
//! also requires some fade to move one, so the shortest-arc path is exercised.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_crossfade -- --ignored --nocapture
//! ```

use std::time::Duration;

use a2d_core::{AnimatedModel, LoadReport, PlayOptions};
use a2d_cubism::{GenericCubismModel, Pose};

const VISIBLE: f32 = 0.01;
const FPS: f32 = 60.0;
const MIX: f32 = 0.25;
/// Weight's steepest step per frame: pi / 2 / MIX / FPS.
const STEEPEST: f32 = std::f32::consts::FRAC_PI_2 / MIX / FPS;
/// Slack over the linear estimate, for the nonlinearity of vertices in the
/// parameters (see the module documentation).
const SLACK: f32 = 2.5;

/// Largest vertex move of a drawable visible in both poses.
fn moved(a: &Pose, b: &Pose) -> f32 {
    let mut out = 0.0f32;
    for (d, (pa, pb)) in a.drawables.iter().zip(&b.drawables).enumerate() {
        if a.opacities[d] > VISIBLE && b.opacities[d] > VISIBLE {
            for (p, q) in pa.iter().zip(pb) {
                out = out.max(((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt());
            }
        }
    }
    out
}

fn model() -> Option<GenericCubismModel> {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return None };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let package = a2d_import::import_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should reconstruct");
    let mut ir = package.model.as_cubism().expect("a Cubism package").clone();
    ir.physics = None;
    Some(GenericCubismModel::load(ir, "crossfade"))
}

/// The worst move over the switch from `a` (at `t0`) to `b`, and over `frames`
/// frames after it.
///
/// Also returns how far any of the `watch` parameters moved from its value at
/// the switch, so a caller can tell the fade actually drove them.
fn switch(
    m: &mut GenericCubismModel,
    a: &str,
    t0: f32,
    b: &str,
    mix: f32,
    frames: usize,
    watch: &[String],
) -> (f32, f32) {
    m.pose_at(a, t0).expect("pose");
    let mut last = m.pose().clone();
    m.play_animation(b, PlayOptions::looping().with_mix(Duration::from_secs_f32(mix))).expect("play");
    let mut worst = moved(&last, m.pose());
    last = m.pose().clone();
    let start: Vec<f32> = watch.iter().map(|id| m.parameter_value(id).unwrap_or(0.0)).collect();
    let mut travel = 0.0f32;
    for _ in 0..frames {
        m.update(Duration::from_secs_f32(1.0 / FPS)).expect("update");
        worst = worst.max(moved(&last, m.pose()));
        last = m.pose().clone();
        for (id, v0) in watch.iter().zip(&start) {
            travel = travel.max((m.parameter_value(id).unwrap_or(0.0) - v0).abs());
        }
    }
    (worst, travel)
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn switching_between_motions_fades_without_a_larger_move_than_the_motions_make() {
    let Some(mut m) = model() else { return };
    let motions: Vec<(String, f32)> = m.animations().iter().map(|a| (a.name.clone(), a.duration)).collect();
    assert!(motions.len() >= 2, "the bundle needs at least two motions");

    // The ordinary per-frame move of each motion.
    let mut ordinary = Vec::new();
    for (name, duration) in &motions {
        let frames = (duration * FPS).floor() as usize;
        let mut worst = 0.0f32;
        m.pose_at(name, 0.0).expect("pose");
        let mut last = m.pose().clone();
        for f in 1..frames {
            m.pose_at(name, f as f32 / FPS).expect("pose");
            worst = worst.max(moved(&last, m.pose()));
            last = m.pose().clone();
        }
        println!("  ordinary {name:<32} {worst:.5}");
        ordinary.push(worst);
    }

    let frames = (MIX * FPS) as usize;
    let repeating: Vec<String> =
        m.moc().repeating_parameters().into_iter().map(|p| m.moc().parameters[p].id.clone()).collect();
    println!("{} repeating parameters: {repeating:?}", repeating.len());
    let (mut enforced, mut exercised) = (0, 0);
    let (mut worst_fade, mut worst_cut, mut worst_ratio, mut pairs) = (0.0f32, 0.0f32, 0.0f32, 0);
    for (i, (a, da)) in motions.iter().enumerate() {
        for (j, (b, db)) in motions.iter().enumerate() {
            let t0 = da * 0.4;
            if i == j || t0 + MIX >= *da || *db <= MIX {
                continue;
            }
            // The gap between "A alone" and "B alone" over the fade.
            let mut gap = 0.0f32;
            let mut a_alone = Vec::new();
            for f in 1..=frames {
                m.pose_at(a, t0 + f as f32 / FPS).expect("pose");
                a_alone.push(m.pose().clone());
            }
            for (f, pa) in a_alone.iter().enumerate() {
                m.pose_at(b, (f + 1) as f32 / FPS).expect("pose");
                gap = gap.max(moved(pa, m.pose()));
            }
            let (cut, _) = switch(&mut m, a, t0, b, 0.0, frames, &repeating);
            let (fade, travel) = switch(&mut m, a, t0, b, MIX, frames, &repeating);
            if travel > 1e-3 {
                exercised += 1;
            }
            let ord = ordinary[i].max(ordinary[j]);
            let bound = SLACK * (ord + STEEPEST * gap);
            let ratio = fade / (ord + STEEPEST * gap);
            println!(
                "  {a:<28} -> {b:<28} gap {gap:.5}  cut {cut:.5}  fade {fade:.5}  bound {bound:.5}  ratio {ratio:.2}"
            );
            assert!(fade <= bound, "{a} -> {b}: a frame of the fade moves {fade}, bound {bound}");
            // Where the cut is a real pop (more than five ordinary frames of
            // motion) the fade must be at least twice as smooth. Below that the
            // switch is hidden in ordinary motion and the two are not comparable.
            // `gap` is not used here: it is measured over the whole fade, the cut
            // only at the switch.
            if cut > 5.0 * ord {
                enforced += 1;
                assert!(
                    fade * 2.0 <= cut,
                    "{a} -> {b}: the fade ({fade}) is not clearly smoother than the cut ({cut})"
                );
            }
            worst_fade = worst_fade.max(fade);
            worst_ratio = worst_ratio.max(ratio);
            worst_cut = worst_cut.max(cut);
            pairs += 1;
        }
    }
    assert!(pairs > 0, "no pair of motions was long enough to test");
    assert!(enforced > 0, "no pair had a cut large enough for the fade-versus-cut check to apply");
    if !repeating.is_empty() {
        assert!(exercised > 0, "no fade moved a repeating parameter, so the shortest-arc blend went untested");
    }
    println!("{enforced} pairs checked against the cut; {exercised} fades moved a repeating parameter");
    println!("{pairs} pairs; worst frame: cut {worst_cut:.5}, fade {worst_fade:.5}, worst ratio to the linear estimate {worst_ratio:.2}");
}
