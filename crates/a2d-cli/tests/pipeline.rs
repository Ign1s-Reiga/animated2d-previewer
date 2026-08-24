//! End-to-end pipeline test: source assets → inspect → import → validate → preview.
//!
//! The fixture is synthetic and generated at test time. Spec §11 forbids
//! committing extracted game assets, and a hand-authored minimal model exercises
//! the same code paths. Tests that need a real asset belong behind `#[ignore]`
//! and an env var — see `tests/README.md`.

use std::sync::atomic::{AtomicU32, Ordering};

#[path = "support/mod.rs"]
mod support;

use support::{png_chunks_are_valid, Fixture, TempDir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("a2d-{}-{}-{}", label, std::process::id(), n));
    TempDir::new(path)
}

/// Runs a command and returns what it printed.
fn capture(f: impl FnOnce(&mut Vec<u8>) -> Result<(), String>) -> String {
    let mut out = Vec::new();
    match f(&mut out) {
        Ok(()) => String::from_utf8(out).expect("output should be UTF-8"),
        Err(e) => panic!("command failed: {e}\npartial output:\n{}", String::from_utf8_lossy(&out)),
    }
}

#[test]
fn inspect_identifies_every_file_and_the_character() {
    let dir = temp_dir("inspect");
    Fixture::spine_json().write_to(dir.path());

    let text = capture(|out| a2d_cli::inspect(out, dir.path(), None).map_err(|e| e.to_string()));

    // Every file is classified by content.
    assert!(text.contains("hero.json  —  spine skeleton 3.8.99 (json)"), "{text}");
    assert!(text.contains("hero.atlas  —  spine atlas"), "{text}");
    assert!(text.contains("hero.png  —  texture (png)"), "{text}");

    // The character is described.
    assert!(text.contains("Character: hero"), "{text}");
    assert!(text.contains("version:  spine 3.8.99"), "{text}");
    assert!(text.contains("bones:    3"), "{text}");
    assert!(text.contains("slots:    2"), "{text}");
    assert!(text.contains("hero.png (64x64)"), "{text}");
    assert!(text.contains("idle"), "{text}");
    assert!(text.contains("walk"), "{text}");
    assert!(text.contains("bounds:"), "{text}");
    assert!(text.contains("Loaded cleanly."), "{text}");
}

#[test]
fn inspect_accepts_a_single_file_and_finds_its_siblings() {
    let dir = temp_dir("inspect-file");
    Fixture::spine_json().write_to(dir.path());
    let text = capture(|out| a2d_cli::inspect(out, &dir.path().join("hero.json"), None).map_err(|e| e.to_string()));
    assert!(text.contains("Character: hero"), "{text}");
    assert!(text.contains("hero.atlas"), "{text}");
}

#[test]
fn import_writes_a_package_that_validates_cleanly() {
    let dir = temp_dir("import");
    Fixture::spine_json().write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");

    let text = capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));
    assert!(text.contains("Imported hero"), "{text}");
    assert!(text.contains("source format: spine-3.8"), "{text}");
    assert!(text.contains("default:       idle"), "{text}");
    assert!(text.contains("Loaded cleanly."), "{text}");

    // The documented package layout is on disk.
    assert!(package_dir.join("manifest.json").is_file(), "manifest.json should exist");
    assert!(package_dir.join("model.bin").is_file(), "model.bin should exist");
    assert!(package_dir.join("textures").join("hero.png").is_file(), "the texture page should be copied");

    let text = capture(|out| a2d_cli::validate(out, &package_dir).map(|_| ()).map_err(|e| e.to_string()));
    assert!(text.contains("textures:      1/1"), "{text}");
    assert!(text.contains("Loaded cleanly."), "{text}");
}

#[test]
fn the_manifest_matches_its_committed_golden() {
    let dir = temp_dir("golden-manifest");
    Fixture::spine_json().write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");
    capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));

    let actual = std::fs::read_to_string(package_dir.join("manifest.json")).unwrap();
    let expected = "\
{
  \"formatVersion\": 1,
  \"modelType\": \"spine\",
  \"sourceGame\": \"generic\",
  \"sourceFormat\": \"spine-3.8\",
  \"displayName\": \"hero\",
  \"defaultAnimation\": \"idle\",
  \"textures\": [
    {
      \"file\": \"hero.png\",
      \"size\": [
        64,
        64
      ],
      \"premultipliedAlpha\": false
    }
  ],
  \"animations\": [
    {
      \"name\": \"idle\",
      \"duration\": 1.0
    },
    {
      \"name\": \"walk\",
      \"duration\": 0.5
    }
  ]
}
";
    assert_eq!(actual, expected, "manifest golden mismatch");
}

#[test]
fn importing_twice_produces_byte_identical_output() {
    // Deterministic serialisation is what golden tests depend on (spec §10).
    let dir = temp_dir("determinism");
    Fixture::spine_json().write_to(dir.path());

    let first = dir.path().join("a.a2dpack");
    let second = dir.path().join("b.a2dpack");
    capture(|out| a2d_cli::import(out, dir.path(), &first, None).map_err(|e| e.to_string()));
    capture(|out| a2d_cli::import(out, dir.path(), &second, None).map_err(|e| e.to_string()));

    for file in ["manifest.json", "model.bin"] {
        let a = std::fs::read(first.join(file)).unwrap();
        let b = std::fs::read(second.join(file)).unwrap();
        assert_eq!(a, b, "{file} differs between runs");
    }
}

#[test]
fn a_reimported_package_reproduces_the_same_model_bytes() {
    // source → IR → model.bin → IR → model.bin must be a fixed point.
    let dir = temp_dir("fixed-point");
    Fixture::spine_json().write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");
    capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));

    let first = std::fs::read(package_dir.join("model.bin")).unwrap();
    let package = a2d_pack::Package::read_from(&package_dir).unwrap();
    let second = package.encode_model();
    assert_eq!(first, second);
}

#[test]
fn preview_poses_the_regression_timestamps() {
    let dir = temp_dir("preview");
    Fixture::spine_json().write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");
    capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));

    let text = capture(|out| a2d_cli::preview(out, &package_dir).map_err(|e| e.to_string()));
    assert!(text.contains("Model:     hero"), "{text}");
    assert!(text.contains("Animation: idle"), "{text}");
    for time in ["t=0", "t=0.25", "t=0.5", "t=1"] {
        assert!(text.contains(time), "expected {time} in:\n{text}");
    }
    // Both slots draw at every frame.
    assert!(text.contains("2 meshes"), "{text}");
}

#[test]
fn a_missing_texture_is_reported_rather_than_failing_the_import() {
    let dir = temp_dir("missing-texture");
    let mut fixture = Fixture::spine_json();
    fixture.texture = None;
    fixture.write_to(dir.path());

    let package_dir = dir.path().join("hero.a2dpack");
    let text = capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));
    assert!(text.contains("Loaded with warnings:"), "{text}");
    assert!(text.contains("hero.png"), "{text}");

    // And the package still validates — reporting the gap, not hiding it.
    let text = capture(|out| a2d_cli::validate(out, &package_dir).map(|_| ()).map_err(|e| e.to_string()));
    assert!(text.contains("texture page"), "{text}");
}

#[test]
fn validate_reports_a_package_with_warnings_via_its_return_value() {
    let dir = temp_dir("validate-status");
    let mut fixture = Fixture::spine_json();
    fixture.texture = None;
    fixture.write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");
    capture(|out| a2d_cli::import(out, dir.path(), &package_dir, None).map_err(|e| e.to_string()));

    let mut out = Vec::new();
    let clean = a2d_cli::validate(&mut out, &package_dir).unwrap();
    assert!(!clean, "a package with a missing texture should not report clean");
}

#[test]
fn a_directory_with_no_skeleton_is_a_missing_skeleton_error() {
    let dir = temp_dir("empty");
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(dir.path().join("readme.txt"), "nothing to see").unwrap();

    let mut out = Vec::new();
    let err = a2d_cli::inspect(&mut out, dir.path(), None).unwrap_err();
    assert!(err.to_string().contains("missing skeleton"), "{err}");
}

#[test]
fn two_atlases_with_no_stem_match_are_ambiguous_rather_than_guessed() {
    let dir = temp_dir("ambiguous");
    let mut fixture = Fixture::spine_json();
    fixture.skeleton_name = "skeleton.json".into();
    fixture.write_to(dir.path());
    // A second, unrelated atlas.
    std::fs::write(dir.path().join("other.atlas"), Fixture::atlas_text("other.png")).unwrap();

    let mut out = Vec::new();
    let err = a2d_cli::inspect(&mut out, dir.path(), None).unwrap_err();
    assert!(err.to_string().contains("ambiguous"), "{err}");
}

#[test]
fn an_unknown_importer_name_is_refused() {
    let dir = temp_dir("bad-game");
    Fixture::spine_json().write_to(dir.path());
    let mut out = Vec::new();
    let err = a2d_cli::inspect(&mut out, dir.path(), Some("genshin")).unwrap_err();
    assert!(err.to_string().contains("genshin"), "{err}");
}

#[test]
fn aeons_echo_style_suffixes_are_handled() {
    // `.skel.bytes` / `.atlas.txt`, which is what AEONS ECHO ships (spec §9.2).
    let dir = temp_dir("aeons");
    let mut fixture = Fixture::spine_json();
    fixture.skeleton_name = "hero.skel.bytes".into();
    fixture.atlas_name = "hero.atlas.txt".into();
    fixture.write_to(dir.path());

    let text = capture(|out| a2d_cli::inspect(out, dir.path(), Some("aeons_echo")).map_err(|e| e.to_string()));
    assert!(text.contains("Importer: aeons_echo"), "{text}");
    assert!(text.contains("Character: hero"), "{text}");
    assert!(text.contains("Loaded cleanly."), "{text}");
}

#[test]
fn the_unimplemented_importers_explain_themselves() {
    let dir = temp_dir("stubs");
    Fixture::spine_json().write_to(dir.path());
    for (game, expected) in [("nikke", "--game generic"), ("depose_girls", "Cubism Core")] {
        let mut out = Vec::new();
        let err = a2d_cli::inspect(&mut out, dir.path(), Some(game)).unwrap_err();
        assert!(err.to_string().contains(expected), "for {game}: {err}");
    }
}

/// Sanity check that the fixture's PNG is a real one, not just a header.
#[test]
fn the_fixture_png_is_a_valid_png() {
    let bytes = Fixture::png(64, 64);
    assert_eq!(a2d_import::classify(&bytes), a2d_import::AssetKind::Texture("png"));
    assert_eq!(a2d_import::generic::png_size(&bytes), Some((64, 64)));
    // Every chunk's CRC must check out.
    assert!(png_chunks_are_valid(&bytes), "fixture PNG has a bad chunk CRC");
}
