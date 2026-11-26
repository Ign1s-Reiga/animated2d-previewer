//! Runtime-driver components of a real Unity bundle are reported, not dropped.
//!
//! The prefabs carry one look-at rig and no eye blink, mouth, harmonic motion,
//! breath, expression or pose-part components. The rig is recovered into the
//! package, and playback applies it at its rest target, so the import says so;
//! the others must not be reported when they are absent.
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
    assert!(look[0].contains("recovered") && look[0].contains("applied at its rest target"), "{}", look[0]);
    assert!(!look[0].contains("no readable owner") && !look[0].contains("not recovered"), "{}", look[0]);
    let model = package.model.as_cubism().expect("a Cubism package");
    let rig = model.look.as_ref().expect("the look rig is recovered into the model");
    println!("rig: {rig:?}");
    assert!(!rig.parameters.is_empty());
    assert_eq!(rig.problem(&model.parameters), None);
    assert!(rig.damping.is_finite() && rig.damping >= 0.0);
    let (tx, ty, tz) = rig.rest_target;
    assert!([tx, ty, tz].iter().all(|v| v.is_finite()));
    // Within the model's own extent: a target far outside it would mean the unit is wrong.
    assert!(tx.abs() < 10.0 && ty.abs() < 10.0 && tz.abs() < 10.0, "{:?}", rig.rest_target);
    assert_eq!(rig.center, (0.0, 0.0, 0.0), "a null Center is the controller's own origin");
    assert!(look[0].contains(&format!("{} look parameter(s)", rig.parameters.len())), "{}", look[0]);
    for p in &rig.parameters {
        assert!(p.factor.is_finite() && p.factor != 0.0, "{p:?}");
        println!("  {} {:?} {}", p.parameter, p.axis, p.factor);
    }

    // Inspecting reports the same outcome as importing.
    let mut inspected = LoadReport::default();
    let inventory = a2d_import::inspect_bundle(&bytes, &mut inspected).expect("the bundle should inspect");
    assert_eq!(inventory.look.as_ref(), Some(rig));
    let inspected_note = inspected.warnings().iter().map(|w| w.to_string()).find(|n| n.starts_with("look-at rig"));
    assert_eq!(inspected_note.as_ref(), Some(look[0]));

    // The rig survives the package: write, read back, and `validate` names it.
    let dir = std::env::temp_dir().join(format!("a2d-real-drivers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package_dir = dir.join("look.a2dpack");
    package.write_to(&package_dir).expect("the package should write");
    let back = a2d_pack::Package::read_from(&package_dir).expect("the package should read back");
    assert_eq!(back.model.as_cubism().and_then(|m| m.look.as_ref()), Some(rig));
    let mut out = Vec::new();
    a2d_cli::validate(&mut out, &package_dir).expect("validate should run");
    let text = String::from_utf8_lossy(&out).into_owned();
    println!("{text}");
    assert!(text.contains("look-at rig:") && text.contains("recovered: "), "{text}");
    let _ = std::fs::remove_dir_all(&dir);

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
    // The note is persisted, so `validate` and `preview` show it after the bundle is gone. It is
    // information only for the rig shape whose reconstruction is verified; any other shape keeps it
    // a warning. Either way it is recorded exactly once.
    let in_notes = package.manifest.import_notes.iter().filter(|w| w.starts_with("look-at rig")).count();
    let in_warnings = package.manifest.import_warnings.iter().filter(|w| w.starts_with("look-at rig")).count();
    assert_eq!(in_notes + in_warnings, 1, "{:?}", package.manifest.recorded_at_import());
}
