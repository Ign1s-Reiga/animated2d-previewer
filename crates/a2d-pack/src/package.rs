//! Reading and writing `.a2dpack` directories.
//!
//! ```text
//! character.a2dpack/
//! ├─ manifest.json    human-readable index
//! ├─ model.bin        normalized IR, deterministic binary
//! ├─ textures/        texture pages, byte-for-byte as imported
//! ├─ animations/      motion files that live outside the IR (Cubism)
//! └─ metadata/        import report and other provenance
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use a2d_core::ir::cubism::CubismIr;
use a2d_core::ir::spine::SpineIr;
use a2d_core::{DecodeError, LoadReport};

use crate::bin_io::{Reader, Writer};
use crate::manifest::{AnimationEntry, Manifest, ModelType, TextureEntry, FORMAT_VERSION};
use crate::{cubism_io, spine_io};

/// `model.bin` magic. Present so a stray file is diagnosed rather than parsed.
pub const MODEL_MAGIC: &[u8; 4] = b"A2DM";

/// A texture page as stored, still in its original encoding.
///
/// The package never re-encodes images: decoding belongs to the renderer, and
/// re-encoding would break byte-level golden comparisons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureFile {
    pub file: String,
    pub bytes: Vec<u8>,
}

/// The normalized model a package carries.
#[derive(Debug, Clone, PartialEq)]
pub enum PackageModel {
    Spine(SpineIr),
    Cubism(CubismIr),
}

impl PackageModel {
    pub fn model_type(&self) -> ModelType {
        match self {
            PackageModel::Spine(_) => ModelType::Spine,
            PackageModel::Cubism(_) => ModelType::Cubism,
        }
    }

    pub fn as_spine(&self) -> Option<&SpineIr> {
        match self {
            PackageModel::Spine(ir) => Some(ir),
            _ => None,
        }
    }

    pub fn as_cubism(&self) -> Option<&CubismIr> {
        match self {
            PackageModel::Cubism(ir) => Some(ir),
            _ => None,
        }
    }
}

/// A whole package, in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub manifest: Manifest,
    pub model: PackageModel,
    pub textures: Vec<TextureFile>,
}

impl Package {
    /// Builds a package from a decoded Spine skeleton, filling in the manifest
    /// fields that can be derived from the model itself.
    pub fn from_spine(ir: SpineIr, display_name: impl Into<String>) -> Package {
        let mut manifest = Manifest::new(ModelType::Spine, display_name);
        manifest.source_format = source_format_of(&ir);
        manifest.animations =
            ir.animations.iter().map(|a| AnimationEntry { name: a.name.clone(), duration: a.duration }).collect();
        manifest.default_animation = default_animation_of(&ir);
        manifest.textures = ir
            .atlas
            .pages
            .iter()
            .map(|p| TextureEntry {
                file: p.name.clone(),
                size: p.size.map(|(w, h)| [w, h]),
                premultiplied_alpha: p.premultiplied_alpha,
            })
            .collect();
        Package { manifest, model: PackageModel::Spine(ir), textures: Vec::new() }
    }

    /// Builds a package from a decoded Cubism model.
    ///
    /// A Cubism model carries no animations of its own -- its motions live
    /// outside the MOC3 and are not decoded yet -- so the manifest lists none
    /// and names no default. That is the honest state rather than an empty
    /// placeholder: `inspect` and `validate` both report it.
    pub fn from_cubism(ir: CubismIr, display_name: impl Into<String>, source_format: impl Into<String>) -> Package {
        let mut manifest = Manifest::new(ModelType::Cubism, display_name);
        manifest.source_format = source_format.into();
        Package { manifest, model: PackageModel::Cubism(ir), textures: Vec::new() }
    }

    /// Encodes `model.bin`.
    pub fn encode_model(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(64 * 1024);
        w.raw(MODEL_MAGIC);
        w.u32(FORMAT_VERSION);
        match &self.model {
            PackageModel::Spine(ir) => {
                w.u8(0);
                spine_io::write(&mut w, ir);
            }
            PackageModel::Cubism(ir) => {
                w.u8(1);
                cubism_io::write(&mut w, ir);
            }
        }
        w.into_bytes()
    }

    /// Decodes `model.bin`.
    pub fn decode_model(bytes: &[u8]) -> Result<PackageModel, DecodeError> {
        if bytes.len() < 9 || &bytes[..4] != MODEL_MAGIC {
            return Err(DecodeError::UnsupportedFormat("model.bin does not start with the A2DM magic".into()));
        }
        let mut r = Reader::new(&bytes[4..]);
        let version = r.u32()?;
        if version > FORMAT_VERSION {
            return Err(DecodeError::UnsupportedFormat(format!(
                "model.bin format version {version} is newer than this build supports ({FORMAT_VERSION})"
            )));
        }
        let model = match r.u8()? {
            0 => PackageModel::Spine(spine_io::read(&mut r)?),
            1 => PackageModel::Cubism(cubism_io::read(&mut r)?),
            other => {
                return Err(DecodeError::UnsupportedFormat(format!("unknown model kind tag {other} in model.bin")))
            }
        };
        r.expect_eof()?;
        Ok(model)
    }

    /// Writes the package to `dir`, creating it if needed.
    pub fn write_to(&self, dir: &Path) -> Result<(), DecodeError> {
        let io = |path: &Path, e: std::io::Error| DecodeError::io(path.display().to_string(), e);

        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        let manifest_path = dir.join("manifest.json");
        fs::write(&manifest_path, self.manifest.to_json()?).map_err(|e| io(&manifest_path, e))?;
        let model_path = dir.join("model.bin");
        fs::write(&model_path, self.encode_model()).map_err(|e| io(&model_path, e))?;

        if !self.textures.is_empty() {
            let textures = dir.join("textures");
            fs::create_dir_all(&textures).map_err(|e| io(&textures, e))?;
            for texture in &self.textures {
                let name = safe_file_name(&texture.file)?;
                let path = textures.join(&name);
                fs::write(&path, &texture.bytes).map_err(|e| io(&path, e))?;
            }
        }

        if !self.manifest.import_warnings.is_empty() {
            let metadata = dir.join("metadata");
            fs::create_dir_all(&metadata).map_err(|e| io(&metadata, e))?;
            let report = metadata.join("import-report.txt");
            let mut text = String::from("Loaded with warnings:\n");
            for warning in &self.manifest.import_warnings {
                text.push_str("- ");
                text.push_str(warning);
                text.push('\n');
            }
            fs::write(&report, text).map_err(|e| io(&report, e))?;
        }
        Ok(())
    }

    /// Reads a package from `dir`.
    pub fn read_from(dir: &Path) -> Result<Package, DecodeError> {
        let io = |path: &PathBuf, e: std::io::Error| DecodeError::io(path.display().to_string(), e);

        let manifest_path = dir.join("manifest.json");
        let manifest_text = fs::read_to_string(&manifest_path).map_err(|e| io(&manifest_path, e))?;
        let manifest = Manifest::from_json(&manifest_text)?;

        let model_path = dir.join("model.bin");
        let model_bytes = fs::read(&model_path).map_err(|e| io(&model_path, e))?;
        let model = Package::decode_model(&model_bytes)?;

        if model.model_type() != manifest.model_type {
            return Err(DecodeError::corrupt(format!(
                "manifest says {} but model.bin holds {}",
                manifest.model_type.as_str(),
                model.model_type().as_str()
            )));
        }

        let mut textures = Vec::with_capacity(manifest.textures.len());
        for entry in &manifest.textures {
            let name = safe_file_name(&entry.file)?;
            let path = dir.join("textures").join(&name);
            match fs::read(&path) {
                Ok(bytes) => textures.push(TextureFile { file: entry.file.clone(), bytes }),
                // A missing page is reported by `validate`, not by refusing to
                // open the package; a partial load is more useful than none.
                Err(_) => continue,
            }
        }

        Ok(Package { manifest, model, textures })
    }

    /// Checks the package for the problems spec §14 lists for `validate`.
    pub fn validate(&self) -> LoadReport {
        let mut report = LoadReport::new();
        for warning in &self.manifest.import_warnings {
            report.note(format!("recorded at import: {warning}"));
        }
        match &self.model {
            PackageModel::Spine(ir) => crate::validate::validate_spine(ir, &self.textures, &mut report),
            PackageModel::Cubism(ir) => crate::validate::validate_cubism(ir, &self.textures, &mut report),
        }
        report
    }
}

/// Rejects a file name that would escape the package directory.
///
/// Region and page names come from an atlas written by a game's toolchain, so
/// they are untrusted for path purposes even though the assets are the user's.
pub fn safe_file_name(name: &str) -> Result<String, DecodeError> {
    let trimmed = name.trim();
    let rejected = trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        // Both separators are checked explicitly rather than via `Path`, whose
        // parsing is platform-dependent: a package written on Windows must not
        // become unsafe when read on Linux.
        || trimmed.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'));
    if rejected {
        return Err(DecodeError::corrupt(format!("texture file name {name:?} is not a plain file name")));
    }
    Ok(trimmed.to_string())
}

fn source_format_of(ir: &SpineIr) -> String {
    let version = &ir.metadata.source_version;
    let mut parts = version.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if !major.is_empty() => format!("spine-{major}.{minor}"),
        _ => "spine".to_string(),
    }
}

fn default_animation_of(ir: &SpineIr) -> Option<String> {
    ir.animations
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("idle"))
        .or_else(|| ir.animations.iter().find(|a| a.name.to_ascii_lowercase().contains("idle")))
        .or_else(|| ir.animations.first())
        .map(|a| a.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, AttachmentId, BoneId, SlotId};
    use a2d_core::ir::spine::*;
    use a2d_core::{BlendMode, Interpolation, Rgb, Rgba, Vec2};

    /// A skeleton exercising every construct the encoder handles.
    fn rich_ir() -> SpineIr {
        let mut ir = SpineIr {
            metadata: SkeletonMetadata {
                name: Some("hero".into()),
                source_version: "3.8.99".into(),
                hash: Some("abc".into()),
                origin: Vec2::new(-1.0, -2.0),
                size: Vec2::new(100.0, 200.0),
                fps: Some(30.0),
                images_path: Some("./images/".into()),
                audio_path: None,
            },
            bones: vec![
                Bone::new("root", None),
                Bone {
                    length: 50.0,
                    setup: BoneLocal {
                        position: Vec2::new(1.0, 2.0),
                        rotation: 30.0,
                        scale: Vec2::new(2.0, 3.0),
                        shear: Vec2::new(4.0, 5.0),
                    },
                    inherit: TransformInherit::NoScaleOrReflection,
                    skin_required: true,
                    ..Bone::new("torso", Some(BoneId(0)))
                },
            ],
            slots: vec![
                Slot {
                    color: Rgba::new(0.1, 0.2, 0.3, 0.4),
                    dark_color: Some(Rgb::new(0.5, 0.6, 0.7)),
                    setup_attachment: Some("body".into()),
                    blend_mode: BlendMode::Screen,
                    ..Slot::new("body", BoneId(1))
                },
                Slot::new("clip", BoneId(0)),
            ],
            skins: vec![Skin::new("default"), Skin { bones: vec![BoneId(1)], ..Skin::new("blue") }],
            attachments: vec![
                Attachment {
                    name: "body".into(),
                    kind: AttachmentKind::Mesh(MeshAttachment {
                        path: "body".into(),
                        region: Some(a2d_core::ir::ids::AtlasRegionId(0)),
                        uvs: vec![Vec2::ZERO, Vec2::ONE],
                        triangles: vec![0, 1, 0],
                        vertices: VertexData::Weighted(WeightedVertices {
                            offsets: vec![0, 1, 2],
                            influences: vec![
                                VertexInfluence { bone: BoneId(0), position: Vec2::new(1.0, 2.0), weight: 0.5 },
                                VertexInfluence { bone: BoneId(1), position: Vec2::new(3.0, 4.0), weight: 0.5 },
                            ],
                        }),
                        hull_length: 2,
                        edges: vec![0, 1],
                        size: Vec2::new(10.0, 20.0),
                        color: Rgba::new(1.0, 0.5, 0.25, 1.0),
                        linked_to: Some(LinkedMesh {
                            skin: Some("default".into()),
                            slot: SlotId(0),
                            parent: "body".into(),
                            inherit_timelines: false,
                            resolved: Some(AttachmentId(0)),
                        }),
                        sequence: Some(Sequence { count: 4, start: 1, digits: 2, setup_index: 0 }),
                    }),
                },
                Attachment {
                    name: "clip".into(),
                    kind: AttachmentKind::Clipping(ClippingAttachment {
                        end_slot: Some(SlotId(0)),
                        vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::ONE, Vec2::new(0.0, 1.0)]),
                        color: Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "hit".into(),
                    kind: AttachmentKind::BoundingBox(BoundingBoxAttachment {
                        vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::ONE, Vec2::new(1.0, 0.0)]),
                        color: Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "muzzle".into(),
                    kind: AttachmentKind::Point(PointAttachment {
                        position: Vec2::new(5.0, 6.0),
                        rotation: 45.0,
                        color: Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "route".into(),
                    kind: AttachmentKind::Path(PathAttachment {
                        closed: true,
                        constant_speed: false,
                        lengths: vec![1.0, 2.0],
                        vertices: VertexData::Rigid(vec![Vec2::ZERO; 6]),
                        color: Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "plain".into(),
                    kind: AttachmentKind::Region(RegionAttachment {
                        path: "plain".into(),
                        region: None,
                        position: Vec2::new(1.0, 2.0),
                        rotation: 15.0,
                        scale: Vec2::new(1.5, 2.5),
                        size: Vec2::new(8.0, 9.0),
                        color: Rgba::WHITE,
                        sequence: None,
                    }),
                },
            ],
            ik_constraints: vec![IkConstraint {
                name: "aim".into(),
                order: 1,
                skin_required: true,
                bones: vec![BoneId(1)],
                target: BoneId(0),
                mix: 0.75,
                softness: 2.0,
                bend_positive: false,
                compress: true,
                stretch: true,
                uniform: true,
            }],
            transform_constraints: vec![TransformConstraint {
                name: "tc".into(),
                order: 0,
                skin_required: false,
                bones: vec![BoneId(1)],
                target: BoneId(0),
                offset_rotation: 45.0,
                offset_x: 1.0,
                offset_y: 2.0,
                offset_scale_x: 0.5,
                offset_scale_y: 0.25,
                offset_shear_y: 0.125,
                mix_rotate: 0.9,
                mix_x: 0.8,
                mix_y: 0.7,
                mix_scale_x: 0.6,
                mix_scale_y: 0.5,
                mix_shear_y: 0.4,
                relative: true,
                local: true,
            }],
            path_constraints: vec![PathConstraint {
                name: "pc".into(),
                order: 2,
                skin_required: false,
                bones: vec![BoneId(1)],
                target_slot: SlotId(1),
                position_mode: PathPositionMode::Fixed,
                spacing_mode: PathSpacingMode::Proportional,
                rotate_mode: PathRotateMode::ChainScale,
                offset_rotation: 10.0,
                position: 20.0,
                spacing: 30.0,
                mix_rotate: 0.1,
                mix_x: 0.2,
                mix_y: 0.3,
            }],
            events: vec![EventData {
                name: "step".into(),
                int_value: 7,
                float_value: 1.5,
                string_value: "left".into(),
                audio_path: Some("step.wav".into()),
                volume: 0.5,
                balance: -0.25,
            }],
            atlas: Atlas {
                pages: vec![AtlasPage {
                    name: "hero.png".into(),
                    size: Some((1024, 2048)),
                    premultiplied_alpha: true,
                    ..AtlasPage::new("hero.png")
                }],
                regions: vec![AtlasRegion {
                    name: "body".into(),
                    page: AtlasPageId(0),
                    xy: (2, 3),
                    size: (10, 20),
                    rotate_deg: 90,
                    offset: (-1, -2),
                    original_size: (20, 10),
                    index: 3,
                    splits: Some([1, 2, 3, 4]),
                    pads: Some([5, 6, 7, 8]),
                }],
            },
            animations: vec![Animation {
                name: "idle".into(),
                duration: 1.5,
                timelines: vec![
                    Timeline::BoneRotate {
                        bone: BoneId(1),
                        keys: vec![ScalarKey {
                            time: 0.0,
                            value: 45.0,
                            interp: Interpolation::Bezier(a2d_core::math::Bezier::new(0.1, 0.2, 0.3, 0.4)),
                        }],
                    },
                    Timeline::BoneTranslate {
                        bone: BoneId(1),
                        axes: Axes::X,
                        keys: vec![Vec2Key::shared(0.5, Vec2::new(1.0, 2.0), Interpolation::Stepped)],
                    },
                    Timeline::BoneScale {
                        bone: BoneId(1),
                        axes: Axes::Y,
                        keys: vec![Vec2Key::shared(0.0, Vec2::ONE, Interpolation::Linear)],
                    },
                    Timeline::BoneShear {
                        bone: BoneId(1),
                        axes: Axes::Both,
                        keys: vec![Vec2Key::shared(0.0, Vec2::ZERO, Interpolation::Linear)],
                    },
                    Timeline::SlotColor {
                        slot: SlotId(0),
                        channels: ColorChannels::Rgb,
                        keys: vec![ColorKey::shared(0.0, Rgba::WHITE, Interpolation::Linear)],
                    },
                    Timeline::SlotTwoColor {
                        slot: SlotId(0),
                        channels: ColorChannels::Rgba,
                        keys: vec![TwoColorKey::shared(0.0, Rgba::WHITE, Rgb::BLACK, Interpolation::Linear)],
                    },
                    Timeline::SlotAlpha {
                        slot: SlotId(0),
                        keys: vec![ScalarKey { time: 0.0, value: 0.5, interp: Interpolation::Linear }],
                    },
                    Timeline::SlotAttachment {
                        slot: SlotId(0),
                        keys: vec![
                            AttachmentKey { time: 0.0, name: Some("body".into()) },
                            AttachmentKey { time: 1.0, name: None },
                        ],
                    },
                    Timeline::Deform {
                        slot: SlotId(0),
                        skin: DEFAULT_SKIN,
                        attachment: AttachmentId(0),
                        keys: vec![DeformKey {
                            time: 0.0,
                            offset: 2,
                            values: vec![1.0, 2.0],
                            interp: Interpolation::Linear,
                        }],
                    },
                    Timeline::DrawOrder {
                        keys: vec![
                            DrawOrderKey { time: 0.0, order: Some(vec![SlotId(1), SlotId(0)]) },
                            DrawOrderKey { time: 1.0, order: None },
                        ],
                    },
                    Timeline::Event {
                        keys: vec![EventKey {
                            time: 0.25,
                            event: a2d_core::ir::ids::EventId(0),
                            int_value: 9,
                            float_value: 2.5,
                            string_value: Some("right".into()),
                            volume: 1.0,
                            balance: 0.0,
                        }],
                    },
                    Timeline::IkConstraint {
                        constraint: a2d_core::ir::ids::IkConstraintId(0),
                        keys: vec![IkKey {
                            time: 0.0,
                            mix: 1.0,
                            softness: 0.0,
                            bend_positive: true,
                            compress: false,
                            stretch: true,
                            interp: Interpolation::Linear,
                        }],
                    },
                    Timeline::TransformConstraint {
                        constraint: a2d_core::ir::ids::TransformConstraintId(0),
                        keys: vec![TransformKey {
                            time: 0.0,
                            mix_rotate: 0.1,
                            mix_x: 0.2,
                            mix_y: 0.3,
                            mix_scale_x: 0.4,
                            mix_scale_y: 0.5,
                            mix_shear_y: 0.6,
                            interp: Interpolation::Linear,
                        }],
                    },
                    Timeline::PathPosition {
                        constraint: a2d_core::ir::ids::PathConstraintId(0),
                        keys: vec![ScalarKey { time: 0.0, value: 1.0, interp: Interpolation::Linear }],
                    },
                    Timeline::PathSpacing {
                        constraint: a2d_core::ir::ids::PathConstraintId(0),
                        keys: vec![ScalarKey { time: 0.0, value: 2.0, interp: Interpolation::Linear }],
                    },
                    Timeline::PathMix {
                        constraint: a2d_core::ir::ids::PathConstraintId(0),
                        keys: vec![PathMixKey {
                            time: 0.0,
                            mix_rotate: 0.1,
                            mix_x: 0.2,
                            mix_y: 0.3,
                            interp: Interpolation::Linear,
                        }],
                    },
                ],
            }],
            constraint_order: Vec::new(),
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "clip".into(), attachment: AttachmentId(1) });
        ir.skins[1].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(5) });
        ir.rebuild_derived();
        ir
    }

    #[test]
    fn a_rich_skeleton_round_trips_through_model_bin() {
        let ir = rich_ir();
        let package = Package::from_spine(ir.clone(), "Hero");
        let bytes = package.encode_model();
        let decoded = Package::decode_model(&bytes).unwrap();
        assert_eq!(decoded.as_spine().unwrap(), &ir);
    }

    #[test]
    fn encoding_is_byte_stable_across_runs() {
        let a = Package::from_spine(rich_ir(), "Hero").encode_model();
        let b = Package::from_spine(rich_ir(), "Hero").encode_model();
        assert_eq!(a, b);
    }

    #[test]
    fn re_encoding_a_decoded_model_reproduces_the_same_bytes() {
        // This is the property golden tests rely on.
        let first = Package::from_spine(rich_ir(), "Hero").encode_model();
        let decoded = Package::decode_model(&first).unwrap();
        let PackageModel::Spine(ir) = decoded else { panic!("a Spine package must decode as one") };
        let second = Package::from_spine(ir, "Hero").encode_model();
        assert_eq!(first, second);
    }

    #[test]
    fn model_bin_starts_with_its_magic_and_version() {
        let bytes = Package::from_spine(rich_ir(), "Hero").encode_model();
        assert_eq!(&bytes[..4], MODEL_MAGIC);
        assert_eq!(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]), FORMAT_VERSION);
    }

    #[test]
    fn a_file_without_the_magic_is_refused() {
        let err = Package::decode_model(b"not a model at all").unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedFormat(_)), "{err}");
    }

    #[test]
    fn an_empty_file_is_refused() {
        assert!(Package::decode_model(&[]).is_err());
    }

    #[test]
    fn a_newer_model_version_is_refused_rather_than_misread() {
        let mut bytes = Package::from_spine(rich_ir(), "Hero").encode_model();
        bytes[4..8].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        let err = Package::decode_model(&bytes).unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");
    }

    #[test]
    fn truncating_model_bin_anywhere_is_an_error_and_never_a_panic() {
        let full = Package::from_spine(rich_ir(), "Hero").encode_model();
        for cut in (0..full.len()).step_by(17) {
            let _ = Package::decode_model(&full[..cut]);
        }
    }

    #[test]
    fn corrupting_single_bytes_never_panics() {
        let full = Package::from_spine(rich_ir(), "Hero").encode_model();
        for at in (8..full.len()).step_by(29) {
            let mut bytes = full.clone();
            bytes[at] ^= 0xff;
            let _ = Package::decode_model(&bytes);
        }
    }

    #[test]
    fn a_dangling_bone_parent_is_rejected_on_load() {
        let mut ir = rich_ir();
        ir.bones[1].parent = Some(BoneId(99));
        let bytes = Package::from_spine(ir, "Hero").encode_model();
        let err = Package::decode_model(&bytes).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn a_bone_parented_to_a_later_bone_is_rejected_on_load() {
        let mut ir = rich_ir();
        ir.bones[0].parent = Some(BoneId(1));
        let bytes = Package::from_spine(ir, "Hero").encode_model();
        let err = Package::decode_model(&bytes).unwrap_err();
        assert!(err.to_string().contains("not before it"), "{err}");
    }

    #[test]
    fn a_dangling_skin_attachment_is_rejected_on_load() {
        let mut ir = rich_ir();
        ir.skins[0].entries[0].attachment = AttachmentId(999);
        let bytes = Package::from_spine(ir, "Hero").encode_model();
        let err = Package::decode_model(&bytes).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn the_manifest_is_derived_from_the_model() {
        let package = Package::from_spine(rich_ir(), "Hero");
        assert_eq!(package.manifest.display_name, "Hero");
        assert_eq!(package.manifest.source_format, "spine-3.8");
        assert_eq!(package.manifest.default_animation.as_deref(), Some("idle"));
        assert_eq!(package.manifest.animations.len(), 1);
        assert_eq!(package.manifest.textures.len(), 1);
        assert_eq!(package.manifest.textures[0].size, Some([1024, 2048]));
        assert!(package.manifest.textures[0].premultiplied_alpha);
    }

    #[test]
    fn a_model_with_no_idle_falls_back_to_its_first_animation() {
        let mut ir = rich_ir();
        ir.animations[0].name = "attack".into();
        ir.rebuild_derived();
        assert_eq!(Package::from_spine(ir, "X").manifest.default_animation.as_deref(), Some("attack"));
    }

    #[test]
    fn plain_file_names_are_accepted() {
        assert_eq!(safe_file_name("texture_00.png").unwrap(), "texture_00.png");
        assert_eq!(safe_file_name("  hero.png  ").unwrap(), "hero.png");
    }

    #[test]
    fn file_names_that_would_escape_the_package_are_refused() {
        for name in ["../secret", "a/b.png", "/etc/passwd", "..", "", "   "] {
            assert!(safe_file_name(name).is_err(), "{name:?} should be refused");
        }
    }

    #[test]
    fn windows_style_paths_are_refused() {
        assert!(safe_file_name("..\\secret").is_err());
        assert!(safe_file_name("dir\\file.png").is_err());
    }
}
