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
use a2d_import::SpineRig;

fn bundle() -> Option<(Vec<SpineRig>, LoadReport)> {
    let path = std::env::var("A2D_FIXTURE_SPINE_BUNDLE").ok()?;
    let bytes = std::fs::read(path).ok()?;
    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_spine_bundle(&bytes, &mut report).ok()?;
    assert!(
        !inventory.components.is_empty(),
        "a spine-unity bundle names its component classes; without one, the match is a guess"
    );
    assert!(!inventory.textures.is_empty(), "a rig with no texture page cannot be drawn");
    Some((inventory.rigs, report))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn a_spine_bundle_yields_a_skeleton_and_an_atlas() {
    let Some((rigs, _)) = bundle() else { return };
    assert!(!rigs.is_empty(), "no rig found");
    for rig in &rigs {
        // Detection reads the payload, not the asset's name.
        assert!(!rig.skeleton_version.is_empty(), "{rig:?}");
        assert!(rig.atlas.is_some(), "both halves are needed to draw: {rig:?}");
        assert!(rig.pages.iter().all(|(_, image)| image.is_some()), "a page has no texture: {rig:?}");
    }
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn the_extracted_rig_decodes_with_no_unity_specific_handling() {
    let Some((rigs, mut report)) = bundle() else { return };
    let ir = a2d_import::decode_rig(&rigs[0], &mut report).expect("should decode");

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
}

/// Every draw order keyframe in a real rig must resolve to a permutation.
///
/// This is the strongest kind of check available here, because it holds the
/// decoder against a constraint the format imposes rather than against our own
/// idea of the format: a draw order is a rearrangement of the slots, so each
/// keyframe's offsets have to land every moved slot on a distinct index that
/// exists. A misread offset almost always violates that, and cannot be talked
/// into satisfying it.
///
/// It is what established that the offset is a raw 32-bit varint rather than a
/// zig-zagged one. Under the zig-zagged reading, 62 keyframes across a sample
/// of 120 rigs failed this; under the raw reading, none did. The round-trip
/// fixture test passed the whole time, because the fixture writer shared the
/// misunderstanding.
#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn every_draw_order_keyframe_forms_a_permutation_of_the_slots() {
    let Some((rigs, mut report)) = bundle() else { return };
    let ir = a2d_import::decode_rig(&rigs[0], &mut report).expect("should decode");

    // The decoder degrades rather than fails on an impossible order (§10), so
    // the failure is in the report, not in the return value.
    let rejected: Vec<String> =
        report.warnings().iter().map(ToString::to_string).filter(|w| w.contains("drawOrder offset")).collect();
    assert!(rejected.is_empty(), "draw order offsets did not resolve: {rejected:#?}");

    // And what did resolve has to be a genuine permutation, not merely
    // something the resolver was willing to return.
    for animation in &ir.animations {
        for timeline in &animation.timelines {
            let a2d_core::ir::spine::Timeline::DrawOrder { keys } = timeline else { continue };
            for key in keys {
                let Some(order) = &key.order else { continue };
                assert_eq!(order.len(), ir.slots.len(), "{:?} at {}", animation.name, key.time);
                let mut seen = vec![false; ir.slots.len()];
                for slot in order {
                    let index = slot.0 as usize;
                    assert!(index < seen.len(), "{:?} at {} names slot {index}", animation.name, key.time);
                    assert!(!seen[index], "{:?} at {} draws slot {index} twice", animation.name, key.time);
                    seen[index] = true;
                }
            }
        }
    }
}

/// The fingerprint lines `preview -o` prints, one per rendered frame.
fn frame_lines(printed: &str) -> Vec<String> {
    printed
        .lines()
        .filter(|l| l.contains("fingerprint"))
        .map(|l| l.split("->").next().unwrap_or(l).to_string())
        .collect()
}

/// A bundle imports into a package that validates and renders as the bundle
/// does when previewed directly.
///
/// Both go through discovery in the importer, but the package then takes the
/// long way round -- IR serialised, textures re-encoded as PNG, everything read
/// back -- and the bundle does not. Identical frame fingerprints at every
/// regression timestamp are what say that trip lost nothing.
#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn an_imported_bundle_validates_and_renders_as_the_bundle_does() {
    let Ok(path) = std::env::var("A2D_FIXTURE_SPINE_BUNDLE") else { return };
    let dir = std::env::temp_dir().join(format!("a2d-real-spine-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let package = dir.join("out.a2dpack");

    let mut out = Vec::new();
    a2d_cli::import(&mut out, std::path::Path::new(&path), &package, None).expect("the bundle should import");
    let printed = String::from_utf8_lossy(&out).into_owned();
    assert!(printed.contains("source game:   unity_spine"), "{printed}");

    let mut out = Vec::new();
    let clean = a2d_cli::validate(&mut out, &package).expect("validate runs");
    assert!(clean, "the imported package should validate with exit 0:\n{}", String::from_utf8_lossy(&out));

    // Rendering needs a GPU; without one the checks above still stand.
    if a2d_render::GpuContext::headless().is_err() {
        eprintln!("no GPU adapter: skipping the render comparison");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let render = |input: &std::path::Path, frames: &str| {
        let mut out = Vec::new();
        a2d_cli::preview(&mut out, input, Some(&dir.join(frames)), a2d_cli::ViewerFlags::default())
            .expect("preview renders");
        frame_lines(&String::from_utf8_lossy(&out))
    };
    let from_bundle = render(std::path::Path::new(&path), "bundle");
    let from_package = render(&package, "package");
    assert_eq!(from_bundle.len(), a2d_cli::REGRESSION_TIMESTAMPS.len(), "{from_bundle:?}");
    assert_eq!(from_bundle, from_package, "the package must render exactly as the bundle does");
    let _ = std::fs::remove_dir_all(&dir);
}
