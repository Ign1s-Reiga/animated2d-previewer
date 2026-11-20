//! `preview -o` on a Cubism package: one frame per regression timestamp of the
//! default motion, or a single rest-pose frame when there is none.
//!
//! The package is synthetic (see `support::cubism_package`); no game asset is
//! involved. Skips when no GPU adapter exists, unless `A2D_REQUIRE_GPU` is set.

use std::sync::atomic::{AtomicU32, Ordering};

use a2d_cli::{preview, CliError, ViewerFlags, REGRESSION_TIMESTAMPS};
use a2d_render::GpuContext;

#[path = "support/mod.rs"]
mod support;

use support::{cubism_package, TempDir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempDir::new(std::env::temp_dir().join(format!("a2d-cubprev-{}-{}-{}", label, std::process::id(), n)))
}

fn has_gpu() -> bool {
    match GpuContext::headless() {
        Ok(_) => true,
        Err(e) => {
            if std::env::var("A2D_REQUIRE_GPU").is_ok() {
                panic!("A2D_REQUIRE_GPU is set, but no GPU adapter is available: {e}");
            }
            eprintln!("skipping: {e}");
            false
        }
    }
}

fn export(with_motion: bool, label: &str) -> Option<(TempDir, String)> {
    export_package(cubism_package(with_motion), label)
}

fn export_package(package: a2d_pack::Package, label: &str) -> Option<(TempDir, String)> {
    if !has_gpu() {
        return None;
    }
    let dir = temp_dir(label);
    let package_dir = dir.path().join("synthetic.a2dpack");
    package.write_to(&package_dir).expect("the package should write");
    let frames = dir.path().join("frames");
    let mut out = Vec::new();
    let result: Result<(), CliError> = preview(&mut out, &package_dir, Some(&frames), ViewerFlags::default());
    let text = String::from_utf8_lossy(&out).into_owned();
    result.unwrap_or_else(|e| panic!("preview -o failed: {e}\n{text}"));
    Some((dir, text))
}

#[test]
fn a_cubism_package_with_a_motion_writes_a_frame_per_timestamp() {
    let Some((dir, text)) = export(true, "motion") else { return };
    let frames = dir.path().join("frames");

    let mut hashes = Vec::new();
    for time in REGRESSION_TIMESTAMPS {
        let path = frames.join(format!("frame_{time:.2}.png"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{} should exist: {e}\n{text}", path.display()));
        assert!(bytes.starts_with(b"\x89PNG"), "{} is not a PNG", path.display());
        hashes.push(a2d_render::decode_png(&bytes, "frame").expect("frame should decode").fingerprint());
    }
    assert_eq!(hashes.len(), 4);
    assert!(text.contains("Animation: idle"), "{text}");
    // The triangle slides across the canvas, so the timestamps must differ.
    assert_ne!(hashes[0], hashes[3], "t=0 and t=1 drew the same picture: the motion was not played");
    assert!(!frames.join("pose.png").exists(), "a model with motions should not fall back to a rest pose");
}

#[test]
fn a_cubism_package_without_motions_keeps_one_rest_pose_frame() {
    let Some((dir, text)) = export(false, "rest") else { return };
    let frames = dir.path().join("frames");
    assert!(frames.join("pose.png").is_file(), "{text}");
    assert!(text.contains("no motions"), "the fallback should be announced:\n{text}");
    assert!(!frames.join("frame_0.00.png").exists());
}

#[test]
fn import_warnings_recorded_in_the_manifest_are_printed() {
    let mut package = cubism_package(true);
    package.manifest.import_warnings.push("a curve was dropped".into());
    let Some((_dir, text)) = export_package(package, "warn") else { return };
    assert!(text.contains("recorded at import: a curve was dropped"), "{text}");
}

#[test]
fn a_missing_first_page_previews_with_a_placeholder_and_says_so() {
    let mut package = cubism_package(true);
    // Page zero is listed but its file is gone; a second page exists.
    package.manifest.textures.insert(
        0,
        a2d_pack::TextureEntry { file: "gone.png".into(), size: Some([64, 64]), premultiplied_alpha: false },
    );
    let Some((dir, text)) = export_package(package, "missing-page") else { return };
    assert!(
        text.contains("gone.png") && text.contains("missing"),
        "the missing page should be noted:
{text}"
    );
    assert!(dir.path().join("frames").join("frame_0.00.png").is_file(), "{text}");
}
