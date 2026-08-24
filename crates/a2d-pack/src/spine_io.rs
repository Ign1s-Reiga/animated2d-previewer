//! Deterministic encoding of the Generic Spine IR into `model.bin`.
//!
//! Field order here *is* the format. Adding, removing or reordering a field is
//! a layout change: bump [`crate::FORMAT_VERSION`] and handle the old layout in
//! the reader, or every existing package stops loading.
//!
//! Derived data (`constraint_order`, sort invariants) is not written. It is
//! rebuilt on load, which keeps the file smaller and makes it impossible for a
//! package to contain a derived value that disagrees with its source.

use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion, TextureFilter, TextureWrap};
use a2d_core::ir::ids::{
    AtlasPageId, AtlasRegionId, AttachmentId, BoneId, EventId, IkConstraintId, PathConstraintId, SkinId, SlotId,
    TransformConstraintId,
};
use a2d_core::ir::spine::*;
use a2d_core::math::Bezier;
use a2d_core::{BlendMode, DecodeError, Interpolation, Rgb, Rgba, Vec2};

use crate::bin_io::{Reader, Writer};

/// Writes a whole skeleton.
pub fn write(w: &mut Writer, ir: &SpineIr) {
    write_metadata(w, &ir.metadata);
    w.seq(&ir.bones, write_bone);
    w.seq(&ir.slots, write_slot);
    w.seq(&ir.skins, write_skin);
    w.seq(&ir.attachments, write_attachment);
    w.seq(&ir.ik_constraints, write_ik);
    w.seq(&ir.transform_constraints, write_transform);
    w.seq(&ir.path_constraints, write_path);
    w.seq(&ir.events, write_event_data);
    w.seq(&ir.animations, write_animation);
    write_atlas(w, &ir.atlas);
}

/// Reads a whole skeleton and restores its derived invariants.
pub fn read(r: &mut Reader<'_>) -> Result<SpineIr, DecodeError> {
    let mut ir = SpineIr {
        metadata: read_metadata(r)?,
        bones: r.seq("bone", 24, read_bone)?,
        slots: r.seq("slot", 16, read_slot)?,
        skins: r.seq("skin", 8, read_skin)?,
        attachments: r.seq("attachment", 8, read_attachment)?,
        ik_constraints: r.seq("ik constraint", 24, read_ik)?,
        transform_constraints: r.seq("transform constraint", 48, read_transform)?,
        path_constraints: r.seq("path constraint", 32, read_path)?,
        events: r.seq("event", 16, read_event_data)?,
        animations: r.seq("animation", 8, read_animation)?,
        atlas: read_atlas(r)?,
        constraint_order: Vec::new(),
    };
    ir.rebuild_derived();
    validate_references(&ir)?;
    Ok(ir)
}

/// Rejects handles that point outside their arena.
///
/// A package is not trusted input in the security sense, but it can be stale or
/// hand-edited, and an out-of-range handle would otherwise surface as silently
/// missing geometry much later.
fn validate_references(ir: &SpineIr) -> Result<(), DecodeError> {
    let fail = |what: String| Err(DecodeError::corrupt(what));
    for (i, bone) in ir.bones.iter().enumerate() {
        if let Some(parent) = bone.parent {
            if parent.index() >= ir.bones.len() {
                return fail(format!("bone {i} names parent {} which does not exist", parent.0));
            }
            if parent.index() >= i {
                return fail(format!("bone {i} names parent {} which is not before it", parent.0));
            }
        }
    }
    for (i, slot) in ir.slots.iter().enumerate() {
        if slot.bone.index() >= ir.bones.len() {
            return fail(format!("slot {i} targets bone {} which does not exist", slot.bone.0));
        }
    }
    for skin in &ir.skins {
        for entry in &skin.entries {
            if entry.slot.index() >= ir.slots.len() {
                return fail(format!("skin {:?} binds to slot {} which does not exist", skin.name, entry.slot.0));
            }
            if entry.attachment.index() >= ir.attachments.len() {
                return fail(format!(
                    "skin {:?} binds attachment {} which does not exist",
                    skin.name, entry.attachment.0
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- metadata

fn write_metadata(w: &mut Writer, m: &SkeletonMetadata) {
    w.opt_str(m.name.as_deref());
    w.str(&m.source_version);
    w.opt_str(m.hash.as_deref());
    write_vec2(w, m.origin);
    write_vec2(w, m.size);
    w.opt(m.fps, |w, v| w.f32(v));
    w.opt_str(m.images_path.as_deref());
    w.opt_str(m.audio_path.as_deref());
}

fn read_metadata(r: &mut Reader<'_>) -> Result<SkeletonMetadata, DecodeError> {
    Ok(SkeletonMetadata {
        name: r.opt_str()?,
        source_version: r.str()?,
        hash: r.opt_str()?,
        origin: read_vec2(r)?,
        size: read_vec2(r)?,
        fps: r.opt(|r| r.f32())?,
        images_path: r.opt_str()?,
        audio_path: r.opt_str()?,
    })
}

// ---------------------------------------------------------------- primitives

fn write_vec2(w: &mut Writer, v: Vec2) {
    w.f32(v.x);
    w.f32(v.y);
}

fn read_vec2(r: &mut Reader<'_>) -> Result<Vec2, DecodeError> {
    Ok(Vec2::new(r.f32()?, r.f32()?))
}

fn write_rgba(w: &mut Writer, c: Rgba) {
    w.f32(c.r);
    w.f32(c.g);
    w.f32(c.b);
    w.f32(c.a);
}

fn read_rgba(r: &mut Reader<'_>) -> Result<Rgba, DecodeError> {
    Ok(Rgba::new(r.f32()?, r.f32()?, r.f32()?, r.f32()?))
}

fn write_rgb(w: &mut Writer, c: Rgb) {
    w.f32(c.r);
    w.f32(c.g);
    w.f32(c.b);
}

fn read_rgb(r: &mut Reader<'_>) -> Result<Rgb, DecodeError> {
    Ok(Rgb::new(r.f32()?, r.f32()?, r.f32()?))
}

fn write_interp(w: &mut Writer, i: Interpolation) {
    match i {
        Interpolation::Linear => w.u8(0),
        Interpolation::Stepped => w.u8(1),
        Interpolation::Bezier(b) => {
            w.u8(2);
            w.f32(b.cx1);
            w.f32(b.cy1);
            w.f32(b.cx2);
            w.f32(b.cy2);
        }
    }
}

fn read_interp(r: &mut Reader<'_>) -> Result<Interpolation, DecodeError> {
    match r.u8()? {
        0 => Ok(Interpolation::Linear),
        1 => Ok(Interpolation::Stepped),
        2 => Ok(Interpolation::Bezier(Bezier::new(r.f32()?, r.f32()?, r.f32()?, r.f32()?))),
        other => Err(DecodeError::corrupt(format!("unknown interpolation tag {other}"))),
    }
}

fn write_blend(w: &mut Writer, b: BlendMode) {
    w.u8(match b {
        BlendMode::Normal => 0,
        BlendMode::Additive => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
    });
}

fn read_blend(r: &mut Reader<'_>) -> Result<BlendMode, DecodeError> {
    Ok(match r.u8()? {
        0 => BlendMode::Normal,
        1 => BlendMode::Additive,
        2 => BlendMode::Multiply,
        3 => BlendMode::Screen,
        other => return Err(DecodeError::corrupt(format!("unknown blend mode tag {other}"))),
    })
}

// ---------------------------------------------------------------- skeleton

fn write_bone(w: &mut Writer, b: &Bone) {
    w.str(&b.name);
    w.opt(b.parent, |w, id| w.u16(id.0));
    w.f32(b.length);
    write_vec2(w, b.setup.position);
    w.f32(b.setup.rotation);
    write_vec2(w, b.setup.scale);
    write_vec2(w, b.setup.shear);
    w.u8(match b.inherit {
        TransformInherit::Normal => 0,
        TransformInherit::OnlyTranslation => 1,
        TransformInherit::NoRotationOrReflection => 2,
        TransformInherit::NoScale => 3,
        TransformInherit::NoScaleOrReflection => 4,
    });
    w.bool(b.skin_required);
}

fn read_bone(r: &mut Reader<'_>) -> Result<Bone, DecodeError> {
    let name = r.str()?;
    let parent = r.opt(|r| r.u16())?.map(BoneId);
    let length = r.f32()?;
    let position = read_vec2(r)?;
    let rotation = r.f32()?;
    let scale = read_vec2(r)?;
    let shear = read_vec2(r)?;
    let ordinal = r.u8()? as u32;
    let inherit = TransformInherit::from_ordinal(ordinal)
        .ok_or_else(|| DecodeError::corrupt(format!("unknown transform inherit tag {ordinal}")))?;
    Ok(Bone {
        name,
        parent,
        length,
        setup: BoneLocal { position, rotation, scale, shear },
        inherit,
        skin_required: r.bool()?,
    })
}

fn write_slot(w: &mut Writer, s: &Slot) {
    w.str(&s.name);
    w.u16(s.bone.0);
    write_rgba(w, s.color);
    w.opt(s.dark_color, write_rgb);
    w.opt_str(s.setup_attachment.as_deref());
    write_blend(w, s.blend_mode);
}

fn read_slot(r: &mut Reader<'_>) -> Result<Slot, DecodeError> {
    Ok(Slot {
        name: r.str()?,
        bone: BoneId(r.u16()?),
        color: read_rgba(r)?,
        dark_color: r.opt(read_rgb)?,
        setup_attachment: r.opt_str()?,
        blend_mode: read_blend(r)?,
    })
}

fn write_skin(w: &mut Writer, s: &Skin) {
    w.str(&s.name);
    w.seq(&s.entries, |w, e| {
        w.u16(e.slot.0);
        w.str(&e.name);
        w.u32(e.attachment.0);
    });
    w.seq(&s.bones, |w, b| w.u16(b.0));
}

fn read_skin(r: &mut Reader<'_>) -> Result<Skin, DecodeError> {
    Ok(Skin {
        name: r.str()?,
        entries: r.seq("skin entry", 10, |r| {
            Ok(SkinEntry { slot: SlotId(r.u16()?), name: r.str()?, attachment: AttachmentId(r.u32()?) })
        })?,
        bones: r.seq("skin bone", 2, |r| Ok(BoneId(r.u16()?)))?,
    })
}

// ---------------------------------------------------------------- attachments

fn write_vertices(w: &mut Writer, v: &VertexData) {
    match v {
        VertexData::Rigid(positions) => {
            w.u8(0);
            w.seq(positions, |w, p| write_vec2(w, *p));
        }
        VertexData::Weighted(weighted) => {
            w.u8(1);
            w.u32_seq(&weighted.offsets);
            w.seq(&weighted.influences, |w, i| {
                w.u16(i.bone.0);
                write_vec2(w, i.position);
                w.f32(i.weight);
            });
        }
    }
}

fn read_vertices(r: &mut Reader<'_>) -> Result<VertexData, DecodeError> {
    match r.u8()? {
        0 => Ok(VertexData::Rigid(r.seq("vertex", 8, read_vec2)?)),
        1 => {
            let offsets = r.u32_seq()?;
            let influences = r.seq("influence", 14, |r| {
                Ok(VertexInfluence { bone: BoneId(r.u16()?), position: read_vec2(r)?, weight: r.f32()? })
            })?;
            let weighted = WeightedVertices { offsets, influences };
            if !weighted.is_well_formed() {
                return Err(DecodeError::corrupt("weighted vertex offsets are not a valid CSR layout"));
            }
            Ok(VertexData::Weighted(weighted))
        }
        other => Err(DecodeError::corrupt(format!("unknown vertex layout tag {other}"))),
    }
}

fn write_sequence(w: &mut Writer, s: Sequence) {
    w.u32(s.count);
    w.u32(s.start);
    w.u32(s.digits);
    w.u32(s.setup_index);
}

fn read_sequence(r: &mut Reader<'_>) -> Result<Sequence, DecodeError> {
    Ok(Sequence { count: r.u32()?, start: r.u32()?, digits: r.u32()?, setup_index: r.u32()? })
}

fn write_attachment(w: &mut Writer, a: &Attachment) {
    w.str(&a.name);
    match &a.kind {
        AttachmentKind::Region(r) => {
            w.u8(0);
            w.str(&r.path);
            w.opt(r.region, |w, id| w.u32(id.0));
            write_vec2(w, r.position);
            w.f32(r.rotation);
            write_vec2(w, r.scale);
            write_vec2(w, r.size);
            write_rgba(w, r.color);
            w.opt(r.sequence, write_sequence);
        }
        AttachmentKind::Mesh(m) => {
            w.u8(1);
            w.str(&m.path);
            w.opt(m.region, |w, id| w.u32(id.0));
            w.seq(&m.uvs, |w, uv| write_vec2(w, *uv));
            w.u16_seq(&m.triangles);
            write_vertices(w, &m.vertices);
            w.u32(m.hull_length);
            w.u16_seq(&m.edges);
            write_vec2(w, m.size);
            write_rgba(w, m.color);
            w.opt(m.linked_to.as_ref(), |w, link| {
                w.opt_str(link.skin.as_deref());
                w.u16(link.slot.0);
                w.str(&link.parent);
                w.bool(link.inherit_timelines);
                w.opt(link.resolved, |w, id| w.u32(id.0));
            });
            w.opt(m.sequence, write_sequence);
        }
        AttachmentKind::Clipping(c) => {
            w.u8(2);
            w.opt(c.end_slot, |w, id| w.u16(id.0));
            write_vertices(w, &c.vertices);
            write_rgba(w, c.color);
        }
        AttachmentKind::BoundingBox(b) => {
            w.u8(3);
            write_vertices(w, &b.vertices);
            write_rgba(w, b.color);
        }
        AttachmentKind::Point(p) => {
            w.u8(4);
            write_vec2(w, p.position);
            w.f32(p.rotation);
            write_rgba(w, p.color);
        }
        AttachmentKind::Path(p) => {
            w.u8(5);
            w.bool(p.closed);
            w.bool(p.constant_speed);
            w.f32_seq(&p.lengths);
            write_vertices(w, &p.vertices);
            write_rgba(w, p.color);
        }
    }
}

fn read_attachment(r: &mut Reader<'_>) -> Result<Attachment, DecodeError> {
    let name = r.str()?;
    let kind = match r.u8()? {
        0 => AttachmentKind::Region(RegionAttachment {
            path: r.str()?,
            region: r.opt(|r| r.u32())?.map(AtlasRegionId),
            position: read_vec2(r)?,
            rotation: r.f32()?,
            scale: read_vec2(r)?,
            size: read_vec2(r)?,
            color: read_rgba(r)?,
            sequence: r.opt(read_sequence)?,
        }),
        1 => AttachmentKind::Mesh(MeshAttachment {
            path: r.str()?,
            region: r.opt(|r| r.u32())?.map(AtlasRegionId),
            uvs: r.seq("uv", 8, read_vec2)?,
            triangles: r.u16_seq()?,
            vertices: read_vertices(r)?,
            hull_length: r.u32()?,
            edges: r.u16_seq()?,
            size: read_vec2(r)?,
            color: read_rgba(r)?,
            linked_to: r.opt(|r| {
                Ok(LinkedMesh {
                    skin: r.opt_str()?,
                    slot: SlotId(r.u16()?),
                    parent: r.str()?,
                    inherit_timelines: r.bool()?,
                    resolved: r.opt(|r| r.u32())?.map(AttachmentId),
                })
            })?,
            sequence: r.opt(read_sequence)?,
        }),
        2 => AttachmentKind::Clipping(ClippingAttachment {
            end_slot: r.opt(|r| r.u16())?.map(SlotId),
            vertices: read_vertices(r)?,
            color: read_rgba(r)?,
        }),
        3 => AttachmentKind::BoundingBox(BoundingBoxAttachment { vertices: read_vertices(r)?, color: read_rgba(r)? }),
        4 => {
            AttachmentKind::Point(PointAttachment { position: read_vec2(r)?, rotation: r.f32()?, color: read_rgba(r)? })
        }
        5 => AttachmentKind::Path(PathAttachment {
            closed: r.bool()?,
            constant_speed: r.bool()?,
            lengths: r.f32_seq()?,
            vertices: read_vertices(r)?,
            color: read_rgba(r)?,
        }),
        other => return Err(DecodeError::corrupt(format!("unknown attachment tag {other}"))),
    };
    Ok(Attachment { name, kind })
}

// ---------------------------------------------------------------- constraints

fn write_ik(w: &mut Writer, c: &IkConstraint) {
    w.str(&c.name);
    w.u32(c.order);
    w.bool(c.skin_required);
    w.seq(&c.bones, |w, b| w.u16(b.0));
    w.u16(c.target.0);
    w.f32(c.mix);
    w.f32(c.softness);
    w.bool(c.bend_positive);
    w.bool(c.compress);
    w.bool(c.stretch);
    w.bool(c.uniform);
}

fn read_ik(r: &mut Reader<'_>) -> Result<IkConstraint, DecodeError> {
    Ok(IkConstraint {
        name: r.str()?,
        order: r.u32()?,
        skin_required: r.bool()?,
        bones: r.seq("ik bone", 2, |r| Ok(BoneId(r.u16()?)))?,
        target: BoneId(r.u16()?),
        mix: r.f32()?,
        softness: r.f32()?,
        bend_positive: r.bool()?,
        compress: r.bool()?,
        stretch: r.bool()?,
        uniform: r.bool()?,
    })
}

fn write_transform(w: &mut Writer, c: &TransformConstraint) {
    w.str(&c.name);
    w.u32(c.order);
    w.bool(c.skin_required);
    w.seq(&c.bones, |w, b| w.u16(b.0));
    w.u16(c.target.0);
    for v in [c.offset_rotation, c.offset_x, c.offset_y, c.offset_scale_x, c.offset_scale_y, c.offset_shear_y] {
        w.f32(v);
    }
    for v in [c.mix_rotate, c.mix_x, c.mix_y, c.mix_scale_x, c.mix_scale_y, c.mix_shear_y] {
        w.f32(v);
    }
    w.bool(c.relative);
    w.bool(c.local);
}

fn read_transform(r: &mut Reader<'_>) -> Result<TransformConstraint, DecodeError> {
    Ok(TransformConstraint {
        name: r.str()?,
        order: r.u32()?,
        skin_required: r.bool()?,
        bones: r.seq("transform bone", 2, |r| Ok(BoneId(r.u16()?)))?,
        target: BoneId(r.u16()?),
        offset_rotation: r.f32()?,
        offset_x: r.f32()?,
        offset_y: r.f32()?,
        offset_scale_x: r.f32()?,
        offset_scale_y: r.f32()?,
        offset_shear_y: r.f32()?,
        mix_rotate: r.f32()?,
        mix_x: r.f32()?,
        mix_y: r.f32()?,
        mix_scale_x: r.f32()?,
        mix_scale_y: r.f32()?,
        mix_shear_y: r.f32()?,
        relative: r.bool()?,
        local: r.bool()?,
    })
}

fn write_path(w: &mut Writer, c: &PathConstraint) {
    w.str(&c.name);
    w.u32(c.order);
    w.bool(c.skin_required);
    w.seq(&c.bones, |w, b| w.u16(b.0));
    w.u16(c.target_slot.0);
    w.u8(match c.position_mode {
        PathPositionMode::Fixed => 0,
        PathPositionMode::Percent => 1,
    });
    w.u8(match c.spacing_mode {
        PathSpacingMode::Length => 0,
        PathSpacingMode::Fixed => 1,
        PathSpacingMode::Percent => 2,
        PathSpacingMode::Proportional => 3,
    });
    w.u8(match c.rotate_mode {
        PathRotateMode::Tangent => 0,
        PathRotateMode::Chain => 1,
        PathRotateMode::ChainScale => 2,
    });
    for v in [c.offset_rotation, c.position, c.spacing, c.mix_rotate, c.mix_x, c.mix_y] {
        w.f32(v);
    }
}

fn read_path(r: &mut Reader<'_>) -> Result<PathConstraint, DecodeError> {
    let name = r.str()?;
    let order = r.u32()?;
    let skin_required = r.bool()?;
    let bones = r.seq("path bone", 2, |r| Ok(BoneId(r.u16()?)))?;
    let target_slot = SlotId(r.u16()?);
    let position_mode = match r.u8()? {
        0 => PathPositionMode::Fixed,
        1 => PathPositionMode::Percent,
        other => return Err(DecodeError::corrupt(format!("unknown path position mode {other}"))),
    };
    let spacing_mode = match r.u8()? {
        0 => PathSpacingMode::Length,
        1 => PathSpacingMode::Fixed,
        2 => PathSpacingMode::Percent,
        3 => PathSpacingMode::Proportional,
        other => return Err(DecodeError::corrupt(format!("unknown path spacing mode {other}"))),
    };
    let rotate_mode = match r.u8()? {
        0 => PathRotateMode::Tangent,
        1 => PathRotateMode::Chain,
        2 => PathRotateMode::ChainScale,
        other => return Err(DecodeError::corrupt(format!("unknown path rotate mode {other}"))),
    };
    Ok(PathConstraint {
        name,
        order,
        skin_required,
        bones,
        target_slot,
        position_mode,
        spacing_mode,
        rotate_mode,
        offset_rotation: r.f32()?,
        position: r.f32()?,
        spacing: r.f32()?,
        mix_rotate: r.f32()?,
        mix_x: r.f32()?,
        mix_y: r.f32()?,
    })
}

// ---------------------------------------------------------------- animations

fn write_event_data(w: &mut Writer, e: &EventData) {
    w.str(&e.name);
    w.i32(e.int_value);
    w.f32(e.float_value);
    w.str(&e.string_value);
    w.opt_str(e.audio_path.as_deref());
    w.f32(e.volume);
    w.f32(e.balance);
}

fn read_event_data(r: &mut Reader<'_>) -> Result<EventData, DecodeError> {
    Ok(EventData {
        name: r.str()?,
        int_value: r.i32()?,
        float_value: r.f32()?,
        string_value: r.str()?,
        audio_path: r.opt_str()?,
        volume: r.f32()?,
        balance: r.f32()?,
    })
}

fn write_axes(w: &mut Writer, a: Axes) {
    w.u8(match a {
        Axes::Both => 0,
        Axes::X => 1,
        Axes::Y => 2,
    });
}

fn read_axes(r: &mut Reader<'_>) -> Result<Axes, DecodeError> {
    Ok(match r.u8()? {
        0 => Axes::Both,
        1 => Axes::X,
        2 => Axes::Y,
        other => return Err(DecodeError::corrupt(format!("unknown axis mask tag {other}"))),
    })
}

fn write_channels(w: &mut Writer, c: ColorChannels) {
    w.u8(match c {
        ColorChannels::Rgba => 0,
        ColorChannels::Rgb => 1,
    });
}

fn read_channels(r: &mut Reader<'_>) -> Result<ColorChannels, DecodeError> {
    Ok(match r.u8()? {
        0 => ColorChannels::Rgba,
        1 => ColorChannels::Rgb,
        other => return Err(DecodeError::corrupt(format!("unknown colour channel tag {other}"))),
    })
}

fn write_animation(w: &mut Writer, a: &Animation) {
    w.str(&a.name);
    w.f32(a.duration);
    w.seq(&a.timelines, write_timeline);
}

fn read_animation(r: &mut Reader<'_>) -> Result<Animation, DecodeError> {
    Ok(Animation { name: r.str()?, duration: r.f32()?, timelines: r.seq("timeline", 5, read_timeline)? })
}

fn write_timeline(w: &mut Writer, t: &Timeline) {
    match t {
        Timeline::BoneRotate { bone, keys } => {
            w.u8(0);
            w.u16(bone.0);
            w.seq(keys, write_scalar_key);
        }
        Timeline::BoneTranslate { bone, axes, keys } => write_vec2_timeline(w, 1, *bone, *axes, keys),
        Timeline::BoneScale { bone, axes, keys } => write_vec2_timeline(w, 2, *bone, *axes, keys),
        Timeline::BoneShear { bone, axes, keys } => write_vec2_timeline(w, 3, *bone, *axes, keys),
        Timeline::SlotColor { slot, channels, keys } => {
            w.u8(4);
            w.u16(slot.0);
            write_channels(w, *channels);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                write_rgba(w, k.value);
                for i in k.interp {
                    write_interp(w, i);
                }
            });
        }
        Timeline::SlotTwoColor { slot, channels, keys } => {
            w.u8(5);
            w.u16(slot.0);
            write_channels(w, *channels);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                write_rgba(w, k.light);
                write_rgb(w, k.dark);
                for i in k.interp_light {
                    write_interp(w, i);
                }
                for i in k.interp_dark {
                    write_interp(w, i);
                }
            });
        }
        Timeline::SlotAlpha { slot, keys } => {
            w.u8(6);
            w.u16(slot.0);
            w.seq(keys, write_scalar_key);
        }
        Timeline::SlotAttachment { slot, keys } => {
            w.u8(7);
            w.u16(slot.0);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.opt_str(k.name.as_deref());
            });
        }
        Timeline::Deform { slot, skin, attachment, keys } => {
            w.u8(8);
            w.u16(slot.0);
            w.u16(skin.0);
            w.u32(attachment.0);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.u32(k.offset);
                w.f32_seq(&k.values);
                write_interp(w, k.interp);
            });
        }
        Timeline::DrawOrder { keys } => {
            w.u8(9);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.opt(k.order.as_ref(), |w, order| w.seq(order, |w, s| w.u16(s.0)));
            });
        }
        Timeline::Event { keys } => {
            w.u8(10);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.u16(k.event.0);
                w.i32(k.int_value);
                w.f32(k.float_value);
                w.opt_str(k.string_value.as_deref());
                w.f32(k.volume);
                w.f32(k.balance);
            });
        }
        Timeline::IkConstraint { constraint, keys } => {
            w.u8(11);
            w.u16(constraint.0);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.f32(k.mix);
                w.f32(k.softness);
                w.bool(k.bend_positive);
                w.bool(k.compress);
                w.bool(k.stretch);
                write_interp(w, k.interp);
            });
        }
        Timeline::TransformConstraint { constraint, keys } => {
            w.u8(12);
            w.u16(constraint.0);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                for v in [k.mix_rotate, k.mix_x, k.mix_y, k.mix_scale_x, k.mix_scale_y, k.mix_shear_y] {
                    w.f32(v);
                }
                write_interp(w, k.interp);
            });
        }
        Timeline::PathPosition { constraint, keys } => {
            w.u8(13);
            w.u16(constraint.0);
            w.seq(keys, write_scalar_key);
        }
        Timeline::PathSpacing { constraint, keys } => {
            w.u8(14);
            w.u16(constraint.0);
            w.seq(keys, write_scalar_key);
        }
        Timeline::PathMix { constraint, keys } => {
            w.u8(15);
            w.u16(constraint.0);
            w.seq(keys, |w, k| {
                w.f32(k.time);
                w.f32(k.mix_rotate);
                w.f32(k.mix_x);
                w.f32(k.mix_y);
                write_interp(w, k.interp);
            });
        }
    }
}

fn write_vec2_timeline(w: &mut Writer, tag: u8, bone: BoneId, axes: Axes, keys: &[Vec2Key]) {
    w.u8(tag);
    w.u16(bone.0);
    write_axes(w, axes);
    w.seq(keys, |w, k| {
        w.f32(k.time);
        write_vec2(w, k.value);
        write_interp(w, k.interp_x);
        write_interp(w, k.interp_y);
    });
}

fn write_scalar_key(w: &mut Writer, k: &ScalarKey) {
    w.f32(k.time);
    w.f32(k.value);
    write_interp(w, k.interp);
}

fn read_scalar_keys(r: &mut Reader<'_>) -> Result<Vec<ScalarKey>, DecodeError> {
    r.seq("keyframe", 9, |r| Ok(ScalarKey { time: r.f32()?, value: r.f32()?, interp: read_interp(r)? }))
}

fn read_vec2_keys(r: &mut Reader<'_>) -> Result<Vec<Vec2Key>, DecodeError> {
    r.seq("keyframe", 14, |r| {
        Ok(Vec2Key { time: r.f32()?, value: read_vec2(r)?, interp_x: read_interp(r)?, interp_y: read_interp(r)? })
    })
}

fn read_timeline(r: &mut Reader<'_>) -> Result<Timeline, DecodeError> {
    Ok(match r.u8()? {
        0 => Timeline::BoneRotate { bone: BoneId(r.u16()?), keys: read_scalar_keys(r)? },
        1 => Timeline::BoneTranslate { bone: BoneId(r.u16()?), axes: read_axes(r)?, keys: read_vec2_keys(r)? },
        2 => Timeline::BoneScale { bone: BoneId(r.u16()?), axes: read_axes(r)?, keys: read_vec2_keys(r)? },
        3 => Timeline::BoneShear { bone: BoneId(r.u16()?), axes: read_axes(r)?, keys: read_vec2_keys(r)? },
        4 => Timeline::SlotColor {
            slot: SlotId(r.u16()?),
            channels: read_channels(r)?,
            keys: r.seq("keyframe", 24, |r| {
                Ok(ColorKey {
                    time: r.f32()?,
                    value: read_rgba(r)?,
                    interp: [read_interp(r)?, read_interp(r)?, read_interp(r)?, read_interp(r)?],
                })
            })?,
        },
        5 => Timeline::SlotTwoColor {
            slot: SlotId(r.u16()?),
            channels: read_channels(r)?,
            keys: r.seq("keyframe", 40, |r| {
                Ok(TwoColorKey {
                    time: r.f32()?,
                    light: read_rgba(r)?,
                    dark: read_rgb(r)?,
                    interp_light: [read_interp(r)?, read_interp(r)?, read_interp(r)?, read_interp(r)?],
                    interp_dark: [read_interp(r)?, read_interp(r)?, read_interp(r)?],
                })
            })?,
        },
        6 => Timeline::SlotAlpha { slot: SlotId(r.u16()?), keys: read_scalar_keys(r)? },
        7 => Timeline::SlotAttachment {
            slot: SlotId(r.u16()?),
            keys: r.seq("keyframe", 5, |r| Ok(AttachmentKey { time: r.f32()?, name: r.opt_str()? }))?,
        },
        8 => Timeline::Deform {
            slot: SlotId(r.u16()?),
            skin: SkinId(r.u16()?),
            attachment: AttachmentId(r.u32()?),
            keys: r.seq("keyframe", 13, |r| {
                Ok(DeformKey { time: r.f32()?, offset: r.u32()?, values: r.f32_seq()?, interp: read_interp(r)? })
            })?,
        },
        9 => Timeline::DrawOrder {
            keys: r.seq("keyframe", 5, |r| {
                Ok(DrawOrderKey { time: r.f32()?, order: r.opt(|r| r.seq("slot", 2, |r| Ok(SlotId(r.u16()?))))? })
            })?,
        },
        10 => Timeline::Event {
            keys: r.seq("keyframe", 23, |r| {
                Ok(EventKey {
                    time: r.f32()?,
                    event: EventId(r.u16()?),
                    int_value: r.i32()?,
                    float_value: r.f32()?,
                    string_value: r.opt_str()?,
                    volume: r.f32()?,
                    balance: r.f32()?,
                })
            })?,
        },
        11 => Timeline::IkConstraint {
            constraint: IkConstraintId(r.u16()?),
            keys: r.seq("keyframe", 16, |r| {
                Ok(IkKey {
                    time: r.f32()?,
                    mix: r.f32()?,
                    softness: r.f32()?,
                    bend_positive: r.bool()?,
                    compress: r.bool()?,
                    stretch: r.bool()?,
                    interp: read_interp(r)?,
                })
            })?,
        },
        12 => Timeline::TransformConstraint {
            constraint: TransformConstraintId(r.u16()?),
            keys: r.seq("keyframe", 29, |r| {
                Ok(TransformKey {
                    time: r.f32()?,
                    mix_rotate: r.f32()?,
                    mix_x: r.f32()?,
                    mix_y: r.f32()?,
                    mix_scale_x: r.f32()?,
                    mix_scale_y: r.f32()?,
                    mix_shear_y: r.f32()?,
                    interp: read_interp(r)?,
                })
            })?,
        },
        13 => Timeline::PathPosition { constraint: PathConstraintId(r.u16()?), keys: read_scalar_keys(r)? },
        14 => Timeline::PathSpacing { constraint: PathConstraintId(r.u16()?), keys: read_scalar_keys(r)? },
        15 => Timeline::PathMix {
            constraint: PathConstraintId(r.u16()?),
            keys: r.seq("keyframe", 17, |r| {
                Ok(PathMixKey {
                    time: r.f32()?,
                    mix_rotate: r.f32()?,
                    mix_x: r.f32()?,
                    mix_y: r.f32()?,
                    interp: read_interp(r)?,
                })
            })?,
        },
        other => return Err(DecodeError::corrupt(format!("unknown timeline tag {other}"))),
    })
}

// ---------------------------------------------------------------- atlas

fn write_atlas(w: &mut Writer, atlas: &Atlas) {
    w.seq(&atlas.pages, |w, p| {
        w.str(&p.name);
        w.opt(p.size, |w, (width, height)| {
            w.u32(width);
            w.u32(height);
        });
        w.u8(filter_tag(p.min_filter));
        w.u8(filter_tag(p.mag_filter));
        w.u8(wrap_tag(p.u_wrap));
        w.u8(wrap_tag(p.v_wrap));
        w.bool(p.premultiplied_alpha);
    });
    w.seq(&atlas.regions, |w, r| {
        w.str(&r.name);
        w.u16(r.page.0);
        w.u32(r.xy.0);
        w.u32(r.xy.1);
        w.u32(r.size.0);
        w.u32(r.size.1);
        w.u16(r.rotate_deg);
        w.i32(r.offset.0);
        w.i32(r.offset.1);
        w.u32(r.original_size.0);
        w.u32(r.original_size.1);
        w.i32(r.index);
        w.opt(r.splits, |w, s| {
            for v in s {
                w.i32(v);
            }
        });
        w.opt(r.pads, |w, s| {
            for v in s {
                w.i32(v);
            }
        });
    });
}

fn read_atlas(r: &mut Reader<'_>) -> Result<Atlas, DecodeError> {
    Ok(Atlas {
        pages: r.seq("atlas page", 10, |r| {
            Ok(AtlasPage {
                name: r.str()?,
                size: r.opt(|r| Ok((r.u32()?, r.u32()?)))?,
                min_filter: read_filter(r)?,
                mag_filter: read_filter(r)?,
                u_wrap: read_wrap(r)?,
                v_wrap: read_wrap(r)?,
                premultiplied_alpha: r.bool()?,
            })
        })?,
        regions: r.seq("atlas region", 44, |r| {
            Ok(AtlasRegion {
                name: r.str()?,
                page: AtlasPageId(r.u16()?),
                xy: (r.u32()?, r.u32()?),
                size: (r.u32()?, r.u32()?),
                rotate_deg: r.u16()?,
                offset: (r.i32()?, r.i32()?),
                original_size: (r.u32()?, r.u32()?),
                index: r.i32()?,
                splits: r.opt(|r| Ok([r.i32()?, r.i32()?, r.i32()?, r.i32()?]))?,
                pads: r.opt(|r| Ok([r.i32()?, r.i32()?, r.i32()?, r.i32()?]))?,
            })
        })?,
    })
}

fn filter_tag(f: TextureFilter) -> u8 {
    match f {
        TextureFilter::Nearest => 0,
        TextureFilter::Linear => 1,
        TextureFilter::MipMap => 2,
        TextureFilter::MipMapNearestNearest => 3,
        TextureFilter::MipMapLinearNearest => 4,
        TextureFilter::MipMapNearestLinear => 5,
        TextureFilter::MipMapLinearLinear => 6,
    }
}

fn read_filter(r: &mut Reader<'_>) -> Result<TextureFilter, DecodeError> {
    Ok(match r.u8()? {
        0 => TextureFilter::Nearest,
        1 => TextureFilter::Linear,
        2 => TextureFilter::MipMap,
        3 => TextureFilter::MipMapNearestNearest,
        4 => TextureFilter::MipMapLinearNearest,
        5 => TextureFilter::MipMapNearestLinear,
        6 => TextureFilter::MipMapLinearLinear,
        other => return Err(DecodeError::corrupt(format!("unknown texture filter tag {other}"))),
    })
}

fn wrap_tag(w: TextureWrap) -> u8 {
    match w {
        TextureWrap::MirroredRepeat => 0,
        TextureWrap::ClampToEdge => 1,
        TextureWrap::Repeat => 2,
    }
}

fn read_wrap(r: &mut Reader<'_>) -> Result<TextureWrap, DecodeError> {
    Ok(match r.u8()? {
        0 => TextureWrap::MirroredRepeat,
        1 => TextureWrap::ClampToEdge,
        2 => TextureWrap::Repeat,
        other => return Err(DecodeError::corrupt(format!("unknown texture wrap tag {other}"))),
    })
}
