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

    // Notes recorded at import are expected; anything else is a real problem.
    let problems: Vec<String> = package
        .validate()
        .warnings()
        .iter()
        .map(|w| w.to_string())
        .filter(|w| !w.starts_with("recorded at import:"))
        .collect();
    assert!(problems.is_empty(), "validate reported: {problems:#?}");
}
