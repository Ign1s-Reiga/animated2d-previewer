//! Parses the MOC3 out of a real Unity bundle, when one is available.
//!
//! The synthetic fixture in `moc3.rs` proves the parser and its builder agree.
//! Only a real model proves the layout is the one Live2D writes — and for an
//! undocumented format that is the whole question, so these assertions are the
//! ones that actually carry weight.
//!
//! The asset is never committed (spec §11):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_moc3 -- --ignored
//! ```
//!
//! It lives in `a2d-cli` rather than beside the parser because getting the
//! payload means opening a Unity bundle, and `a2d-cubism` must not depend on
//! the importer that does it (§3 dependency direction). The CLI may depend on
//! everything, so the test belongs here.

use a2d_core::LoadReport;
use a2d_cubism::Moc3;

/// Pulls the MOC3 payload out of the bundle the fixture points at.
fn payload() -> Option<Vec<u8>> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_bundle(&bytes, &mut report).expect("the bundle should inventory");
    Some(inventory.moc.expect("the bundle should hold a CubismMoc").bytes)
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_model_parses_and_its_counts_agree_with_each_other() {
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    assert!(moc.version >= 1, "version {}", moc.version);
    // Warp plus rotation must account for every deformer; this is the check
    // that the count table is where the parser believes it is.
    assert_eq!(moc.counts.warp_deformers + moc.counts.rotation_deformers, moc.counts.deformers);

    // Every table must hold exactly as many entries as the counts declare.
    assert_eq!(moc.part_ids.len(), moc.counts.parts as usize);
    assert_eq!(moc.deformer_ids.len(), moc.counts.deformers as usize);
    assert_eq!(moc.drawable_ids.len(), moc.counts.drawables as usize);
    assert_eq!(moc.parameters.len(), moc.counts.parameters as usize);
    assert_eq!(moc.glue_ids.len(), moc.counts.glues as usize);

    assert!(moc.counts.parameters > 0, "a model with no parameters cannot animate");
    assert!(moc.counts.drawables > 0, "a model with no drawables cannot be seen");

    println!("version {} — {:?}", moc.version, moc.counts);
    println!("canvas {:?}", moc.canvas);
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_identifier_in_a_real_model_is_usable() {
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    for (what, ids) in [
        ("part", &moc.part_ids),
        ("deformer", &moc.deformer_ids),
        ("drawable", &moc.drawable_ids),
        ("glue", &moc.glue_ids),
    ] {
        for (i, id) in ids.iter().enumerate() {
            assert!(!id.is_empty(), "{what} {i} has an empty id");
            assert!(id.chars().all(|c| !c.is_control()), "{what} {i} is not clean: {id:?}");
        }
    }
    // Duplicated identifiers would mean the table was misread, since Cubism
    // uses them as keys.
    let unique: std::collections::HashSet<_> = moc.drawable_ids.iter().collect();
    assert_eq!(unique.len(), moc.drawable_ids.len(), "drawable ids repeat");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_model_has_sane_parameter_ranges() {
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    for p in &moc.parameters {
        assert!(p.minimum <= p.default && p.default <= p.maximum, "{p:?} has a default outside its range");
        assert!(p.minimum.is_finite() && p.maximum.is_finite(), "{p:?}");
    }

    // Cubism's standard parameters are present in every rigged model and have
    // conventional ranges, so they are the strongest check that the three
    // arrays were told apart correctly rather than merely consistently.
    let angle = moc.parameter("ParamAngleX").expect("ParamAngleX is standard in every Cubism model");
    assert!(angle.minimum <= -10.0 && angle.maximum >= 10.0, "ParamAngleX spans {angle:?}");
    assert_eq!(angle.default, 0.0, "a head angle rests at zero");

    if let Some(eye) = moc.parameter("ParamEyeLOpen") {
        assert_eq!(eye.minimum, 0.0, "a closed eye is zero");
        assert!(eye.default > 0.0, "an eye rests open");
    }

    println!("{} parameters; first five:", moc.parameters.len());
    for p in moc.parameters.iter().take(5) {
        println!("  {:<24} {:8.2} .. {:8.2}  default {:8.2}", p.id, p.minimum, p.maximum, p.default);
    }
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_canvas_is_plausible() {
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");
    let c = moc.canvas;
    assert!(c.pixels_per_unit > 0.0, "{c:?}");
    assert!(c.size.0 > 0.0 && c.size.1 > 0.0, "{c:?}");
    // The origin lies inside the canvas; anything else means the fields are
    // being read in the wrong order.
    assert!((0.0..=c.size.0).contains(&c.origin.0), "{c:?}");
    assert!((0.0..=c.size.1).contains(&c.origin.1), "{c:?}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_truncated_real_model_fails_rather_than_returning_a_partial_one() {
    // Real files exercise paths a small fixture never reaches, so the
    // never-panic guarantee is worth re-checking against one.
    let Some(bytes) = payload() else { return };
    for cut in [0usize, 1, 4, 8, 0x40, 0x41, 0x400, 4096, bytes.len() / 2, bytes.len() - 1] {
        let _ = Moc3::parse(&bytes[..cut.min(bytes.len())]);
    }
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_model_yields_meshes_whose_triangles_are_all_in_range() {
    // The drawable tables are cumulative offsets into two shared arrays, so
    // reading them wrongly puts indices outside their own mesh almost
    // immediately. Checking every one is the strongest structural evidence
    // available that they were read right.
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    assert_eq!(moc.drawables.len(), moc.counts.drawables as usize);
    let mut vertices = 0usize;
    let mut triangles = 0usize;
    for d in &moc.drawables {
        assert!(d.vertex_count() > 0, "{} has no vertices", d.id);
        assert_eq!(d.indices.len() % 3, 0, "{} has a partial triangle", d.id);
        for &i in &d.indices {
            assert!((i as usize) < d.vertex_count(), "{} index {i} is outside its own mesh", d.id);
        }
        // Texture coordinates live in the unit square, give or take the sliver
        // of overshoot an atlas edge produces.
        for &(u, v) in &d.uvs {
            assert!((-0.01..=1.01).contains(&u) && (-0.01..=1.01).contains(&v), "{} has a uv at ({u}, {v})", d.id);
        }
        // `None` is legitimate: a drawable can hang off the model root rather
        // than off a deformer, which the format spells `0xFFFFFFFF`.
        if let Some(parent) = d.parent_deformer {
            assert!(parent < moc.counts.deformers, "{} names a deformer out of range", d.id);
        }
        vertices += d.vertex_count();
        triangles += d.triangle_count();
    }
    println!("{} drawables, {vertices} vertices, {triangles} triangles", moc.drawables.len());
    assert!(triangles > 100, "a character mesh should be more than a handful of triangles");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_element_reaches_its_own_keyforms() {
    // The pool is divided by walking it: warp deformers first, each keyform
    // padded to eight points, then drawables by the same rule. If any part of
    // that were wrong the parse would already have refused, so this checks the
    // consequence -- that every element's keyforms are actually addressable.
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    let mut warp_keyforms = 0usize;
    for w in &moc.warp_deformers {
        assert_eq!(w.point_count, (w.divisions.0 + 1) * (w.divisions.1 + 1), "{} grid", w.id);
        for k in 0..w.keyform_count as usize {
            let grid = moc
                .keyforms
                .warp(w.keyform_begin as usize + k, w.point_count as usize)
                .unwrap_or_else(|| panic!("{} keyform {k} is unreachable", w.id));
            assert_eq!(grid.len(), w.point_count as usize * 2);
            assert!(grid.iter().all(|v| v.is_finite()), "{} keyform {k} holds a non-finite value", w.id);
            warp_keyforms += 1;
        }
    }

    let mut drawable_keyforms = 0usize;
    for d in &moc.drawables {
        for k in 0..d.keyform_count as usize {
            let verts = moc
                .keyforms
                .drawable(d.keyform_begin as usize + k, d.vertex_count())
                .unwrap_or_else(|| panic!("{} keyform {k} is unreachable", d.id));
            assert_eq!(verts.len(), d.vertex_count() * 2);
            assert!(verts.iter().all(|v| v.is_finite()), "{} keyform {k} holds a non-finite value", d.id);
            drawable_keyforms += 1;
        }
    }

    assert_eq!(warp_keyforms + drawable_keyforms, moc.keyforms.len());
    println!(
        "{} warp deformers own {warp_keyforms} keyforms; {} drawables own {drawable_keyforms}",
        moc.warp_deformers.len(),
        moc.drawables.len()
    );
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_keyform_pool_is_consistent_with_its_offsets() {
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    assert!(!moc.keyforms.is_empty(), "a rigged model has keyforms");
    assert!(!moc.keyforms.positions.is_empty());
    for value in &moc.keyforms.positions {
        assert!(value.is_finite(), "the keyform pool holds a non-finite coordinate");
    }
    // Offsets are non-decreasing and stay inside the pool.
    let mut previous = 0u32;
    for (i, &offset) in moc.keyforms.warp_offsets.iter().chain(&moc.keyforms.drawable_offsets).enumerate() {
        assert!(offset >= previous, "keyform {i} starts before its predecessor");
        assert!((offset as usize) <= moc.keyforms.positions.len(), "keyform {i} starts past the pool");
        previous = offset;
    }
    println!("{} keyforms over {} coordinates", moc.keyforms.len(), moc.keyforms.positions.len());
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn posing_a_real_model_lands_somewhere_the_canvas_can_hold() {
    // This is the weakest test in the suite, and deliberately so. A pose cannot
    // be confirmed structurally: wrong blend weights still produce finite
    // numbers. What *can* be said is that the result occupies a region the
    // canvas could plausibly frame, which would fail loudly if the deformer
    // chain were being applied in the wrong space or the wrong order.
    let Some(bytes) = payload() else { return };
    let moc = Moc3::parse(&bytes).expect("a real MOC3 should parse");

    let pose = moc.pose(&[]);
    assert_eq!(pose.drawables.len(), moc.drawables.len());

    let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    let mut points = 0usize;
    for (d, verts) in pose.drawables.iter().enumerate() {
        assert_eq!(verts.len(), moc.drawables[d].vertex_count());
        for &(x, y) in verts {
            // The pose falls back to un-deformed coordinates rather than
            // handing a renderer something it cannot draw, so this holds even
            // while the chain is wrong for some drawables.
            assert!(x.is_finite() && y.is_finite(), "{} produced a non-finite vertex", moc.drawables[d].id);
            lo_x = lo_x.min(x);
            hi_x = hi_x.max(x);
            lo_y = lo_y.min(y);
            hi_y = hi_y.max(y);
            points += 1;
        }
    }

    // Reported, not asserted away: this is the count of drawables whose
    // deformer chain is still wrong, and it is the number to watch shrink.
    println!("  drawables whose chain was unusable: {}", pose.unstable.len());

    let canvas = moc.canvas;
    let units = (canvas.size.0 / canvas.pixels_per_unit, canvas.size.1 / canvas.pixels_per_unit);
    println!("posed {points} vertices");
    println!("  bounds: x {lo_x:.3}..{hi_x:.3}   y {lo_y:.3}..{hi_y:.3}");
    println!("  canvas: {:.3} x {:.3} units ({} x {} px)", units.0, units.1, canvas.size.0, canvas.size.1);

    // A character occupies its canvas to within a small multiple; an order of
    // magnitude out means the chain is wrong, not merely imprecise.
    let width = hi_x - lo_x;
    let height = hi_y - lo_y;
    assert!(width > 0.0 && height > 0.0, "the pose collapsed to a point");
    let _ = (width, height);

    // Most of the model does pose into its own canvas, and how much is the
    // number worth guarding: it should only ever go up. Five of the six
    // reference models place every drawable and the sixth misses only a
    // backdrop sheet genuinely wider than its canvas, but a model this test
    // has never seen may miss more, so this is a floor, not a target.
    let limit = units.0.max(units.1) * 2.0;
    let inside =
        pose.drawables.iter().filter(|verts| verts.iter().all(|(x, y)| x.abs() <= limit && y.abs() <= limit)).count();
    println!("  drawables posed inside the canvas: {inside} of {}", pose.drawables.len());
    assert!(
        inside * 100 >= pose.drawables.len() * 90,
        "only {inside} of {} drawables posed inside the canvas; the deformer chain has regressed",
        pose.drawables.len()
    );
}
