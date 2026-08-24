//! Spine JSON skeleton decoder.
//!
//! One decoder covers the 2.x/3.x and 4.x JSON dialects; every place they
//! differ is handled explicitly and commented with which version wrote what.
//! Nothing about the version escapes into the IR.

pub mod animation;
pub mod attachment;
pub mod curve;
pub mod fields;

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::ids::{AttachmentId, BoneId, SlotId};
use a2d_core::ir::spine::EventData;
use a2d_core::ir::spine::{
    Attachment, AttachmentKind, Bone, BoneLocal, IkConstraint, PathConstraint, PathPositionMode, PathRotateMode,
    PathSpacingMode, SkeletonMetadata, Skin, SkinEntry, Slot, SpineIr, TransformConstraint, TransformInherit,
};
use a2d_core::{BlendMode, DecodeError, Degradation, LoadReport, Rgb, Rgba, Vec2};
use serde_json::Value;

use crate::detect::{SpineDetection, SpineFamily};
use crate::json::attachment::{clipping_end_name, read_attachment, read_color};
use crate::json::fields::Fields;

/// Decodes a JSON skeleton into the Generic Spine IR.
///
/// `atlas` is consumed into the IR; pass [`Atlas::default`] when regions are
/// resolved later, which is what `inspect` does when only the skeleton is
/// available.
pub fn decode(
    bytes: &[u8],
    detection: &SpineDetection,
    atlas: Atlas,
    report: &mut LoadReport,
) -> Result<SpineIr, DecodeError> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|e| DecodeError::corrupt_at(format!("skeleton JSON is not parseable: {e}"), e.line() as u64))?;
    let family = detection.family().ok_or_else(|| {
        DecodeError::unsupported_version(
            a2d_core::ModelKind::Spine,
            detection.raw_version.clone(),
            "no decoder for this Spine major version",
        )
    })?;

    let mut f = Fields::new(&root, "skeleton");
    let mut ir = SpineIr { atlas, ..Default::default() };

    ir.metadata = read_metadata(&mut f, detection, report);
    read_bones(&mut f, &mut ir, report)?;
    read_slots(&mut f, &mut ir, report)?;
    read_ik(&mut f, &mut ir, report)?;
    read_transform(&mut f, &mut ir, family, report)?;
    read_path(&mut f, &mut ir, family, report)?;
    read_skins(&mut f, &mut ir, report)?;
    read_events(&mut f, &mut ir, report);

    // Skins must be complete before animations, because deform timelines
    // address attachments through them.
    ir.rebuild_derived();
    if let Some(animations) = f.get("animations") {
        animation::read_animations(animations, &mut ir, family, report)?;
    }
    f.finish(report);

    crate::normalize::finish(&mut ir, report);
    Ok(ir)
}

fn read_metadata(f: &mut Fields<'_>, detection: &SpineDetection, report: &mut LoadReport) -> SkeletonMetadata {
    let Some(value) = f.get("skeleton") else {
        return SkeletonMetadata { source_version: detection.raw_version.clone(), ..Default::default() };
    };
    let mut s = Fields::new(value, "skeleton.skeleton");
    let meta = SkeletonMetadata {
        name: s.string("name"),
        source_version: s.string("spine").unwrap_or_else(|| detection.raw_version.clone()),
        hash: s.string("hash"),
        origin: Vec2::new(s.f32("x", 0.0), s.f32("y", 0.0)),
        size: Vec2::new(s.f32("width", 0.0), s.f32("height", 0.0)),
        fps: s.opt_f32("fps"),
        images_path: s.string("images"),
        audio_path: s.string("audio"),
    };
    s.finish(report);
    meta
}

fn read_bones(f: &mut Fields<'_>, ir: &mut SpineIr, report: &mut LoadReport) -> Result<(), DecodeError> {
    let Some(bones) = f.array("bones") else { return Ok(()) };
    for (i, value) in bones.iter().enumerate() {
        let context = format!("bones[{i}]");
        let mut b = Fields::new(value, context.clone());
        let name = b.require_str("name")?.to_string();

        // Spine writes parents before children, so a forward reference means a
        // malformed file rather than an ordering the decoder should fix up.
        let parent = match b.str("parent") {
            None => None,
            Some(parent_name) => match ir.bone_by_name(parent_name) {
                Some(id) => Some(id),
                None => {
                    return Err(DecodeError::corrupt(format!(
                        "{context}: bone {name:?} names parent {parent_name:?}, which is not defined before it"
                    )))
                }
            },
        };
        if parent.is_none() && i != 0 {
            report.note(format!("{context}: bone {name:?} has no parent but is not the root"));
        }

        // Spine 4.2 renamed `transform` to `inherit`; the value names are the same.
        let inherit = match b.str("transform") {
            Some(s) => TransformInherit::parse(s).unwrap_or_else(|| {
                report.note(format!("{context}: unknown transform mode {s:?}, using normal"));
                TransformInherit::Normal
            }),
            None => match b.str("inherit") {
                Some(s) => TransformInherit::parse(s).unwrap_or_else(|| {
                    report.note(format!("{context}: unknown inherit mode {s:?}, using normal"));
                    TransformInherit::Normal
                }),
                None => TransformInherit::Normal,
            },
        };

        let bone = Bone {
            name,
            parent,
            length: b.f32("length", 0.0),
            setup: BoneLocal {
                position: Vec2::new(b.f32("x", 0.0), b.f32("y", 0.0)),
                rotation: b.f32("rotation", 0.0),
                scale: Vec2::new(b.f32("scaleX", 1.0), b.f32("scaleY", 1.0)),
                shear: Vec2::new(b.f32("shearX", 0.0), b.f32("shearY", 0.0)),
            },
            inherit,
            skin_required: b.bool("skin", false),
        };
        // Editor-only decoration.
        b.mark("color");
        b.mark("icon");
        b.mark("visible");
        b.finish(report);

        if BoneId::from_index(ir.bones.len()).is_none() {
            return Err(DecodeError::corrupt("skeleton has more bones than the bone handle can address"));
        }
        ir.bones.push(bone);
    }
    Ok(())
}

fn read_slots(f: &mut Fields<'_>, ir: &mut SpineIr, report: &mut LoadReport) -> Result<(), DecodeError> {
    let Some(slots) = f.array("slots") else { return Ok(()) };
    for (i, value) in slots.iter().enumerate() {
        let context = format!("slots[{i}]");
        let mut s = Fields::new(value, context.clone());
        let name = s.require_str("name")?.to_string();
        let bone_name = s.require_str("bone")?;
        let bone = ir.bone_by_name(bone_name).ok_or_else(|| {
            DecodeError::corrupt(format!("{context}: slot {name:?} targets undefined bone {bone_name:?}"))
        })?;

        let blend_name = s.str("blend").unwrap_or("normal").to_string();
        let blend_mode = parse_blend_mode(&blend_name).unwrap_or_else(|| {
            report.warn(Degradation::UnsupportedBlendMode {
                slot: name.clone(),
                requested: blend_name.clone(),
                fallback: "normal".into(),
            });
            BlendMode::Normal
        });

        let slot = Slot {
            color: read_color(&mut s, "color", Rgba::WHITE, &context, report),
            dark_color: s.str("dark").map(|hex| {
                Rgba::from_hex(hex).map(|c| c.rgb()).unwrap_or_else(|| {
                    report.note(format!("{context}: malformed `dark` value {hex:?}, using black"));
                    Rgb::BLACK
                })
            }),
            setup_attachment: s.string("attachment"),
            blend_mode,
            name,
            bone,
        };
        s.finish(report);

        if SlotId::from_index(ir.slots.len()).is_none() {
            return Err(DecodeError::corrupt("skeleton has more slots than the slot handle can address"));
        }
        ir.slots.push(slot);
    }
    Ok(())
}

fn parse_blend_mode(s: &str) -> Option<BlendMode> {
    Some(match s {
        "normal" => BlendMode::Normal,
        "additive" => BlendMode::Additive,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        _ => return None,
    })
}

fn read_bone_list(
    f: &mut Fields<'_>,
    ir: &SpineIr,
    context: &str,
    report: &mut LoadReport,
) -> Result<Vec<BoneId>, DecodeError> {
    let Some(names) = f.array("bones") else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(names.len());
    for value in names {
        let Some(name) = value.as_str() else {
            return Err(DecodeError::corrupt(format!("{context}: `bones` contains a non-string entry")));
        };
        match ir.bone_by_name(name) {
            Some(id) => out.push(id),
            None => report.warn(Degradation::MissingReference { kind: "constrained bone".into(), name: name.into() }),
        }
    }
    Ok(out)
}

fn read_ik(f: &mut Fields<'_>, ir: &mut SpineIr, report: &mut LoadReport) -> Result<(), DecodeError> {
    let Some(list) = f.array("ik") else { return Ok(()) };
    let list = list.clone();
    for (i, value) in list.iter().enumerate() {
        let context = format!("ik[{i}]");
        let mut c = Fields::new(value, context.clone());
        let name = c.require_str("name")?.to_string();
        let bones = read_bone_list(&mut c, ir, &context, report)?;
        let target_name = c.require_str("target")?;
        let Some(target) = ir.bone_by_name(target_name) else {
            report.warn(Degradation::MissingReference { kind: "ik target bone".into(), name: target_name.into() });
            c.finish(report);
            continue;
        };
        if bones.len() > 2 {
            report.warn(Degradation::UnsupportedConstraint {
                name: name.clone(),
                kind: format!("ik with a {}-bone chain", bones.len()),
            });
        }
        let constraint = IkConstraint {
            order: c.u32("order", i as u32),
            skin_required: c.bool("skin", false),
            mix: c.f32("mix", 1.0),
            softness: c.f32("softness", 0.0),
            bend_positive: c.bool("bendPositive", true),
            compress: c.bool("compress", false),
            stretch: c.bool("stretch", false),
            uniform: c.bool("uniform", false),
            name,
            bones,
            target,
        };
        c.finish(report);
        ir.ik_constraints.push(constraint);
    }
    Ok(())
}

fn read_transform(
    f: &mut Fields<'_>,
    ir: &mut SpineIr,
    _family: SpineFamily,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let Some(list) = f.array("transform") else { return Ok(()) };
    let list = list.clone();
    for (i, value) in list.iter().enumerate() {
        let context = format!("transform[{i}]");
        let mut c = Fields::new(value, context.clone());
        let name = c.require_str("name")?.to_string();
        let bones = read_bone_list(&mut c, ir, &context, report)?;
        let target_name = c.require_str("target")?;
        let Some(target) = ir.bone_by_name(target_name) else {
            report
                .warn(Degradation::MissingReference { kind: "transform target bone".into(), name: target_name.into() });
            c.finish(report);
            continue;
        };

        // Spine 3.x uses one mix per channel group; 4.x splits them per axis.
        let legacy_translate = c.opt_f32("translateMix");
        let legacy_scale = c.opt_f32("scaleMix");
        let legacy_rotate = c.opt_f32("rotateMix");
        let legacy_shear = c.opt_f32("shearMix");

        let constraint = TransformConstraint {
            order: c.u32("order", i as u32),
            skin_required: c.bool("skin", false),
            offset_rotation: c.f32("rotation", 0.0),
            offset_x: c.f32("x", 0.0),
            offset_y: c.f32("y", 0.0),
            offset_scale_x: c.f32("scaleX", 0.0),
            offset_scale_y: c.f32("scaleY", 0.0),
            offset_shear_y: c.f32("shearY", 0.0),
            mix_rotate: c.opt_f32("mixRotate").or(legacy_rotate).unwrap_or(1.0),
            mix_x: c.opt_f32("mixX").or(legacy_translate).unwrap_or(1.0),
            mix_y: c.opt_f32("mixY").or(legacy_translate).unwrap_or(1.0),
            mix_scale_x: c.opt_f32("mixScaleX").or(legacy_scale).unwrap_or(1.0),
            mix_scale_y: c.opt_f32("mixScaleY").or(legacy_scale).unwrap_or(1.0),
            mix_shear_y: c.opt_f32("mixShearY").or(legacy_shear).unwrap_or(1.0),
            relative: c.bool("relative", false),
            local: c.bool("local", false),
            name,
            bones,
            target,
        };
        c.finish(report);
        ir.transform_constraints.push(constraint);
    }
    Ok(())
}

fn read_path(
    f: &mut Fields<'_>,
    ir: &mut SpineIr,
    _family: SpineFamily,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let Some(list) = f.array("path") else { return Ok(()) };
    let list = list.clone();
    for (i, value) in list.iter().enumerate() {
        let context = format!("path[{i}]");
        let mut c = Fields::new(value, context.clone());
        let name = c.require_str("name")?.to_string();
        let bones = read_bone_list(&mut c, ir, &context, report)?;
        let target_name = c.require_str("target")?;
        let Some(target_slot) = ir.slot_by_name(target_name) else {
            report.warn(Degradation::MissingReference { kind: "path target slot".into(), name: target_name.into() });
            c.finish(report);
            continue;
        };

        let position_mode = match c.str("positionMode").unwrap_or("percent") {
            "fixed" => PathPositionMode::Fixed,
            "percent" => PathPositionMode::Percent,
            other => {
                report.note(format!("{context}: unknown positionMode {other:?}, using percent"));
                PathPositionMode::Percent
            }
        };
        let spacing_mode = match c.str("spacingMode").unwrap_or("length") {
            "length" => PathSpacingMode::Length,
            "fixed" => PathSpacingMode::Fixed,
            "percent" => PathSpacingMode::Percent,
            "proportional" => PathSpacingMode::Proportional,
            other => {
                report.note(format!("{context}: unknown spacingMode {other:?}, using length"));
                PathSpacingMode::Length
            }
        };
        let rotate_mode = match c.str("rotateMode").unwrap_or("tangent") {
            "tangent" => PathRotateMode::Tangent,
            "chain" => PathRotateMode::Chain,
            "chainScale" => PathRotateMode::ChainScale,
            other => {
                report.note(format!("{context}: unknown rotateMode {other:?}, using tangent"));
                PathRotateMode::Tangent
            }
        };

        let legacy_translate = c.opt_f32("translateMix");
        let legacy_rotate = c.opt_f32("rotateMix");
        let constraint = PathConstraint {
            order: c.u32("order", i as u32),
            skin_required: c.bool("skin", false),
            offset_rotation: c.f32("rotation", 0.0),
            position: c.f32("position", 0.0),
            spacing: c.f32("spacing", 0.0),
            mix_rotate: c.opt_f32("mixRotate").or(legacy_rotate).unwrap_or(1.0),
            mix_x: c.opt_f32("mixX").or(legacy_translate).unwrap_or(1.0),
            mix_y: c.opt_f32("mixY").or(legacy_translate).unwrap_or(1.0),
            name,
            bones,
            target_slot,
            position_mode,
            spacing_mode,
            rotate_mode,
        };
        c.finish(report);
        ir.path_constraints.push(constraint);
    }
    Ok(())
}

/// Clipping attachments name their end slot; resolution needs the slot table,
/// so the pairs are collected here and applied once every skin is read.
type PendingClip = (AttachmentId, String);

fn read_skins(f: &mut Fields<'_>, ir: &mut SpineIr, report: &mut LoadReport) -> Result<(), DecodeError> {
    let mut pending_clips: Vec<PendingClip> = Vec::new();

    // Spine 3.8 and later write an array of skin objects; 3.7 and earlier write
    // an object keyed by skin name.
    let skins: Vec<(String, &Value, Option<&Value>)> = match f.get("skins") {
        None => Vec::new(),
        Some(Value::Array(list)) => {
            let mut out = Vec::with_capacity(list.len());
            for (i, value) in list.iter().enumerate() {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| DecodeError::corrupt(format!("skins[{i}]: skin has no name")))?;
                out.push((name.to_string(), value.get("attachments").unwrap_or(&Value::Null), Some(value)));
            }
            out
        }
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v, None)).collect(),
        Some(_) => return Err(DecodeError::corrupt("`skins` is neither an array nor an object")),
    };

    // The default skin is always index 0, so lookups never special-case it.
    ir.skins.push(Skin::new("default"));
    for (name, _, _) in &skins {
        if name != "default" {
            ir.skins.push(Skin::new(name));
        }
    }

    for (skin_name, attachments, meta) in skins {
        let skin_id =
            ir.skin_by_name(&skin_name).ok_or_else(|| DecodeError::corrupt("skin vanished between passes"))?;

        if let Some(meta) = meta {
            let mut m = Fields::new(meta, format!("skins.{skin_name}"));
            m.mark("name");
            m.mark("attachments");
            let bones = read_bone_list(&mut m, ir, &format!("skins.{skin_name}"), report)?;
            ir.skins[skin_id.index()].bones = bones;
            // Skin-scoped constraint lists are recorded but not yet applied.
            for key in ["ik", "transform", "path"] {
                if m.get(match key {
                    "ik" => "ik",
                    "transform" => "transform",
                    _ => "path",
                })
                .is_some()
                {
                    report.warn(Degradation::UnsupportedConstraint {
                        name: skin_name.clone(),
                        kind: format!("skin-scoped {key}"),
                    });
                }
            }
            m.mark("color");
            m.finish(report);
        }

        for (slot_name, placeholders) in Fields::new(attachments, format!("skins.{skin_name}")).entries() {
            let Some(slot) = ir.slot_by_name(slot_name) else {
                report.warn(Degradation::MissingReference { kind: "slot".into(), name: slot_name.clone() });
                continue;
            };
            for (placeholder, value) in Fields::new(placeholders, format!("skins.{skin_name}")).entries() {
                let context = format!("skins.{skin_name}.{slot_name}.{placeholder}");
                let attachment = read_attachment(value, placeholder, slot, &context, report)?;
                let id = AttachmentId::from_index(ir.attachments.len()).ok_or_else(|| {
                    DecodeError::corrupt("skeleton has more attachments than the attachment handle can address")
                })?;
                if let Some(end) = clipping_end_name(value) {
                    pending_clips.push((id, end.to_string()));
                }
                ir.attachments.push(attachment);
                ir.skins[skin_id.index()].entries.push(SkinEntry { slot, name: placeholder.clone(), attachment: id });
            }
        }
    }

    for (id, end_name) in pending_clips {
        match ir.slot_by_name(&end_name) {
            Some(end) => {
                if let Some(Attachment { kind: AttachmentKind::Clipping(c), .. }) = ir.attachments.get_mut(id.index()) {
                    c.end_slot = Some(end);
                }
            }
            None => report.warn(Degradation::MissingReference { kind: "clipping end slot".into(), name: end_name }),
        }
    }
    Ok(())
}

fn read_events(f: &mut Fields<'_>, ir: &mut SpineIr, report: &mut LoadReport) {
    let Some(events) = f.get("events") else { return };
    for (name, value) in Fields::new(events, "events").entries() {
        let mut e = Fields::new(value, format!("events.{name}"));
        let data = EventData {
            name: name.clone(),
            int_value: e.i32("int", 0),
            float_value: e.f32("float", 0.0),
            string_value: e.string("string").unwrap_or_default(),
            audio_path: e.string("audio"),
            volume: e.f32("volume", 1.0),
            balance: e.f32("balance", 0.0),
        };
        e.finish(report);
        ir.events.push(data);
    }
    ir.events.sort_by(|a, b| a.name.cmp(&b.name));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect;
    use a2d_core::ir::spine::DEFAULT_SKIN;

    fn skeleton(body: &str) -> Vec<u8> {
        format!("{{\"skeleton\":{{\"spine\":\"3.8.99\",\"hash\":\"abc\"}},{body}}}").into_bytes()
    }

    fn decode_str(bytes: &[u8]) -> (SpineIr, LoadReport) {
        let d = detect::detect(bytes).expect("detectable");
        let mut report = LoadReport::new();
        let ir = decode(bytes, &d, Atlas::default(), &mut report).expect("decodable");
        (ir, report)
    }

    #[test]
    fn metadata_is_read_from_the_skeleton_object() {
        let bytes = br#"{"skeleton":{"spine":"3.8.99","hash":"h1","x":-10,"y":-20,"width":100,"height":200,
                        "fps":30,"images":"./images/"},"bones":[{"name":"root"}]}"#;
        let (ir, report) = decode_str(bytes);
        assert_eq!(ir.metadata.source_version, "3.8.99");
        assert_eq!(ir.metadata.hash.as_deref(), Some("h1"));
        assert_eq!(ir.metadata.origin, Vec2::new(-10.0, -20.0));
        assert_eq!(ir.metadata.size, Vec2::new(100.0, 200.0));
        assert_eq!(ir.metadata.fps, Some(30.0));
        assert_eq!(ir.metadata.images_path.as_deref(), Some("./images/"));
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn bones_are_read_with_their_hierarchy_and_setup_pose() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},
                       {"name":"torso","parent":"root","length":50,"x":1,"y":2,"rotation":30,
                        "scaleX":2,"scaleY":3,"shearX":4,"shearY":5,"transform":"noScale"}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        assert_eq!(ir.bones.len(), 2);
        let torso = &ir.bones[1];
        assert_eq!(torso.parent, Some(BoneId(0)));
        assert_eq!(torso.length, 50.0);
        assert_eq!(torso.setup.position, Vec2::new(1.0, 2.0));
        assert_eq!(torso.setup.rotation, 30.0);
        assert_eq!(torso.setup.scale, Vec2::new(2.0, 3.0));
        assert_eq!(torso.setup.shear, Vec2::new(4.0, 5.0));
        assert_eq!(torso.inherit, TransformInherit::NoScale);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn the_spine_42_inherit_key_is_accepted_too() {
        let bytes = skeleton(r#""bones":[{"name":"root"},{"name":"b","parent":"root","inherit":"onlyTranslation"}]"#);
        let (ir, _) = decode_str(&bytes);
        assert_eq!(ir.bones[1].inherit, TransformInherit::OnlyTranslation);
    }

    #[test]
    fn a_forward_parent_reference_is_corrupt() {
        let bytes = skeleton(r#""bones":[{"name":"child","parent":"parent"},{"name":"parent"}]"#);
        let d = detect::detect(&bytes).unwrap();
        let mut report = LoadReport::new();
        let err = decode(&bytes, &d, Atlas::default(), &mut report).unwrap_err();
        assert!(err.to_string().contains("not defined before"), "{err}");
    }

    #[test]
    fn slots_resolve_their_bone_and_blend_mode() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],
               "slots":[{"name":"body","bone":"root","color":"ff0000ff","dark":"112233",
                         "attachment":"shirt","blend":"additive"}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        let slot = &ir.slots[0];
        assert_eq!(slot.bone, BoneId(0));
        assert_eq!(slot.color, Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert!(slot.dark_color.is_some());
        assert_eq!(slot.setup_attachment.as_deref(), Some("shirt"));
        assert_eq!(slot.blend_mode, BlendMode::Additive);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn an_unknown_blend_mode_is_reported_and_falls_back() {
        let bytes = skeleton(r#""bones":[{"name":"root"}],"slots":[{"name":"s","bone":"root","blend":"overlay"}]"#);
        let (ir, report) = decode_str(&bytes);
        assert_eq!(ir.slots[0].blend_mode, BlendMode::Normal);
        assert!(report.to_string().contains("overlay"), "{report}");
    }

    #[test]
    fn a_slot_on_an_undefined_bone_is_corrupt() {
        let bytes = skeleton(r#""bones":[{"name":"root"}],"slots":[{"name":"s","bone":"ghost"}]"#);
        let d = detect::detect(&bytes).unwrap();
        let mut report = LoadReport::new();
        assert!(decode(&bytes, &d, Atlas::default(), &mut report).is_err());
    }

    #[test]
    fn the_default_skin_exists_even_when_the_source_declares_none() {
        let bytes = skeleton(r#""bones":[{"name":"root"}]"#);
        let (ir, _) = decode_str(&bytes);
        assert_eq!(ir.skins.len(), 1);
        assert_eq!(ir.skins[DEFAULT_SKIN.index()].name, "default");
    }

    #[test]
    fn the_38_skin_array_form_is_read() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],"slots":[{"name":"body","bone":"root"}],
               "skins":[{"name":"default","attachments":{"body":{"shirt":{}}}},
                        {"name":"blue","attachments":{"body":{"shirt":{"path":"shirt_blue"}}}}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        assert_eq!(ir.skins.len(), 2);
        assert_eq!(ir.skins[0].name, "default");
        let blue = ir.skin_by_name("blue").unwrap();
        let id = ir.resolve_attachment(blue, SlotId(0), "shirt").unwrap();
        assert_eq!(ir.attachment(id).unwrap().kind.region_path(), Some("shirt_blue"));
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn the_pre_38_skin_object_form_is_read() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],"slots":[{"name":"body","bone":"root"}],
               "skins":{"default":{"body":{"shirt":{}}}}"#,
        );
        let (ir, _) = decode_str(&bytes);
        assert_eq!(ir.skins.len(), 1);
        assert!(ir.resolve_attachment(DEFAULT_SKIN, SlotId(0), "shirt").is_some());
    }

    #[test]
    fn a_clipping_attachment_resolves_its_end_slot() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],
               "slots":[{"name":"clip","bone":"root"},{"name":"body","bone":"root"}],
               "skins":[{"name":"default","attachments":{"clip":{"mask":{
                   "type":"clipping","end":"body","vertexCount":3,"vertices":[0,0,1,0,0,1]}}}}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        let id = ir.resolve_attachment(DEFAULT_SKIN, SlotId(0), "mask").unwrap();
        match &ir.attachment(id).unwrap().kind {
            AttachmentKind::Clipping(c) => assert_eq!(c.end_slot, Some(SlotId(1))),
            other => panic!("expected clipping, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_clipping_end_slot_that_does_not_exist_is_reported() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],"slots":[{"name":"clip","bone":"root"}],
               "skins":[{"name":"default","attachments":{"clip":{"mask":{
                   "type":"clipping","end":"ghost","vertexCount":3,"vertices":[0,0,1,0,0,1]}}}}]"#,
        );
        let (_, report) = decode_str(&bytes);
        assert!(report.to_string().contains("ghost"), "{report}");
    }

    #[test]
    fn ik_constraints_are_read() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},{"name":"a","parent":"root"},{"name":"t","parent":"root"}],
               "ik":[{"name":"aim","order":3,"bones":["a"],"target":"t","mix":0.5,"softness":2,
                      "bendPositive":false,"compress":true,"stretch":true}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        let c = &ir.ik_constraints[0];
        assert_eq!(c.name, "aim");
        assert_eq!(c.order, 3);
        assert_eq!(c.bones, vec![BoneId(1)]);
        assert_eq!(c.target, BoneId(2));
        assert_eq!(c.mix, 0.5);
        assert_eq!(c.softness, 2.0);
        assert!(!c.bend_positive);
        assert!(c.compress && c.stretch);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn v3_transform_mixes_expand_to_the_per_axis_form() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},{"name":"a","parent":"root"}],
               "transform":[{"name":"tc","bones":["a"],"target":"root",
                             "rotateMix":0.5,"translateMix":0.25,"scaleMix":0.75,"shearMix":0.1}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        let c = &ir.transform_constraints[0];
        assert_eq!(c.mix_rotate, 0.5);
        assert_eq!((c.mix_x, c.mix_y), (0.25, 0.25));
        assert_eq!((c.mix_scale_x, c.mix_scale_y), (0.75, 0.75));
        assert_eq!(c.mix_shear_y, 0.1);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn v4_transform_mixes_are_read_directly() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},{"name":"a","parent":"root"}],
               "transform":[{"name":"tc","bones":["a"],"target":"root",
                             "mixRotate":0.1,"mixX":0.2,"mixY":0.3,"mixScaleX":0.4,"mixScaleY":0.5,"mixShearY":0.6}]"#,
        );
        let (ir, _) = decode_str(&bytes);
        let c = &ir.transform_constraints[0];
        assert_eq!((c.mix_rotate, c.mix_x, c.mix_y), (0.1, 0.2, 0.3));
        assert_eq!((c.mix_scale_x, c.mix_scale_y, c.mix_shear_y), (0.4, 0.5, 0.6));
    }

    #[test]
    fn path_constraints_read_their_modes() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},{"name":"a","parent":"root"}],
               "slots":[{"name":"route","bone":"root"}],
               "path":[{"name":"pc","bones":["a"],"target":"route","positionMode":"fixed",
                        "spacingMode":"percent","rotateMode":"chain","rotation":45,"position":10,"spacing":2}]"#,
        );
        let (ir, report) = decode_str(&bytes);
        let c = &ir.path_constraints[0];
        assert_eq!(c.position_mode, PathPositionMode::Fixed);
        assert_eq!(c.spacing_mode, PathSpacingMode::Percent);
        assert_eq!(c.rotate_mode, PathRotateMode::Chain);
        assert_eq!(c.offset_rotation, 45.0);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn constraint_update_order_is_built() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"},{"name":"a","parent":"root"}],
               "ik":[{"name":"i","bones":["a"],"target":"root","order":5}],
               "transform":[{"name":"t","bones":["a"],"target":"root","order":1}]"#,
        );
        let (ir, _) = decode_str(&bytes);
        assert_eq!(ir.constraint_order.len(), 2);
        assert_eq!(ir.constraint_order[0].order, 1);
        assert_eq!(ir.constraint_order[1].order, 5);
    }

    #[test]
    fn events_are_declared_and_sorted() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],
               "events":{"zzz":{"int":1},"aaa":{"float":2.5,"string":"s","volume":0.5,"balance":-1}}"#,
        );
        let (ir, report) = decode_str(&bytes);
        assert_eq!(ir.events.len(), 2);
        assert_eq!(ir.events[0].name, "aaa");
        assert_eq!(ir.events[0].float_value, 2.5);
        assert_eq!(ir.events[0].string_value, "s");
        assert_eq!(ir.events[1].int_value, 1);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn unknown_top_level_keys_are_reported() {
        let bytes = skeleton(r#""bones":[{"name":"root"}],"futureSection":{"a":1}"#);
        let (_, report) = decode_str(&bytes);
        assert!(report.to_string().contains("futureSection"), "{report}");
    }

    #[test]
    fn malformed_json_is_a_located_corruption() {
        let bytes = br#"{"skeleton":{"spine":"3.8.99"},"bones":[}"#;
        let d = detect::detect(bytes).unwrap();
        let mut report = LoadReport::new();
        let err = decode(bytes, &d, Atlas::default(), &mut report).unwrap_err();
        assert!(matches!(err, DecodeError::Corrupt { .. }), "{err}");
    }

    #[test]
    fn an_animation_survives_a_full_round_of_decoding() {
        let bytes = skeleton(
            r#""bones":[{"name":"root"}],"slots":[{"name":"body","bone":"root"}],
               "animations":{"idle":{"bones":{"root":{"rotate":[{"time":0,"angle":0},{"time":1,"angle":10}]}}}}"#,
        );
        let (ir, report) = decode_str(&bytes);
        assert_eq!(ir.animations.len(), 1);
        assert_eq!(ir.animations[0].name, "idle");
        assert_eq!(ir.animations[0].duration, 1.0);
        assert!(ir.animation_by_name("idle").is_some());
        assert!(report.is_empty(), "{report}");
    }
}
