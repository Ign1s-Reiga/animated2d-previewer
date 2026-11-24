//! `import` on a synthetic Unity bundle holding a Spine rig.
//!
//! The bundle is built at test time (spec §11 forbids committing extracted
//! assets). It carries a skeleton and an atlas as `TextAsset`s and one
//! `Texture2D`, which is the shape a real spine-unity bundle has.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use a2d_unity::testing::BundleBuilder;

static COUNTER: AtomicU32 = AtomicU32::new(0);

const SKELETON: &str = r#"{
  "skeleton": { "spine": "4.1.00", "width": 16, "height": 16 },
  "bones": [ { "name": "root" } ],
  "slots": [ { "name": "body", "bone": "root", "attachment": "body" } ],
  "skins": [ { "name": "default", "attachments": { "body": { "body": { "width": 16, "height": 16 } } } } ],
  "animations": { "idle": { "bones": { "root": { "rotate": [ { "time": 0, "angle": 0 }, { "time": 1, "angle": 10 } ] } } } }
}"#;

const ATLAS: &str = "\nhero.png\nsize: 2,2\nformat: RGBA8888\nfilter: Linear,Linear\nrepeat: none\nbody\n  rotate: false\n  xy: 0, 0\n  size: 2, 2\n  orig: 2, 2\n  offset: 0, 0\n  index: -1\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Scratch {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("a2d-us-import-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rig() -> BundleBuilder {
    BundleBuilder::new()
        .text_asset("hero.skel", SKELETON.as_bytes())
        .text_asset("hero.atlas", ATLAS.as_bytes())
        .texture("hero", 2, 2, &[255; 16])
}

fn run_import(input: &Path, output: &Path, game: Option<&str>) -> Result<String, String> {
    let mut out = Vec::new();
    a2d_cli::import(&mut out, input, output, game).map_err(|e| e.to_string())?;
    Ok(String::from_utf8(out).expect("utf-8"))
}

#[test]
fn a_spine_bundle_imports_without_naming_an_importer_and_validates() {
    let dir = Scratch::new("auto");
    let bundle = dir.write("rig.bundle", &rig().build());
    let package = dir.path().join("out.a2dpack");

    let text = run_import(&bundle, &package, None).expect("import");
    assert!(text.contains("Imported hero"), "{text}");
    assert!(text.contains("source game:   unity_spine"), "{text}");
    assert!(text.contains("default:       idle"), "{text}");
    assert!(package.join("textures").join("hero.png").is_file());

    let mut out = Vec::new();
    let clean = a2d_cli::validate(&mut out, &package).expect("validate");
    assert!(clean, "{}", String::from_utf8_lossy(&out));
}

#[test]
fn naming_the_importer_gives_the_same_package() {
    let dir = Scratch::new("named");
    let bundle = dir.write("rig.bundle", &rig().build());
    let auto = dir.path().join("auto.a2dpack");
    let named = dir.path().join("named.a2dpack");
    run_import(&bundle, &auto, None).expect("auto");
    run_import(&bundle, &named, Some("unity_spine")).expect("named");
    assert_eq!(
        std::fs::read(auto.join("model.bin")).expect("auto model"),
        std::fs::read(named.join("model.bin")).expect("named model")
    );
}

#[test]
fn a_bundle_holding_two_rigs_is_refused_and_names_both() {
    let dir = Scratch::new("two");
    let bundle = dir.write("rig.bundle", &rig().text_asset("zed.skel", SKELETON.as_bytes()).build());
    let err = run_import(&bundle, &dir.path().join("out.a2dpack"), None).unwrap_err();
    assert!(err.contains("ambiguous") && err.contains("hero") && err.contains("zed"), "{err}");
    assert!(!dir.path().join("out.a2dpack").exists(), "nothing is written for an ambiguous bundle");
}

#[test]
fn a_bundle_of_neither_shape_is_an_error_that_says_what_was_expected() {
    let dir = Scratch::new("neither");
    let bundle = dir.write("other.bundle", &BundleBuilder::new().text_asset("notes", b"hello").build());
    let err = run_import(&bundle, &dir.path().join("out.a2dpack"), None).unwrap_err();
    assert!(
        err.contains("expected a Spine skeleton") && err.contains("CubismMoc") && err.contains("found neither"),
        "{err}"
    );
}

#[test]
fn asking_the_wrong_importer_fails_loudly_and_does_not_fall_back() {
    let dir = Scratch::new("wrong");
    let bundle = dir.write("rig.bundle", &rig().build());
    let err = run_import(&bundle, &dir.path().join("out.a2dpack"), Some("unity_cubism")).unwrap_err();
    assert!(err.contains("CubismMoc"), "{err}");
    let err = run_import(&bundle, &dir.path().join("out.a2dpack"), Some("spine_bytes")).unwrap_err();
    assert!(err.contains("loose files"), "{err}");
}

#[test]
fn a_missing_texture_page_is_carried_into_the_package_warnings() {
    let dir = Scratch::new("missing");
    let bundle =
        BundleBuilder::new().text_asset("hero.skel", SKELETON.as_bytes()).text_asset("hero.atlas", ATLAS.as_bytes());
    let bundle = dir.write("rig.bundle", &bundle.build());
    let package = dir.path().join("out.a2dpack");
    let text = run_import(&bundle, &package, None).expect("a partial import still succeeds");
    assert!(text.contains("texture page") && text.contains("hero.png"), "{text}");

    let manifest = std::fs::read_to_string(package.join("manifest.json")).expect("manifest");
    assert!(manifest.contains("importWarnings") && manifest.contains("hero.png"), "{manifest}");
    let mut out = Vec::new();
    assert!(!a2d_cli::validate(&mut out, &package).expect("validate"), "validate must flag the missing page");
}

#[test]
fn a_directory_is_named_as_not_a_bundle_and_not_reported_as_an_io_error() {
    let dir = Scratch::new("directory");
    dir.write("rig.bundle", &rig().build());
    let err = run_import(dir.path(), &dir.path().join("out.a2dpack"), Some("unity_spine")).unwrap_err();
    assert!(err.contains("is not a Unity bundle"), "{err}");
}

#[test]
fn previewing_a_two_rig_bundle_is_refused_as_ambiguous_before_any_rendering() {
    let dir = Scratch::new("preview-two");
    let bundle = dir.write("rig.bundle", &rig().text_asset("zed.skel", SKELETON.as_bytes()).build());
    let frames = dir.path().join("frames");
    let mut out = Vec::new();
    let err = a2d_cli::preview(&mut out, &bundle, Some(&frames), a2d_cli::ViewerFlags::default()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("ambiguous") && text.contains("hero") && text.contains("zed"), "{text}");
    assert!(!frames.exists(), "nothing is rendered or written for an ambiguous bundle");
}

#[test]
fn inspecting_a_two_rig_bundle_lists_both_with_their_atlas() {
    let dir = Scratch::new("inspect-two");
    let bundle = dir.write("rig.bundle", &rig().text_asset("zed.skel", SKELETON.as_bytes()).build());
    let mut out = Vec::new();
    a2d_cli::inspect(&mut out, &bundle, None).expect("inspect");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(text.contains("rigs:        2"), "{text}");
    assert!(text.contains("Spine rig: hero.skel") && text.contains("Spine rig: zed.skel"), "{text}");
    assert_eq!(text.matches("atlas:       hero.atlas").count(), 2, "{text}");
}

#[test]
fn a_plain_image_with_a_bundle_like_name_is_not_parsed_as_a_bundle() {
    let dir = Scratch::new("png-named-like-a-bundle");
    // A PNG signature and nothing else, under a name no extension gives away.
    let image = dir.write("rig_prefab", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01");
    // A skeleton beside it under another name must not be picked up for it.
    dir.write("other.json", SKELETON.as_bytes());

    let mut out = Vec::new();
    // The listing comes first and the failure after it, as for any lone file.
    let err = a2d_cli::inspect(&mut out, &image, None).unwrap_err();
    assert!(err.to_string().contains("is not a Spine skeleton"), "{err}");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(text.contains("Importer: generic") && text.contains("texture (png)"), "{text}");

    let err = run_import(&image, &dir.path().join("out.a2dpack"), None).unwrap_err();
    assert!(err.contains("missing skeleton") && err.contains("is not a Spine skeleton"), "{err}");
    assert!(!dir.path().join("out.a2dpack").exists());
}

#[test]
fn an_unreadable_text_asset_next_to_a_rig_is_still_printed_by_inspect() {
    let dir = Scratch::new("inspect-broken");
    let bundle = dir.write("rig.bundle", &rig().broken_text_asset("damaged").build());
    let mut out = Vec::new();
    a2d_cli::inspect(&mut out, &bundle, None).expect("inspect");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(text.contains("could not be read"), "{text}");
}

/// A bundle detection refuses: no Spine skeleton, and a TextAsset that cannot
/// be read, which may have been one. (A real Cubism bundle with a corrupt
/// TextAsset is the case this stands in for; the builder cannot write a
/// `CubismMoc`, so that exact combination is untested.)
fn undecidable() -> Vec<u8> {
    BundleBuilder::new().broken_text_asset("damaged").text_asset("notes", b"hello").build()
}

#[test]
fn a_bundle_detection_refuses_names_the_override_and_the_override_bypasses_it() {
    let dir = Scratch::new("override");
    let bundle = dir.write("odd.bundle", &undecidable());
    let package = dir.path().join("out.a2dpack");

    // Without --game: strict, with the way out in the message, for all three commands.
    let err = run_import(&bundle, &package, None).unwrap_err();
    assert!(err.contains("could not be read") && err.contains("--game unity_cubism or --game unity_spine"), "{err}");
    let err = a2d_cli::inspect(&mut Vec::new(), &bundle, None).unwrap_err().to_string();
    assert!(err.contains("--game unity_cubism"), "{err}");
    let err = a2d_cli::preview(&mut Vec::new(), &bundle, None, a2d_cli::ViewerFlags::default()).unwrap_err();
    assert!(err.to_string().contains("--game unity_cubism"), "{err}");

    // With it, detection is not consulted: the named importer runs and fails (or not) on its own terms.
    let err = run_import(&bundle, &package, Some("unity_cubism")).unwrap_err();
    assert!(err.contains("CubismMoc") && !err.contains("could not be read"), "{err}");
    let mut out = Vec::new();
    a2d_cli::inspect(&mut out, &bundle, Some("unity_cubism")).expect("inspect honours --game for a bundle");
    assert!(String::from_utf8_lossy(&out).contains("Unity bundle"));
    let flags =
        a2d_cli::ViewerFlags { game: a2d_cli::parse_game(Some("unity_cubism")).expect("name"), ..Default::default() };
    let err = a2d_cli::preview(&mut Vec::new(), &bundle, None, flags).unwrap_err();
    assert!(err.to_string().contains("CubismMoc"), "{err}");

    // A loose-file importer is refused on a bundle by every command.
    let err = a2d_cli::inspect(&mut Vec::new(), &bundle, Some("spine_bytes")).unwrap_err().to_string();
    assert!(err.contains("loose files"), "{err}");
    let flags =
        a2d_cli::ViewerFlags { game: a2d_cli::parse_game(Some("generic")).expect("name"), ..Default::default() };
    let err = a2d_cli::preview(&mut Vec::new(), &bundle, None, flags).unwrap_err().to_string();
    assert!(err.contains("loose files"), "{err}");
    assert!(a2d_cli::parse_game(Some("nonsense")).is_err());
}
