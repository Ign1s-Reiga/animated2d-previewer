//! `animated2d inspect` against Unity bundles.
//!
//! The synthetic cases run everywhere and cover the shape of the output and the
//! failure paths. The real-asset case is `#[ignore]` and reads
//! `A2D_FIXTURE_CUBISM`, because game assets are never committed (spec §11):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test unity_inspect -- --ignored
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

#[path = "support/mod.rs"]
mod support;

use support::TempDir;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempDir::new(std::env::temp_dir().join(format!("a2d-unity-{}-{}-{}", label, std::process::id(), n)))
}

fn capture(f: impl FnOnce(&mut Vec<u8>) -> Result<(), String>) -> String {
    let mut out = Vec::new();
    match f(&mut out) {
        Ok(()) => String::from_utf8_lossy(&out).into_owned(),
        Err(e) => format!("{}\nERROR: {e}", String::from_utf8_lossy(&out)),
    }
}

/// A minimal stored `UnityFS` bundle wrapping `payload` as one serialized node.
fn bundle(payload: &[u8]) -> Vec<u8> {
    let mut info = Vec::new();
    info.extend_from_slice(&[0u8; 16]);
    info.extend_from_slice(&1i32.to_be_bytes());
    info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    info.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    info.extend_from_slice(&0u16.to_be_bytes());
    info.extend_from_slice(&1i32.to_be_bytes());
    info.extend_from_slice(&0u64.to_be_bytes());
    info.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    info.extend_from_slice(&4u32.to_be_bytes());
    info.extend_from_slice(b"CAB-test");
    info.push(0);

    let mut out = Vec::new();
    out.extend_from_slice(b"UnityFS\0");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(b"5.x.x\0");
    out.extend_from_slice(b"2022.3.20p1\0");
    let size_at = out.len();
    out.extend_from_slice(&0i64.to_be_bytes());
    out.extend_from_slice(&(info.len() as u32).to_be_bytes());
    out.extend_from_slice(&(info.len() as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&info);
    out.extend_from_slice(payload);
    let total = out.len() as i64;
    out[size_at..size_at + 8].copy_from_slice(&total.to_be_bytes());
    out
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("fixture should be writable");
    path
}

#[test]
fn a_bundle_is_recognised_and_opened_rather_than_listed_as_a_file() {
    // A bundle holds a model, not files on disk, so `inspect` opens it instead
    // of reporting one opaque entry.
    let dir = temp_dir("recognised");
    // A serialized file this short cannot parse; what matters here is that the
    // container was recognised and the failure names the inside, not the file.
    let path = write(&dir, "thing_prefab", &bundle(b"not a serialized file"));
    let text = capture(|out| a2d_cli::inspect(out, &path, None).map_err(|e| e.to_string()));
    assert!(text.contains("Input:"), "{text}");
    assert!(!text.contains("Importer: generic"), "a bundle must not be guessed as a Spine folder:\n{text}");
    assert!(text.contains("ERROR"), "an unreadable serialized file should fail loudly:\n{text}");
}

#[test]
fn a_bundle_holding_no_serialized_file_says_so() {
    let dir = temp_dir("no-serialized");
    let mut bytes = bundle(b"resource payload");
    // Clear the serialized-file flag on the only node, leaving a resource blob.
    let at = bytes.windows(8).position(|w| w == b"CAB-test").expect("node path");
    bytes[at - 4..at].copy_from_slice(&0u32.to_be_bytes());
    let path = write(&dir, "resources_prefab", &bytes);
    let text = capture(|out| a2d_cli::inspect(out, &path, None).map_err(|e| e.to_string()));
    assert!(text.contains("no serialized file"), "{text}");
}

#[test]
fn a_file_that_is_not_a_bundle_still_takes_the_spine_path() {
    // The bundle branch keys off content, so a Spine skeleton must be unaffected.
    let dir = temp_dir("not-a-bundle");
    let path = write(&dir, "hero.json", br#"{"skeleton":{"spine":"3.8.99"},"bones":[{"name":"root"}]}"#);
    let text = capture(|out| a2d_cli::inspect(out, &path, None).map_err(|e| e.to_string()));
    assert!(text.contains("Importer:"), "{text}");
    assert!(!text.contains("Unity bundle"), "{text}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_cubism_bundle_yields_the_inventory_the_spec_asks_for() {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return };
    let path = PathBuf::from(path);
    let text = capture(|out| a2d_cli::inspect(out, &path, None).map_err(|e| e.to_string()));
    assert!(!text.contains("ERROR"), "{text}");

    // Spec §12 names what the inventory must identify.
    for expected in [
        "Importer: unity_cubism",
        "Unity bundle",
        "built with:",   // Unity version
        "Cubism model:", // the MOC object
        "moc3:",         // the MOC3 payload
        "parameters:",
        "parts:",
        "drawables:",
        "GameObjects", // prefab hierarchy
        "textures:",   // Texture2D assets
        "motions:",    // AnimationClip names
        "animator controllers:",
        "original motion sources:", // fade motion data, and the authored paths
    ] {
        assert!(text.contains(expected), "the inventory is missing {expected:?}:\n{text}");
    }

    // The payload must be a real MOC3, not merely a byte range that parsed.
    assert!(text.contains("format version"), "{text}");
    println!("{text}");
}
