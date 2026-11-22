//! Pins what a real bundle does and does not carry about part opacity.
//!
//! Part opacity and pose are not applied by the runtime. That is only
//! harmless while no target bundle has any, so this checks it on real data:
//! the clips must be converted and none may bind a part, and the MOC3's part count
//! must agree with the bundle's `CubismPart` components (the importer's note
//! for a part curve names parts through those components).
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_part_opacity -- --ignored --nocapture
//! ```

use a2d_core::LoadReport;

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_bundle_carries_no_part_opacity_and_its_parts_match_the_moc3() {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));

    let inventory = a2d_import::inspect_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should inventory");
    let mut report = LoadReport::new();
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should reconstruct");
    let ir = package.model.as_cubism().expect("a Cubism package");

    let text = report.to_string();
    println!("{} parts, {} CubismPart components", ir.part_ids.len(), inventory.parts);
    // The classifier must have run, or the absence below proves nothing.
    assert!(
        text.contains("recovered from its AnimationClip"),
        "no clip was converted:
{text}"
    );
    assert!(!ir.motions.is_empty(), "no motion was recovered");
    // Neither the named note nor the generic "CubismPart property 0x..." one.
    assert!(
        !text.contains("CubismPart") && !text.contains("part-opacity"),
        "a clip binds a part:
{text}"
    );
    assert_eq!(ir.part_ids.len(), inventory.parts, "the MOC3 and the prefab disagree about how many parts there are");
}
