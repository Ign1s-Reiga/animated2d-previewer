//! Visual regression tests for the Generic Spine runtime family (spec §17.3).
//!
//! These render the fixed timestamps `0.0 / 0.25 / 0.5 / 1.0` through the whole
//! stack — source assets, importer, IR, runtime, renderer — and check the
//! pixels. Subtle deformation regressions are exactly what unit tests miss and
//! what this catches.
//!
//! # Why there is no committed pixel baseline
//!
//! Rasterisation differs by a least significant bit between GPUs and driver
//! versions. A baseline committed from one machine would fail on every other
//! one, and a test that fails for reasons unrelated to the change tells you
//! nothing. So the always-on assertions are the properties that hold on *any*
//! correct renderer:
//!
//! * rendering is deterministic — the same pose twice is byte-identical;
//! * the animation actually moves — distinct timestamps produce distinct frames;
//! * the character is actually drawn — pixels are covered, in the right place.
//!
//! To pin exact pixels on your own machine, point `A2D_BASELINE_DIR` at a
//! directory. The first run writes the baselines; later runs compare against
//! them with a small per-channel tolerance. See `tests/README.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use a2d_core::{AnimatedModel, RenderList};
use a2d_render::{Camera, FrameSettings, GpuContext, OffscreenTarget, Renderer, Rgba8Image, SamplerConfig, Viewport};
use a2d_runtime::GenericSpineModel;

#[path = "support/mod.rs"]
mod support;

use support::{Fixture, TempDir};

const SIZE: u32 = 256;
const TIMESTAMPS: [f32; 4] = a2d_cli::REGRESSION_TIMESTAMPS;
/// Tolerance for a baseline comparison, in 0-255 channel units. Wide enough to
/// absorb a driver's rounding, narrow enough that real movement fails it.
const TOLERANCE: u8 = 2;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempDir::new(std::env::temp_dir().join(format!("a2d-vis-{}-{}-{}", label, std::process::id(), n)))
}

fn gpu() -> Option<GpuContext> {
    match GpuContext::headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var("A2D_REQUIRE_GPU").is_ok() {
                panic!("A2D_REQUIRE_GPU is set, but no GPU adapter is available: {e}");
            }
            eprintln!("skipping visual regression test: {e}");
            None
        }
    }
}

/// Everything needed to render one character, built from a synthetic fixture.
struct Harness {
    gpu: GpuContext,
    renderer: Renderer,
    target: OffscreenTarget,
    model: GenericSpineModel,
    camera: Camera,
    viewport: Viewport,
    _dir: TempDir,
}

impl Harness {
    fn build(gpu: GpuContext, label: &str) -> Harness {
        let dir = temp_dir(label);
        Fixture::spine_json().write_to(dir.path());
        let package_dir = dir.path().join("hero.a2dpack");

        let mut sink = Vec::new();
        a2d_cli::import(&mut sink, dir.path(), &package_dir, None).expect("fixture should import");
        let package = a2d_pack::Package::read_from(&package_dir).expect("package should read");
        let ir = Arc::new(package.model.as_spine().cloned().expect("fixture is a Spine model"));

        let mut renderer = Renderer::new(gpu.clone());
        for page in &ir.atlas.pages {
            let file = package.textures.iter().find(|t| t.file == page.name).expect("page should be present");
            let image = a2d_render::decode_png(&file.bytes, &page.name).expect("page should decode");
            renderer
                .textures_mut()
                .upload(&gpu, &page.name, &image, page.premultiplied_alpha, SamplerConfig::default())
                .expect("page should upload");
        }

        let target = OffscreenTarget::new(&gpu, SIZE, SIZE).expect("target should be creatable");
        let viewport = Viewport::new(SIZE, SIZE);
        let mut model = GenericSpineModel::load(ir, "hero");
        // Framed once from the setup pose and then held. Refitting per frame
        // would cancel out the very movement being measured.
        model.pose_at("idle", 0.0).expect("idle should exist");
        let camera = Camera::fit(model.bounds(), viewport, 0.1);

        Harness { gpu, renderer, target, model, camera, viewport, _dir: dir }
    }

    /// Poses at `time` and renders, returning the pixels.
    fn frame(&mut self, time: f32) -> Rgba8Image {
        self.model.pose_at("idle", time).expect("idle should exist");
        let mut list = RenderList::new();
        self.model.emit(&mut list);
        self.renderer
            .render(self.target.view(), self.target.format(), FrameSettings::new(self.viewport, self.camera), &list)
            .expect("render should succeed");
        self.target.read_pixels(&self.gpu).expect("read-back should succeed")
    }
}

/// Fraction of pixels with any coverage.
fn coverage(image: &Rgba8Image) -> f32 {
    let covered = image.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
    covered as f32 / (image.width * image.height) as f32
}

#[test]
fn the_character_is_actually_drawn_at_every_timestamp() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "drawn");

    for time in TIMESTAMPS {
        let image = harness.frame(time);
        let covered = coverage(&image);
        assert!(covered > 0.02, "t={time}: almost nothing was drawn ({:.1}% covered)", covered * 100.0);
        assert!(
            covered < 0.95,
            "t={time}: the frame is nearly full ({:.1}%), which suggests a bad camera fit",
            covered * 100.0
        );
    }
}

#[test]
fn rendering_the_same_pose_twice_is_byte_identical() {
    // Without this, a fingerprint baseline would be meaningless.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "determinism");

    for time in TIMESTAMPS {
        let first = harness.frame(time);
        let second = harness.frame(time);
        let diff = first.diff(&second).expect("same size");
        assert!(diff.is_identical(), "t={time}: {diff}");
        assert_eq!(first.fingerprint(), second.fingerprint());
    }
}

#[test]
fn distinct_timestamps_produce_distinct_frames() {
    // The fixture rotates the torso, translates the head and deforms the body
    // mesh, so every one of these instants must look different. If they stop
    // differing, animation evaluation has silently gone flat.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "movement");

    let frames: Vec<Rgba8Image> = TIMESTAMPS.iter().map(|t| harness.frame(*t)).collect();
    for (i, a) in frames.iter().enumerate() {
        for (j, b) in frames.iter().enumerate().skip(i + 1) {
            let diff = a.diff(b).expect("same size");
            assert!(!diff.within(TOLERANCE), "t={} and t={} render the same: {diff}", TIMESTAMPS[i], TIMESTAMPS[j]);
        }
    }
}

#[test]
fn a_fresh_model_reproduces_the_same_frames() {
    // Rebuilding from the package must give identical pixels, which is what
    // makes the importer and the runtime jointly deterministic.
    let Some(gpu) = gpu() else { return };
    let mut first = Harness::build(gpu.clone(), "rebuild-a");
    let mut second = Harness::build(gpu, "rebuild-b");

    for time in TIMESTAMPS {
        let a = first.frame(time);
        let b = second.frame(time);
        assert_eq!(a.fingerprint(), b.fingerprint(), "t={time}: {}", a.diff(&b).expect("same size"));
    }
}

#[test]
fn scrubbing_backwards_lands_on_the_same_frame() {
    // `pose_at` must be a pure function of time, not of the sequence of calls.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "scrub");

    let forward = harness.frame(0.5);
    harness.frame(1.0);
    harness.frame(0.0);
    let backward = harness.frame(0.5);
    assert_eq!(forward.fingerprint(), backward.fingerprint());
}

#[test]
fn frames_match_a_stored_baseline_when_one_is_configured() {
    // Opt-in: pixel baselines are machine-specific, so they are not committed.
    let Some(dir) = std::env::var_os("A2D_BASELINE_DIR").map(PathBuf::from) else {
        eprintln!("skipping baseline comparison: set A2D_BASELINE_DIR to enable it");
        return;
    };
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "baseline");
    std::fs::create_dir_all(&dir).expect("baseline directory should be creatable");

    let mut written = 0;
    for time in TIMESTAMPS {
        let image = harness.frame(time);
        let path = dir.join(format!("hero_idle_{time:.2}.png"));

        if !path.exists() {
            std::fs::write(&path, a2d_render::encode_png(&image).expect("encode")).expect("write baseline");
            written += 1;
            continue;
        }

        let stored = a2d_render::decode_png(&std::fs::read(&path).expect("read baseline"), "baseline")
            .expect("baseline should decode");
        let diff = image.diff(&stored).unwrap_or_else(|| {
            panic!(
                "baseline {} is {}x{}, this render is {}x{}",
                path.display(),
                stored.width,
                stored.height,
                image.width,
                image.height
            )
        });
        assert!(
            diff.within(TOLERANCE),
            "t={time} differs from {}: {diff}\ndelete the baseline to re-record it",
            path.display()
        );
    }
    if written > 0 {
        eprintln!("recorded {written} baseline frame(s) in {}", dir.display());
    }
}

#[test]
fn preview_writes_a_png_per_timestamp() {
    let Some(_gpu) = gpu() else { return };
    let dir = temp_dir("frames");
    Fixture::spine_json().write_to(dir.path());
    let package_dir = dir.path().join("hero.a2dpack");
    let frames = dir.path().join("frames");

    let mut sink = Vec::new();
    a2d_cli::import(&mut sink, dir.path(), &package_dir, None).expect("fixture should import");

    let mut out = Vec::new();
    a2d_cli::preview(&mut out, &package_dir, Some(&frames), None).expect("preview should render");
    let text = String::from_utf8(out).expect("output should be UTF-8");

    for time in TIMESTAMPS {
        let path = frames.join(format!("frame_{time:.2}.png"));
        assert!(path.is_file(), "expected {} in:\n{text}", path.display());
        let bytes = std::fs::read(&path).expect("frame should read");
        let image = a2d_render::decode_png(&bytes, "frame").expect("frame should be a valid PNG");
        assert_eq!((image.width, image.height), (512, 512));
        assert!(coverage(&image) > 0.02, "t={time}: the written frame is blank");
    }
    assert!(Path::new(&frames).is_dir());
}
