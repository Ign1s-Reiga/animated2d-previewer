//! Visual regression tests for the Generic Cubism runtime family (spec §11).
//!
//! The Cubism counterpart of `visual.rs`: a synthetic package goes through the
//! package round-trip, the model, the renderer and read-back, at the fixed
//! timestamps `0.0 / 0.25 / 0.5 / 1.0`. See that file for why no pixel baseline
//! is committed; `A2D_BASELINE_DIR` opts in to one here too (files are prefixed
//! `cubism_`, so the two families can share a directory).
//!
//! The model comes from `support::cubism_deformation_package`, built so a
//! deformation regression moves pixels by an amount that can be computed by
//! hand:
//!
//! * triangle A slides by `(p + 30) / 60` as `ParamAngleX` goes -30 -> 30,
//!   eased by a Hermite segment. At 0.25 s the parameter is -20.625 (the
//!   linear answer would be -15), so a Hermite segment sampled linearly, or a lost blend, moves
//!   the triangle by about eleven pixels;
//! * triangle B rises by `0.3 * (p + 30) / 60` and is driven by a *stepped*
//!   curve, so it must not move at all until after 0.5 s.
//!
//! No game asset is involved. Skips without a GPU unless `A2D_REQUIRE_GPU`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use a2d_core::{AnimatedModel, RenderList};
use a2d_cubism::GenericCubismModel;
use a2d_render::{Camera, FrameSettings, GpuContext, OffscreenTarget, Renderer, Rgba8Image, SamplerConfig, Viewport};

#[path = "support/mod.rs"]
mod support;

use support::{coverage, gpu, TempDir};

const SIZE: u32 = 256;
const TIMESTAMPS: [f32; 4] = a2d_cli::REGRESSION_TIMESTAMPS;
const TOLERANCE: u8 = 2;
/// Slack for a centroid measured from pixels, in pixels.
const CENTROID_SLACK: f32 = 1.5;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempDir::new(std::env::temp_dir().join(format!("a2d-viscub-{}-{}-{}", label, std::process::id(), n)))
}

/// The model as a viewer would get it: written to a package directory and read
/// back, so serialisation is part of what is under test.
fn load_model(label: &str) -> (GenericCubismModel, a2d_pack::Package) {
    load_package(label, support::cubism_deformation_package())
}

fn load_package(label: &str, source: a2d_pack::Package) -> (GenericCubismModel, a2d_pack::Package) {
    let dir = temp_dir(label);
    let package_dir = dir.path().join("synthetic.a2dpack");
    source.write_to(&package_dir).expect("package should write");
    let package = a2d_pack::Package::read_from(&package_dir).expect("package should read");
    let ir = package.model.as_cubism().cloned().expect("fixture is a Cubism model");
    (GenericCubismModel::load(ir, "synthetic"), package)
}

struct Harness {
    gpu: GpuContext,
    renderer: Renderer,
    target: OffscreenTarget,
    model: GenericCubismModel,
    camera: Camera,
    viewport: Viewport,
}

impl Harness {
    fn build(gpu: GpuContext, label: &str) -> Harness {
        Harness::from_loaded(gpu, load_model(label))
    }

    fn from_loaded(gpu: GpuContext, loaded: (GenericCubismModel, a2d_pack::Package)) -> Harness {
        let (mut model, package) = loaded;
        let mut renderer = Renderer::new(gpu.clone());
        let entry = &package.manifest.textures[0];
        let file = package.textures.iter().find(|t| t.file == entry.file).expect("page should be present");
        let id = renderer
            .textures_mut()
            .upload_png(&gpu, &entry.file, &file.bytes, entry.premultiplied_alpha, SamplerConfig::default())
            .expect("page should upload");
        model.set_texture(id);

        let target = OffscreenTarget::new(&gpu, SIZE, SIZE).expect("target should be creatable");
        let viewport = Viewport::new(SIZE, SIZE);
        // Framed once, from the canvas, and then held.
        let camera = Camera::fit(model.bounds(), viewport, 0.1);
        Harness { gpu, renderer, target, model, camera, viewport }
    }

    fn frame(&mut self, time: f32) -> Rgba8Image {
        self.model.pose_at("idle", time).expect("idle should exist");
        let mut list = RenderList::new();
        self.model.emit(&mut list);
        self.renderer
            .render(self.target.view(), self.target.format(), FrameSettings::new(self.viewport, self.camera), &list)
            .expect("render should succeed");
        self.target.read_pixels(&self.gpu).expect("read-back should succeed")
    }

    /// The pixel at a point given in canvas units (y up).
    fn pixel_at(&self, image: &Rgba8Image, x: f32, y: f32) -> [u8; 4] {
        let ppu = self.camera.pixels_per_unit;
        let px = (SIZE as f32 * 0.5 + (x - self.camera.center.x) * ppu).floor() as u32;
        let py = (SIZE as f32 * 0.5 - (y - self.camera.center.y) * ppu).floor() as u32;
        let at = ((py * image.width + px) * 4) as usize;
        [image.pixels[at], image.pixels[at + 1], image.pixels[at + 2], image.pixels[at + 3]]
    }

    /// Centroid of the opaque pixels in rows `rows`, in canvas units with y up
    /// (so it compares directly with the keyform positions).
    fn centroid(&self, image: &Rgba8Image, rows: std::ops::Range<u32>) -> Option<(f32, f32)> {
        let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
        for y in rows {
            for x in 0..image.width {
                if image.pixels[((y * image.width + x) * 4 + 3) as usize] > 127 {
                    sx += x as f64 + 0.5;
                    sy += y as f64 + 0.5;
                    n += 1;
                }
            }
        }
        if n == 0 {
            return None;
        }
        let ppu = self.camera.pixels_per_unit;
        let (cx, cy) = (self.camera.center.x, self.camera.center.y);
        let px = (sx / n as f64) as f32;
        let py = (sy / n as f64) as f32;
        Some((cx + (px - SIZE as f32 * 0.5) / ppu, cy - (py - SIZE as f32 * 0.5) / ppu))
    }
}

/// Vertex mean of each triangle at its `-30` keyform.
const A_BASE_CENTROID: (f32, f32) = ((-0.9 - 0.1 - 0.9) / 3.0, (-0.8 - 0.8 - 0.2) / 3.0);
const B_BASE_CENTROID: (f32, f32) = (0.0, (0.1 + 0.1 + 0.5) / 3.0);

/// Hand-computed blend weight of each timestamp for A (Hermite, zero slopes,
/// -30 at 0 s to 30 at 1 s): `3s^2 - 2s^3` with `s = t`.
const A_WEIGHTS: [f32; 4] = [0.0, 0.15625, 0.5, 1.0];
/// Same for B (stepped until 0.75 s, then 30): moves only after 0.5 s.
const B_WEIGHTS: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

fn expected(i: usize) -> ((f32, f32), (f32, f32)) {
    ((A_BASE_CENTROID.0 + A_WEIGHTS[i], A_BASE_CENTROID.1), (B_BASE_CENTROID.0, B_BASE_CENTROID.1 + 0.3 * B_WEIGHTS[i]))
}

#[test]
fn posed_geometry_matches_the_hand_computed_blend() {
    // CPU-only: asserts the vertices the model emits, before any rasteriser
    // gets a say. Runs everywhere, GPU or not.
    let (mut model, _) = load_model("geometry");
    for (i, time) in TIMESTAMPS.iter().enumerate() {
        model.pose_at("idle", *time).expect("idle should exist");
        let mut list = RenderList::new();
        model.emit(&mut list);
        assert_eq!(list.meshes().len(), 2, "t={time}: both drawables should be emitted");

        let mean = |mesh: &a2d_core::RenderMesh| {
            let n = mesh.vertices.len() as f32;
            (mesh.vertices.iter().map(|v| v.x).sum::<f32>() / n, mesh.vertices.iter().map(|v| v.y).sum::<f32>() / n)
        };
        let (want_a, want_b) = expected(i);
        for (got, want, name) in [(mean(&list.meshes()[0]), want_a, "A"), (mean(&list.meshes()[1]), want_b, "B")] {
            assert!(
                (got.0 - want.0).abs() < 1e-4 && (got.1 - want.1).abs() < 1e-4,
                "t={time} triangle {name}: centroid {got:?}, expected {want:?}"
            );
        }
    }
}

#[test]
fn rendered_triangles_land_where_the_blend_puts_them() {
    // The same numbers as above, read back from pixels: the lower half of the
    // frame holds only A and the upper half only B.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "centroids");
    let slack = CENTROID_SLACK / harness.camera.pixels_per_unit;
    let half = SIZE / 2;

    for (i, time) in TIMESTAMPS.iter().enumerate() {
        let image = harness.frame(*time);
        let a = harness.centroid(&image, half..SIZE).unwrap_or_else(|| panic!("t={time}: A was not drawn"));
        let b = harness.centroid(&image, 0..half).unwrap_or_else(|| panic!("t={time}: B was not drawn"));
        let (want_a, want_b) = expected(i);
        for (got, want, name) in [(a, want_a, "A"), (b, want_b, "B")] {
            assert!(
                (got.0 - want.0).abs() < slack && (got.1 - want.1).abs() < slack,
                "t={time}: {name} at ({:.3}, {:.3}), expected {want:?} (+-{slack:.3})",
                got.0,
                got.1
            );
        }
    }
}

#[test]
fn the_model_is_actually_drawn_at_every_timestamp() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "drawn");
    for time in TIMESTAMPS {
        let covered = coverage(&harness.frame(time));
        assert!(covered > 0.02, "t={time}: almost nothing was drawn ({:.1}% covered)", covered * 100.0);
        assert!(covered < 0.95, "t={time}: the frame is nearly full ({:.1}%)", covered * 100.0);
    }
}

#[test]
fn rendering_the_same_pose_twice_is_byte_identical() {
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
    let Some(dir) = std::env::var_os("A2D_BASELINE_DIR").map(PathBuf::from) else {
        eprintln!("skipping baseline comparison: set A2D_BASELINE_DIR to enable it");
        return;
    };
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::build(gpu, "baseline");
    for time in TIMESTAMPS {
        let image = harness.frame(time);
        support::check_baseline(&dir, &format!("cubism_idle_{time:.2}.png"), &image, TOLERANCE);
    }
}

// ---- animated draw order -------------------------------------------------
//
// Quad A is red, quad B is blue; they overlap on x -0.2..0.2. A's draw order
// is 400 + 200 * p for the parameter p, B's is 500, and the motion drives p
// linearly to 0 / 0.25 / 0.5 / 1.0 at the four fixed timestamps. By hand:
//
//   t     p      A      B    on top in the overlap
//   0.00  0.00   400    500  B (blue): A is behind
//   0.25  0.25   450    500  B (blue)
//   0.50  0.50   500    500  tie: whichever the object list names last
//   1.00  1.00   600    500  A (red): A is in front
//
// Both are opaque with opacity 1 and the page is not premultiplied, so a
// covered pixel is its texel exactly: red is [255, 0, 0, 255], blue
// [0, 0, 255, 255]. Outside the overlap the same holds for whichever quad is
// there, and outside both quads the frame stays transparent.
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const A_ONLY: (f32, f32) = (-0.5, 0.0);
const B_ONLY: (f32, f32) = (0.5, 0.0);
const OVERLAP: (f32, f32) = (0.0, 0.0);
const OUTSIDE: (f32, f32) = (0.0, 0.8);

fn draw_order_harness(gpu: GpuContext, label: &str, tie_order: Vec<u32>) -> Harness {
    Harness::from_loaded(gpu, load_package(label, support::cubism_draw_order_package(tie_order)))
}

#[test]
fn animated_draw_order_changes_which_mesh_is_emitted_first() {
    // CPU-only. Meshes come out back to front; A is the one whose vertices reach x = -0.8.
    let (mut model, _) = load_package("order-cpu", support::cubism_draw_order_package(vec![0, 1]));
    let first_is_a = |model: &mut GenericCubismModel, time: f32| {
        model.pose_at("idle", time).expect("idle should exist");
        let mut list = RenderList::new();
        model.emit(&mut list);
        assert_eq!(list.meshes().len(), 2, "t={time}");
        assert_eq!((list.meshes()[0].z_order, list.meshes()[1].z_order), (0, 1));
        let lowest_x = |m: &a2d_core::RenderMesh| m.vertices.iter().map(|v| v.x).fold(f32::MAX, f32::min);
        lowest_x(&list.meshes()[0]) < -0.7
    };
    // A first (behind) until its order passes B's; the tie at 0.5 goes to the list [A, B].
    assert!(first_is_a(&mut model, 0.0));
    assert!(first_is_a(&mut model, 0.25));
    assert!(first_is_a(&mut model, 0.5));
    assert!(!first_is_a(&mut model, 1.0));
}

#[test]
fn the_overlap_flips_from_one_quad_to_the_other_as_the_draw_order_crosses() {
    let Some(gpu) = gpu() else { return };
    let mut harness = draw_order_harness(gpu, "order-flip", vec![0, 1]);
    // The tie at 0.5 is settled by the list [A, B]: A is painted first, B on top.
    let expected = [(0.0, BLUE), (0.25, BLUE), (0.5, BLUE), (1.0, RED)];
    for (time, want) in expected {
        let image = harness.frame(time);
        // The rest of the frame does not move: a flip must come from order alone.
        assert_eq!(harness.pixel_at(&image, A_ONLY.0, A_ONLY.1), RED, "t={time}: A alone");
        assert_eq!(harness.pixel_at(&image, B_ONLY.0, B_ONLY.1), BLUE, "t={time}: B alone");
        assert_eq!(harness.pixel_at(&image, OUTSIDE.0, OUTSIDE.1)[3], 0, "t={time}: outside both quads");
        assert_eq!(harness.pixel_at(&image, OVERLAP.0, OVERLAP.1), want, "t={time}: overlap");
    }
}

#[test]
fn a_tie_in_draw_order_is_settled_by_the_object_list_when_rendered() {
    let Some(gpu) = gpu() else { return };
    // At 0.5 s both are 500. With the list [B, A], A is painted last and is on top.
    let mut harness = draw_order_harness(gpu, "order-tie", vec![1, 0]);
    let image = harness.frame(0.5);
    assert_eq!(harness.pixel_at(&image, OVERLAP.0, OVERLAP.1), RED);
    // Away from the tie the list does not matter: at 0 s A (400) is still behind B.
    let image = harness.frame(0.0);
    assert_eq!(harness.pixel_at(&image, OVERLAP.0, OVERLAP.1), BLUE);
}

#[test]
fn a_mirrored_model_renders_as_the_horizontally_flipped_image() {
    let Some(gpu) = gpu() else { return };
    let mut plain = Harness::build(gpu.clone(), "mirror-plain");
    let mut mirrored = Harness::build(gpu, "mirror-flipped");
    // The frame is symmetric about the origin, so the same camera serves both.
    assert_eq!(plain.camera.center.x, 0.0);
    mirrored.model.set_scale(a2d_core::Vec2::new(-1.0, 1.0));
    for time in TIMESTAMPS {
        let a = plain.frame(time);
        let b = mirrored.frame(time);
        let (w, h) = (a.width as usize, a.height as usize);
        let mut flipped = a.clone();
        for y in 0..h {
            for x in 0..w {
                let from = (y * w + (w - 1 - x)) * 4;
                let to = (y * w + x) * 4;
                flipped.pixels[to..to + 4].copy_from_slice(&a.pixels[from..from + 4]);
            }
        }
        let diff = flipped.diff(&b).expect("same size");
        // Rasterisation rounds the two sides differently along edges, so a few
        // edge pixels may differ; the bulk must agree.
        assert!(diff.differing_pixels * 100 < diff.total_pixels, "t={time}: {diff}");
        assert!(coverage(&b) > 0.02, "t={time}: the mirrored model drew nothing");
    }
}
