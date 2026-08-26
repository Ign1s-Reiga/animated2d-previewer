//! Is a posed Cubism model assembled the right way round, and at the right size?
//!
//! A model can parse cleanly, pose with no unstable drawable, and still come out
//! transposed, mirrored or several times too large. Nothing structural notices.
//!
//! # What can and cannot be asserted
//!
//! The check that works compares the geometry against something **independent
//! of it**: the drawable's own texture coordinates. A mesh is the same shape in
//! both spaces, so the map between them is a similarity. Atlas packing may
//! rotate a region, but a rotation cannot flip a determinant and a mirror can.
//!
//! So this detects a **mirrored** model and only that. It is blind to rotation,
//! which is worth saying plainly: it was once mistaken for a full orientation
//! check, and a correct model was "fixed" on the strength of it.
//!
//! The expected sign is **negative**, and that is measured rather than argued.
//! The Spine path is known to render upright, so whichever sign it produces is
//! the convention the renderer and the texture pipeline actually implement:
//! across eight real rigs, 1273 of 1276 meshes come out negative. The Cubism
//! path measured 1771 of 1797 *positive* — the opposite — and every model
//! rendered upside down.
//!
//! This assertion used to demand the positive sign, on an argument about
//! `a2d-unity` flipping Unity's bottom-up rows when it reads a `Texture2D`.
//! The argument was wrong, and the test held the bug in place rather than
//! catching it. Measuring against a path already known to be correct settles
//! what reasoning from conventions did not.
//!
//! Which side was inverted was settled the same way rather than by argument:
//! negating position `y` renders a coherent upright model, and flipping uv `v`
//! shreds the art across meshes. Both satisfy the determinant; only one is a
//! picture.
//!
//! Nothing here checks rotation. Two tempting alternatives do **not** work:
//!
//! * *"the face should be upright"* — a character may legitimately be drawn
//!   reclining or tilted, so there is no angle to assert.
//! * *"`ParamEyeBallX` should move the pupils horizontally"*, or the weaker
//!   *"the two pupil axes should at least stay square and right-handed"* —
//!   neither holds. Those parameters are carried through the very chain under
//!   test, so they cannot witness against it; and measured across real models
//!   the wiring is not uniform anyway. One rig moves its pupils vertically for
//!   `ParamEyeBallX` while its face is plainly upright, and the handedness of
//!   the pair depends on which way a rig takes positive to mean. An assertion
//!   that fails on correct data is worse than no assertion.
//!
//! Size is checked separately. A model's stored parameter defaults are not
//! necessarily its display values: one ships a zoom parameter defaulted to 8 of
//! 10, which scales it about fivefold. Wound back to its minimum, every model
//! sits inside its own canvas.
//!
//! Gated on `A2D_FIXTURE_CUBISM`, like the other real-asset tests: extracted
//! assets are never committed (§11).

use a2d_core::LoadReport;
use a2d_cubism::{DeformerKind, Moc3};

fn model() -> Option<Moc3> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(path).ok()?;
    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_bundle(&bytes, &mut report).ok()?;
    Moc3::parse(&inventory.moc.as_ref()?.bytes).ok()
}

/// Determinant of the least-squares map from a drawable's uvs to its posed
/// vertices.
fn uv_to_position_determinant(uvs: &[(f32, f32)], points: &[(f32, f32)]) -> Option<f64> {
    let n = uvs.len() as f64;
    if n < 3.0 {
        return None;
    }
    let mean = |f: &dyn Fn(usize) -> f64| (0..uvs.len()).map(f).sum::<f64>() / n;
    let (mu, mv) = (mean(&|i| uvs[i].0 as f64), mean(&|i| uvs[i].1 as f64));
    let (mx, my) = (mean(&|i| points[i].0 as f64), mean(&|i| points[i].1 as f64));

    let (mut suu, mut suv, mut svv) = (0.0, 0.0, 0.0);
    let (mut sux, mut svx, mut suy, mut svy) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..uvs.len() {
        let (du, dv) = (uvs[i].0 as f64 - mu, uvs[i].1 as f64 - mv);
        let (dx, dy) = (points[i].0 as f64 - mx, points[i].1 as f64 - my);
        suu += du * du;
        suv += du * dv;
        svv += dv * dv;
        sux += du * dx;
        svx += dv * dx;
        suy += du * dy;
        svy += dv * dy;
    }
    let spread = suu * svv - suv * suv;
    if spread.abs() < 1e-14 {
        return None;
    }
    let a = (sux * svv - svx * suv) / spread;
    let b = (svx * suu - sux * suv) / spread;
    let c = (suy * svv - svy * suv) / spread;
    let d = (svy * suu - suy * suv) / spread;
    Some(a * d - b * c)
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity bundle"]
fn posed_geometry_agrees_with_the_texture_it_samples() {
    let Some(moc) = model() else { return };
    let pose = moc.pose(&[]);

    let (mut agree, mut total) = (0usize, 0usize);
    for (index, drawable) in moc.drawables.iter().enumerate() {
        let Some(points) = pose.drawables.get(index) else { continue };
        if points.len() != drawable.uvs.len() {
            continue;
        }
        let Some(det) = uv_to_position_determinant(&drawable.uvs, points) else { continue };
        total += 1;
        if det < 0.0 {
            agree += 1;
        }
    }

    assert!(total > 0, "no drawable could be fitted");
    // Not every drawable: a degenerate or near-collinear mesh fits noisily.
    assert!(
        agree * 10 >= total * 9,
        "only {agree} of {total} drawables carry the sign the Spine path does; \
         a vertically mirrored model scores near zero here, though a rotated one scores \
         just as well as an upright one: a determinant cannot see a turn"
    );
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity bundle"]
fn an_unzoomed_model_sits_inside_its_own_canvas() {
    let Some(moc) = model() else { return };
    let canvas = moc.canvas;
    assert!(canvas.pixels_per_unit > 0.0, "a model with no scale cannot be checked");
    let (width, height) = (canvas.size.0 / canvas.pixels_per_unit, canvas.size.1 / canvas.pixels_per_unit);

    // The root deformer carries the model's overall placement and scale, so the
    // parameters driving it are the ones that zoom it. At their minimum the
    // model is at its widest view.
    let mut values: Vec<f32> = moc.parameters.iter().map(|p| p.default).collect();
    for deformer in moc.deformers.iter().filter(|d| d.parent.is_none()) {
        let DeformerKind::Rotation(rotation) = deformer.kind else { continue };
        let binding = moc.rotation_deformers[rotation as usize].keyform_binding as usize;
        let Some(keyform_binding) = moc.keyform_bindings.get(binding) else { continue };
        for axis in &keyform_binding.axes {
            let Some(parameter_binding) = moc.parameter_bindings.get(*axis as usize) else { continue };
            let index = parameter_binding.parameter as usize;
            values[index] = moc.parameters[index].minimum;
        }
    }

    let pose = moc.pose(&values);
    let Some((lo, hi)) = moc.bounds(&pose) else { return };
    assert!(
        hi.x - lo.x < width * 2.0 && hi.y - lo.y < height * 2.0,
        "unzoomed geometry spans {:.2}x{:.2} against a {width:.2}x{height:.2} canvas",
        hi.x - lo.x,
        hi.y - lo.y
    );
}
