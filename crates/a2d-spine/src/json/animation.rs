//! Animation decoding from JSON.
//!
//! Curves are resolved in a second pass because Spine 4.x stores control points
//! in absolute coordinates that only make sense relative to the *next*
//! keyframe. Each timeline is therefore read into [`RawKey`] values first and
//! converted afterwards.

use a2d_core::ir::ids::{BoneId, EventId, SlotId};
use a2d_core::ir::spine::{
    Animation, AttachmentKey, Axes, ColorChannels, ColorKey, DeformKey, DrawOrderKey, EventKey, IkKey, PathMixKey,
    ScalarKey, SpineIr, Timeline, TransformKey, TwoColorKey, Vec2Key,
};
use a2d_core::{DecodeError, Degradation, Interpolation, LoadReport, Rgb, Rgba, Vec2};
use serde_json::Value;

use crate::detect::SpineFamily;
use crate::json::curve::{read_raw_curve, resolve, RawCurve};
use crate::json::fields::Fields;

/// The mixes and flags one IK keyframe carries.
#[derive(Clone, Copy)]
struct RawIk {
    mix: f32,
    softness: f32,
    bend_positive: bool,
    compress: bool,
    stretch: bool,
}

/// A keyframe as read, before its curve has been resolved.
struct RawKey<V> {
    time: f32,
    value: V,
    curve: RawCurve,
}

/// Decodes every animation in the `animations` object.
pub fn read_animations(
    value: &Value,
    ir: &mut SpineIr,
    family: SpineFamily,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    let animations = Fields::new(value, "animations");
    let mut out = Vec::with_capacity(animations.len());
    for (name, body) in animations.entries() {
        out.push(read_animation(name, body, ir, family, report)?);
    }
    ir.animations = out;
    Ok(())
}

fn read_animation(
    name: &str,
    value: &Value,
    ir: &SpineIr,
    family: SpineFamily,
    report: &mut LoadReport,
) -> Result<Animation, DecodeError> {
    let context = format!("animations.{name}");
    let mut f = Fields::new(value, context.clone());
    let mut anim = Animation::new(name);

    if let Some(bones) = f.get("bones") {
        read_bone_timelines(bones, &context, ir, family, &mut anim, report)?;
    }
    if let Some(slots) = f.get("slots") {
        read_slot_timelines(slots, &context, ir, family, &mut anim, report)?;
    }
    if let Some(ik) = f.get("ik") {
        read_ik_timelines(ik, &context, ir, family, &mut anim, report);
    }
    if let Some(tc) = f.get("transform") {
        read_transform_timelines(tc, &context, ir, family, &mut anim, report);
    }
    if let Some(path) = f.get("path") {
        read_path_timelines(path, &context, ir, family, &mut anim, report);
    }
    // Spine 4.2 renamed `deform` to `attachments`.
    let deform = f.get("deform").or_else(|| f.get("attachments"));
    if let Some(deform) = deform {
        read_deform_timelines(deform, &context, ir, family, &mut anim, report)?;
    }
    if let Some(order) = f.get("drawOrder").or_else(|| f.get("draworder")) {
        read_draw_order(order, &context, ir, &mut anim, report);
    }
    if let Some(events) = f.get("events") {
        read_events(events, &context, ir, &mut anim, report);
    }
    f.finish(report);

    anim.timelines.retain(|t| !t.is_empty());
    anim.duration = anim.max_key_time();
    Ok(anim)
}

// ---------------------------------------------------------------- bones

fn read_bone_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for (bone_name, channels) in Fields::new(value, context).entries() {
        let Some(bone) = ir.bone_by_name(bone_name) else {
            report.warn(Degradation::MissingReference { kind: "bone".into(), name: bone_name.clone() });
            continue;
        };
        let context = format!("{context}.bones.{bone_name}");
        let mut f = Fields::new(channels, context.clone());

        if let Some(keys) = f.get("rotate") {
            // Spine 3.x names the value `angle`; 4.x names it `value`.
            let raw = read_scalar_keys(keys, &context, family, 1, &["angle", "value"], 0.0, report)?;
            anim.timelines.push(Timeline::BoneRotate { bone, keys: resolve_scalar(&raw) });
        }
        for (key, default, make) in [
            ("translate", 0.0f32, TranslateKind::Translate),
            ("scale", 1.0, TranslateKind::Scale),
            ("shear", 0.0, TranslateKind::Shear),
        ] {
            if let Some(keys) = f.get(key) {
                let raw = read_vec2_keys(keys, &context, family, default, report)?;
                anim.timelines.push(make.build(bone, Axes::Both, resolve_vec2(&raw)));
            }
        }
        // Spine 4.x single-axis channels.
        for (key, kind, axis, default) in [
            ("translatex", TranslateKind::Translate, Axes::X, 0.0f32),
            ("translatey", TranslateKind::Translate, Axes::Y, 0.0),
            ("scalex", TranslateKind::Scale, Axes::X, 1.0),
            ("scaley", TranslateKind::Scale, Axes::Y, 1.0),
            ("shearx", TranslateKind::Shear, Axes::X, 0.0),
            ("sheary", TranslateKind::Shear, Axes::Y, 0.0),
        ] {
            if let Some(keys) = f.get(key) {
                let raw = read_scalar_keys(keys, &context, family, 1, &["value"], default, report)?;
                let resolved = resolve_scalar(&raw);
                let keys = resolved
                    .into_iter()
                    .map(|k| {
                        let value = match axis {
                            Axes::Y => Vec2::new(default, k.value),
                            _ => Vec2::new(k.value, default),
                        };
                        Vec2Key { time: k.time, value, interp_x: k.interp, interp_y: k.interp }
                    })
                    .collect();
                anim.timelines.push(kind.build(bone, axis, keys));
            }
        }
        f.finish(report);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TranslateKind {
    Translate,
    Scale,
    Shear,
}

impl TranslateKind {
    fn build(self, bone: BoneId, axes: Axes, keys: Vec<Vec2Key>) -> Timeline {
        match self {
            TranslateKind::Translate => Timeline::BoneTranslate { bone, axes, keys },
            TranslateKind::Scale => Timeline::BoneScale { bone, axes, keys },
            TranslateKind::Shear => Timeline::BoneShear { bone, axes, keys },
        }
    }
}

// ---------------------------------------------------------------- slots

fn read_slot_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for (slot_name, channels) in Fields::new(value, context).entries() {
        let Some(slot) = ir.slot_by_name(slot_name) else {
            report.warn(Degradation::MissingReference { kind: "slot".into(), name: slot_name.clone() });
            continue;
        };
        let context = format!("{context}.slots.{slot_name}");
        let mut f = Fields::new(channels, context.clone());

        if let Some(keys) = f.get("attachment") {
            anim.timelines.push(Timeline::SlotAttachment { slot, keys: read_attachment_keys(keys, &context, report) });
        }

        // `color` is the 3.x spelling of `rgba`.
        for (key, channels) in
            [("color", ColorChannels::Rgba), ("rgba", ColorChannels::Rgba), ("rgb", ColorChannels::Rgb)]
        {
            if let Some(keys) = f.get(key) {
                let raw = read_color_keys(keys, &context, family, channels, report)?;
                anim.timelines.push(Timeline::SlotColor { slot, channels, keys: resolve_color(&raw, channels) });
            }
        }

        if let Some(keys) = f.get("alpha") {
            let raw = read_scalar_keys(keys, &context, family, 1, &["value", "color"], 1.0, report)?;
            anim.timelines.push(Timeline::SlotAlpha { slot, keys: resolve_scalar(&raw) });
        }

        // `twoColor` is the 3.x spelling of `rgba2`.
        for (key, channels) in
            [("twoColor", ColorChannels::Rgba), ("rgba2", ColorChannels::Rgba), ("rgb2", ColorChannels::Rgb)]
        {
            if let Some(keys) = f.get(key) {
                let raw = read_two_color_keys(keys, &context, family, channels, report)?;
                anim.timelines.push(Timeline::SlotTwoColor { slot, channels, keys: resolve_two_color(&raw, channels) });
            }
        }
        f.finish(report);
    }
    Ok(())
}

fn read_attachment_keys(value: &Value, context: &str, report: &mut LoadReport) -> Vec<AttachmentKey> {
    let mut keys = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}.attachment[{i}]"));
        let time = f.f32("time", 0.0);
        // A null name hides the slot, which is different from the key being absent.
        let name = f.get("name").and_then(|v| v.as_str()).map(str::to_string);
        f.finish(report);
        keys.push(AttachmentKey { time, name });
    }
    sort_by_time(&mut keys, |k| k.time);
    keys
}

// ---------------------------------------------------------------- constraints

fn read_ik_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) {
    for (name, keys) in Fields::new(value, context).entries() {
        let Some(index) = ir.ik_constraints.iter().position(|c| c.name == *name) else {
            report.warn(Degradation::MissingReference { kind: "ik constraint".into(), name: name.clone() });
            continue;
        };
        let context = format!("{context}.ik.{name}");
        let mut raw: Vec<RawKey<RawIk>> = Vec::new();
        for (i, key) in array_of(keys).iter().enumerate() {
            let mut f = Fields::new(key, format!("{context}[{i}]"));
            let time = f.f32("time", 0.0);
            let mix = f.f32("mix", 1.0);
            let softness = f.f32("softness", 0.0);
            let bend = f.bool("bendPositive", true);
            let compress = f.bool("compress", false);
            let stretch = f.bool("stretch", false);
            let curve = read_raw_curve(&mut f, family, 2, report);
            f.finish(report);
            raw.push(RawKey { time, value: RawIk { mix, softness, bend_positive: bend, compress, stretch }, curve });
        }
        sort_by_time(&mut raw, |k| k.time);
        let keys = raw
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let next = raw.get(i + 1);
                IkKey {
                    time: k.time,
                    mix: k.value.mix,
                    softness: k.value.softness,
                    bend_positive: k.value.bend_positive,
                    compress: k.value.compress,
                    stretch: k.value.stretch,
                    interp: resolve_one(&k.curve, 0, k.time, k.value.mix, next.map(|n| (n.time, n.value.mix))),
                }
            })
            .collect();
        anim.timelines
            .push(Timeline::IkConstraint { constraint: a2d_core::ir::ids::IkConstraintId(index as u16), keys });
    }
}

fn read_transform_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) {
    for (name, keys) in Fields::new(value, context).entries() {
        let Some(index) = ir.transform_constraints.iter().position(|c| c.name == *name) else {
            report.warn(Degradation::MissingReference { kind: "transform constraint".into(), name: name.clone() });
            continue;
        };
        let context = format!("{context}.transform.{name}");
        let mut raw: Vec<RawKey<[f32; 6]>> = Vec::new();
        for (i, key) in array_of(keys).iter().enumerate() {
            let mut f = Fields::new(key, format!("{context}[{i}]"));
            let time = f.f32("time", 0.0);
            // Spine 3.x groups translate/scale into single mixes; 4.x splits
            // them per axis. Reading both spellings makes the IR uniform.
            let legacy_translate = f.opt_f32("translateMix");
            let legacy_scale = f.opt_f32("scaleMix");
            let legacy_rotate = f.opt_f32("rotateMix");
            let legacy_shear = f.opt_f32("shearMix");
            let mix = [
                f.opt_f32("mixRotate").or(legacy_rotate).unwrap_or(1.0),
                f.opt_f32("mixX").or(legacy_translate).unwrap_or(1.0),
                f.opt_f32("mixY").or(legacy_translate).unwrap_or(1.0),
                f.opt_f32("mixScaleX").or(legacy_scale).unwrap_or(1.0),
                f.opt_f32("mixScaleY").or(legacy_scale).unwrap_or(1.0),
                f.opt_f32("mixShearY").or(legacy_shear).unwrap_or(1.0),
            ];
            let curve = read_raw_curve(&mut f, family, 6, report);
            f.finish(report);
            raw.push(RawKey { time, value: mix, curve });
        }
        sort_by_time(&mut raw, |k| k.time);
        let keys = raw
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let next = raw.get(i + 1);
                TransformKey {
                    time: k.time,
                    mix_rotate: k.value[0],
                    mix_x: k.value[1],
                    mix_y: k.value[2],
                    mix_scale_x: k.value[3],
                    mix_scale_y: k.value[4],
                    mix_shear_y: k.value[5],
                    interp: resolve_one(&k.curve, 0, k.time, k.value[0], next.map(|n| (n.time, n.value[0]))),
                }
            })
            .collect();
        anim.timelines.push(Timeline::TransformConstraint {
            constraint: a2d_core::ir::ids::TransformConstraintId(index as u16),
            keys,
        });
    }
}

fn read_path_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) {
    for (name, channels) in Fields::new(value, context).entries() {
        let Some(index) = ir.path_constraints.iter().position(|c| c.name == *name) else {
            report.warn(Degradation::MissingReference { kind: "path constraint".into(), name: name.clone() });
            continue;
        };
        let id = a2d_core::ir::ids::PathConstraintId(index as u16);
        let context = format!("{context}.path.{name}");
        let mut f = Fields::new(channels, context.clone());

        for (key, make) in [("position", 0usize), ("spacing", 1usize)] {
            if let Some(keys) = f.get(key) {
                match read_scalar_keys(keys, &context, family, 1, &["position", "spacing", "value"], 0.0, report) {
                    Ok(raw) => {
                        let resolved = resolve_scalar(&raw);
                        anim.timelines.push(if make == 0 {
                            Timeline::PathPosition { constraint: id, keys: resolved }
                        } else {
                            Timeline::PathSpacing { constraint: id, keys: resolved }
                        });
                    }
                    Err(e) => report.note(format!("{context}.{key}: {e}")),
                }
            }
        }

        if let Some(keys) = f.get("mix") {
            let mut raw: Vec<RawKey<[f32; 3]>> = Vec::new();
            for (i, key) in array_of(keys).iter().enumerate() {
                let mut kf = Fields::new(key, format!("{context}.mix[{i}]"));
                let time = kf.f32("time", 0.0);
                let legacy_translate = kf.opt_f32("translateMix");
                let legacy_rotate = kf.opt_f32("rotateMix");
                let mix = [
                    kf.opt_f32("mixRotate").or(legacy_rotate).unwrap_or(1.0),
                    kf.opt_f32("mixX").or(legacy_translate).unwrap_or(1.0),
                    kf.opt_f32("mixY").or(legacy_translate).unwrap_or(1.0),
                ];
                let curve = read_raw_curve(&mut kf, family, 3, report);
                kf.finish(report);
                raw.push(RawKey { time, value: mix, curve });
            }
            sort_by_time(&mut raw, |k| k.time);
            let keys = raw
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let next = raw.get(i + 1);
                    PathMixKey {
                        time: k.time,
                        mix_rotate: k.value[0],
                        mix_x: k.value[1],
                        mix_y: k.value[2],
                        interp: resolve_one(&k.curve, 0, k.time, k.value[0], next.map(|n| (n.time, n.value[0]))),
                    }
                })
                .collect();
            anim.timelines.push(Timeline::PathMix { constraint: id, keys });
        }
        f.finish(report);
    }
}

// ---------------------------------------------------------------- deform

fn read_deform_timelines(
    value: &Value,
    context: &str,
    ir: &SpineIr,
    family: SpineFamily,
    anim: &mut Animation,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    for (skin_name, slots) in Fields::new(value, context).entries() {
        let Some(skin) = ir.skin_by_name(skin_name) else {
            report.warn(Degradation::MissingReference { kind: "skin".into(), name: skin_name.clone() });
            continue;
        };
        for (slot_name, attachments) in Fields::new(slots, context).entries() {
            let Some(slot) = ir.slot_by_name(slot_name) else {
                report.warn(Degradation::MissingReference { kind: "slot".into(), name: slot_name.clone() });
                continue;
            };
            for (att_name, keys) in Fields::new(attachments, context).entries() {
                let Some(attachment) = ir.resolve_attachment(skin, slot, att_name) else {
                    report.warn(Degradation::MissingReference {
                        kind: "deform target attachment".into(),
                        name: format!("{skin_name}/{slot_name}/{att_name}"),
                    });
                    continue;
                };
                let context = format!("{context}.deform.{skin_name}.{slot_name}.{att_name}");
                let deform_len =
                    ir.attachment(attachment).and_then(|a| a.kind.deformable_vertices()).map(|v| v.deform_len());
                let Some(deform_len) = deform_len else {
                    report.warn(Degradation::UnsupportedTimeline {
                        animation: anim.name.clone(),
                        kind: format!("deform on non-deformable attachment {att_name:?}"),
                    });
                    continue;
                };
                let keys = read_deform_keys(keys, &context, family, deform_len, report)?;
                anim.timelines.push(Timeline::Deform { slot, skin, attachment, keys });
            }
        }
    }
    Ok(())
}

fn read_deform_keys(
    value: &Value,
    context: &str,
    family: SpineFamily,
    deform_len: usize,
    report: &mut LoadReport,
) -> Result<Vec<DeformKey>, DecodeError> {
    let mut raw: Vec<RawKey<(u32, Vec<f32>)>> = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}[{i}]"));
        let time = f.f32("time", 0.0);
        let offset = f.u32("offset", 0);
        let values = f.f32_array("vertices")?;
        if offset as usize + values.len() > deform_len {
            report.warn(Degradation::ClampedValue {
                context: context.to_string(),
                field: "vertices".into(),
                detail: format!(
                    "key {i} writes {} floats at offset {offset} but the attachment has {deform_len}",
                    values.len()
                ),
            });
        }
        let curve = read_raw_curve(&mut f, family, 1, report);
        f.finish(report);
        raw.push(RawKey { time, value: (offset, values), curve });
    }
    sort_by_time(&mut raw, |k| k.time);
    Ok(raw
        .iter()
        .enumerate()
        .map(|(i, k)| DeformKey {
            time: k.time,
            offset: k.value.0,
            values: k.value.1.clone(),
            // Deform curves ease the whole vertex set together, so the curve is
            // normalised against a synthetic 0..1 value span.
            interp: resolve_one(&k.curve, 0, k.time, 0.0, raw.get(i + 1).map(|n| (n.time, 1.0))),
        })
        .collect())
}

// ---------------------------------------------------------------- draw order & events

fn read_draw_order(value: &Value, context: &str, ir: &SpineIr, anim: &mut Animation, report: &mut LoadReport) {
    let mut keys = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}.drawOrder[{i}]"));
        let time = f.f32("time", 0.0);
        let order = f.get("offsets").map(|offsets| apply_draw_order_offsets(offsets, ir, context, report));
        f.finish(report);
        keys.push(DrawOrderKey { time, order });
    }
    sort_by_time(&mut keys, |k| k.time);
    anim.timelines.push(Timeline::DrawOrder { keys });
}

/// Turns Spine's sparse `{slot, offset}` list into an explicit draw order.
///
/// Spine stores only the slots that move. The reference algorithm removes those
/// slots from the setup order, places each at `setup_index + offset`, then fills
/// the gaps with the untouched slots in their original order.
fn apply_draw_order_offsets(offsets: &Value, ir: &SpineIr, context: &str, report: &mut LoadReport) -> Vec<SlotId> {
    let n = ir.slots.len();
    let mut result: Vec<Option<SlotId>> = vec![None; n];
    let mut unchanged: Vec<SlotId> = Vec::with_capacity(n);
    let mut moved = vec![false; n];

    let mut placements: Vec<(usize, SlotId)> = Vec::new();
    for entry in array_of(offsets) {
        let mut f = Fields::new(entry, context);
        let Some(name) = f.str("slot") else {
            report.note(format!("{context}: draw order entry without a slot name"));
            continue;
        };
        let Some(slot) = ir.slot_by_name(name) else {
            report.warn(Degradation::MissingReference { kind: "slot".into(), name: name.to_string() });
            continue;
        };
        let offset = f.i32("offset", 0);
        f.finish(report);
        let target = slot.index() as i64 + offset as i64;
        if target < 0 || target as usize >= n {
            report.warn(Degradation::ClampedValue {
                context: context.to_string(),
                field: "drawOrder offset".into(),
                detail: format!("slot {name:?} offset {offset} lands outside the slot range"),
            });
            continue;
        }
        moved[slot.index()] = true;
        placements.push((target as usize, slot));
    }

    for (i, _) in ir.slots.iter().enumerate() {
        if !moved[i] {
            if let Some(id) = SlotId::from_index(i) {
                unchanged.push(id);
            }
        }
    }

    for (target, slot) in placements {
        if result[target].is_some() {
            report.warn(Degradation::ClampedValue {
                context: context.to_string(),
                field: "drawOrder offset".into(),
                detail: format!("two slots claim draw position {target}"),
            });
            continue;
        }
        result[target] = Some(slot);
    }

    let mut fill = unchanged.into_iter();
    result.into_iter().map(|slot| slot.or_else(|| fill.next()).unwrap_or(SlotId(0))).collect()
}

fn read_events(value: &Value, context: &str, ir: &SpineIr, anim: &mut Animation, report: &mut LoadReport) {
    let mut keys = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}.events[{i}]"));
        let time = f.f32("time", 0.0);
        let Some(name) = f.str("name") else {
            report.note(format!("{context}.events[{i}]: event without a name"));
            continue;
        };
        let Some(index) = ir.events.iter().position(|e| e.name == name) else {
            report.warn(Degradation::MissingReference { kind: "event".into(), name: name.to_string() });
            continue;
        };
        let default = &ir.events[index];
        let key = EventKey {
            time,
            event: EventId(index as u16),
            int_value: f.i32("int", default.int_value),
            float_value: f.f32("float", default.float_value),
            string_value: f.string("string"),
            volume: f.f32("volume", default.volume),
            balance: f.f32("balance", default.balance),
        };
        f.finish(report);
        keys.push(key);
    }
    sort_by_time(&mut keys, |k| k.time);
    anim.timelines.push(Timeline::Event { keys });
}

// ---------------------------------------------------------------- shared key readers

fn read_scalar_keys(
    value: &Value,
    context: &str,
    family: SpineFamily,
    components: usize,
    value_keys: &[&'static str],
    default: f32,
    report: &mut LoadReport,
) -> Result<Vec<RawKey<f32>>, DecodeError> {
    let mut raw = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}[{i}]"));
        let time = f.f32("time", 0.0);
        // Try each spelling; every one is marked consumed so that the unused
        // spellings are not reported as unknown keys.
        let mut found = None;
        for name in value_keys {
            let v = f.opt_f32(name);
            if found.is_none() {
                found = v;
            }
        }
        let curve = read_raw_curve(&mut f, family, components, report);
        f.finish(report);
        raw.push(RawKey { time, value: found.unwrap_or(default), curve });
    }
    sort_by_time(&mut raw, |k| k.time);
    Ok(raw)
}

fn read_vec2_keys(
    value: &Value,
    context: &str,
    family: SpineFamily,
    default: f32,
    report: &mut LoadReport,
) -> Result<Vec<RawKey<Vec2>>, DecodeError> {
    let mut raw = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let mut f = Fields::new(key, format!("{context}[{i}]"));
        let time = f.f32("time", 0.0);
        let v = Vec2::new(f.f32("x", default), f.f32("y", default));
        let curve = read_raw_curve(&mut f, family, 2, report);
        f.finish(report);
        raw.push(RawKey { time, value: v, curve });
    }
    sort_by_time(&mut raw, |k| k.time);
    Ok(raw)
}

fn read_color_keys(
    value: &Value,
    context: &str,
    family: SpineFamily,
    channels: ColorChannels,
    report: &mut LoadReport,
) -> Result<Vec<RawKey<Rgba>>, DecodeError> {
    let mut raw = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let ctx = format!("{context}[{i}]");
        let mut f = Fields::new(key, ctx.clone());
        let time = f.f32("time", 0.0);
        let color = read_hex(&mut f, "color", Rgba::WHITE, &ctx, report);
        let curve = read_raw_curve(&mut f, family, channels.component_count(), report);
        f.finish(report);
        raw.push(RawKey { time, value: color, curve });
    }
    sort_by_time(&mut raw, |k| k.time);
    Ok(raw)
}

fn read_two_color_keys(
    value: &Value,
    context: &str,
    family: SpineFamily,
    channels: ColorChannels,
    report: &mut LoadReport,
) -> Result<Vec<RawKey<(Rgba, Rgb)>>, DecodeError> {
    let mut raw = Vec::new();
    for (i, key) in array_of(value).iter().enumerate() {
        let ctx = format!("{context}[{i}]");
        let mut f = Fields::new(key, ctx.clone());
        let time = f.f32("time", 0.0);
        let light = read_hex(&mut f, "light", Rgba::WHITE, &ctx, report);
        let dark = read_hex(&mut f, "dark", Rgba::new(0.0, 0.0, 0.0, 1.0), &ctx, report).rgb();
        let curve = read_raw_curve(&mut f, family, channels.component_count() + 3, report);
        f.finish(report);
        raw.push(RawKey { time, value: (light, dark), curve });
    }
    sort_by_time(&mut raw, |k| k.time);
    Ok(raw)
}

fn read_hex(f: &mut Fields<'_>, key: &'static str, default: Rgba, context: &str, report: &mut LoadReport) -> Rgba {
    match f.str(key) {
        None => default,
        Some(hex) => match Rgba::from_hex(hex) {
            Some(c) => c,
            None => {
                report.note(format!("{context}: malformed `{key}` value {hex:?}, using the default"));
                default
            }
        },
    }
}

// ---------------------------------------------------------------- curve resolution

fn resolve_scalar(raw: &[RawKey<f32>]) -> Vec<ScalarKey> {
    raw.iter()
        .enumerate()
        .map(|(i, k)| ScalarKey {
            time: k.time,
            value: k.value,
            interp: resolve_one(&k.curve, 0, k.time, k.value, raw.get(i + 1).map(|n| (n.time, n.value))),
        })
        .collect()
}

fn resolve_vec2(raw: &[RawKey<Vec2>]) -> Vec<Vec2Key> {
    raw.iter()
        .enumerate()
        .map(|(i, k)| {
            let next = raw.get(i + 1);
            Vec2Key {
                time: k.time,
                value: k.value,
                interp_x: resolve_one(&k.curve, 0, k.time, k.value.x, next.map(|n| (n.time, n.value.x))),
                interp_y: resolve_one(&k.curve, 1, k.time, k.value.y, next.map(|n| (n.time, n.value.y))),
            }
        })
        .collect()
}

fn resolve_color(raw: &[RawKey<Rgba>], channels: ColorChannels) -> Vec<ColorKey> {
    raw.iter()
        .enumerate()
        .map(|(i, k)| {
            let next = raw.get(i + 1);
            let comps = k.value.to_array();
            let mut interp = [Interpolation::Linear; 4];
            for (c, slot) in interp.iter_mut().enumerate() {
                // An `rgb` timeline has no alpha curve; alpha keeps linear.
                if c == 3 && !channels.has_alpha() {
                    continue;
                }
                *slot = resolve_one(&k.curve, c, k.time, comps[c], next.map(|n| (n.time, n.value.to_array()[c])));
            }
            ColorKey { time: k.time, value: k.value, interp }
        })
        .collect()
}

fn resolve_two_color(raw: &[RawKey<(Rgba, Rgb)>], channels: ColorChannels) -> Vec<TwoColorKey> {
    let light_components = channels.component_count();
    raw.iter()
        .enumerate()
        .map(|(i, k)| {
            let next = raw.get(i + 1);
            let light = k.value.0.to_array();
            let dark = k.value.1.to_array();
            let mut interp_light = [Interpolation::Linear; 4];
            for (c, slot) in interp_light.iter_mut().enumerate().take(light_components) {
                *slot = resolve_one(&k.curve, c, k.time, light[c], next.map(|n| (n.time, n.value.0.to_array()[c])));
            }
            let mut interp_dark = [Interpolation::Linear; 3];
            for (c, slot) in interp_dark.iter_mut().enumerate() {
                *slot = resolve_one(
                    &k.curve,
                    light_components + c,
                    k.time,
                    dark[c],
                    next.map(|n| (n.time, n.value.1.to_array()[c])),
                );
            }
            TwoColorKey { time: k.time, light: k.value.0, dark: k.value.1, interp_light, interp_dark }
        })
        .collect()
}

/// Resolves one component's curve, defaulting to linear for the final keyframe.
fn resolve_one(raw: &RawCurve, component: usize, t0: f32, v0: f32, next: Option<(f32, f32)>) -> Interpolation {
    match next {
        // The last keyframe has nothing to ease into. Stepped still matters,
        // because it holds the value for the rest of the animation.
        None => match raw {
            RawCurve::Stepped => Interpolation::Stepped,
            _ => Interpolation::Linear,
        },
        Some((t1, v1)) => resolve(raw, component, t0, v0, t1, v1),
    }
}

fn array_of(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// Sorts keyframes by time, stably.
///
/// Exports are normally already sorted, but a hand-edited or tool-merged file
/// may not be, and every timeline lookup binary searches.
fn sort_by_time<T>(keys: &mut [T], time: impl Fn(&T) -> f32) {
    keys.sort_by(|a, b| time(a).partial_cmp(&time(b)).unwrap_or(std::cmp::Ordering::Equal));
}

/// Re-exported for the binary decoder, which shares the draw-order algorithm.
pub(crate) fn draw_order_from_offsets(setup_len: usize, moves: &[(SlotId, i32)]) -> Option<Vec<SlotId>> {
    let n = setup_len;
    let mut result: Vec<Option<SlotId>> = vec![None; n];
    let mut moved = vec![false; n];
    for (slot, offset) in moves {
        let target = slot.index() as i64 + *offset as i64;
        if target < 0 || target as usize >= n {
            return None;
        }
        moved[slot.index()] = true;
        if result[target as usize].is_some() {
            return None;
        }
        result[target as usize] = Some(*slot);
    }
    let mut fill = (0..n).filter(|i| !moved[*i]).filter_map(SlotId::from_index);
    Some(result.into_iter().map(|s| s.or_else(|| fill.next()).unwrap_or(SlotId(0))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::spine::{Bone, EventData, Skin, Slot};
    use serde_json::json;

    fn base_ir() -> SpineIr {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None), Bone::new("torso", Some(BoneId(0)))],
            slots: vec![Slot::new("body", BoneId(1)), Slot::new("head", BoneId(1)), Slot::new("hat", BoneId(1))],
            skins: vec![Skin::new("default")],
            events: vec![EventData {
                name: "footstep".into(),
                int_value: 1,
                float_value: 2.0,
                string_value: "left".into(),
                audio_path: None,
                volume: 1.0,
                balance: 0.0,
            }],
            ..Default::default()
        };
        ir.rebuild_derived();
        ir
    }

    fn decode(json: serde_json::Value, family: SpineFamily) -> (Animation, LoadReport) {
        let mut ir = base_ir();
        let mut report = LoadReport::new();
        read_animations(&json, &mut ir, family, &mut report).unwrap();
        (ir.animations.into_iter().next().expect("one animation"), report)
    }

    #[test]
    fn v3_rotate_uses_the_angle_key() {
        let (anim, report) = decode(
            json!({"idle": {"bones": {"torso": {"rotate": [{"time": 0, "angle": 0}, {"time": 1, "angle": 45}]}}}}),
            SpineFamily::V3,
        );
        assert_eq!(anim.name, "idle");
        assert_eq!(anim.duration, 1.0);
        match &anim.timelines[0] {
            Timeline::BoneRotate { bone, keys } => {
                assert_eq!(*bone, BoneId(1));
                assert_eq!(keys.len(), 2);
                assert_eq!(keys[1].value, 45.0);
            }
            other => panic!("expected a rotate timeline, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn v4_rotate_uses_the_value_key() {
        let (anim, report) = decode(
            json!({"idle": {"bones": {"torso": {"rotate": [{"time": 0, "value": 0}, {"time": 1, "value": 45}]}}}}),
            SpineFamily::V4,
        );
        match &anim.timelines[0] {
            Timeline::BoneRotate { keys, .. } => assert_eq!(keys[1].value, 45.0),
            other => panic!("expected a rotate timeline, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn translate_reads_both_axes_and_defaults_them_to_zero() {
        let (anim, _) = decode(
            json!({"idle": {"bones": {"torso": {"translate": [{"time": 0, "x": 5}, {"time": 1, "y": 7}]}}}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::BoneTranslate { axes, keys, .. } => {
                assert_eq!(*axes, Axes::Both);
                assert_eq!(keys[0].value, Vec2::new(5.0, 0.0));
                assert_eq!(keys[1].value, Vec2::new(0.0, 7.0));
            }
            other => panic!("expected a translate timeline, got {other:?}"),
        }
    }

    #[test]
    fn scale_defaults_to_one_not_zero() {
        let (anim, _) =
            decode(json!({"idle": {"bones": {"torso": {"scale": [{"time": 0, "x": 2}]}}}}), SpineFamily::V3);
        match &anim.timelines[0] {
            Timeline::BoneScale { keys, .. } => assert_eq!(keys[0].value, Vec2::new(2.0, 1.0)),
            other => panic!("expected a scale timeline, got {other:?}"),
        }
    }

    #[test]
    fn v4_single_axis_timelines_carry_an_axis_mask() {
        let (anim, report) = decode(
            json!({"idle": {"bones": {"torso": {
                "translatex": [{"time": 0, "value": 3}],
                "scaley": [{"time": 0, "value": 2}]
            }}}}),
            SpineFamily::V4,
        );
        let mut seen = Vec::new();
        for t in &anim.timelines {
            match t {
                Timeline::BoneTranslate { axes, keys, .. } => {
                    assert_eq!(*axes, Axes::X);
                    assert_eq!(keys[0].value.x, 3.0);
                    seen.push("translatex");
                }
                Timeline::BoneScale { axes, keys, .. } => {
                    assert_eq!(*axes, Axes::Y);
                    assert_eq!(keys[0].value.y, 2.0);
                    // The unkeyed axis holds the channel's neutral value.
                    assert_eq!(keys[0].value.x, 1.0);
                    seen.push("scaley");
                }
                other => panic!("unexpected timeline {other:?}"),
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec!["scaley", "translatex"]);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_timeline_for_an_unknown_bone_is_reported_not_fatal() {
        let (anim, report) = decode(json!({"idle": {"bones": {"ghost": {"rotate": [{"time": 0}]}}}}), SpineFamily::V3);
        assert!(anim.timelines.is_empty());
        assert!(report.to_string().contains("ghost"), "{report}");
    }

    #[test]
    fn v3_color_and_v4_rgba_produce_the_same_timeline() {
        for (family, key) in [(SpineFamily::V3, "color"), (SpineFamily::V4, "rgba")] {
            let (anim, report) =
                decode(json!({"idle": {"slots": {"body": {key: [{"time": 0, "color": "ff0000ff"}]}}}}), family);
            match &anim.timelines[0] {
                Timeline::SlotColor { slot, channels, keys } => {
                    assert_eq!(*slot, SlotId(0));
                    assert_eq!(*channels, ColorChannels::Rgba);
                    assert_eq!(keys[0].value, Rgba::new(1.0, 0.0, 0.0, 1.0));
                }
                other => panic!("expected a colour timeline, got {other:?}"),
            }
            assert!(report.is_empty(), "{report}");
        }
    }

    #[test]
    fn an_rgb_timeline_is_marked_as_leaving_alpha_alone() {
        let (anim, _) =
            decode(json!({"idle": {"slots": {"body": {"rgb": [{"time": 0, "color": "00ff00"}]}}}}), SpineFamily::V4);
        match &anim.timelines[0] {
            Timeline::SlotColor { channels, .. } => assert_eq!(*channels, ColorChannels::Rgb),
            other => panic!("expected a colour timeline, got {other:?}"),
        }
    }

    #[test]
    fn an_alpha_timeline_is_read_on_its_own() {
        let (anim, _) =
            decode(json!({"idle": {"slots": {"body": {"alpha": [{"time": 0.5, "value": 0.25}]}}}}), SpineFamily::V4);
        match &anim.timelines[0] {
            Timeline::SlotAlpha { keys, .. } => {
                assert_eq!(keys[0].time, 0.5);
                assert_eq!(keys[0].value, 0.25);
            }
            other => panic!("expected an alpha timeline, got {other:?}"),
        }
    }

    #[test]
    fn two_colour_timelines_read_light_and_dark() {
        for (family, key) in [(SpineFamily::V3, "twoColor"), (SpineFamily::V4, "rgba2")] {
            let (anim, report) = decode(
                json!({"idle": {"slots": {"body": {key: [{"time": 0, "light": "ff0000ff", "dark": "000080"}]}}}}),
                family,
            );
            match &anim.timelines[0] {
                Timeline::SlotTwoColor { keys, .. } => {
                    assert_eq!(keys[0].light, Rgba::new(1.0, 0.0, 0.0, 1.0));
                    assert!((keys[0].dark.b - 128.0 / 255.0).abs() < 1e-5);
                }
                other => panic!("expected a two-colour timeline, got {other:?}"),
            }
            assert!(report.is_empty(), "{report}");
        }
    }

    #[test]
    fn attachment_timelines_distinguish_hiding_from_naming() {
        let (anim, _) = decode(
            json!({"idle": {"slots": {"body": {"attachment": [
                {"time": 0, "name": "shirt"}, {"time": 1, "name": null}
            ]}}}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::SlotAttachment { keys, .. } => {
                assert_eq!(keys[0].name.as_deref(), Some("shirt"));
                assert_eq!(keys[1].name, None);
            }
            other => panic!("expected an attachment timeline, got {other:?}"),
        }
    }

    #[test]
    fn v3_transform_mixes_expand_into_the_per_axis_form() {
        let mut ir = base_ir();
        ir.transform_constraints.push(a2d_core::ir::spine::TransformConstraint {
            name: "tc".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1)],
            target: BoneId(0),
            offset_rotation: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_scale_x: 0.0,
            offset_scale_y: 0.0,
            offset_shear_y: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
            mix_scale_x: 1.0,
            mix_scale_y: 1.0,
            mix_shear_y: 1.0,
            relative: false,
            local: false,
        });
        ir.rebuild_derived();
        let mut report = LoadReport::new();
        let json = json!({"idle": {"transform": {"tc": [
            {"time": 0, "rotateMix": 0.5, "translateMix": 0.25, "scaleMix": 0.75, "shearMix": 0.1}
        ]}}});
        read_animations(&json, &mut ir, SpineFamily::V3, &mut report).unwrap();
        match &ir.animations[0].timelines[0] {
            Timeline::TransformConstraint { keys, .. } => {
                let k = keys[0];
                assert_eq!(k.mix_rotate, 0.5);
                assert_eq!(k.mix_x, 0.25);
                assert_eq!(k.mix_y, 0.25);
                assert_eq!(k.mix_scale_x, 0.75);
                assert_eq!(k.mix_scale_y, 0.75);
                assert_eq!(k.mix_shear_y, 0.1);
            }
            other => panic!("expected a transform timeline, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn draw_order_offsets_expand_into_an_explicit_order() {
        // Move `hat` (index 2) back two places, to the front.
        let (anim, report) = decode(
            json!({"idle": {"drawOrder": [{"time": 0, "offsets": [{"slot": "hat", "offset": -2}]}]}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::DrawOrder { keys } => {
                let order = keys[0].order.as_ref().expect("offsets produce an explicit order");
                assert_eq!(order, &vec![SlotId(2), SlotId(0), SlotId(1)]);
            }
            other => panic!("expected a draw order timeline, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_draw_order_key_without_offsets_restores_the_setup_order() {
        let (anim, _) = decode(json!({"idle": {"drawOrder": [{"time": 0.5}]}}), SpineFamily::V3);
        match &anim.timelines[0] {
            Timeline::DrawOrder { keys } => assert!(keys[0].order.is_none()),
            other => panic!("expected a draw order timeline, got {other:?}"),
        }
    }

    #[test]
    fn an_out_of_range_draw_order_offset_is_reported() {
        let (_, report) = decode(
            json!({"idle": {"drawOrder": [{"time": 0, "offsets": [{"slot": "body", "offset": -5}]}]}}),
            SpineFamily::V3,
        );
        assert!(report.to_string().contains("outside"), "{report}");
    }

    #[test]
    fn events_inherit_their_declared_defaults() {
        let (anim, report) = decode(json!({"idle": {"events": [{"time": 0.5, "name": "footstep"}]}}), SpineFamily::V3);
        match &anim.timelines[0] {
            Timeline::Event { keys } => {
                assert_eq!(keys[0].event, EventId(0));
                assert_eq!(keys[0].int_value, 1);
                assert_eq!(keys[0].float_value, 2.0);
                assert_eq!(keys[0].string_value, None);
            }
            other => panic!("expected an event timeline, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn events_can_override_their_defaults() {
        let (anim, _) = decode(
            json!({"idle": {"events": [{"time": 0, "name": "footstep", "int": 9, "string": "right"}]}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::Event { keys } => {
                assert_eq!(keys[0].int_value, 9);
                assert_eq!(keys[0].string_value.as_deref(), Some("right"));
            }
            other => panic!("expected an event timeline, got {other:?}"),
        }
    }

    #[test]
    fn an_undeclared_event_is_reported_not_fatal() {
        let (anim, report) = decode(json!({"idle": {"events": [{"time": 0, "name": "explode"}]}}), SpineFamily::V3);
        assert!(anim.timelines.is_empty());
        assert!(report.to_string().contains("explode"), "{report}");
    }

    #[test]
    fn keyframes_are_sorted_by_time() {
        let (anim, _) = decode(
            json!({"idle": {"bones": {"torso": {"rotate": [
                {"time": 2, "angle": 20}, {"time": 0, "angle": 0}, {"time": 1, "angle": 10}
            ]}}}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::BoneRotate { keys, .. } => {
                let times: Vec<f32> = keys.iter().map(|k| k.time).collect();
                assert_eq!(times, vec![0.0, 1.0, 2.0]);
            }
            other => panic!("expected a rotate timeline, got {other:?}"),
        }
    }

    #[test]
    fn the_last_keyframe_gets_a_linear_curve_but_keeps_stepped() {
        let (anim, _) = decode(
            json!({"idle": {"bones": {"torso": {"rotate": [
                {"time": 0, "angle": 0, "curve": "stepped"},
                {"time": 1, "angle": 10, "curve": "stepped"}
            ]}}}}),
            SpineFamily::V3,
        );
        match &anim.timelines[0] {
            Timeline::BoneRotate { keys, .. } => {
                assert_eq!(keys[0].interp, Interpolation::Stepped);
                assert_eq!(keys[1].interp, Interpolation::Stepped);
            }
            other => panic!("expected a rotate timeline, got {other:?}"),
        }
    }

    #[test]
    fn empty_timelines_are_dropped() {
        let (anim, _) = decode(json!({"idle": {"bones": {"torso": {"rotate": []}}}}), SpineFamily::V3);
        assert!(anim.timelines.is_empty());
        assert_eq!(anim.duration, 0.0);
    }

    #[test]
    fn unknown_animation_keys_are_reported() {
        let (_, report) = decode(json!({"idle": {"mysteryChannel": {}}}), SpineFamily::V4);
        assert!(report.to_string().contains("mysteryChannel"), "{report}");
    }

    #[test]
    fn draw_order_helper_rejects_conflicting_placements() {
        assert!(draw_order_from_offsets(3, &[(SlotId(0), 1), (SlotId(2), -1)]).is_none());
        assert!(draw_order_from_offsets(3, &[(SlotId(0), 9)]).is_none());
        assert_eq!(draw_order_from_offsets(3, &[(SlotId(2), -2)]), Some(vec![SlotId(2), SlotId(0), SlotId(1)]));
    }
}
