//! Timeline sampling and application.
//!
//! Two conventions inherited from the source format matter here, and getting
//! either wrong looks like a subtle rigging bug rather than an obvious one:
//!
//! * Bone **rotate**, **translate** and **shear** timelines store *offsets from
//!   the setup pose*; **scale** stores *multipliers* of the setup scale.
//! * Slot **colour** timelines store *absolute* colours.
//!
//! Deform keyframes are offsets in the IR for both rigid and weighted meshes,
//! which makes blending them a plain multiply by alpha in either case.

use a2d_core::ir::spine::{
    search_keys, Animation, Axes, ColorKey, EventKey, IkKey, PathMixKey, ScalarKey, Timeline, TransformKey,
    TwoColorKey, Vec2Key,
};
use a2d_core::math::lerp;
use a2d_core::{Rgb, Rgba, Vec2};

use crate::spine::pose::SkeletonPose;

/// How a timeline combines with what is already in the pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixBlend {
    /// Blend from the setup pose. Used by the lowest track.
    Setup,
    /// Blend from whatever the pose currently holds. Used by higher tracks and
    /// by the incoming animation of a crossfade.
    Replace,
    /// Add on top of the current pose.
    Add,
}

/// An event fired during one `apply` call.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredEvent {
    pub name: String,
    pub time: f32,
    pub int_value: i32,
    pub float_value: f32,
    pub string_value: Option<String>,
}

/// Applies every timeline of `animation` at `time`.
///
/// `last_time` is the previous evaluation time on the same track; it bounds the
/// window that event timelines fire in. Pass a negative value to fire nothing.
#[allow(clippy::too_many_arguments)]
pub fn apply_animation(
    pose: &mut SkeletonPose,
    animation: &Animation,
    last_time: f32,
    time: f32,
    alpha: f32,
    blend: MixBlend,
    events: &mut Vec<FiredEvent>,
) {
    for timeline in &animation.timelines {
        apply_timeline(pose, timeline, last_time, time, alpha, blend, events);
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_timeline(
    pose: &mut SkeletonPose,
    timeline: &Timeline,
    last_time: f32,
    time: f32,
    alpha: f32,
    blend: MixBlend,
    events: &mut Vec<FiredEvent>,
) {
    match timeline {
        Timeline::BoneRotate { bone, keys } => {
            let i = bone.index();
            let Some(setup) = pose.ir().bones.get(i).map(|b| b.setup.rotation) else { return };
            let Some(value) = sample_scalar(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(b) = pose.bones.get_mut(i) {
                        b.local.rotation = setup;
                    }
                }
                return;
            };
            if let Some(b) = pose.bones.get_mut(i) {
                b.local.rotation = blend_offset(blend, b.local.rotation, setup, value, alpha);
            }
        }

        Timeline::BoneTranslate { bone, axes, keys } => {
            apply_bone_vec2(pose, bone.index(), *axes, keys, time, alpha, blend, Channel::Translate)
        }
        Timeline::BoneScale { bone, axes, keys } => {
            apply_bone_vec2(pose, bone.index(), *axes, keys, time, alpha, blend, Channel::Scale)
        }
        Timeline::BoneShear { bone, axes, keys } => {
            apply_bone_vec2(pose, bone.index(), *axes, keys, time, alpha, blend, Channel::Shear)
        }

        Timeline::SlotColor { slot, channels, keys } => {
            let i = slot.index();
            let Some(setup) = pose.ir().slots.get(i).map(|s| s.color) else { return };
            let Some(value) = sample_color(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(s) = pose.slots.get_mut(i) {
                        s.color = setup;
                    }
                }
                return;
            };
            let Some(s) = pose.slots.get_mut(i) else { return };
            let from = if blend == MixBlend::Setup { setup } else { s.color };
            let mut next = from.lerp(value, alpha);
            if !channels.has_alpha() {
                // An `rgb` timeline must not disturb alpha.
                next.a = s.color.a;
            }
            s.color = next;
        }

        Timeline::SlotAlpha { slot, keys } => {
            let i = slot.index();
            let Some(setup) = pose.ir().slots.get(i).map(|s| s.color.a) else { return };
            let Some(value) = sample_scalar(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(s) = pose.slots.get_mut(i) {
                        s.color.a = setup;
                    }
                }
                return;
            };
            if let Some(s) = pose.slots.get_mut(i) {
                let from = if blend == MixBlend::Setup { setup } else { s.color.a };
                s.color.a = lerp(from, value, alpha);
            }
        }

        Timeline::SlotTwoColor { slot, channels, keys } => {
            let i = slot.index();
            let Some(setup_slot) = pose.ir().slots.get(i) else { return };
            let (setup_light, setup_dark) = (setup_slot.color, setup_slot.dark_color.unwrap_or(Rgb::BLACK));
            let Some((light, dark)) = sample_two_color(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(s) = pose.slots.get_mut(i) {
                        s.color = setup_light;
                        s.dark_color = Some(setup_dark);
                    }
                }
                return;
            };
            let Some(s) = pose.slots.get_mut(i) else { return };
            let from_light = if blend == MixBlend::Setup { setup_light } else { s.color };
            let from_dark = if blend == MixBlend::Setup { setup_dark } else { s.dark_color.unwrap_or(Rgb::BLACK) };
            let mut next_light = from_light.lerp(light, alpha);
            if !channels.has_alpha() {
                next_light.a = s.color.a;
            }
            s.color = next_light;
            s.dark_color = Some(from_dark.lerp(dark, alpha));
        }

        Timeline::SlotAttachment { slot, keys } => {
            // Attachment switches are discrete: they either happen or they do
            // not, so a partial mix must not apply them.
            if alpha < 1.0 && blend != MixBlend::Setup {
                return;
            }
            match search_keys(keys, time, |k| k.time) {
                None => {
                    if blend == MixBlend::Setup {
                        let setup = pose.ir().slots.get(slot.index()).and_then(|s| s.setup_attachment.clone());
                        pose.set_slot_attachment(*slot, setup.as_deref());
                    }
                }
                Some(i) => {
                    let name = keys[i].name.clone();
                    pose.set_slot_attachment(*slot, name.as_deref());
                }
            }
        }

        Timeline::Deform { slot, attachment, keys, .. } => {
            let i = slot.index();
            // A deform timeline only applies while its own attachment is shown.
            if pose.slots.get(i).map(|s| s.attachment) != Some(Some(*attachment)) {
                return;
            }
            let Some(len) =
                pose.ir().attachment(*attachment).and_then(|a| a.kind.deformable_vertices()).map(|v| v.deform_len())
            else {
                return;
            };
            let Some(key) = search_keys(keys, time, |k| k.time) else {
                if blend == MixBlend::Setup {
                    if let Some(s) = pose.slots.get_mut(i) {
                        s.deform.clear();
                    }
                }
                return;
            };

            let Some(s) = pose.slots.get_mut(i) else { return };
            if s.deform.len() != len {
                s.deform.clear();
                s.deform.resize(len, 0.0);
            }
            let next = keys.get(key + 1);
            let blend_t = next.map(|n| {
                let span = n.time - keys[key].time;
                let raw = if span.abs() <= f32::EPSILON { 0.0 } else { (time - keys[key].time) / span };
                keys[key].interp.apply(raw.clamp(0.0, 1.0))
            });

            for (v, dst) in s.deform.iter_mut().enumerate().take(len) {
                let a = keys[key].value_at(v);
                let target = match (next, blend_t) {
                    (Some(n), Some(t)) => lerp(a, n.value_at(v), t),
                    _ => a,
                };
                let from = if blend == MixBlend::Setup { 0.0 } else { *dst };
                *dst = lerp(from, target, alpha);
            }
        }

        Timeline::DrawOrder { keys } => {
            if alpha < 1.0 && blend != MixBlend::Setup {
                return;
            }
            match search_keys(keys, time, |k| k.time) {
                None => {
                    if blend == MixBlend::Setup {
                        pose.reset_draw_order();
                    }
                }
                Some(i) => match &keys[i].order {
                    None => pose.reset_draw_order(),
                    Some(order) => {
                        pose.draw_order.clear();
                        pose.draw_order.extend_from_slice(order);
                    }
                },
            }
        }

        Timeline::Event { keys } => collect_events(pose, keys, last_time, time, events),

        Timeline::IkConstraint { constraint, keys } => {
            let i = constraint.index();
            let Some(setup) = pose.ir().ik_constraints.get(i).cloned() else { return };
            let Some(key) = search_keys(keys, time, |k| k.time) else {
                if blend == MixBlend::Setup {
                    if let Some(p) = pose.ik.get_mut(i) {
                        p.mix = setup.mix;
                        p.softness = setup.softness;
                        p.bend_positive = setup.bend_positive;
                        p.compress = setup.compress;
                        p.stretch = setup.stretch;
                    }
                }
                return;
            };
            let k = interpolate_ik(keys, key, time);
            let Some(p) = pose.ik.get_mut(i) else { return };
            let (from_mix, from_soft) =
                if blend == MixBlend::Setup { (setup.mix, setup.softness) } else { (p.mix, p.softness) };
            p.mix = lerp(from_mix, k.mix, alpha);
            p.softness = lerp(from_soft, k.softness, alpha);
            // Flags are discrete; they follow the keyframe outright.
            p.bend_positive = k.bend_positive;
            p.compress = k.compress;
            p.stretch = k.stretch;
        }

        Timeline::TransformConstraint { constraint, keys } => {
            let i = constraint.index();
            let Some(setup) = pose.ir().transform_constraints.get(i).cloned() else { return };
            let Some(key) = search_keys(keys, time, |k| k.time) else {
                if blend == MixBlend::Setup {
                    if let Some(p) = pose.transform.get_mut(i) {
                        p.mix_rotate = setup.mix_rotate;
                        p.mix_x = setup.mix_x;
                        p.mix_y = setup.mix_y;
                        p.mix_scale_x = setup.mix_scale_x;
                        p.mix_scale_y = setup.mix_scale_y;
                        p.mix_shear_y = setup.mix_shear_y;
                    }
                }
                return;
            };
            let k = interpolate_transform(keys, key, time);
            let Some(p) = pose.transform.get_mut(i) else { return };
            let from = if blend == MixBlend::Setup {
                [setup.mix_rotate, setup.mix_x, setup.mix_y, setup.mix_scale_x, setup.mix_scale_y, setup.mix_shear_y]
            } else {
                [p.mix_rotate, p.mix_x, p.mix_y, p.mix_scale_x, p.mix_scale_y, p.mix_shear_y]
            };
            let to = [k.mix_rotate, k.mix_x, k.mix_y, k.mix_scale_x, k.mix_scale_y, k.mix_shear_y];
            p.mix_rotate = lerp(from[0], to[0], alpha);
            p.mix_x = lerp(from[1], to[1], alpha);
            p.mix_y = lerp(from[2], to[2], alpha);
            p.mix_scale_x = lerp(from[3], to[3], alpha);
            p.mix_scale_y = lerp(from[4], to[4], alpha);
            p.mix_shear_y = lerp(from[5], to[5], alpha);
        }

        Timeline::PathPosition { constraint, keys } => {
            let i = constraint.index();
            let Some(setup) = pose.ir().path_constraints.get(i).map(|c| c.position) else { return };
            let Some(value) = sample_scalar(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(p) = pose.path.get_mut(i) {
                        p.position = setup;
                    }
                }
                return;
            };
            let Some(p) = pose.path.get_mut(i) else { return };
            p.position = match blend {
                MixBlend::Add => p.position + (value - setup) * alpha,
                MixBlend::Setup => lerp(setup, value, alpha),
                MixBlend::Replace => lerp(p.position, value, alpha),
            };
        }

        Timeline::PathSpacing { constraint, keys } => {
            let i = constraint.index();
            let Some(setup) = pose.ir().path_constraints.get(i).map(|c| c.spacing) else { return };
            let Some(value) = sample_scalar(keys, time) else {
                if blend == MixBlend::Setup {
                    if let Some(p) = pose.path.get_mut(i) {
                        p.spacing = setup;
                    }
                }
                return;
            };
            let Some(p) = pose.path.get_mut(i) else { return };
            p.spacing = match blend {
                MixBlend::Add => p.spacing + (value - setup) * alpha,
                MixBlend::Setup => lerp(setup, value, alpha),
                MixBlend::Replace => lerp(p.spacing, value, alpha),
            };
        }

        Timeline::PathMix { constraint, keys } => {
            let i = constraint.index();
            let Some(setup) = pose.ir().path_constraints.get(i).cloned() else { return };
            let Some(key) = search_keys(keys, time, |k| k.time) else {
                if blend == MixBlend::Setup {
                    if let Some(p) = pose.path.get_mut(i) {
                        p.mix_rotate = setup.mix_rotate;
                        p.mix_x = setup.mix_x;
                        p.mix_y = setup.mix_y;
                    }
                }
                return;
            };
            let k = interpolate_path_mix(keys, key, time);
            let Some(p) = pose.path.get_mut(i) else { return };
            let from = if blend == MixBlend::Setup {
                [setup.mix_rotate, setup.mix_x, setup.mix_y]
            } else {
                [p.mix_rotate, p.mix_x, p.mix_y]
            };
            p.mix_rotate = lerp(from[0], k.mix_rotate, alpha);
            p.mix_x = lerp(from[1], k.mix_x, alpha);
            p.mix_y = lerp(from[2], k.mix_y, alpha);
        }
    }
}

/// Interpolates a path mix keyframe, all three channels sharing one curve.
fn interpolate_path_mix(keys: &[PathMixKey], i: usize, time: f32) -> PathMixKey {
    let a = keys[i];
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return a };
    let b = keys[i + 1];
    let t = a.interp.apply(t);
    PathMixKey {
        mix_rotate: lerp(a.mix_rotate, b.mix_rotate, t),
        mix_x: lerp(a.mix_x, b.mix_x, t),
        mix_y: lerp(a.mix_y, b.mix_y, t),
        ..a
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Channel {
    Translate,
    Scale,
    Shear,
}

#[allow(clippy::too_many_arguments)]
fn apply_bone_vec2(
    pose: &mut SkeletonPose,
    index: usize,
    axes: Axes,
    keys: &[Vec2Key],
    time: f32,
    alpha: f32,
    blend: MixBlend,
    channel: Channel,
) {
    let Some(setup_bone) = pose.ir().bones.get(index).map(|b| b.setup) else { return };
    let setup = match channel {
        Channel::Translate => setup_bone.position,
        Channel::Scale => setup_bone.scale,
        Channel::Shear => setup_bone.shear,
    };

    let Some(value) = sample_vec2(keys, time) else {
        if blend == MixBlend::Setup {
            if let Some(b) = pose.bones.get_mut(index) {
                write_channel(&mut b.local, channel, setup, axes, setup);
            }
        }
        return;
    };

    let Some(b) = pose.bones.get_mut(index) else { return };
    let current = match channel {
        Channel::Translate => b.local.position,
        Channel::Scale => b.local.scale,
        Channel::Shear => b.local.shear,
    };

    let next = match channel {
        // Scale timelines are multiplicative against the setup scale.
        Channel::Scale => Vec2::new(
            blend_scale(blend, current.x, setup.x, value.x, alpha),
            blend_scale(blend, current.y, setup.y, value.y, alpha),
        ),
        _ => Vec2::new(
            blend_offset(blend, current.x, setup.x, value.x, alpha),
            blend_offset(blend, current.y, setup.y, value.y, alpha),
        ),
    };
    write_channel(&mut b.local, channel, next, axes, current);
}

/// Writes only the axes the timeline actually keys.
fn write_channel(local: &mut a2d_core::ir::spine::BoneLocal, channel: Channel, value: Vec2, axes: Axes, keep: Vec2) {
    let merged = Vec2::new(if axes.has_x() { value.x } else { keep.x }, if axes.has_y() { value.y } else { keep.y });
    match channel {
        Channel::Translate => local.position = merged,
        Channel::Scale => local.scale = merged,
        Channel::Shear => local.shear = merged,
    }
}

/// Additive channel: the keyframe value is an offset from the setup value.
fn blend_offset(blend: MixBlend, current: f32, setup: f32, value: f32, alpha: f32) -> f32 {
    match blend {
        MixBlend::Setup => setup + value * alpha,
        MixBlend::Replace => current + (setup + value - current) * alpha,
        MixBlend::Add => current + value * alpha,
    }
}

/// Multiplicative channel: the keyframe value scales the setup value.
fn blend_scale(blend: MixBlend, current: f32, setup: f32, value: f32, alpha: f32) -> f32 {
    let target = setup * value;
    match blend {
        MixBlend::Setup => setup + (target - setup) * alpha,
        MixBlend::Replace => current + (target - current) * alpha,
        MixBlend::Add => current + (target - setup) * alpha,
    }
}

fn collect_events(pose: &SkeletonPose, keys: &[EventKey], last_time: f32, time: f32, out: &mut Vec<FiredEvent>) {
    if last_time < 0.0 || keys.is_empty() {
        return;
    }
    for key in keys {
        // Half-open window, so a key exactly on a frame boundary fires once.
        if key.time > last_time && key.time <= time {
            let Some(data) = pose.ir().events.get(key.event.index()) else { continue };
            out.push(FiredEvent {
                name: data.name.clone(),
                time: key.time,
                int_value: key.int_value,
                float_value: key.float_value,
                string_value: key.string_value.clone().or_else(|| {
                    if data.string_value.is_empty() {
                        None
                    } else {
                        Some(data.string_value.clone())
                    }
                }),
            });
        }
    }
}

// ---------------------------------------------------------------- samplers

/// Normalised progress between keyframe `i` and the next one.
fn span_t<K>(keys: &[K], i: usize, time: f32, key_time: impl Fn(&K) -> f32) -> Option<f32> {
    let next = keys.get(i + 1)?;
    let start = key_time(&keys[i]);
    let span = key_time(next) - start;
    if span.abs() <= f32::EPSILON {
        return Some(0.0);
    }
    Some(((time - start) / span).clamp(0.0, 1.0))
}

/// Samples a scalar timeline. `None` means the time precedes the first key.
pub fn sample_scalar(keys: &[ScalarKey], time: f32) -> Option<f32> {
    let i = search_keys(keys, time, |k| k.time)?;
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return Some(keys[i].value) };
    Some(lerp(keys[i].value, keys[i + 1].value, keys[i].interp.apply(t)))
}

/// Samples a two-component timeline, each component using its own curve.
pub fn sample_vec2(keys: &[Vec2Key], time: f32) -> Option<Vec2> {
    let i = search_keys(keys, time, |k| k.time)?;
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return Some(keys[i].value) };
    let a = &keys[i];
    let b = &keys[i + 1];
    Some(Vec2::new(lerp(a.value.x, b.value.x, a.interp_x.apply(t)), lerp(a.value.y, b.value.y, a.interp_y.apply(t))))
}

/// Samples a colour timeline, each channel using its own curve.
pub fn sample_color(keys: &[ColorKey], time: f32) -> Option<Rgba> {
    let i = search_keys(keys, time, |k| k.time)?;
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return Some(keys[i].value) };
    let a = &keys[i];
    let b = &keys[i + 1];
    Some(Rgba::new(
        lerp(a.value.r, b.value.r, a.interp[0].apply(t)),
        lerp(a.value.g, b.value.g, a.interp[1].apply(t)),
        lerp(a.value.b, b.value.b, a.interp[2].apply(t)),
        lerp(a.value.a, b.value.a, a.interp[3].apply(t)),
    ))
}

/// Samples a two-colour timeline.
pub fn sample_two_color(keys: &[TwoColorKey], time: f32) -> Option<(Rgba, Rgb)> {
    let i = search_keys(keys, time, |k| k.time)?;
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return Some((keys[i].light, keys[i].dark)) };
    let a = &keys[i];
    let b = &keys[i + 1];
    Some((
        Rgba::new(
            lerp(a.light.r, b.light.r, a.interp_light[0].apply(t)),
            lerp(a.light.g, b.light.g, a.interp_light[1].apply(t)),
            lerp(a.light.b, b.light.b, a.interp_light[2].apply(t)),
            lerp(a.light.a, b.light.a, a.interp_light[3].apply(t)),
        ),
        Rgb::new(
            lerp(a.dark.r, b.dark.r, a.interp_dark[0].apply(t)),
            lerp(a.dark.g, b.dark.g, a.interp_dark[1].apply(t)),
            lerp(a.dark.b, b.dark.b, a.interp_dark[2].apply(t)),
        ),
    ))
}

fn interpolate_ik(keys: &[IkKey], i: usize, time: f32) -> IkKey {
    let a = keys[i];
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return a };
    let b = keys[i + 1];
    let t = a.interp.apply(t);
    IkKey { mix: lerp(a.mix, b.mix, t), softness: lerp(a.softness, b.softness, t), ..a }
}

fn interpolate_transform(keys: &[TransformKey], i: usize, time: f32) -> TransformKey {
    let a = keys[i];
    let Some(t) = span_t(keys, i, time, |k| k.time) else { return a };
    let b = keys[i + 1];
    let t = a.interp.apply(t);
    TransformKey {
        mix_rotate: lerp(a.mix_rotate, b.mix_rotate, t),
        mix_x: lerp(a.mix_x, b.mix_x, t),
        mix_y: lerp(a.mix_y, b.mix_y, t),
        mix_scale_x: lerp(a.mix_scale_x, b.mix_scale_x, t),
        mix_scale_y: lerp(a.mix_scale_y, b.mix_scale_y, t),
        mix_shear_y: lerp(a.mix_shear_y, b.mix_shear_y, t),
        ..a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::ids::{AttachmentId, BoneId, EventId, SlotId};
    use a2d_core::ir::spine::{
        Attachment, AttachmentKey, AttachmentKind, Bone, BoneLocal, ColorChannels, DeformKey, DrawOrderKey, EventData,
        MeshAttachment, PointAttachment, Skin, SkinEntry, Slot, SpineIr, VertexData,
    };
    use a2d_core::Interpolation;
    use std::sync::Arc;

    fn key(time: f32, value: f32) -> ScalarKey {
        ScalarKey { time, value, interp: Interpolation::Linear }
    }

    fn test_pose() -> SkeletonPose {
        let mut ir = SpineIr {
            bones: vec![
                Bone::new("root", None),
                Bone {
                    setup: BoneLocal {
                        position: Vec2::new(10.0, 20.0),
                        rotation: 30.0,
                        scale: Vec2::new(2.0, 2.0),
                        shear: Vec2::ZERO,
                    },
                    ..Bone::new("torso", Some(BoneId(0)))
                },
            ],
            slots: vec![
                Slot {
                    color: Rgba::new(0.5, 0.5, 0.5, 1.0),
                    setup_attachment: Some("a".into()),
                    ..Slot::new("body", BoneId(1))
                },
                Slot::new("head", BoneId(1)),
            ],
            skins: vec![Skin::new("default")],
            attachments: vec![
                Attachment {
                    name: "a".into(),
                    kind: AttachmentKind::Point(PointAttachment {
                        position: Vec2::ZERO,
                        rotation: 0.0,
                        color: Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "b".into(),
                    kind: AttachmentKind::Mesh(MeshAttachment {
                        path: "b".into(),
                        region: None,
                        uvs: vec![Vec2::ZERO; 2],
                        triangles: vec![],
                        vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::ONE]),
                        hull_length: 0,
                        edges: vec![],
                        size: Vec2::ZERO,
                        color: Rgba::WHITE,
                        linked_to: None,
                        sequence: None,
                    }),
                },
            ],
            events: vec![EventData {
                name: "step".into(),
                int_value: 0,
                float_value: 0.0,
                string_value: String::new(),
                audio_path: None,
                volume: 1.0,
                balance: 0.0,
            }],
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "a".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "b".into(), attachment: AttachmentId(1) });
        ir.rebuild_derived();
        SkeletonPose::new(Arc::new(ir))
    }

    fn apply(pose: &mut SkeletonPose, timeline: Timeline, time: f32, alpha: f32, blend: MixBlend) {
        let mut events = Vec::new();
        apply_timeline(pose, &timeline, -1.0, time, alpha, blend, &mut events);
    }

    #[test]
    fn scalar_sampling_interpolates_between_keys() {
        let keys = vec![key(0.0, 0.0), key(1.0, 10.0)];
        assert_eq!(sample_scalar(&keys, 0.0), Some(0.0));
        assert_eq!(sample_scalar(&keys, 0.5), Some(5.0));
        assert_eq!(sample_scalar(&keys, 1.0), Some(10.0));
    }

    #[test]
    fn scalar_sampling_holds_the_last_value_past_the_end() {
        let keys = vec![key(0.0, 0.0), key(1.0, 10.0)];
        assert_eq!(sample_scalar(&keys, 99.0), Some(10.0));
    }

    #[test]
    fn scalar_sampling_before_the_first_key_has_no_opinion() {
        let keys = vec![key(1.0, 10.0)];
        assert_eq!(sample_scalar(&keys, 0.5), None);
    }

    #[test]
    fn a_stepped_key_holds_its_value_across_the_whole_span() {
        let keys = vec![ScalarKey { time: 0.0, value: 0.0, interp: Interpolation::Stepped }, key(1.0, 10.0)];
        assert_eq!(sample_scalar(&keys, 0.99), Some(0.0));
        assert_eq!(sample_scalar(&keys, 1.0), Some(10.0));
    }

    #[test]
    fn duplicate_key_times_do_not_divide_by_zero() {
        let keys = vec![key(1.0, 5.0), key(1.0, 9.0)];
        assert_eq!(sample_scalar(&keys, 1.0), Some(9.0));
    }

    #[test]
    fn rotate_timeline_values_are_offsets_from_the_setup_pose() {
        let mut pose = test_pose();
        apply(
            &mut pose,
            Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(0.0, 45.0)] },
            0.0,
            1.0,
            MixBlend::Setup,
        );
        // Setup rotation is 30, the timeline adds 45.
        assert_eq!(pose.bones[1].local.rotation, 75.0);
    }

    #[test]
    fn translate_timeline_values_are_offsets_from_the_setup_pose() {
        let mut pose = test_pose();
        let keys = vec![Vec2Key::shared(0.0, Vec2::new(5.0, -5.0), Interpolation::Linear)];
        apply(
            &mut pose,
            Timeline::BoneTranslate { bone: BoneId(1), axes: Axes::Both, keys },
            0.0,
            1.0,
            MixBlend::Setup,
        );
        assert_eq!(pose.bones[1].local.position, Vec2::new(15.0, 15.0));
    }

    #[test]
    fn scale_timeline_values_multiply_the_setup_scale() {
        let mut pose = test_pose();
        let keys = vec![Vec2Key::shared(0.0, Vec2::new(3.0, 0.5), Interpolation::Linear)];
        apply(&mut pose, Timeline::BoneScale { bone: BoneId(1), axes: Axes::Both, keys }, 0.0, 1.0, MixBlend::Setup);
        // Setup scale is 2, so 2*3 and 2*0.5.
        assert_eq!(pose.bones[1].local.scale, Vec2::new(6.0, 1.0));
    }

    #[test]
    fn half_alpha_blends_half_way_from_setup() {
        let mut pose = test_pose();
        apply(
            &mut pose,
            Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(0.0, 40.0)] },
            0.0,
            0.5,
            MixBlend::Setup,
        );
        assert_eq!(pose.bones[1].local.rotation, 50.0);
    }

    #[test]
    fn setup_blend_before_the_first_key_restores_the_setup_value() {
        let mut pose = test_pose();
        pose.bones[1].local.rotation = 999.0;
        apply(
            &mut pose,
            Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(5.0, 40.0)] },
            0.0,
            1.0,
            MixBlend::Setup,
        );
        assert_eq!(pose.bones[1].local.rotation, 30.0);
    }

    #[test]
    fn replace_blend_before_the_first_key_leaves_the_pose_alone() {
        let mut pose = test_pose();
        pose.bones[1].local.rotation = 999.0;
        apply(
            &mut pose,
            Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(5.0, 40.0)] },
            0.0,
            1.0,
            MixBlend::Replace,
        );
        assert_eq!(pose.bones[1].local.rotation, 999.0);
    }

    #[test]
    fn a_single_axis_timeline_leaves_the_other_axis_untouched() {
        let mut pose = test_pose();
        let keys = vec![Vec2Key::shared(0.0, Vec2::new(7.0, 0.0), Interpolation::Linear)];
        apply(&mut pose, Timeline::BoneTranslate { bone: BoneId(1), axes: Axes::X, keys }, 0.0, 1.0, MixBlend::Setup);
        assert_eq!(pose.bones[1].local.position.x, 17.0);
        assert_eq!(pose.bones[1].local.position.y, 20.0, "the unkeyed axis keeps the setup value");
    }

    #[test]
    fn colour_timeline_values_are_absolute() {
        let mut pose = test_pose();
        let keys = vec![ColorKey::shared(0.0, Rgba::new(1.0, 0.0, 0.0, 1.0), Interpolation::Linear)];
        apply(
            &mut pose,
            Timeline::SlotColor { slot: SlotId(0), channels: ColorChannels::Rgba, keys },
            0.0,
            1.0,
            MixBlend::Setup,
        );
        assert_eq!(pose.slots[0].color, Rgba::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn an_rgb_timeline_leaves_alpha_alone() {
        let mut pose = test_pose();
        pose.slots[0].color.a = 0.25;
        let keys = vec![ColorKey::shared(0.0, Rgba::new(1.0, 0.0, 0.0, 1.0), Interpolation::Linear)];
        apply(
            &mut pose,
            Timeline::SlotColor { slot: SlotId(0), channels: ColorChannels::Rgb, keys },
            0.0,
            1.0,
            MixBlend::Setup,
        );
        assert_eq!(pose.slots[0].color.r, 1.0);
        assert_eq!(pose.slots[0].color.a, 0.25);
    }

    #[test]
    fn an_alpha_timeline_only_touches_alpha() {
        let mut pose = test_pose();
        let keys = vec![key(0.0, 0.25)];
        apply(&mut pose, Timeline::SlotAlpha { slot: SlotId(0), keys }, 0.0, 1.0, MixBlend::Setup);
        assert_eq!(pose.slots[0].color.a, 0.25);
        assert_eq!(pose.slots[0].color.r, 0.5);
    }

    #[test]
    fn attachment_timelines_switch_and_hide() {
        let mut pose = test_pose();
        let keys = vec![AttachmentKey { time: 0.0, name: Some("b".into()) }, AttachmentKey { time: 1.0, name: None }];
        apply(&mut pose, Timeline::SlotAttachment { slot: SlotId(0), keys: keys.clone() }, 0.5, 1.0, MixBlend::Setup);
        assert_eq!(pose.slots[0].attachment, Some(AttachmentId(1)));
        apply(&mut pose, Timeline::SlotAttachment { slot: SlotId(0), keys }, 1.5, 1.0, MixBlend::Setup);
        assert_eq!(pose.slots[0].attachment, None);
    }

    #[test]
    fn a_partial_mix_does_not_switch_attachments() {
        let mut pose = test_pose();
        let keys = vec![AttachmentKey { time: 0.0, name: Some("b".into()) }];
        apply(&mut pose, Timeline::SlotAttachment { slot: SlotId(0), keys }, 0.0, 0.5, MixBlend::Replace);
        assert_eq!(pose.slots[0].attachment, Some(AttachmentId(0)), "should still show the setup attachment");
    }

    #[test]
    fn draw_order_timelines_reorder_and_restore() {
        let mut pose = test_pose();
        let keys = vec![
            DrawOrderKey { time: 0.0, order: Some(vec![SlotId(1), SlotId(0)]) },
            DrawOrderKey { time: 1.0, order: None },
        ];
        apply(&mut pose, Timeline::DrawOrder { keys: keys.clone() }, 0.5, 1.0, MixBlend::Setup);
        assert_eq!(pose.draw_order, vec![SlotId(1), SlotId(0)]);
        apply(&mut pose, Timeline::DrawOrder { keys }, 1.5, 1.0, MixBlend::Setup);
        assert_eq!(pose.draw_order, vec![SlotId(0), SlotId(1)]);
    }

    #[test]
    fn deform_applies_only_while_its_attachment_is_shown() {
        let mut pose = test_pose();
        let keys =
            vec![DeformKey { time: 0.0, offset: 0, values: vec![1.0, 2.0, 3.0, 4.0], interp: Interpolation::Linear }];
        let timeline = Timeline::Deform {
            slot: SlotId(0),
            skin: a2d_core::ir::spine::DEFAULT_SKIN,
            attachment: AttachmentId(1),
            keys,
        };
        // Slot 0 shows attachment 0, so the timeline for attachment 1 is inert.
        apply(&mut pose, timeline.clone(), 0.0, 1.0, MixBlend::Setup);
        assert!(pose.slots[0].deform.is_empty());

        pose.set_slot_attachment(SlotId(0), Some("b"));
        apply(&mut pose, timeline, 0.0, 1.0, MixBlend::Setup);
        assert_eq!(pose.slots[0].deform, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn deform_blends_by_alpha_in_offset_space() {
        let mut pose = test_pose();
        pose.set_slot_attachment(SlotId(0), Some("b"));
        let keys =
            vec![DeformKey { time: 0.0, offset: 0, values: vec![2.0, 4.0, 6.0, 8.0], interp: Interpolation::Linear }];
        apply(
            &mut pose,
            Timeline::Deform {
                slot: SlotId(0),
                skin: a2d_core::ir::spine::DEFAULT_SKIN,
                attachment: AttachmentId(1),
                keys,
            },
            0.0,
            0.5,
            MixBlend::Setup,
        );
        assert_eq!(pose.slots[0].deform, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn deform_interpolates_between_keyframes() {
        let mut pose = test_pose();
        pose.set_slot_attachment(SlotId(0), Some("b"));
        let keys = vec![
            DeformKey { time: 0.0, offset: 0, values: vec![0.0, 0.0, 0.0, 0.0], interp: Interpolation::Linear },
            DeformKey { time: 1.0, offset: 0, values: vec![10.0, 10.0, 10.0, 10.0], interp: Interpolation::Linear },
        ];
        apply(
            &mut pose,
            Timeline::Deform {
                slot: SlotId(0),
                skin: a2d_core::ir::spine::DEFAULT_SKIN,
                attachment: AttachmentId(1),
                keys,
            },
            0.5,
            1.0,
            MixBlend::Setup,
        );
        assert_eq!(pose.slots[0].deform, vec![5.0, 5.0, 5.0, 5.0]);
    }

    #[test]
    fn events_fire_once_inside_their_window() {
        let pose = test_pose();
        let keys = vec![EventKey {
            time: 0.5,
            event: EventId(0),
            int_value: 3,
            float_value: 1.5,
            string_value: Some("s".into()),
            volume: 1.0,
            balance: 0.0,
        }];
        let mut fired = Vec::new();
        collect_events(&pose, &keys, 0.0, 1.0, &mut fired);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "step");
        assert_eq!(fired[0].int_value, 3);

        // The same window again must not re-fire.
        let mut again = Vec::new();
        collect_events(&pose, &keys, 0.5, 1.0, &mut again);
        assert!(again.is_empty());
    }

    #[test]
    fn events_do_not_fire_on_the_first_evaluation() {
        let pose = test_pose();
        let keys = vec![EventKey {
            time: 0.0,
            event: EventId(0),
            int_value: 0,
            float_value: 0.0,
            string_value: None,
            volume: 1.0,
            balance: 0.0,
        }];
        let mut fired = Vec::new();
        collect_events(&pose, &keys, -1.0, 1.0, &mut fired);
        assert!(fired.is_empty(), "a negative last_time means no window");
    }

    #[test]
    fn applying_an_animation_reaches_every_timeline() {
        let mut pose = test_pose();
        let animation = Animation {
            name: "idle".into(),
            duration: 1.0,
            timelines: vec![
                Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(0.0, 10.0)] },
                Timeline::SlotAlpha { slot: SlotId(0), keys: vec![key(0.0, 0.5)] },
            ],
        };
        let mut events = Vec::new();
        apply_animation(&mut pose, &animation, -1.0, 0.0, 1.0, MixBlend::Setup, &mut events);
        assert_eq!(pose.bones[1].local.rotation, 40.0);
        assert_eq!(pose.slots[0].color.a, 0.5);
    }
}
