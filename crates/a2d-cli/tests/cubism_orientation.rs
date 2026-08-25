//! Is a posed Cubism model assembled coherently, and at the right size?
//!
//! Nothing structural answers either question. A model can parse cleanly, pose
//! with no unstable drawable, and still come out mirrored, sheared, or several
//! times too large.
//!
//! # What can and cannot be asserted
//!
//! Cubism's parameter names are conventional: `ParamEyeBallX` slides the pupils
//! left and right, `ParamEyeBallY` up and down. It is tempting to assert that
//! the first therefore moves them along the screen's x axis -- but that only
//! holds for a character whose head is drawn upright, and a reclining or
//! tumbling one is drawn at whatever angle the artist chose. Measured across
//! three real models the pupil axes came out at 0, -60 and -95 degrees, all
//! with total agreement between drawables: artwork, not error.
//!
//! What *does* hold whatever the pose is that the two axes stay perpendicular
//! and right-handed. A deformer chain that transposed a grid, mirrored a warp
//! or sheared a rotation would break that; a tilted head does not.
//!
//! The second test covers size. A model's own parameter defaults are not
//! necessarily its display values -- one of the three ships a zoom parameter
//! defaulted to 8 of 10, which scales the whole model by about five and makes
//! an ordinary canvas-wide backdrop measure four and a half canvases across.
//! With the parameters that drive the root wound back to their minimum, every
//! model should sit inside its own canvas.
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

fn centroids(moc: &Moc3, values: &[f32]) -> Vec<(f32, f32)> {
    moc.pose(values)
        .drawables
        .iter()
        .map(|points| {
            if points.is_empty() {
                return (f32::NAN, f32::NAN);
            }
            let n = points.len() as f32;
            let sum = points.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
            (sum.0 / n, sum.1 / n)
        })
        .collect()
}

/// Which way the model travels as one parameter sweeps, as a unit vector, with
/// how strongly the responding drawables agree on it.
///
/// The direction is a mean of *unit* vectors rather than of displacements. A
/// magnitude-weighted mean is decided by whichever drawable moves furthest, so
/// a single mis-scaled one would speak for the whole model.
fn travel(moc: &Moc3, id: &str) -> Option<(f32, f32, f32)> {
    let index = moc.parameters.iter().position(|p| p.id == id)?;
    let parameter = &moc.parameters[index];
    let base: Vec<f32> = moc.parameters.iter().map(|p| p.default).collect();

    let at = |value: f32| {
        let mut values = base.clone();
        values[index] = value;
        centroids(moc, &values)
    };
    let (lo, hi) = (at(parameter.minimum), at(parameter.maximum));

    let mut moved: Vec<(f32, f32, f32)> = lo
        .iter()
        .zip(&hi)
        .filter(|(a, b)| a.0.is_finite() && b.0.is_finite())
        .map(|(a, b)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            ((dx * dx + dy * dy).sqrt(), dx, dy)
        })
        .filter(|m| m.0 > 1e-6)
        .collect();
    if moved.is_empty() {
        return None;
    }
    moved.sort_by(|l, r| r.0.total_cmp(&l.0));

    // Only the drawables that really respond; the rest are noise around zero.
    let largest = moved[0].0;
    let responsive: Vec<_> = moved.iter().filter(|m| m.0 > largest * 0.25).collect();
    let n = responsive.len() as f32;
    let sum = responsive.iter().fold((0.0, 0.0), |a, m| (a.0 + m.1 / m.0, a.1 + m.2 / m.0));
    let (x, y) = (sum.0 / n, sum.1 / n);
    Some((x, y, (x * x + y * y).sqrt()))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity bundle"]
fn the_two_pupil_axes_stay_square_to_each_other() {
    let Some(moc) = model() else { return };
    let (Some(x), Some(y)) = (travel(&moc, "ParamEyeBallX"), travel(&moc, "ParamEyeBallY")) else { return };

    // Without agreement there is no direction to speak of, so there would be
    // nothing to compare and the test would be asserting on noise.
    assert!(x.2 > 0.9 && y.2 > 0.9, "the pupils disagree about where they are going: {x:?} {y:?}");

    let dot = x.0 * y.0 + x.1 * y.1;
    assert!(
        dot.abs() < 0.15,
        "the pupil axes are {:.0} degrees apart, not 90; the deformer chain is shearing them",
        (dot.clamp(-1.0, 1.0).acos()).to_degrees()
    );

    let cross = x.0 * y.1 - x.1 * y.0;
    assert!(cross > 0.0, "the pupil axes come out left-handed ({cross:.3}); the chain mirrors the model");
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
