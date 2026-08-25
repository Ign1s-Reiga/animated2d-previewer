//! A real Unity bundle holding a Spine rig, read end to end.
//!
//! Spine survives Unity intact: the skeleton and atlas are `TextAsset`s
//! carrying the editor's own bytes. That makes this test unusually strong for
//! an importer test — if the extraction is right, the existing decoder reads
//! the result with no Unity-specific handling at all, so a green run means both
//! halves agree with a file the Spine editor actually wrote.
//!
//! Gated on `A2D_FIXTURE_SPINE_BUNDLE`, like the other real-asset tests:
//! extracted assets are never committed (§11).

use a2d_core::LoadReport;
use a2d_import::{AssetKind, SpineInventory};

fn bundle() -> Option<(SpineInventory, LoadReport)> {
    let path = std::env::var("A2D_FIXTURE_SPINE_BUNDLE").ok()?;
    let bytes = std::fs::read(path).ok()?;
    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_spine_bundle(&bytes, &mut report).ok()?;
    Some((inventory, report))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn a_spine_bundle_yields_a_skeleton_and_an_atlas() {
    let Some((inventory, _)) = bundle() else { return };
    assert!(inventory.is_spine(), "both halves are needed: {inventory:?}");
    assert!(
        !inventory.components.is_empty(),
        "a spine-unity bundle names its component classes; without one, the match is a guess"
    );

    // Detection reads the payload, not the asset's name.
    let kind = inventory.skeleton_kind.as_ref().expect("a skeleton was found");
    assert!(matches!(kind, AssetKind::SpineSkeleton { .. }), "{kind:?}");
    assert!(!inventory.textures.is_empty(), "a rig with no texture page cannot be drawn");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn the_extracted_rig_decodes_with_no_unity_specific_handling() {
    let Some((inventory, mut report)) = bundle() else { return };
    let (Some(skeleton), Some(atlas)) = (&inventory.skeleton, &inventory.atlas) else { return };

    let text = String::from_utf8_lossy(&atlas.bytes);
    let (pages, atlas_report) = a2d_spine::parse_atlas(&text).expect("the atlas should parse");
    report.absorb(atlas_report);
    let (ir, detection) = a2d_spine::decode_skeleton(&skeleton.bytes, pages, &mut report).expect("should decode");

    assert!(!ir.bones.is_empty(), "a rig has bones");
    assert!(!ir.slots.is_empty(), "a rig has slots");
    assert!(!ir.animations.is_empty(), "a rig has animations");
    // Every slot must name a bone that exists, which is the check that a
    // mis-extracted payload would fail rather than merely look odd.
    for slot in &ir.slots {
        assert!(
            (slot.bone.0 as usize) < ir.bones.len(),
            "slot {:?} names bone {} of {}",
            slot.name,
            slot.bone.0,
            ir.bones.len()
        );
    }
    assert!(!detection.version.to_string().is_empty());
}
