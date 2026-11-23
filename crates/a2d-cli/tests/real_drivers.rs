//! Runtime-driver components of a real Unity bundle are reported, not dropped.
//!
//! The prefabs carry one look-at rig and no eye blink, mouth, harmonic motion,
//! breath, expression or pose-part components. The rig is not applied, so the
//! import has to say so; the others must not be reported when they are absent.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_drivers -- --ignored --nocapture
//! ```

use a2d_core::LoadReport;

fn bundle_bytes() -> Option<Vec<u8>> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}")))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn the_look_rig_is_noted_with_its_parameters_and_absent_drivers_are_not() {
    let Some(bytes) = bundle_bytes() else { return };
    let mut report = LoadReport::default();
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should import");
    let notes: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();

    let look: Vec<&String> = notes.iter().filter(|n| n.starts_with("look-at rig")).collect();
    assert_eq!(look.len(), 1, "exactly one look note expected: {notes:?}");
    println!("{}", look[0]);
    assert!(!look[0].contains("no readable owner"), "{}", look[0]);
    let count: usize = look[0]
        .split("not applied: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no parameter count in {:?}", look[0]));
    assert!(count > 0, "{}", look[0]);
    let ids = look[0]
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(ids, _)| ids)
        .unwrap_or_else(|| panic!("no id list in {:?}", look[0]));
    let model = package.model.as_cubism().expect("a Cubism package");
    for id in ids.split(", ") {
        assert!(model.parameters.iter().any(|p| p.id == id), "look parameter {id:?} is not a parameter of the model");
    }

    // The absent drivers are matched by this module's own note prefixes.
    for absent in [
        "eye blink components",
        "mouth components",
        "harmonic motion components",
        "breath components",
        "pose part components",
        "an expression list",
        "an expression controller",
    ] {
        assert!(!notes.iter().any(|n| n.starts_with(absent)), "unexpected {absent:?} note in {notes:?}");
    }

    // Hit areas are reported exactly when the bundle carries hit drawables.
    let hit: Vec<&String> = notes.iter().filter(|n| n.starts_with("hit area components")).collect();
    let bundle = a2d_unity::Bundle::parse(&bytes).expect("the bundle should parse");
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("a serialized file");
    let file = a2d_unity::SerializedFile::parse(bundle.node_data(node).expect("node data")).expect("serialized file");
    let carried = a2d_unity::Inventory::build(&file).by_script("CubismHitDrawable").count();
    match hit.as_slice() {
        [] => assert_eq!(carried, 0, "hit drawables carried but not reported"),
        [note] => {
            println!("{note}");
            assert!(carried > 0 && note.contains(&format!("CubismHitDrawable x{carried}")), "{note}");
        }
        _ => panic!("more than one hit area note: {notes:?}"),
    }
    // The notes are persisted, so `validate` and `preview` show them after the bundle is gone.
    assert!(package.manifest.import_warnings.iter().any(|w| w.starts_with("look-at rig")));
}
