//! Synthetic test fixtures.
//!
//! Shared by every integration test binary via `#[path]`. Each of them uses a
//! different subset, so unused items here are expected rather than dead.
#![allow(dead_code)]

//!
//! Everything here is hand-authored. Spec §11 forbids committing extracted game
//! assets, so the pipeline is exercised against a minimal model that is built
//! from scratch — including a genuinely valid PNG, so that texture handling is
//! tested rather than faked.

use std::path::{Path, PathBuf};

/// A directory that is removed when the test finishes.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(path: PathBuf) -> TempDir {
        // A leftover directory from a previous crashed run would poison the
        // test, so start from a clean slate.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir should be creatable");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A synthetic Spine character on disk.
pub struct Fixture {
    pub skeleton_name: String,
    pub skeleton: String,
    pub atlas_name: String,
    pub atlas: String,
    /// The texture page, or `None` to leave it missing on purpose.
    pub texture: Option<(String, Vec<u8>)>,
}

impl Fixture {
    /// A three-bone, two-slot character with a region and a weighted mesh, two
    /// animations, and one texture page.
    pub fn spine_json() -> Fixture {
        Fixture {
            skeleton_name: "hero.json".into(),
            skeleton: SKELETON_JSON.to_string(),
            atlas_name: "hero.atlas".into(),
            atlas: Fixture::atlas_text("hero.png"),
            texture: Some(("hero.png".into(), Fixture::png(64, 64))),
        }
    }

    /// A two-region atlas for `page`.
    pub fn atlas_text(page: &str) -> String {
        format!(
            "\n{page}\n\
             size: 64,64\n\
             format: RGBA8888\n\
             filter: Linear,Linear\n\
             repeat: none\n\
             body\n\
             \x20 rotate: false\n\
             \x20 xy: 0, 0\n\
             \x20 size: 32, 32\n\
             \x20 orig: 32, 32\n\
             \x20 offset: 0, 0\n\
             \x20 index: -1\n\
             head\n\
             \x20 rotate: false\n\
             \x20 xy: 32, 0\n\
             \x20 size: 16, 16\n\
             \x20 orig: 16, 16\n\
             \x20 offset: 0, 0\n\
             \x20 index: -1\n"
        )
    }

    pub fn write_to(&self, dir: &Path) {
        std::fs::create_dir_all(dir).expect("fixture dir should be creatable");
        std::fs::write(dir.join(&self.skeleton_name), &self.skeleton).expect("skeleton should be writable");
        std::fs::write(dir.join(&self.atlas_name), &self.atlas).expect("atlas should be writable");
        if let Some((name, bytes)) = &self.texture {
            std::fs::write(dir.join(name), bytes).expect("texture should be writable");
        }
    }

    /// Builds a valid 8-bit greyscale PNG of the given size.
    ///
    /// The image data uses stored (uncompressed) deflate blocks, which is a
    /// legal zlib stream and avoids pulling in a compression dependency.
    pub fn png(width: u32, height: u32) -> Vec<u8> {
        let mut raw = Vec::with_capacity((height * (width + 1)) as usize);
        for y in 0..height {
            raw.push(0u8); // filter: none
            for x in 0..width {
                // A simple gradient, so the bytes are not all identical.
                raw.push(((x + y) % 256) as u8);
            }
        }

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // depth 8, greyscale, no interlace
        write_chunk(&mut png, b"IHDR", &ihdr);
        write_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
        write_chunk(&mut png, b"IEND", &[]);
        png
    }
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Wraps `data` in a zlib stream made of stored deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CM=deflate, no preset dict, fastest
    let mut chunks = data.chunks(0xffff).peekable();
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none())); // BFINAL on the last block
        out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            // Reflected IEEE 802.3 polynomial.
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Walks a PNG's chunks and checks every CRC.
pub fn png_chunks_are_valid(bytes: &[u8]) -> bool {
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return false;
    }
    let mut at = 8usize;
    let mut saw_end = false;
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let Some(body_end) = at.checked_add(8).and_then(|s| s.checked_add(len)) else { return false };
        if body_end + 4 > bytes.len() {
            return false;
        }
        let expected =
            u32::from_be_bytes([bytes[body_end], bytes[body_end + 1], bytes[body_end + 2], bytes[body_end + 3]]);
        if crc32(&bytes[at + 4..body_end]) != expected {
            return false;
        }
        if &bytes[at + 4..at + 8] == b"IEND" {
            saw_end = true;
        }
        at = body_end + 4;
    }
    saw_end && at == bytes.len()
}

/// A Spine 3.8 JSON skeleton: three bones, two slots, a region attachment, a
/// weighted mesh, and two animations covering several timeline types.
const SKELETON_JSON: &str = r#"{
  "skeleton": { "hash": "TESTHASH123", "spine": "3.8.99", "x": -16, "y": 0, "width": 32, "height": 48,
                "fps": 30, "images": "./images/" },
  "bones": [
    { "name": "root" },
    { "name": "torso", "parent": "root", "y": 8, "length": 20 },
    { "name": "head", "parent": "torso", "y": 20, "length": 10 }
  ],
  "slots": [
    { "name": "body", "bone": "torso", "attachment": "body" },
    { "name": "head", "bone": "head", "attachment": "head", "color": "ffffffff" }
  ],
  "skins": [
    { "name": "default", "attachments": {
        "body": { "body": {
          "type": "mesh",
          "uvs": [0, 0, 1, 0, 1, 1, 0, 1],
          "triangles": [0, 1, 2, 2, 3, 0],
          "vertices": [
            1, 1, -16, -16, 1,
            1, 1, 16, -16, 1,
            2, 1, 16, 16, 0.5, 2, 16, -4, 0.5,
            2, 1, -16, 16, 0.5, 2, -16, -4, 0.5
          ],
          "hull": 4, "width": 32, "height": 32
        } },
        "head": { "head": { "width": 16, "height": 16 } }
    } }
  ],
  "events": { "footstep": { "int": 1, "string": "left" } },
  "animations": {
    "idle": {
      "bones": {
        "torso": { "rotate": [ { "time": 0, "angle": 0 }, { "time": 0.5, "angle": 5, "curve": "stepped" },
                               { "time": 1, "angle": 0 } ] },
        "head": { "translate": [ { "time": 0, "x": 0, "y": 0 }, { "time": 1, "x": 0, "y": 2 } ] }
      },
      "slots": { "head": { "color": [ { "time": 0, "color": "ffffffff" },
                                      { "time": 1, "color": "ffffff80" } ] } },
      "deform": { "default": { "body": { "body": [
        { "time": 0 },
        { "time": 1, "offset": 2, "vertices": [1, 1] }
      ] } } },
      "events": [ { "time": 0.25, "name": "footstep" } ]
    },
    "walk": {
      "bones": { "torso": { "translate": [ { "time": 0, "x": 0 }, { "time": 0.5, "x": 4 } ] } },
      "drawOrder": [ { "time": 0, "offsets": [ { "slot": "head", "offset": -1 } ] } ]
    }
  }
}
"#;

/// A synthetic Cubism package: one triangle that slides across the canvas as
/// `ParamAngleX` goes from -30 to 30, and optionally a motion ("idle", two
/// seconds) that sweeps the parameter over that range.
///
/// Built straight as IR rather than from MOC3 bytes, so it exercises the package
/// path (the one a viewer loads) without any game asset.
pub fn cubism_package(with_motion: bool) -> a2d_pack::Package {
    use a2d_core::ir::cubism::{
        Canvas, CubismIr, CurveKey, Drawable, KeyformBinding, Keyforms, Motion, MotionCurve, Parameter,
        ParameterBinding,
    };

    let mut ir = CubismIr {
        canvas: Canvas { pixels_per_unit: 100.0, origin: (100.0, 100.0), size: (200.0, 200.0) },
        parameters: vec![Parameter { id: "ParamAngleX".into(), minimum: -30.0, maximum: 30.0, default: 0.0 }],
        part_ids: vec![],
        deformer_ids: vec![],
        drawable_ids: vec!["ArtMesh1".into()],
        glue_ids: vec![],
        glues: vec![],
        glue_intensities: vec![],
        drawables: vec![Drawable {
            id: "ArtMesh1".into(),
            parent_deformer: None,
            uvs: vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)],
            indices: vec![0, 1, 2],
            keyform_begin: 0,
            keyform_count: 2,
            keyform_binding: 0,
            masks: vec![],
            flags: 0,
            part: None,
            texture: 0,
        }],
        parameter_bindings: vec![ParameterBinding { parameter: 0, keys: vec![-30.0, 30.0] }],
        keyform_bindings: vec![KeyformBinding { axes: vec![0] }],
        keyforms: Keyforms {
            // Two keyforms of three vertices: the triangle on the left, then on the right.
            positions: vec![-0.9, -0.5, -0.1, -0.5, -0.9, 0.5, 0.1, -0.5, 0.9, -0.5, 0.1, 0.5],
            warp_offsets: vec![],
            drawable_offsets: vec![0, 6],
        },
        draw_order: vec![0],
        drawable_keyform_opacities: vec![1.0, 1.0],
        warp_keyform_opacities: vec![],
        physics: None,
        drawable_keyform_draw_orders: vec![500.0, 500.0],
        ..CubismIr::default()
    };
    if with_motion {
        let key = |time: f32, value: f32| CurveKey { time, value, in_slope: 30.0, out_slope: 30.0, stepped: false };
        ir.motions = vec![Motion {
            name: "idle".into(),
            duration: 2.0,
            curves: vec![MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![key(0.0, -30.0), key(2.0, 30.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        }];
    }

    let mut package = a2d_pack::Package::from_cubism(ir, "synthetic", "cubism-moc3-v3");
    package.manifest.textures.push(a2d_pack::TextureEntry {
        file: "texture_00.png".into(),
        size: Some([64, 64]),
        premultiplied_alpha: false,
    });
    package.textures.push(a2d_pack::TextureFile { file: "texture_00.png".into(), bytes: Fixture::png(64, 64) });
    package
}

/// A headless GPU, or `None` to skip. `A2D_REQUIRE_GPU` turns the skip into a
/// failure, for machines that should have an adapter.
pub fn gpu() -> Option<a2d_render::GpuContext> {
    match a2d_render::GpuContext::headless() {
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

/// Fraction of pixels with any coverage.
pub fn coverage(image: &a2d_render::Rgba8Image) -> f32 {
    let covered = image.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
    covered as f32 / (image.width * image.height) as f32
}

/// Records `image` as `dir/name` if there is no baseline yet, otherwise asserts
/// it matches the stored one within `tolerance` (0-255 channel units).
pub fn check_baseline(dir: &Path, name: &str, image: &a2d_render::Rgba8Image, tolerance: u8) {
    std::fs::create_dir_all(dir).expect("baseline directory should be creatable");
    let path = dir.join(name);
    if !path.exists() {
        std::fs::write(&path, a2d_render::encode_png(image).expect("encode")).expect("write baseline");
        eprintln!("recorded baseline {}", path.display());
        return;
    }
    let stored =
        a2d_render::decode_png(&std::fs::read(&path).expect("read baseline"), "baseline").expect("baseline decodes");
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
        diff.within(tolerance),
        "{name} differs from {}: {diff}
delete the baseline to re-record it",
        path.display()
    );
}

/// A synthetic Cubism package built to break visibly if deformation regresses.
///
/// Two triangles in a 2 x 2 unit canvas (the descriptions below are as drawn, y up,
/// i.e. after the runtime flips the file's y-down positions), driven by two parameters through a
/// 2-key keyform blend each:
///
/// * `A` (lower half) slides right as `ParamAngleX` goes -30 -> 30: its x
///   offset is `(p + 30) / 60`. The motion eases `ParamAngleX` with a Hermite
///   segment (zero slopes) from -30 at 0 s to 30 at 1 s, so the value at
///   0.25 s is -20.625, not the linear -15.
/// * `B` (upper half) rises by `0.3 * (p + 30) / 60` as `ParamAngleY` goes
///   -30 -> 30. Its curve is a stepped key at 0 s (-30) then 30 at 0.75 s, so
///   it holds still through 0.5 s and has jumped by 1.0 s.
///
/// Motion "idle" lasts two seconds.
pub fn cubism_deformation_package() -> a2d_pack::Package {
    use a2d_core::ir::cubism::{
        Canvas, CubismIr, CurveKey, Drawable, KeyformBinding, Keyforms, Motion, MotionCurve, Parameter,
        ParameterBinding,
    };

    let drawable = |id: &str, begin: u32, binding: u32| Drawable {
        id: id.into(),
        parent_deformer: None,
        uvs: vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)],
        indices: vec![0, 1, 2],
        keyform_begin: begin,
        keyform_count: 2,
        keyform_binding: binding,
        masks: vec![],
        flags: 0,
        part: None,
        texture: 0,
    };
    let param = |id: &str| Parameter { id: id.into(), minimum: -30.0, maximum: 30.0, default: 0.0 };

    let mut ir = CubismIr {
        canvas: Canvas { pixels_per_unit: 100.0, origin: (100.0, 100.0), size: (200.0, 200.0) },
        parameters: vec![param("ParamAngleX"), param("ParamAngleY")],
        part_ids: vec![],
        deformer_ids: vec![],
        drawable_ids: vec!["A".into(), "B".into()],
        glue_ids: vec![],
        glues: vec![],
        glue_intensities: vec![],
        drawables: vec![drawable("A", 0, 0), drawable("B", 2, 1)],
        parameter_bindings: vec![
            ParameterBinding { parameter: 0, keys: vec![-30.0, 30.0] },
            ParameterBinding { parameter: 1, keys: vec![-30.0, 30.0] },
        ],
        keyform_bindings: vec![KeyformBinding { axes: vec![0] }, KeyformBinding { axes: vec![1] }],
        keyforms: Keyforms {
            positions: vec![
                // MOC3 y runs down the canvas and the runtime flips it, so a
                // positive y here is drawn in the lower half.
                // A at -30: left, lower half. A at +30: the same shape 1.0 to the right.
                -0.9, 0.8, -0.1, 0.8, -0.9, 0.2, //
                0.1, 0.8, 0.9, 0.8, 0.1, 0.2, //
                // B at -30: upper half. B at +30: the same shape 0.3 higher.
                -0.4, -0.1, 0.4, -0.1, 0.0, -0.5, //
                -0.4, -0.4, 0.4, -0.4, 0.0, -0.8,
            ],
            warp_offsets: vec![],
            drawable_offsets: vec![0, 6, 12, 18],
        },
        draw_order: vec![0, 1],
        drawable_keyform_opacities: vec![1.0; 4],
        warp_keyform_opacities: vec![],
        physics: None,
        drawable_keyform_draw_orders: vec![500.0; 4],
        ..CubismIr::default()
    };
    let key = |time: f32, value: f32, slope: f32, stepped: bool| CurveKey {
        time,
        value,
        in_slope: slope,
        out_slope: slope,
        stepped,
    };
    ir.motions = vec![Motion {
        name: "idle".into(),
        duration: 2.0,
        curves: vec![
            MotionCurve {
                target: "ParamAngleX".into(),
                keys: vec![key(0.0, -30.0, 0.0, false), key(1.0, 30.0, 0.0, false)],
                ..MotionCurve::default()
            },
            MotionCurve {
                target: "ParamAngleY".into(),
                keys: vec![key(0.0, -30.0, 0.0, true), key(0.75, 30.0, 0.0, false)],
                ..MotionCurve::default()
            },
        ],
        ..Motion::default()
    }];

    let mut package = a2d_pack::Package::from_cubism(ir, "synthetic-deform", "cubism-moc3-v3");
    package.manifest.textures.push(a2d_pack::TextureEntry {
        file: "texture_00.png".into(),
        size: Some([64, 64]),
        premultiplied_alpha: false,
    });
    package.textures.push(a2d_pack::TextureFile { file: "texture_00.png".into(), bytes: Fixture::png(64, 64) });
    package
}
