//! Spine 3.x binary skeleton decoder.
//!
//! Layout notes that are easy to get wrong, recorded here because the file
//! format is undocumented and this is the only place that knowledge may live:
//!
//! * A **string table** sits between the header and the bones. Most names in
//!   the rest of the file are one-based indices into it, where 0 means `null`.
//! * The `nonessential` header flag turns on editor-only fields scattered
//!   through the file (bone colours, mesh edges, attachment debug colours).
//!   Skipping them at the wrong place desynchronises everything after.
//! * Deform keyframes store **offsets** on disk for both rigid and weighted
//!   meshes. The reference runtime folds setup positions into rigid meshes at
//!   load time; this decoder does not, because the IR keeps offsets and the
//!   runtime adds the setup pose during evaluation.
//! * Version `3.8.75` was a broken export that the official runtimes refuse.

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::ids::{
    AttachmentId, BoneId, EventId, IkConstraintId, PathConstraintId, SkinId, SlotId, TransformConstraintId,
};
use a2d_core::ir::spine::{
    Animation, Attachment, AttachmentKey, AttachmentKind, Axes, Bone, BoneLocal, BoundingBoxAttachment,
    ClippingAttachment, ColorChannels, ColorKey, DeformKey, DrawOrderKey, EventData, EventKey, IkConstraint, IkKey,
    LinkedMesh, MeshAttachment, PathAttachment, PathConstraint, PathMixKey, PathPositionMode, PathRotateMode,
    PathSpacingMode, PointAttachment, RegionAttachment, ScalarKey, SkeletonMetadata, Skin, SkinEntry, Slot, SpineIr,
    Timeline, TransformConstraint, TransformInherit, TransformKey, TwoColorKey, Vec2Key, VertexData,
};
use a2d_core::math::Bezier;
use a2d_core::{BlendMode, DecodeError, Degradation, Interpolation, LoadReport, ModelKind, Rgba, Vec2};

use crate::detect::SpineDetection;
use crate::json::animation::draw_order_from_offsets;
use crate::json::attachment::parse_vertices;
use crate::reader::BinaryReader;

const CURVE_LINEAR: u8 = 0;
const CURVE_STEPPED: u8 = 1;
const CURVE_BEZIER: u8 = 2;

/// Decodes a Spine 3.x binary skeleton.
pub fn decode(
    bytes: &[u8],
    detection: &SpineDetection,
    atlas: Atlas,
    report: &mut LoadReport,
) -> Result<SpineIr, DecodeError> {
    if detection.raw_version == "3.8.75" {
        return Err(DecodeError::unsupported_version(
            ModelKind::Spine,
            "3.8.75",
            "this exporter build produced skeletons the official runtimes also refuse; re-export from 3.8.76+",
        ));
    }

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
        self.strings
            .get(index - 1)
            .cloned()
            .ok_or_else(|| DecodeError::corrupt_at(format!("string reference {index} is out of range"), 0))
    }
}

fn read_header(r: &mut BinaryReader<'_>, detection: &SpineDetection, ir: &mut SpineIr) -> Result<bool, DecodeError> {
    let hash = r.string_opt()?;
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
    let mut out = Vec::with_capacity(n);
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
        // The root has no parent field at all; every other bone stores an index
        // into the bones already read.
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
        // 0xFFFFFFFF is the sentinel for "no dark colour".
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
    let n = r.count("constrained bone")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let index = r.varint()? as usize;
        out.push(BoneId::from_index(index).filter(|b| b.index() < ir.bones.len()).ok_or_else(|| {
            DecodeError::corrupt_at(format!("constraint references bone index {index}"), r.position() as u64)
        })?);
    }
    Ok(out)
}

fn read_bone_ref(r: &mut BinaryReader<'_>, ir: &SpineIr) -> Result<BoneId, DecodeError> {
    let index = r.varint()? as usize;
    BoneId::from_index(index)
        .filter(|b| b.index() < ir.bones.len())
        .ok_or_else(|| DecodeError::corrupt_at(format!("bone index {index} out of range"), r.position() as u64))
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
        // Stored as a signed direction rather than a boolean.
        let bend_positive = r.i8()? > 0;
        let compress = r.bool()?;
        let stretch = r.bool()?;
        let uniform = r.bool()?;
        if bones.len() > 2 {
            report.warn(Degradation::UnsupportedConstraint {
                name: name.clone(),
                kind: format!("ik with a {}-bone chain", bones.len()),
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
        // Spine 3.x has one mix per channel group; the IR uses the 4.x per-axis
        // shape, so each group is duplicated across its axes.
        let mix_rotate = r.f32()?;
        let mix_translate = r.f32()?;
        let mix_scale = r.f32()?;
        let mix_shear = r.f32()?;
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
            mix_x: mix_translate,
            mix_y: mix_translate,
            mix_scale_x: mix_scale,
            mix_scale_y: mix_scale,
            mix_shear_y: mix_shear,
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
            _ => PathPositionMode::Percent,
        };
        let spacing_mode = match r.varint()? {
            0 => PathSpacingMode::Length,
            1 => PathSpacingMode::Fixed,
            _ => PathSpacingMode::Percent,
        };
        let rotate_mode = match r.varint()? {
            0 => PathRotateMode::Tangent,
            1 => PathRotateMode::Chain,
            _ => PathRotateMode::ChainScale,
        };
        let offset_rotation = r.f32()?;
        let position = r.f32()?;
        let spacing = r.f32()?;
        let mix_rotate = r.f32()?;
        let mix_translate = r.f32()?;
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
            mix_x: mix_translate,
            mix_y: mix_translate,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- skins

fn read_skins(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    // The default skin is always index 0 in the IR, even when the file has none.
    ir.skins.push(Skin::new("default"));
    let mut pending_clips: Vec<(AttachmentId, usize)> = Vec::new();

    // Default skin: a bare slot count, where zero means "no default skin".
    let default_slot_count = r.count("default skin slot")?;
    if default_slot_count > 0 {
        read_skin_body(r, ctx, ir, SkinId(0), default_slot_count, &mut pending_clips, report)?;
    }

    let n = r.count("skin")?;
    for _ in 0..n {
        let name = ctx.string_ref(r)?.unwrap_or_default();
        let id = SkinId::from_index(ir.skins.len())
            .ok_or_else(|| DecodeError::corrupt("skeleton has more skins than the skin handle can address"))?;
        ir.skins.push(Skin::new(name));

        let bones = read_bone_indices(r, ir)?;
        ir.skins[id.index()].bones = bones;
        // Skin-scoped constraint lists: indices only, recorded as unsupported.
        for (kind, len) in [
            ("ik", ir.ik_constraints.len()),
            ("transform", ir.transform_constraints.len()),
            ("path", ir.path_constraints.len()),
        ] {
            let count = r.count("skin constraint")?;
            for _ in 0..count {
                let _ = r.varint()?;
            }
            if count > 0 {
                let _ = len;
                report.warn(Degradation::UnsupportedConstraint {
                    name: ir.skins[id.index()].name.clone(),
                    kind: format!("skin-scoped {kind}"),
                });
            }
        }

        let slot_count = r.count("skin slot")?;
        read_skin_body(r, ctx, ir, id, slot_count, &mut pending_clips, report)?;
    }

    for (id, end_index) in pending_clips {
        let end = SlotId::from_index(end_index).filter(|s| s.index() < ir.slots.len());
        match end {
            Some(end) => {
                if let Some(Attachment { kind: AttachmentKind::Clipping(c), .. }) = ir.attachments.get_mut(id.index()) {
                    c.end_slot = Some(end);
                }
            }
            None => report
                .warn(Degradation::MissingReference { kind: "clipping end slot".into(), name: end_index.to_string() }),
        }
    }
    Ok(())
}

fn read_skin_body(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &mut SpineIr,
    skin: SkinId,
    slot_count: usize,
    pending_clips: &mut Vec<(AttachmentId, usize)>,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for _ in 0..slot_count {
        let slot_index = r.varint()? as usize;
        let slot = SlotId::from_index(slot_index).filter(|s| s.index() < ir.slots.len()).ok_or_else(|| {
            DecodeError::corrupt_at(format!("skin references slot index {slot_index}"), r.position() as u64)
        })?;
        let entries = r.count("skin entry")?;
        for _ in 0..entries {
            let placeholder = ctx.string_ref(r)?.unwrap_or_default();
            let (attachment, clip_end) = read_attachment(r, ctx, ir, slot, &placeholder, report)?;
            let id = AttachmentId::from_index(ir.attachments.len()).ok_or_else(|| {
                DecodeError::corrupt("skeleton has more attachments than the attachment handle can address")
            })?;
            if let Some(end) = clip_end {
                pending_clips.push((id, end));
            }
            ir.attachments.push(attachment);
            ir.skins[skin.index()].entries.push(SkinEntry { slot, name: placeholder, attachment: id });
        }
    }
    Ok(())
}

/// Attachment type ordinals, in the order the 3.x enum declares them.
const TYPE_REGION: u8 = 0;
const TYPE_BOUNDING_BOX: u8 = 1;
const TYPE_MESH: u8 = 2;
const TYPE_LINKED_MESH: u8 = 3;
const TYPE_PATH: u8 = 4;
const TYPE_POINT: u8 = 5;
const TYPE_CLIPPING: u8 = 6;

/// Returns the attachment plus, for clipping attachments, its end slot index.
fn read_attachment(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &SpineIr,
    slot: SlotId,
    placeholder: &str,
    report: &mut LoadReport,
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
            AttachmentKind::Region(RegionAttachment {
                path,
                region: None,
                position,
                rotation,
                scale,
                size,
                color,
                sequence: None,
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
                sequence: None,
            })
        }
        TYPE_LINKED_MESH => {
            let path = ctx.string_ref(r)?.unwrap_or_else(|| name.clone());
            let color = Rgba::from_rgba8888(r.u32()?);
            let skin = ctx.string_ref(r)?;
            let parent = ctx.string_ref(r)?.unwrap_or_default();
            let inherit_timelines = r.bool()?;
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
                sequence: None,
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

    let _ = (ir, report);
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

/// Reads the rigid-or-weighted vertex block.
///
/// Unlike JSON, the binary format states which layout follows, so there is no
/// length heuristic. The weighted branch is re-flattened into the JSON encoding
/// and handed to the shared parser, so both decoders build identical IR.
fn read_vertices_with_count(
    r: &mut BinaryReader<'_>,
    vertex_count: usize,
    context: &str,
) -> Result<VertexData, DecodeError> {
    let weighted = r.bool()?;
    if !weighted {
        let flat = r.f32_array(vertex_count * 2)?;
        return parse_vertices(&flat, vertex_count, context);
    }

    let mut flat = Vec::with_capacity(vertex_count * 8);
    for _ in 0..vertex_count {
        let bone_count = r.count("vertex influence")?;
        flat.push(bone_count as f32);
        for _ in 0..bone_count {
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
        // Volume and balance are only written when the event carries audio.
        let (volume, balance) = if audio_path.is_some() { (r.f32()?, r.f32()?) } else { (1.0, 0.0) };
        ir.events.push(EventData { name, int_value, float_value, string_value, audio_path, volume, balance });
    }
    // Animations reference events by their file order, so the order must be
    // preserved here; the IR's own sort happens on names elsewhere.
    Ok(())
}

// ---------------------------------------------------------------- animations

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

fn read_animation(
    r: &mut BinaryReader<'_>,
    ctx: &Ctx,
    ir: &SpineIr,
    name: String,
    report: &mut LoadReport,
) -> Result<Animation, DecodeError> {
    let mut anim = Animation::new(name);

    // Slot timelines.
    for _ in 0..r.count("slot timeline")? {
        let slot = SlotId(r.varint()? as u16);
        for _ in 0..r.count("slot timeline channel")? {
            let kind = r.u8()?;
            let frames = r.count("keyframe")?;
            match kind {
                0 => {
                    let mut keys = Vec::with_capacity(frames);
                    for _ in 0..frames {
                        keys.push(AttachmentKey { time: r.f32()?, name: ctx.string_ref(r)? });
                    }
                    anim.timelines.push(Timeline::SlotAttachment { slot, keys });
                }
                1 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let value = Rgba::from_rgba8888(r.u32()?);
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(ColorKey { time, value, interp: [interp; 4] });
                    }
                    anim.timelines.push(Timeline::SlotColor { slot, channels: ColorChannels::Rgba, keys });
                }
                2 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let light = Rgba::from_rgba8888(r.u32()?);
                        let dark = Rgba::rgb_from_rgb888(r.u32()?);
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(TwoColorKey {
                            time,
                            light,
                            dark,
                            interp_light: [interp; 4],
                            interp_dark: [interp; 3],
                        });
                    }
                    anim.timelines.push(Timeline::SlotTwoColor { slot, channels: ColorChannels::Rgba, keys });
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

    // Bone timelines.
    for _ in 0..r.count("bone timeline")? {
        let bone = BoneId(r.varint()? as u16);
        for _ in 0..r.count("bone timeline channel")? {
            let kind = r.u8()?;
            let frames = r.count("keyframe")?;
            match kind {
                0 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let value = r.f32()?;
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(ScalarKey { time, value, interp });
                    }
                    anim.timelines.push(Timeline::BoneRotate { bone, keys });
                }
                1..=3 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let value = Vec2::new(r.f32()?, r.f32()?);
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(Vec2Key::shared(time, value, interp));
                    }
                    anim.timelines.push(match kind {
                        1 => Timeline::BoneTranslate { bone, axes: Axes::Both, keys },
                        2 => Timeline::BoneScale { bone, axes: Axes::Both, keys },
                        _ => Timeline::BoneShear { bone, axes: Axes::Both, keys },
                    });
                }
                other => {
                    return Err(DecodeError::corrupt_at(
                        format!("unknown bone timeline type {other}"),
                        r.position() as u64,
                    ))
                }
            }
        }
    }

    // IK constraint timelines.
    for _ in 0..r.count("ik timeline")? {
        let constraint = IkConstraintId(r.varint()? as u16);
        let frames = r.count("keyframe")?;
        let mut keys = Vec::with_capacity(frames);
        for i in 0..frames {
            let time = r.f32()?;
            let mix = r.f32()?;
            let softness = r.f32()?;
            let bend_positive = r.i8()? > 0;
            let compress = r.bool()?;
            let stretch = r.bool()?;
            let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
            keys.push(IkKey { time, mix, softness, bend_positive, compress, stretch, interp });
        }
        anim.timelines.push(Timeline::IkConstraint { constraint, keys });
    }

    // Transform constraint timelines.
    for _ in 0..r.count("transform timeline")? {
        let constraint = TransformConstraintId(r.varint()? as u16);
        let frames = r.count("keyframe")?;
        let mut keys = Vec::with_capacity(frames);
        for i in 0..frames {
            let time = r.f32()?;
            let mix_rotate = r.f32()?;
            let mix_translate = r.f32()?;
            let mix_scale = r.f32()?;
            let mix_shear = r.f32()?;
            let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
            keys.push(TransformKey {
                time,
                mix_rotate,
                mix_x: mix_translate,
                mix_y: mix_translate,
                mix_scale_x: mix_scale,
                mix_scale_y: mix_scale,
                mix_shear_y: mix_shear,
                interp,
            });
        }
        anim.timelines.push(Timeline::TransformConstraint { constraint, keys });
    }

    // Path constraint timelines.
    for _ in 0..r.count("path timeline")? {
        let constraint = PathConstraintId(r.varint()? as u16);
        for _ in 0..r.count("path timeline channel")? {
            let kind = r.u8()?;
            let frames = r.count("keyframe")?;
            match kind {
                0 | 1 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let value = r.f32()?;
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(ScalarKey { time, value, interp });
                    }
                    anim.timelines.push(if kind == 0 {
                        Timeline::PathPosition { constraint, keys }
                    } else {
                        Timeline::PathSpacing { constraint, keys }
                    });
                }
                2 => {
                    let mut keys = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let time = r.f32()?;
                        let mix_rotate = r.f32()?;
                        let mix_translate = r.f32()?;
                        let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                        keys.push(PathMixKey { time, mix_rotate, mix_x: mix_translate, mix_y: mix_translate, interp });
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

    // Deform timelines.
    for _ in 0..r.count("deform skin")? {
        let skin = SkinId(r.varint()? as u16);
        for _ in 0..r.count("deform slot")? {
            let slot = SlotId(r.varint()? as u16);
            for _ in 0..r.count("deform attachment")? {
                let att_name = ctx.string_ref(r)?.unwrap_or_default();
                let attachment = ir.resolve_attachment(skin, slot, &att_name);
                let deform_len = attachment
                    .and_then(|a| ir.attachment(a))
                    .and_then(|a| a.kind.deformable_vertices())
                    .map(|v| v.deform_len());

                let frames = r.count("keyframe")?;
                let mut keys = Vec::with_capacity(frames);
                for i in 0..frames {
                    let time = r.f32()?;
                    let run = r.count("deform value")?;
                    let (offset, values) = if run == 0 {
                        (0, Vec::new())
                    } else {
                        let start = r.varint()?;
                        (start, r.f32_array(run)?)
                    };
                    let interp = if i < frames - 1 { read_curve(r)? } else { Interpolation::Linear };
                    keys.push(DeformKey { time, offset, values, interp });
                }

                match (attachment, deform_len) {
                    (Some(attachment), Some(_)) => {
                        anim.timelines.push(Timeline::Deform { slot, skin, attachment, keys })
                    }
                    _ => report.warn(Degradation::MissingReference {
                        kind: "deform target attachment".into(),
                        name: att_name,
                    }),
                }
            }
        }
    }

    // Draw order timeline.
    let draw_order_frames = r.count("draw order keyframe")?;
    if draw_order_frames > 0 {
        let mut keys = Vec::with_capacity(draw_order_frames);
        for _ in 0..draw_order_frames {
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
    }

    // Event timeline.
    let event_frames = r.count("event keyframe")?;
    if event_frames > 0 {
        let mut keys = Vec::with_capacity(event_frames);
        for _ in 0..event_frames {
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
    }

    anim.timelines.retain(|t| !t.is_empty());
    anim.duration = anim.max_key_time();
    Ok(anim)
}

/// Reads a keyframe curve. Spine 3.x stores normalised control points, which is
/// exactly what the IR wants, so no conversion is needed.
fn read_curve(r: &mut BinaryReader<'_>) -> Result<Interpolation, DecodeError> {
    match r.u8()? {
        CURVE_LINEAR => Ok(Interpolation::Linear),
        CURVE_STEPPED => Ok(Interpolation::Stepped),
        CURVE_BEZIER => {
            let (cx1, cy1, cx2, cy2) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
            Ok(Interpolation::Bezier(Bezier::new(cx1, cy1, cx2, cy2)))
        }
        other => Err(DecodeError::corrupt_at(format!("unknown curve type {other}"), r.position() as u64)),
    }
}
