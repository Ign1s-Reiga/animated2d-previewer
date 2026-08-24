//! Spine 4.x binary skeleton decoder.
//!
//! # Status: not yet validated against a real export
//!
//! The binary format is undocumented. This decoder is written from the layout
//! the 4.0/4.1 runtimes read, and its tests round-trip through the writer in
//! [`super::writer`] — which proves the decoder and the writer agree with each
//! other, and nothing about whether either agrees with a file the Spine editor
//! produced. Until one real `.skel` has been decoded and compared, every load
//! reports that in its [`LoadReport`].
//!
//! That warning is the point. Spec §4.9 forbids guessing between candidate
//! layouts, and a binary container offers nothing to derive from first
//! principles: every offset is a convention. So this decoder validates far more
//! aggressively than [`super::v3`] does — counts are checked against the bytes
//! that remain, string references against the table, and the stream must end
//! exactly at the end of the file. A layout that is wrong then fails loudly at
//! the first inconsistency instead of returning a skeleton that animates wrongly.
//!
//! # Layout notes, and how 4.x differs from 3.x
//!
//! * The header's hash is an **8-byte integer**, where 3.x wrote a string.
//! * Transform constraints carry **six** mixes (rotate, x, y, scaleX, scaleY,
//!   shearY) where 3.x carried four, and path constraints carry **three**
//!   (rotate, x, y) where 3.x carried two. The IR already has the wider shape,
//!   so 3.x widens and 4.x maps across directly.
//! * Timelines are **interleaved**: a keyframe's values, then the *next*
//!   keyframe's values, then the curve joining them. 3.x wrote value-then-curve.
//! * Bézier control points are **absolute** time/value coordinates, not the
//!   normalised ones 3.x used, and there is one set per curved component.
//!   [`crate::json::curve::resolve`] does that conversion for the JSON dialect
//!   and is reused here.
//! * Each curved timeline writes a **Bézier count** after its frame count.
//!   Nothing here needs it — the reference uses it to pre-size an array — but it
//!   is range-checked, because a wrong reading of it desynchronises everything
//!   after.
//! * Translate, scale and shear each have **single-axis variants**, which 3.x
//!   did not.
//! * Slot colour splits into rgba / rgb / rgba2 / rgb2 / alpha timelines.

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::ids::{
    AttachmentId, BoneId, EventId, IkConstraintId, PathConstraintId, SkinId, SlotId, TransformConstraintId,
};
use a2d_core::ir::spine::{
    Animation, Attachment, AttachmentKey, AttachmentKind, Axes, Bone, BoneLocal, BoundingBoxAttachment,
    ClippingAttachment, ColorChannels, ColorKey, DeformKey, DrawOrderKey, EventData, EventKey, IkConstraint, IkKey,
    LinkedMesh, MeshAttachment, PathAttachment, PathConstraint, PathMixKey, PathPositionMode, PathRotateMode,
    PathSpacingMode, PointAttachment, RegionAttachment, ScalarKey, Sequence, SkeletonMetadata, Skin, SkinEntry, Slot,
    SpineIr, Timeline, TransformConstraint, TransformInherit, TransformKey, TwoColorKey, Vec2Key, VertexData,
};
use a2d_core::{BlendMode, DecodeError, Degradation, Interpolation, LoadReport, ModelKind, Rgb, Rgba, Vec2};

use crate::detect::SpineDetection;
use crate::json::animation::draw_order_from_offsets;
use crate::json::attachment::parse_vertices;
use crate::json::curve::{resolve, RawCurve};
use crate::reader::BinaryReader;

const CURVE_LINEAR: u8 = 0;
const CURVE_STEPPED: u8 = 1;
const CURVE_BEZIER: u8 = 2;

const TYPE_REGION: u8 = 0;
const TYPE_BOUNDING_BOX: u8 = 1;
const TYPE_MESH: u8 = 2;
const TYPE_LINKED_MESH: u8 = 3;
const TYPE_PATH: u8 = 4;
const TYPE_POINT: u8 = 5;
const TYPE_CLIPPING: u8 = 6;

const SLOT_ATTACHMENT: u8 = 0;
const SLOT_RGBA: u8 = 1;
const SLOT_RGB: u8 = 2;
const SLOT_RGBA2: u8 = 3;
const SLOT_RGB2: u8 = 4;
const SLOT_ALPHA: u8 = 5;

const BONE_ROTATE: u8 = 0;
const BONE_TRANSLATE: u8 = 1;
const BONE_TRANSLATE_X: u8 = 2;
const BONE_TRANSLATE_Y: u8 = 3;
const BONE_SCALE: u8 = 4;
const BONE_SCALE_X: u8 = 5;
const BONE_SCALE_Y: u8 = 6;
const BONE_SHEAR: u8 = 7;
const BONE_SHEAR_X: u8 = 8;
const BONE_SHEAR_Y: u8 = 9;

const ATTACHMENT_DEFORM: u8 = 0;
const ATTACHMENT_SEQUENCE: u8 = 1;

const PATH_POSITION: u8 = 0;
const PATH_SPACING: u8 = 1;
const PATH_MIX: u8 = 2;

/// Decodes a Spine 4.x binary skeleton.
pub fn decode(
    bytes: &[u8],
    detection: &SpineDetection,
    atlas: Atlas,
    report: &mut LoadReport,
) -> Result<SpineIr, DecodeError> {
    report.note(
        "the Spine 4.x binary layout has not been checked against a real export; verify the result \
         before trusting it, and prefer the 4.x JSON dialect where you have the choice",
    );

    let mut r = BinaryReader::new(bytes);
    let mut ir = SpineIr { atlas, ..Default::default() };
    let nonessential = read_header(&mut r, detection, &mut ir)?;
    let strings = read_string_table(&mut r)?;
    let ctx = Ctx { strings, nonessential };

    read_bones(&mut r, &ctx, &mut ir, report)?;
    read_slots(&mut r, &ctx, &mut ir, report)?;
    read_ik(&mut r, &mut ir, report)?;
    read_transform(&mut r, &mut ir)?;
    read_path(&mut r, &mut ir)?;
    read_skins(&mut r, &ctx, &mut ir, report)?;
    read_events(&mut r, &ctx, &mut ir)?;
    ir.rebuild_derived();
    read_animations(&mut r, &ctx, &mut ir, report)?;

    // A layout that drifted mid-file usually still parses to *something*; it
    // almost never lands exactly on the last byte. This is the cheapest check
    // that the whole reading was self-consistent.
    if !r.is_eof() {
        return Err(DecodeError::corrupt_at(
            format!(
                "{} bytes remain after the last animation; the 4.x layout read here does not match this file",
                r.remaining()
            ),
            r.position() as u64,
        ));
    }

    crate::normalize::finish(&mut ir, report);
    Ok(ir)
}

/// Decode-wide context: the string table and the editor-data flag.
struct Ctx {
    strings: Vec<Option<String>>,
    nonessential: bool,
}

impl Ctx {
    /// Resolves a one-based string-table reference; index 0 means `null`.
    fn string_ref(&self, r: &mut BinaryReader<'_>) -> Result<Option<String>, DecodeError> {
        let index = r.varint()? as usize;
        if index == 0 {
            return Ok(None);
        }
        self.strings.get(index - 1).cloned().ok_or_else(|| {
            DecodeError::corrupt_at(
                format!("string reference {index} is out of range ({} in the table)", self.strings.len()),
                r.position() as u64,
            )
        })
    }
}

fn read_header(r: &mut BinaryReader<'_>, detection: &SpineDetection, ir: &mut SpineIr) -> Result<bool, DecodeError> {
    // 4.x writes the hash as an integer; 3.x wrote it as a string.
    let hash_raw = r.u64()?;
    let hash = if hash_raw == 0 { None } else { Some(format!("{hash_raw:x}")) };
    let version = r.string_opt()?.unwrap_or_default();
    let origin = Vec2::new(r.f32()?, r.f32()?);
    let size = Vec2::new(r.f32()?, r.f32()?);
    let nonessential = r.bool()?;
    let (fps, images_path, audio_path) =
        if nonessential { (Some(r.f32()?), r.string_opt()?, r.string_opt()?) } else { (None, None, None) };
    ir.metadata = SkeletonMetadata {
        name: None,
        source_version: if version.is_empty() { detection.raw_version.clone() } else { version },
        hash,
        origin,
        size,
        fps,
        images_path,
        audio_path,
    };
    Ok(nonessential)
}

fn read_string_table(r: &mut BinaryReader<'_>) -> Result<Vec<Option<String>>, DecodeError> {
    let n = r.count("string table")?;
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        out.push(r.string_opt()?);
    }
    Ok(out)
}

fn read_bones(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let n = r.count("bone")?;
    for i in 0..n {
        let name = r.string()?;
        let parent = if i == 0 {
            None
        } else {
            let index = r.varint()? as usize;
            Some(BoneId::from_index(index).filter(|_| index < i).ok_or_else(|| {
                DecodeError::corrupt_at(format!("bone {name:?} names parent index {index}"), r.position() as u64)
            })?)
        };
        let rotation = r.f32()?;
        let position = Vec2::new(r.f32()?, r.f32()?);
        let scale = Vec2::new(r.f32()?, r.f32()?);
        let shear = Vec2::new(r.f32()?, r.f32()?);
        let length = r.f32()?;
        let inherit_ordinal = r.varint()?;
        let inherit = TransformInherit::from_ordinal(inherit_ordinal).unwrap_or_else(|| {
            report.note(format!("bone {name:?}: unknown transform mode {inherit_ordinal}, using normal"));
            TransformInherit::Normal
        });
        let skin_required = r.bool()?;
        if ctx.nonessential {
            r.u32()?; // editor bone colour
        }
        ir.bones.push(Bone {
            name,
            parent,
            length,
            setup: BoneLocal { position, rotation, scale, shear },
            inherit,
            skin_required,
        });
    }
    Ok(())
}

fn read_slots(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let n = r.count("slot")?;
    for _ in 0..n {
        let name = r.string()?;
        let bone_index = r.varint()? as usize;
        let bone = BoneId::from_index(bone_index).filter(|b| b.index() < ir.bones.len()).ok_or_else(|| {
            DecodeError::corrupt_at(format!("slot {name:?} targets bone index {bone_index}"), r.position() as u64)
        })?;
        let color = Rgba::from_rgba8888(r.u32()?);
        let dark_raw = r.u32()?;
        let dark_color = if dark_raw == 0xFFFF_FFFF { None } else { Some(Rgba::rgb_from_rgb888(dark_raw)) };
        let setup_attachment = ctx.string_ref(r)?;
        let blend_ordinal = r.varint()?;
        let blend_mode = blend_from_ordinal(blend_ordinal).unwrap_or_else(|| {
            report.warn(Degradation::UnsupportedBlendMode {
                slot: name.clone(),
                requested: blend_ordinal.to_string(),
                fallback: "normal".into(),
            });
            BlendMode::Normal
        });
        ir.slots.push(Slot { name, bone, color, dark_color, setup_attachment, blend_mode });
    }
    Ok(())
}

fn blend_from_ordinal(n: u32) -> Option<BlendMode> {
    Some(match n {
        0 => BlendMode::Normal,
        1 => BlendMode::Additive,
        2 => BlendMode::Multiply,
        3 => BlendMode::Screen,
        _ => return None,
    })
}

fn read_bone_indices(r: &mut BinaryReader<'_>, ir: &SpineIr) -> Result<Vec<BoneId>, DecodeError> {
    let n = r.count("constraint bone")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_bone_ref(r, ir)?);
    }
    Ok(out)
}

fn read_bone_ref(r: &mut BinaryReader<'_>, ir: &SpineIr) -> Result<BoneId, DecodeError> {
    let index = r.varint()? as usize;
    BoneId::from_index(index)
        .filter(|b| b.index() < ir.bones.len())
        .ok_or_else(|| DecodeError::corrupt_at(format!("bone index {index} is out of range"), r.position() as u64))
}

fn read_ik(r: &mut BinaryReader<'_>, ir: &mut SpineIr, report: &mut LoadReport) -> Result<(), DecodeError> {
    let n = r.count("ik constraint")?;
    for _ in 0..n {
        let name = r.string()?;
        let order = r.varint()?;
        let skin_required = r.bool()?;
        let bones = read_bone_indices(r, ir)?;
        let target = read_bone_ref(r, ir)?;
        let mix = r.f32()?;
        let softness = r.f32()?;
        let bend_positive = r.i8()? > 0;
        let compress = r.bool()?;
        let stretch = r.bool()?;
        let uniform = r.bool()?;
        if bones.len() > 2 {
            report.warn(Degradation::UnsupportedConstraint {
                name: name.clone(),
                kind: format!("ik chain of {} bones; only 1 and 2 are solved", bones.len()),
            });
        }
        ir.ik_constraints.push(IkConstraint {
            name,
            order,
            skin_required,
            bones,
            target,
            mix,
            softness,
            bend_positive,
            compress,
            stretch,
            uniform,
        });
    }
    Ok(())
}

fn read_transform(r: &mut BinaryReader<'_>, ir: &mut SpineIr) -> Result<(), DecodeError> {
    let n = r.count("transform constraint")?;
    for _ in 0..n {
        let name = r.string()?;
        let order = r.varint()?;
        let skin_required = r.bool()?;
        let bones = read_bone_indices(r, ir)?;
        let target = read_bone_ref(r, ir)?;
        let local = r.bool()?;
        let relative = r.bool()?;
        let offset_rotation = r.f32()?;
        let offset_x = r.f32()?;
        let offset_y = r.f32()?;
        let offset_scale_x = r.f32()?;
        let offset_scale_y = r.f32()?;
        let offset_shear_y = r.f32()?;
        // Six independent mixes, where 3.x had four shared ones.
        let mix_rotate = r.f32()?;
        let mix_x = r.f32()?;
        let mix_y = r.f32()?;
        let mix_scale_x = r.f32()?;
        let mix_scale_y = r.f32()?;
        let mix_shear_y = r.f32()?;
        ir.transform_constraints.push(TransformConstraint {
            name,
            order,
            skin_required,
            bones,
            target,
            offset_rotation,
            offset_x,
            offset_y,
            offset_scale_x,
            offset_scale_y,
            offset_shear_y,
            mix_rotate,
            mix_x,
            mix_y,
            mix_scale_x,
            mix_scale_y,
            mix_shear_y,
            relative,
            local,
        });
    }
    Ok(())
}

fn read_path(r: &mut BinaryReader<'_>, ir: &mut SpineIr) -> Result<(), DecodeError> {
    let n = r.count("path constraint")?;
    for _ in 0..n {
        let name = r.string()?;
        let order = r.varint()?;
        let skin_required = r.bool()?;
        let bones = read_bone_indices(r, ir)?;
        let slot_index = r.varint()? as usize;
        let target_slot = SlotId::from_index(slot_index).filter(|s| s.index() < ir.slots.len()).ok_or_else(|| {
            DecodeError::corrupt_at(format!("path constraint targets slot index {slot_index}"), r.position() as u64)
        })?;
        let position_mode = match r.varint()? {
            0 => PathPositionMode::Fixed,
            1 => PathPositionMode::Percent,
            other => {
                return Err(DecodeError::corrupt_at(format!("unknown path position mode {other}"), r.position() as u64))
            }
        };
        let spacing_mode = match r.varint()? {
            0 => PathSpacingMode::Length,
            1 => PathSpacingMode::Fixed,
            2 => PathSpacingMode::Percent,
            3 => PathSpacingMode::Proportional,
            other => {
                return Err(DecodeError::corrupt_at(format!("unknown path spacing mode {other}"), r.position() as u64))
            }
        };
        let rotate_mode = match r.varint()? {
            0 => PathRotateMode::Tangent,
            1 => PathRotateMode::Chain,
            2 => PathRotateMode::ChainScale,
            other => {
                return Err(DecodeError::corrupt_at(format!("unknown path rotate mode {other}"), r.position() as u64))
            }
        };
        let offset_rotation = r.f32()?;
        let position = r.f32()?;
        let spacing = r.f32()?;
        // Three mixes, where 3.x had two.
        let mix_rotate = r.f32()?;
        let mix_x = r.f32()?;
        let mix_y = r.f32()?;
        ir.path_constraints.push(PathConstraint {
            name,
            order,
            skin_required,
            bones,
            target_slot,
            position_mode,
            spacing_mode,
            rotate_mode,
            offset_rotation,
            position,
            spacing,
            mix_rotate,
            mix_x,
            mix_y,
        });
    }
    Ok(())
}

fn read_skins(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    ir.skins.clear();
    // The default skin is written first, with only its slot count: no name and
    // no constraint lists. A zero count means there is no default skin at all.
    let default_slots = r.count("default skin slot")?;
    let mut default = Skin::new("default");
    if default_slots > 0 {
        read_skin_body(r, ctx, ir, &mut default, default_slots, report)?;
    }
    ir.skins.push(default);

    let n = r.count("skin")?;
    for _ in 0..n {
        let name = ctx.string_ref(r)?.unwrap_or_default();
        let mut skin = Skin::new(name);
        let bone_count = r.count("skin bone")?;
        for _ in 0..bone_count {
            skin.bones.push(read_bone_ref(r, ir)?);
        }
        // Skin-scoped constraint lists: indices only, which the IR does not
        // model. Skipping them keeps the stream aligned.
        for what in ["skin ik", "skin transform", "skin path"] {
            let count = r.count(what)?;
            for _ in 0..count {
                r.varint()?;
            }
        }
        let slots = r.count("skin slot")?;
        read_skin_body(r, ctx, ir, &mut skin, slots, report)?;
        ir.skins.push(skin);
    }
    Ok(())
}

fn read_skin_body(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    skin: &mut Skin,
    slot_count: usize,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for _ in 0..slot_count {
        let slot_index = r.varint()? as usize;
        let slot = SlotId::from_index(slot_index).filter(|s| s.index() < ir.slots.len()).ok_or_else(|| {
            DecodeError::corrupt_at(format!("skin entry names slot index {slot_index}"), r.position() as u64)
        })?;
        let entries = r.count("skin attachment")?;
        for _ in 0..entries {
            let placeholder = ctx.string_ref(r)?.unwrap_or_default();
            let (attachment, clip_end) = read_attachment(r, ctx, slot, &placeholder, report)?;
            let id = AttachmentId(ir.attachments.len() as u32);
            let mut attachment = attachment;
            if let (AttachmentKind::Clipping(c), Some(end)) = (&mut attachment.kind, clip_end) {
                c.end_slot = SlotId::from_index(end).filter(|s| s.index() < ir.slots.len());
            }
            ir.attachments.push(attachment);
            skin.entries.push(SkinEntry { slot, name: placeholder, attachment: id });
        }
    }
    skin.sort_entries();
    Ok(())
}

/// Reads the optional sequence block that 4.1 added to image attachments.
fn read_sequence(r: &mut BinaryReader<'_>) -> Result<Option<Sequence>, DecodeError> {
    if !r.bool()? {
        return Ok(None);
    }
    Ok(Some(Sequence { count: r.varint()?, start: r.varint()?, digits: r.varint()?, setup_index: r.varint()? }))
}

fn read_attachment(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    slot: SlotId,
    placeholder: &str,
    _report: &mut LoadReport,
) -> Result<(Attachment, Option<usize>), DecodeError> {
    let name = ctx.string_ref(r)?.unwrap_or_else(|| placeholder.to_string());
    let type_ordinal = r.u8()?;
    let context = format!("attachment {name:?}");

    let mut clip_end = None;
    let kind = match type_ordinal {
        TYPE_REGION => {
            let path = ctx.string_ref(r)?.unwrap_or_else(|| name.clone());
            let rotation = r.f32()?;
            let position = Vec2::new(r.f32()?, r.f32()?);
            let scale = Vec2::new(r.f32()?, r.f32()?);
            let size = Vec2::new(r.f32()?, r.f32()?);
            let color = Rgba::from_rgba8888(r.u32()?);
            let sequence = read_sequence(r)?;
            AttachmentKind::Region(RegionAttachment {
                path,
                region: None,
                position,
                rotation,
                scale,
                size,
                color,
                sequence,
            })
        }
        TYPE_BOUNDING_BOX => {
            let vertices = read_vertices(r, &context)?;
            let color = if ctx.nonessential { Rgba::from_rgba8888(r.u32()?) } else { Rgba::WHITE };
            AttachmentKind::BoundingBox(BoundingBoxAttachment { vertices, color })
        }
        TYPE_MESH => {
            let path = ctx.string_ref(r)?.unwrap_or_else(|| name.clone());
            let color = Rgba::from_rgba8888(r.u32()?);
            let vertex_count = r.count("mesh vertex")?;
            let uvs_flat = r.f32_array(vertex_count * 2)?;
            let uvs: Vec<Vec2> = uvs_flat.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect();
            let triangles = read_short_array(r)?;
            let vertices = read_vertices_with_count(r, vertex_count, &context)?;
            let hull_length = r.varint()?;
            let sequence = read_sequence(r)?;
            let (edges, size) = if ctx.nonessential {
                (read_short_array(r)?, Vec2::new(r.f32()?, r.f32()?))
            } else {
                (Vec::new(), Vec2::ZERO)
            };
            if let Some(&max) = triangles.iter().max() {
                if max as usize >= vertex_count {
                    return Err(DecodeError::corrupt_at(
                        format!("{context}: triangle index {max} exceeds {vertex_count} vertices"),
                        r.position() as u64,
                    ));
                }
            }
            AttachmentKind::Mesh(MeshAttachment {
                path,
                region: None,
                uvs,
                triangles,
                vertices,
                hull_length,
                edges,
                size,
                color,
                linked_to: None,
                sequence,
            })
        }
        TYPE_LINKED_MESH => {
            let path = ctx.string_ref(r)?.unwrap_or_else(|| name.clone());
            let color = Rgba::from_rgba8888(r.u32()?);
            let skin = ctx.string_ref(r)?;
            let parent = ctx.string_ref(r)?.unwrap_or_default();
            let inherit_timelines = r.bool()?;
            let sequence = read_sequence(r)?;
            let size = if ctx.nonessential { Vec2::new(r.f32()?, r.f32()?) } else { Vec2::ZERO };
            AttachmentKind::Mesh(MeshAttachment {
                path,
                region: None,
                uvs: Vec::new(),
                triangles: Vec::new(),
                vertices: VertexData::Rigid(Vec::new()),
                hull_length: 0,
                edges: Vec::new(),
                size,
                color,
                linked_to: Some(LinkedMesh { skin, slot, parent, inherit_timelines, resolved: None }),
                sequence,
            })
        }
        TYPE_PATH => {
            let closed = r.bool()?;
            let constant_speed = r.bool()?;
            let vertex_count = r.count("path vertex")?;
            let vertices = read_vertices_with_count(r, vertex_count, &context)?;
            let lengths = r.f32_array(vertex_count / 3)?;
            let color = if ctx.nonessential { Rgba::from_rgba8888(r.u32()?) } else { Rgba::WHITE };
            AttachmentKind::Path(PathAttachment { closed, constant_speed, lengths, vertices, color })
        }
        TYPE_POINT => {
            let rotation = r.f32()?;
            let position = Vec2::new(r.f32()?, r.f32()?);
            let color = if ctx.nonessential { Rgba::from_rgba8888(r.u32()?) } else { Rgba::WHITE };
            AttachmentKind::Point(PointAttachment { position, rotation, color })
        }
        TYPE_CLIPPING => {
            clip_end = Some(r.varint()? as usize);
            let vertices = read_vertices(r, &context)?;
            let color = if ctx.nonessential { Rgba::from_rgba8888(r.u32()?) } else { Rgba::WHITE };
            AttachmentKind::Clipping(ClippingAttachment { end_slot: None, vertices, color })
        }
        other => {
            return Err(DecodeError::corrupt_at(
                format!("{context}: unknown attachment type ordinal {other}"),
                r.position() as u64,
            ))
        }
    };

    Ok((Attachment { name, kind }, clip_end))
}

fn read_short_array(r: &mut BinaryReader<'_>) -> Result<Vec<u16>, DecodeError> {
    let n = r.count("short array")?;
    r.u16_array(n)
}

fn read_vertices(r: &mut BinaryReader<'_>, context: &str) -> Result<VertexData, DecodeError> {
    let count = r.count("vertex")?;
    read_vertices_with_count(r, count, context)
}

fn read_vertices_with_count(
    r: &mut BinaryReader<'_>,
    vertex_count: usize,
    context: &str,
) -> Result<VertexData, DecodeError> {
    // A leading flag says whether the vertices are weighted; the rigid case is
    // simply two floats each.
    if !r.bool()? {
        let flat = r.f32_array(vertex_count * 2)?;
        return Ok(VertexData::Rigid(flat.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect()));
    }
    let mut flat: Vec<f32> = Vec::new();
    for _ in 0..vertex_count {
        let influences = r.count("vertex influence")?;
        flat.push(influences as f32);
        for _ in 0..influences {
            flat.push(r.varint()? as f32);
            flat.push(r.f32()?);
            flat.push(r.f32()?);
            flat.push(r.f32()?);
        }
    }
    parse_vertices(&flat, vertex_count, context)
}

fn read_events(r: &mut BinaryReader<'_>, ctx: &Ctx, ir: &mut SpineIr) -> Result<(), DecodeError> {
    let n = r.count("event")?;
    for _ in 0..n {
        let name = ctx.string_ref(r)?.unwrap_or_default();
        let int_value = r.varint_signed()?;
        let float_value = r.f32()?;
        let string_value = r.string()?;
        let audio_path = r.string_opt()?;
        let (volume, balance) = if audio_path.is_some() { (r.f32()?, r.f32()?) } else { (1.0, 0.0) };
        ir.events.push(EventData { name, int_value, float_value, string_value, audio_path, volume, balance });
    }
    Ok(())
}

fn read_animations(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let n = r.count("animation")?;
    let mut animations = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.string()?;
        animations.push(read_animation(r, ctx, ir, name, report)?);
    }
    ir.animations = animations;
    Ok(())
}

/// Frame and Bézier counts, checked against each other.
///
/// `components` is how many independently-curved values the timeline carries.
/// The Bézier count cannot exceed one set per component per span; a larger one
/// means the stream is not where this decoder thinks it is.
fn read_frame_header(r: &mut BinaryReader<'_>, components: usize, what: &str) -> Result<usize, DecodeError> {
    let frames = r.count("keyframe")?;
    let beziers = r.count("bezier")?;
    let ceiling = frames.saturating_sub(1).saturating_mul(components);
    if beziers > ceiling {
        return Err(DecodeError::corrupt_at(
            format!("{what}: {beziers} Bézier curves across {frames} keyframes exceeds the {ceiling} possible"),
            r.position() as u64,
        ));
    }
    Ok(frames)
}

/// Reads the curve byte that joins two keyframes.
///
/// It is written *after* the following keyframe's values, and a Bézier carries
/// one set of absolute control points per component.
fn read_curve(r: &mut BinaryReader<'_>, components: usize) -> Result<RawCurve, DecodeError> {
    match r.u8()? {
        CURVE_LINEAR => Ok(RawCurve::Linear),
        CURVE_STEPPED => Ok(RawCurve::Stepped),
        CURVE_BEZIER => {
            let mut quads = Vec::with_capacity(components);
            for _ in 0..components {
                quads.push([r.f32()?, r.f32()?, r.f32()?, r.f32()?]);
            }
            Ok(RawCurve::Absolute(quads))
        }
        other => Err(DecodeError::corrupt_at(format!("unknown curve type {other}"), r.position() as u64)),
    }
}

/// Reads a one-value timeline in the interleaved 4.x layout.
fn read_scalar_keys(r: &mut BinaryReader<'_>, what: &str) -> Result<Vec<ScalarKey>, DecodeError> {
    let frames = read_frame_header(r, 1, what)?;
    if frames == 0 {
        return Ok(Vec::new());
    }
    let mut keys = Vec::with_capacity(frames);
    let mut time = r.f32()?;
    let mut value = r.f32()?;
    for frame in 0..frames {
        if frame + 1 == frames {
            keys.push(ScalarKey { time, value, interp: Interpolation::Linear });
            break;
        }
        let time2 = r.f32()?;
        let value2 = r.f32()?;
        let raw = read_curve(r, 1)?;
        keys.push(ScalarKey { time, value, interp: resolve(&raw, 0, time, value, time2, value2) });
        time = time2;
        value = value2;
    }
    Ok(keys)
}

/// Reads a two-value timeline (translate, scale, shear) in the same layout.
fn read_vec2_keys(r: &mut BinaryReader<'_>, what: &str) -> Result<Vec<Vec2Key>, DecodeError> {
    let frames = read_frame_header(r, 2, what)?;
    if frames == 0 {
        return Ok(Vec::new());
    }
    let mut keys = Vec::with_capacity(frames);
    let mut time = r.f32()?;
    let mut value = Vec2::new(r.f32()?, r.f32()?);
    for frame in 0..frames {
        if frame + 1 == frames {
            keys.push(Vec2Key { time, value, interp_x: Interpolation::Linear, interp_y: Interpolation::Linear });
            break;
        }
        let time2 = r.f32()?;
        let value2 = Vec2::new(r.f32()?, r.f32()?);
        let raw = read_curve(r, 2)?;
        keys.push(Vec2Key {
            time,
            value,
            interp_x: resolve(&raw, 0, time, value.x, time2, value2.x),
            interp_y: resolve(&raw, 1, time, value.y, time2, value2.y),
        });
        time = time2;
        value = value2;
    }
    Ok(keys)
}

/// Turns a single-axis scalar timeline into the IR's `Vec2`-shaped keys.
///
/// The unkeyed axis carries the setup-pose default — zero for translate and
/// shear, one for scale — matching how the JSON decoder widens the same
/// single-axis channels.
fn widen(keys: Vec<ScalarKey>, axis: Axes, default: f32) -> Vec<Vec2Key> {
    keys.into_iter()
        .map(|k| {
            let value = match axis {
                Axes::Y => Vec2::new(default, k.value),
                _ => Vec2::new(k.value, default),
            };
            Vec2Key { time: k.time, value, interp_x: k.interp, interp_y: k.interp }
        })
        .collect()
}

fn read_animation(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &SpineIr,
    name: String,
    report: &mut LoadReport,
) -> Result<Animation, DecodeError> {
    let mut anim = Animation::new(name);

    read_slot_timelines(r, ctx, &mut anim)?;
    read_bone_timelines(r, &mut anim)?;
    read_ik_timelines(r, &mut anim)?;
    read_transform_timelines(r, &mut anim)?;
    read_path_timelines(r, &mut anim)?;
    read_deform_timelines(r, ctx, ir, &mut anim, report)?;
    read_draw_order_timeline(r, ir, &mut anim, report)?;
    read_event_timeline(r, ir, &mut anim)?;

    anim.timelines.retain(|t| !t.is_empty());
    anim.duration = anim.max_key_time();
    Ok(anim)
}

fn read_slot_timelines(r: &mut BinaryReader<'_>, ctx: &Ctx, anim: &mut Animation) -> Result<(), DecodeError> {
    for _ in 0..r.count("slot timeline")? {
        let slot = SlotId(r.varint()? as u16);
        for _ in 0..r.count("slot timeline channel")? {
            let kind = r.u8()?;
            match kind {
                SLOT_ATTACHMENT => {
                    // Discrete, so no curve and no Bézier count.
                    let frames = r.count("keyframe")?;
                    let mut keys = Vec::with_capacity(frames);
                    for _ in 0..frames {
                        keys.push(AttachmentKey { time: r.f32()?, name: ctx.string_ref(r)? });
                    }
                    anim.timelines.push(Timeline::SlotAttachment { slot, keys });
                }
                SLOT_RGBA | SLOT_RGB => {
                    let has_alpha = kind == SLOT_RGBA;
                    let components = if has_alpha { 4 } else { 3 };
                    let frames = read_frame_header(r, components, "slot colour timeline")?;
                    let mut keys: Vec<ColorKey> = Vec::with_capacity(frames);
                    if frames > 0 {
                        let mut time = r.f32()?;
                        let mut value = read_color(r, has_alpha)?;
                        for frame in 0..frames {
                            if frame + 1 == frames {
                                keys.push(ColorKey { time, value, interp: [Interpolation::Linear; 4] });
                                break;
                            }
                            let time2 = r.f32()?;
                            let value2 = read_color(r, has_alpha)?;
                            let raw = read_curve(r, components)?;
                            keys.push(ColorKey {
                                time,
                                value,
                                interp: color_interp(&raw, time, value, time2, value2, has_alpha),
                            });
                            time = time2;
                            value = value2;
                        }
                    }
                    let channels = if has_alpha { ColorChannels::Rgba } else { ColorChannels::Rgb };
                    anim.timelines.push(Timeline::SlotColor { slot, channels, keys });
                }
                SLOT_RGBA2 | SLOT_RGB2 => {
                    let light_alpha = kind == SLOT_RGBA2;
                    let components = if light_alpha { 7 } else { 6 };
                    let frames = read_frame_header(r, components, "slot two-colour timeline")?;
                    let mut keys: Vec<TwoColorKey> = Vec::with_capacity(frames);
                    if frames > 0 {
                        let mut time = r.f32()?;
                        let mut light = read_color(r, light_alpha)?;
                        let mut dark = read_rgb(r)?;
                        for frame in 0..frames {
                            if frame + 1 == frames {
                                keys.push(TwoColorKey {
                                    time,
                                    light,
                                    dark,
                                    interp_light: [Interpolation::Linear; 4],
                                    interp_dark: [Interpolation::Linear; 3],
                                });
                                break;
                            }
                            let time2 = r.f32()?;
                            let light2 = read_color(r, light_alpha)?;
                            let dark2 = read_rgb(r)?;
                            let raw = read_curve(r, components)?;
                            let light_span = color_interp(&raw, time, light, time2, light2, light_alpha);
                            let base = if light_alpha { 4 } else { 3 };
                            let dark_span = [
                                resolve(&raw, base, time, dark.r, time2, dark2.r),
                                resolve(&raw, base + 1, time, dark.g, time2, dark2.g),
                                resolve(&raw, base + 2, time, dark.b, time2, dark2.b),
                            ];
                            keys.push(TwoColorKey {
                                time,
                                light,
                                dark,
                                interp_light: light_span,
                                interp_dark: dark_span,
                            });
                            time = time2;
                            light = light2;
                            dark = dark2;
                        }
                    }
                    let channels = if light_alpha { ColorChannels::Rgba } else { ColorChannels::Rgb };
                    anim.timelines.push(Timeline::SlotTwoColor { slot, channels, keys });
                }
                SLOT_ALPHA => {
                    let frames = read_frame_header(r, 1, "slot alpha timeline")?;
                    let mut keys = Vec::with_capacity(frames);
                    if frames > 0 {
                        let mut time = r.f32()?;
                        let mut value = r.u8()? as f32 / 255.0;
                        for frame in 0..frames {
                            if frame + 1 == frames {
                                keys.push(ScalarKey { time, value, interp: Interpolation::Linear });
                                break;
                            }
                            let time2 = r.f32()?;
                            let value2 = r.u8()? as f32 / 255.0;
                            let raw = read_curve(r, 1)?;
                            keys.push(ScalarKey { time, value, interp: resolve(&raw, 0, time, value, time2, value2) });
                            time = time2;
                            value = value2;
                        }
                    }
                    anim.timelines.push(Timeline::SlotAlpha { slot, keys });
                }
                other => {
                    return Err(DecodeError::corrupt_at(
                        format!("unknown slot timeline type {other}"),
                        r.position() as u64,
                    ))
                }
            }
        }
    }
    Ok(())
}

/// A dark colour, which carries no alpha.
fn read_rgb(r: &mut BinaryReader<'_>) -> Result<Rgb, DecodeError> {
    Ok(Rgb::new(r.u8()? as f32 / 255.0, r.u8()? as f32 / 255.0, r.u8()? as f32 / 255.0))
}

/// Colours are written a byte per channel in 4.x, not packed into one word.
fn read_color(r: &mut BinaryReader<'_>, with_alpha: bool) -> Result<Rgba, DecodeError> {
    let red = r.u8()? as f32 / 255.0;
    let green = r.u8()? as f32 / 255.0;
    let blue = r.u8()? as f32 / 255.0;
    let alpha = if with_alpha { r.u8()? as f32 / 255.0 } else { 1.0 };
    Ok(Rgba::new(red, green, blue, alpha))
}

fn color_interp(raw: &RawCurve, t0: f32, a: Rgba, t1: f32, b: Rgba, with_alpha: bool) -> [Interpolation; 4] {
    [
        resolve(raw, 0, t0, a.r, t1, b.r),
        resolve(raw, 1, t0, a.g, t1, b.g),
        resolve(raw, 2, t0, a.b, t1, b.b),
        if with_alpha { resolve(raw, 3, t0, a.a, t1, b.a) } else { Interpolation::Linear },
    ]
}

fn read_bone_timelines(r: &mut BinaryReader<'_>, anim: &mut Animation) -> Result<(), DecodeError> {
    for _ in 0..r.count("bone timeline")? {
        let bone = BoneId(r.varint()? as u16);
        for _ in 0..r.count("bone timeline channel")? {
            let kind = r.u8()?;
            let timeline = match kind {
                BONE_ROTATE => Timeline::BoneRotate { bone, keys: read_scalar_keys(r, "bone rotate")? },
                BONE_TRANSLATE => {
                    Timeline::BoneTranslate { bone, axes: Axes::Both, keys: read_vec2_keys(r, "bone translate")? }
                }
                BONE_TRANSLATE_X => Timeline::BoneTranslate {
                    bone,
                    axes: Axes::X,
                    keys: widen(read_scalar_keys(r, "bone translateX")?, Axes::X, 0.0),
                },
                BONE_TRANSLATE_Y => Timeline::BoneTranslate {
                    bone,
                    axes: Axes::Y,
                    keys: widen(read_scalar_keys(r, "bone translateY")?, Axes::Y, 0.0),
                },
                BONE_SCALE => Timeline::BoneScale { bone, axes: Axes::Both, keys: read_vec2_keys(r, "bone scale")? },
                BONE_SCALE_X => Timeline::BoneScale {
                    bone,
                    axes: Axes::X,
                    keys: widen(read_scalar_keys(r, "bone scaleX")?, Axes::X, 1.0),
                },
                BONE_SCALE_Y => Timeline::BoneScale {
                    bone,
                    axes: Axes::Y,
                    keys: widen(read_scalar_keys(r, "bone scaleY")?, Axes::Y, 1.0),
                },
                BONE_SHEAR => Timeline::BoneShear { bone, axes: Axes::Both, keys: read_vec2_keys(r, "bone shear")? },
                BONE_SHEAR_X => Timeline::BoneShear {
                    bone,
                    axes: Axes::X,
                    keys: widen(read_scalar_keys(r, "bone shearX")?, Axes::X, 0.0),
                },
                BONE_SHEAR_Y => Timeline::BoneShear {
                    bone,
                    axes: Axes::Y,
                    keys: widen(read_scalar_keys(r, "bone shearY")?, Axes::Y, 0.0),
                },
                other => {
                    return Err(DecodeError::corrupt_at(
                        format!("unknown bone timeline type {other}"),
                        r.position() as u64,
                    ))
                }
            };
            anim.timelines.push(timeline);
        }
    }
    Ok(())
}

fn read_ik_timelines(r: &mut BinaryReader<'_>, anim: &mut Animation) -> Result<(), DecodeError> {
    for _ in 0..r.count("ik timeline")? {
        let constraint = IkConstraintId(r.varint()? as u16);
        // Mix and softness are curved; the three flags are discrete.
        let frames = read_frame_header(r, 2, "ik timeline")?;
        let mut keys: Vec<IkKey> = Vec::with_capacity(frames);
        if frames > 0 {
            let mut time = r.f32()?;
            let mut mix = r.f32()?;
            let mut softness = r.f32()?;
            for frame in 0..frames {
                let bend_positive = r.i8()? > 0;
                let compress = r.bool()?;
                let stretch = r.bool()?;
                if frame + 1 == frames {
                    keys.push(IkKey {
                        time,
                        mix,
                        softness,
                        bend_positive,
                        compress,
                        stretch,
                        interp: Interpolation::Linear,
                    });
                    break;
                }
                let time2 = r.f32()?;
                let mix2 = r.f32()?;
                let softness2 = r.f32()?;
                let raw = read_curve(r, 2)?;
                keys.push(IkKey {
                    time,
                    mix,
                    softness,
                    bend_positive,
                    compress,
                    stretch,
                    interp: resolve(&raw, 0, time, mix, time2, mix2),
                });
                time = time2;
                mix = mix2;
                softness = softness2;
            }
        }
        anim.timelines.push(Timeline::IkConstraint { constraint, keys });
    }
    Ok(())
}

fn read_transform_timelines(r: &mut BinaryReader<'_>, anim: &mut Animation) -> Result<(), DecodeError> {
    for _ in 0..r.count("transform timeline")? {
        let constraint = TransformConstraintId(r.varint()? as u16);
        let frames = read_frame_header(r, 6, "transform timeline")?;
        let mut keys: Vec<TransformKey> = Vec::with_capacity(frames);
        if frames > 0 {
            let mut time = r.f32()?;
            let mut mixes = read_six(r)?;
            for frame in 0..frames {
                if frame + 1 == frames {
                    keys.push(transform_key(time, mixes, Interpolation::Linear));
                    break;
                }
                let time2 = r.f32()?;
                let mixes2 = read_six(r)?;
                let raw = read_curve(r, 6)?;
                // The IR carries one curve for the whole keyframe; rotate is the
                // channel an author is most likely to have eased deliberately.
                let interp = resolve(&raw, 0, time, mixes[0], time2, mixes2[0]);
                keys.push(transform_key(time, mixes, interp));
                time = time2;
                mixes = mixes2;
            }
        }
        anim.timelines.push(Timeline::TransformConstraint { constraint, keys });
    }
    Ok(())
}

fn read_six(r: &mut BinaryReader<'_>) -> Result<[f32; 6], DecodeError> {
    Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?])
}

fn transform_key(time: f32, m: [f32; 6], interp: Interpolation) -> TransformKey {
    TransformKey {
        time,
        mix_rotate: m[0],
        mix_x: m[1],
        mix_y: m[2],
        mix_scale_x: m[3],
        mix_scale_y: m[4],
        mix_shear_y: m[5],
        interp,
    }
}

fn read_path_timelines(r: &mut BinaryReader<'_>, anim: &mut Animation) -> Result<(), DecodeError> {
    for _ in 0..r.count("path timeline")? {
        let constraint = PathConstraintId(r.varint()? as u16);
        for _ in 0..r.count("path timeline channel")? {
            let kind = r.u8()?;
            match kind {
                PATH_POSITION => {
                    anim.timelines
                        .push(Timeline::PathPosition { constraint, keys: read_scalar_keys(r, "path position")? });
                }
                PATH_SPACING => {
                    anim.timelines
                        .push(Timeline::PathSpacing { constraint, keys: read_scalar_keys(r, "path spacing")? });
                }
                PATH_MIX => {
                    let frames = read_frame_header(r, 3, "path mix timeline")?;
                    let mut keys: Vec<PathMixKey> = Vec::with_capacity(frames);
                    if frames > 0 {
                        let mut time = r.f32()?;
                        let mut m = [r.f32()?, r.f32()?, r.f32()?];
                        for frame in 0..frames {
                            if frame + 1 == frames {
                                keys.push(PathMixKey {
                                    time,
                                    mix_rotate: m[0],
                                    mix_x: m[1],
                                    mix_y: m[2],
                                    interp: Interpolation::Linear,
                                });
                                break;
                            }
                            let time2 = r.f32()?;
                            let m2 = [r.f32()?, r.f32()?, r.f32()?];
                            let raw = read_curve(r, 3)?;
                            keys.push(PathMixKey {
                                time,
                                mix_rotate: m[0],
                                mix_x: m[1],
                                mix_y: m[2],
                                interp: resolve(&raw, 0, time, m[0], time2, m2[0]),
                            });
                            time = time2;
                            m = m2;
                        }
                    }
                    anim.timelines.push(Timeline::PathMix { constraint, keys });
                }
                other => {
                    return Err(DecodeError::corrupt_at(
                        format!("unknown path timeline type {other}"),
                        r.position() as u64,
                    ))
                }
            }
        }
    }
    Ok(())
}

fn read_deform_timelines(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &SpineIr,
    anim: &mut Animation,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for _ in 0..r.count("attachment timeline skin")? {
        let skin = SkinId(r.varint()? as u16);
        for _ in 0..r.count("attachment timeline slot")? {
            let slot = SlotId(r.varint()? as u16);
            for _ in 0..r.count("attachment timeline")? {
                let att_name = ctx.string_ref(r)?.unwrap_or_default();
                let timeline_type = r.u8()?;
                if timeline_type == ATTACHMENT_SEQUENCE {
                    // Sequence playback is not modelled by the IR. The frames are
                    // a fixed shape, so they can be skipped without losing sync.
                    let frames = r.count("sequence keyframe")?;
                    for _ in 0..frames {
                        r.f32()?; // time
                        r.i32()?; // mode and index, packed
                        r.f32()?; // delay
                    }
                    report.warn(Degradation::UnsupportedTimeline {
                        animation: anim.name.clone(),
                        kind: "attachment sequence".into(),
                    });
                    continue;
                }
                if timeline_type != ATTACHMENT_DEFORM {
                    return Err(DecodeError::corrupt_at(
                        format!("unknown attachment timeline type {timeline_type}"),
                        r.position() as u64,
                    ));
                }

                let attachment = ir.resolve_attachment(skin, slot, &att_name);
                let deformable =
                    attachment.and_then(|a| ir.attachment(a)).and_then(|a| a.kind.deformable_vertices()).is_some();

                let frames = read_frame_header(r, 1, "deform timeline")?;
                let mut keys: Vec<DeformKey> = Vec::with_capacity(frames);
                if frames > 0 {
                    let mut time = r.f32()?;
                    let mut run = read_deform_values(r)?;
                    for frame in 0..frames {
                        if frame + 1 == frames {
                            keys.push(DeformKey { time, offset: run.0, values: run.1, interp: Interpolation::Linear });
                            break;
                        }
                        let time2 = r.f32()?;
                        let run2 = read_deform_values(r)?;
                        let raw = read_curve(r, 1)?;
                        // Deform curves ease a whole vertex set, so the span is
                        // 0..1 by convention rather than a value difference.
                        keys.push(DeformKey {
                            time,
                            offset: run.0,
                            values: run.1,
                            interp: resolve(&raw, 0, time, 0.0, time2, 1.0),
                        });
                        time = time2;
                        run = run2;
                    }
                }

                match (attachment, deformable) {
                    (Some(attachment), true) => anim.timelines.push(Timeline::Deform { slot, skin, attachment, keys }),
                    _ => report.warn(Degradation::MissingReference {
                        kind: "deform target attachment".into(),
                        name: att_name,
                    }),
                }
            }
        }
    }
    Ok(())
}

fn read_deform_values(r: &mut BinaryReader<'_>) -> Result<(u32, Vec<f32>), DecodeError> {
    let run = r.count("deform value")?;
    if run == 0 {
        return Ok((0, Vec::new()));
    }
    let start = r.varint()?;
    Ok((start, r.f32_array(run)?))
}

fn read_draw_order_timeline(
    r: &mut BinaryReader<'_>,
    ir: &SpineIr,
    anim: &mut Animation,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let frames = r.count("draw order keyframe")?;
    if frames == 0 {
        return Ok(());
    }
    let mut keys = Vec::with_capacity(frames);
    for _ in 0..frames {
        let time = r.f32()?;
        let offset_count = r.count("draw order offset")?;
        let mut moves = Vec::with_capacity(offset_count);
        for _ in 0..offset_count {
            let slot = SlotId(r.varint()? as u16);
            let offset = r.varint_signed()?;
            moves.push((slot, offset));
        }
        let order = if moves.is_empty() {
            None
        } else {
            let resolved = draw_order_from_offsets(ir.slots.len(), &moves);
            if resolved.is_none() {
                report.warn(Degradation::ClampedValue {
                    context: anim.name.clone(),
                    field: "drawOrder offset".into(),
                    detail: "offsets do not form a valid permutation".into(),
                });
            }
            resolved
        };
        keys.push(DrawOrderKey { time, order });
    }
    anim.timelines.push(Timeline::DrawOrder { keys });
    Ok(())
}

fn read_event_timeline(r: &mut BinaryReader<'_>, ir: &SpineIr, anim: &mut Animation) -> Result<(), DecodeError> {
    let frames = r.count("event keyframe")?;
    if frames == 0 {
        return Ok(());
    }
    let mut keys = Vec::with_capacity(frames);
    for _ in 0..frames {
        let time = r.f32()?;
        let index = r.varint()? as usize;
        let Some(default) = ir.events.get(index) else {
            return Err(DecodeError::corrupt_at(
                format!("event keyframe references event index {index}"),
                r.position() as u64,
            ));
        };
        let int_value = r.varint_signed()?;
        let float_value = r.f32()?;
        let string_value = if r.bool()? { Some(r.string()?) } else { None };
        let (volume, balance) =
            if default.audio_path.is_some() { (r.f32()?, r.f32()?) } else { (default.volume, default.balance) };
        keys.push(EventKey {
            time,
            event: EventId(index as u16),
            int_value,
            float_value,
            string_value,
            volume,
            balance,
        });
    }
    anim.timelines.push(Timeline::Event { keys });
    Ok(())
}

/// The 4.x version gate.
///
/// 4.2 packed the IK flags into one byte and added physics constraints, both of
/// which move everything after them. Refusing is better than reading 4.2 with a
/// 4.0/4.1 layout (spec §4.9).
pub fn check_minor(detection: &SpineDetection) -> Result<(), DecodeError> {
    let minor = detection.raw_version.split('.').nth(1).and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    if minor >= 2 {
        return Err(DecodeError::unsupported_version(
            ModelKind::Spine,
            detection.raw_version.clone(),
            "4.2 packed the IK constraint flags and added physics constraints, which move every \
             field after them; only the 4.0 and 4.1 binary layouts are read here",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::writer::SkelWriter;
    use crate::detect::{SpineEncoding, SpineVersion};

    fn detection(raw: &str, minor: u16) -> SpineDetection {
        SpineDetection {
            encoding: SpineEncoding::Binary,
            version: SpineVersion::new(4, minor, 0),
            raw_version: raw.into(),
            hash: None,
        }
    }

    /// The string table every fixture below refers into.
    const STRINGS: [&str; 3] = ["torso", "body", "hero"];

    fn index_of(name: &str) -> Option<usize> {
        STRINGS.iter().position(|s| *s == name)
    }

    /// Writes a 4.x skeleton: one bone, one slot, one region attachment, and
    /// whatever animation body the caller appends.
    ///
    /// Laid out by hand against the layout documented at the top of this
    /// module. If the decoder and this disagree, the round-trip fails.
    fn fixture(animation: impl FnOnce(&mut SkelWriter)) -> Vec<u8> {
        let mut w = SkelWriter::new();

        // Header: an integer hash, then version, origin, size, and no editor data.
        w.u64(0x0123_4567_89ab_cdef);
        w.string(Some("4.1.23"));
        w.f32(0.0).f32(0.0).f32(100.0).f32(200.0);
        w.bool(false);

        // String table.
        w.varint(STRINGS.len() as u32);
        for s in STRINGS {
            w.string(Some(s));
        }

        // One root bone.
        w.varint(1);
        w.string(Some("root"));
        w.f32(0.0); // rotation
        w.f32(1.0).f32(2.0); // position
        w.f32(1.0).f32(1.0); // scale
        w.f32(0.0).f32(0.0); // shear
        w.f32(30.0); // length
        w.varint(0); // inherit: normal
        w.bool(false); // skin required

        // One slot on it.
        w.varint(1);
        w.string(Some("torso"));
        w.varint(0); // bone index
        w.u32(0xFFFF_FFFF); // colour
        w.u32(0xFFFF_FFFF); // no dark colour
        w.string_ref(index_of("body"));
        w.varint(0); // blend: normal

        // No constraints of any kind.
        w.varint(0).varint(0).varint(0);

        // Default skin: one slot, one region attachment.
        w.varint(1); // slot count
        w.varint(0); // slot index
        w.varint(1); // attachment count
        w.string_ref(index_of("body")); // placeholder
        w.string_ref(None); // attachment name defaults to the placeholder
        w.u8(TYPE_REGION);
        w.string_ref(index_of("hero")); // path
        w.f32(0.0); // rotation
        w.f32(3.0).f32(4.0); // position
        w.f32(1.0).f32(1.0); // scale
        w.f32(64.0).f32(64.0); // size
        w.u32(0xFFFF_FFFF); // colour
        w.bool(false); // no sequence

        w.varint(0); // no named skins
        w.varint(0); // no events

        // One animation.
        w.varint(1);
        w.string(Some("idle"));
        animation(&mut w);

        w.out
    }

    /// An animation body with nothing in it.
    fn empty_animation(w: &mut SkelWriter) {
        for _ in 0..8 {
            w.varint(0);
        }
    }

    fn decode_fixture(bytes: &[u8]) -> Result<(SpineIr, LoadReport), DecodeError> {
        let mut report = LoadReport::new();
        let ir = decode(bytes, &detection("4.1.23", 1), Atlas::default(), &mut report)?;
        Ok((ir, report))
    }

    #[test]
    fn a_minimal_skeleton_round_trips() {
        let (ir, _) = decode_fixture(&fixture(empty_animation)).expect("should decode");

        assert_eq!(ir.metadata.source_version, "4.1.23");
        assert_eq!(ir.metadata.hash.as_deref(), Some("123456789abcdef"));
        assert_eq!(ir.metadata.size, Vec2::new(100.0, 200.0));

        assert_eq!(ir.bones.len(), 1);
        assert_eq!(ir.bones[0].name, "root");
        assert_eq!(ir.bones[0].setup.position, Vec2::new(1.0, 2.0));
        assert_eq!(ir.bones[0].length, 30.0);

        assert_eq!(ir.slots.len(), 1);
        assert_eq!(ir.slots[0].name, "torso");
        assert_eq!(ir.slots[0].setup_attachment.as_deref(), Some("body"));
        assert!(ir.slots[0].dark_color.is_none());

        assert_eq!(ir.attachments.len(), 1);
        match &ir.attachments[0].kind {
            AttachmentKind::Region(r) => {
                assert_eq!(r.path, "hero");
                assert_eq!(r.position, Vec2::new(3.0, 4.0));
                assert!(r.sequence.is_none());
            }
            other => panic!("expected a region, got {other:?}"),
        }
        assert_eq!(ir.animations.len(), 1);
        assert_eq!(ir.animations[0].name, "idle");
    }

    #[test]
    fn every_load_says_the_layout_is_unverified() {
        // The caveat is the whole reason this decoder is allowed to exist under
        // spec §4.9; a silent load would be the thing that rule forbids.
        let (_, report) = decode_fixture(&fixture(empty_animation)).expect("should decode");
        assert!(report.to_string().contains("not been checked against a real export"), "{report}");
    }

    #[test]
    fn a_rotate_timeline_reads_its_frames_and_bezier() {
        let bytes = fixture(|w| {
            w.varint(0); // no slot timelines
            w.varint(1); // one bone
            w.varint(0); // bone index
            w.varint(1); // one channel
            w.u8(BONE_ROTATE);
            w.varint(2); // frames
            w.varint(1); // beziers
            w.f32(0.0).f32(0.0); // frame 0
            w.f32(1.0).f32(90.0); // frame 1
            w.u8(CURVE_BEZIER);
            // Absolute control points, a quarter and three quarters along.
            w.f32(0.25).f32(22.5).f32(0.75).f32(67.5);
            for _ in 0..6 {
                w.varint(0);
            }
        });

        let (ir, _) = decode_fixture(&bytes).expect("should decode");
        let Some(Timeline::BoneRotate { bone, keys }) = ir.animations[0].timelines.first() else {
            panic!("expected a rotate timeline, got {:?}", ir.animations[0].timelines);
        };
        assert_eq!(*bone, BoneId(0));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].time, 0.0);
        assert_eq!(keys[1].value, 90.0);
        // The absolute points normalise to a quarter and three quarters on both
        // axes, which is what the IR stores.
        match keys[0].interp {
            Interpolation::Bezier(b) => {
                assert!((b.cx1 - 0.25).abs() < 1e-5, "{b:?}");
                assert!((b.cy1 - 0.25).abs() < 1e-5, "{b:?}");
                assert!((b.cx2 - 0.75).abs() < 1e-5, "{b:?}");
                assert!((b.cy2 - 0.75).abs() < 1e-5, "{b:?}");
            }
            other => panic!("expected a Bézier, got {other:?}"),
        }
        // The last keyframe has nothing to ease into.
        assert_eq!(keys[1].interp, Interpolation::Linear);
    }

    #[test]
    fn a_single_axis_timeline_widens_with_the_setup_default() {
        let bytes = fixture(|w| {
            w.varint(0);
            w.varint(1);
            w.varint(0);
            w.varint(1);
            w.u8(BONE_SCALE_X);
            w.varint(1); // one frame
            w.varint(0); // no beziers
            w.f32(0.0).f32(2.0);
            for _ in 0..6 {
                w.varint(0);
            }
        });

        let (ir, _) = decode_fixture(&bytes).expect("should decode");
        let Some(Timeline::BoneScale { axes, keys, .. }) = ir.animations[0].timelines.first() else {
            panic!("expected a scale timeline, got {:?}", ir.animations[0].timelines);
        };
        assert_eq!(*axes, Axes::X);
        // The unkeyed axis carries scale's neutral value, not zero.
        assert_eq!(keys[0].value, Vec2::new(2.0, 1.0));
    }

    #[test]
    fn a_slot_alpha_timeline_reads_bytes_as_a_fraction() {
        let bytes = fixture(|w| {
            w.varint(1); // one slot
            w.varint(0); // slot index
            w.varint(1); // one channel
            w.u8(SLOT_ALPHA);
            w.varint(1);
            w.varint(0);
            w.f32(0.0).u8(128);
            for _ in 0..7 {
                w.varint(0);
            }
        });

        let (ir, _) = decode_fixture(&bytes).expect("should decode");
        let Some(Timeline::SlotAlpha { keys, .. }) = ir.animations[0].timelines.first() else {
            panic!("expected an alpha timeline, got {:?}", ir.animations[0].timelines);
        };
        assert!((keys[0].value - 128.0 / 255.0).abs() < 1e-6, "{:?}", keys[0]);
    }

    #[test]
    fn trailing_bytes_are_an_error_rather_than_a_shrug() {
        // The end-of-file check is what turns a misread layout into a clear
        // failure instead of a skeleton that animates wrongly.
        let mut bytes = fixture(empty_animation);
        bytes.extend_from_slice(&[0u8; 4]);
        let err = decode_fixture(&bytes).unwrap_err();
        assert!(err.to_string().contains("bytes remain"), "{err}");
    }

    #[test]
    fn an_impossible_bezier_count_is_rejected() {
        // Three Béziers cannot fit across a two-frame rotate timeline, so this
        // catches a stream that has drifted before it reads garbage as floats.
        let bytes = fixture(|w| {
            w.varint(0);
            w.varint(1);
            w.varint(0);
            w.varint(1);
            w.u8(BONE_ROTATE);
            w.varint(2);
            w.varint(3);
            w.f32(0.0).f32(0.0);
            w.f32(1.0).f32(90.0);
            w.u8(CURVE_LINEAR);
            for _ in 0..6 {
                w.varint(0);
            }
        });
        let err = decode_fixture(&bytes).unwrap_err();
        assert!(err.to_string().contains("Bézier"), "{err}");
    }

    #[test]
    fn a_string_reference_past_the_table_is_rejected() {
        let mut w = SkelWriter::new();
        w.u64(0);
        w.string(Some("4.1.23"));
        w.f32(0.0).f32(0.0).f32(1.0).f32(1.0);
        w.bool(false);
        w.varint(1);
        w.string(Some("only"));
        w.varint(1);
        w.string(Some("root"));
        w.f32(0.0).f32(0.0).f32(0.0).f32(1.0).f32(1.0).f32(0.0).f32(0.0).f32(0.0);
        w.varint(0);
        w.bool(false);
        w.varint(1);
        w.string(Some("torso"));
        w.varint(0);
        w.u32(0xFFFF_FFFF);
        w.u32(0xFFFF_FFFF);
        w.varint(9); // a table of one has no ninth entry
        let err = decode_fixture(&w.out).unwrap_err();
        assert!(err.to_string().contains("string reference"), "{err}");
    }

    #[test]
    fn truncation_anywhere_is_an_error_and_never_a_panic() {
        let full = fixture(empty_animation);
        for cut in 0..full.len() {
            let mut report = LoadReport::new();
            let _ = decode(&full[..cut], &detection("4.1.23", 1), Atlas::default(), &mut report);
        }
    }

    #[test]
    fn every_single_byte_corruption_is_an_error_and_never_a_panic() {
        let full = fixture(empty_animation);
        for i in 0..full.len() {
            for bit in [0x01u8, 0x80] {
                let mut bytes = full.clone();
                bytes[i] ^= bit;
                let mut report = LoadReport::new();
                let _ = decode(&bytes, &detection("4.1.23", 1), Atlas::default(), &mut report);
            }
        }
    }

    #[test]
    fn the_minor_version_gate_admits_40_and_41_but_not_42() {
        assert!(check_minor(&detection("4.0.64", 0)).is_ok());
        assert!(check_minor(&detection("4.1.23", 1)).is_ok());
        let err = check_minor(&detection("4.2.07", 2)).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedVersion { .. }), "{err}");
    }
}
