//! Package validation — the checks spec §14 requires of `animated2d validate`.
//!
//! Validation never fails the load. It reports, so a partially-broken package
//! can still be opened and looked at while the gap is diagnosed.

use a2d_core::ir::cubism::CubismIr;
use a2d_core::ir::ids::BoneId;
use a2d_core::ir::spine::{AttachmentKind, PathConstraint, SpineIr, Timeline};
use a2d_core::{Degradation, LoadReport};

use crate::package::TextureFile;

/// Runs every Spine-side check against a decoded package.
///
/// `format_version` is the manifest's: a few findings exist only for packages
/// written before a decoder fix whose effect is baked into `model.bin`.
pub fn validate_spine(ir: &SpineIr, format_version: u32, textures: &[TextureFile], report: &mut LoadReport) {
    check_textures(ir, textures, report);
    check_atlas_references(ir, report);
    check_attachments(ir, report);
    check_bone_parents(ir, report);
    check_slot_references(ir, report);
    check_constraints(ir, report);
    check_timelines(ir, report);
    check_rotate_keys_unwrapped(ir, format_version, report);
}

/// Spine 3.x rotate keys cross the 360 degree seam the short way; the importer
/// has unwrapped them since package format 10. An older package keeps the raw
/// angles, so a pair of keys more than 180 degrees apart plays as a long spin.
fn check_rotate_keys_unwrapped(ir: &SpineIr, format_version: u32, report: &mut LoadReport) {
    if format_version >= 10 {
        return;
    }
    let spins = ir.animations.iter().flat_map(|a| &a.timelines).any(|t| match t {
        Timeline::BoneRotate { keys, .. } => keys.windows(2).any(|w| {
            // Mirrors the [-180, 180) wrap: an exact +180 step is played the
            // other way round by the old behaviour, an exact -180 is not.
            let d = w[1].value - w[0].value;
            d >= 180.0 || d < -180.0
        }),
        _ => false,
    });
    if spins {
        report.note(
            "imported before shortest-arc rotate keys (package format 10): some bones may spin the long way; \
             re-import the source",
        );
    }
}

/// Missing textures: every atlas page must have a file in `textures/`.
fn check_textures(ir: &SpineIr, textures: &[TextureFile], report: &mut LoadReport) {
    for page in &ir.atlas.pages {
        if !textures.iter().any(|t| t.file == page.name) {
            report.warn(Degradation::MissingReference { kind: "texture page".into(), name: page.name.clone() });
        }
        if page.size.is_none() {
            report.warn(Degradation::MissingReference {
                kind: "texture page size (UVs cannot be computed)".into(),
                name: page.name.clone(),
            });
        }
    }
}

/// Malformed atlas references: a region must name a page that exists.
fn check_atlas_references(ir: &SpineIr, report: &mut LoadReport) {
    for region in &ir.atlas.regions {
        if region.page.index() >= ir.atlas.pages.len() {
            report.warn(Degradation::MissingReference {
                kind: "atlas page for region".into(),
                name: region.name.clone(),
            });
        }
        if region.size.0 == 0 || region.size.1 == 0 {
            report.warn(Degradation::ClampedValue {
                context: format!("atlas region {:?}", region.name),
                field: "size".into(),
                detail: "region has zero area".into(),
            });
        }
    }
}

/// Unresolved attachments: a textured attachment with no atlas region binding.
fn check_attachments(ir: &SpineIr, report: &mut LoadReport) {
    for attachment in &ir.attachments {
        match &attachment.kind {
            AttachmentKind::Region(_) | AttachmentKind::Mesh(_) => {
                if attachment.kind.region().is_none() {
                    let path = attachment.kind.region_path().unwrap_or(&attachment.name);
                    report.warn(Degradation::MissingReference {
                        kind: "atlas region for attachment".into(),
                        name: path.to_string(),
                    });
                }
                if let AttachmentKind::Mesh(m) = &attachment.kind {
                    if m.uvs.len() != m.vertices.vertex_count() {
                        report.warn(Degradation::ClampedValue {
                            context: format!("mesh {:?}", attachment.name),
                            field: "uvs".into(),
                            detail: format!("{} UVs for {} vertices", m.uvs.len(), m.vertices.vertex_count()),
                        });
                    }
                    if let Some(&max) = m.triangles.iter().max() {
                        if max as usize >= m.uvs.len() {
                            report.warn(Degradation::ClampedValue {
                                context: format!("mesh {:?}", attachment.name),
                                field: "triangles".into(),
                                detail: format!("index {max} exceeds {} vertices", m.uvs.len()),
                            });
                        }
                    }
                    if let Some(link) = &m.linked_to {
                        if link.resolved.is_none() {
                            report.warn(Degradation::MissingReference {
                                kind: "linked mesh parent".into(),
                                name: link.parent.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Invalid bone parents.
fn check_bone_parents(ir: &SpineIr, report: &mut LoadReport) {
    for (i, bone) in ir.bones.iter().enumerate() {
        let Some(parent) = bone.parent else { continue };
        if parent.index() >= ir.bones.len() {
            report.warn(Degradation::MissingReference {
                kind: "parent bone".into(),
                name: format!("{} (of {:?})", parent.0, bone.name),
            });
        } else if parent.index() >= i {
            report.warn(Degradation::ClampedValue {
                context: format!("bone {:?}", bone.name),
                field: "parent".into(),
                detail: "parent is not stored before its child".into(),
            });
        }
    }
}

/// Invalid slot references, from slots themselves and from skins.
fn check_slot_references(ir: &SpineIr, report: &mut LoadReport) {
    for slot in &ir.slots {
        if slot.bone.index() >= ir.bones.len() {
            report.warn(Degradation::MissingReference {
                kind: "slot target bone".into(),
                name: format!("{} (of {:?})", slot.bone.0, slot.name),
            });
        }
        if let Some(name) = &slot.setup_attachment {
            let Some(slot_id) = ir.slot_by_name(&slot.name) else { continue };
            if ir.resolve_attachment(a2d_core::ir::spine::DEFAULT_SKIN, slot_id, name).is_none() {
                report.warn(Degradation::MissingReference {
                    kind: "setup attachment".into(),
                    name: format!("{name:?} on slot {:?}", slot.name),
                });
            }
        }
    }
    for skin in &ir.skins {
        for entry in &skin.entries {
            if entry.slot.index() >= ir.slots.len() {
                report.warn(Degradation::MissingReference {
                    kind: "skin entry slot".into(),
                    name: format!("{} (in skin {:?})", entry.slot.0, skin.name),
                });
            }
            if entry.attachment.index() >= ir.attachments.len() {
                report.warn(Degradation::MissingReference {
                    kind: "skin entry attachment".into(),
                    name: format!("{} (in skin {:?})", entry.attachment.0, skin.name),
                });
            }
        }
    }
}

/// Unsupported constraints, and constraints that name bones or slots which do
/// not exist.
///
/// The runtime evaluates IK, transform constraints in all four modes (world or
/// local, absolute or relative) and path constraints in every position,
/// spacing and rotate mode, so none of those is reported merely for being
/// present. What it cannot solve is an IK chain longer than two bones: it
/// solves the first two and leaves the rest alone. The Spine editor never
/// writes such a chain, so seeing one means the source data is off.
fn check_constraints(ir: &SpineIr, report: &mut LoadReport) {
    let bone_missing = |b: &BoneId| b.index() >= ir.bones.len();
    for c in &ir.ik_constraints {
        if c.bones.len() > 2 {
            report.warn(Degradation::UnsupportedConstraint {
                name: c.name.clone(),
                kind: format!("ik with a {}-bone chain", c.bones.len()),
            });
        }
        if c.bones.iter().any(bone_missing) || bone_missing(&c.target) {
            report.warn(Degradation::MissingReference { kind: "ik constraint bone".into(), name: c.name.clone() });
        }
    }
    for c in &ir.transform_constraints {
        if c.bones.iter().any(bone_missing) || bone_missing(&c.target) {
            report
                .warn(Degradation::MissingReference { kind: "transform constraint bone".into(), name: c.name.clone() });
        }
    }
    for c in &ir.path_constraints {
        if c.bones.iter().any(bone_missing) {
            report.warn(Degradation::MissingReference { kind: "path constraint bone".into(), name: c.name.clone() });
        }
        if c.target_slot.index() >= ir.slots.len() {
            report.warn(Degradation::MissingReference {
                kind: "path constraint target slot".into(),
                name: c.name.clone(),
            });
            continue;
        }
        check_path_coverage(ir, c, report);
    }
}

/// Fewest control points a path attachment needs before the runtime can cut a
/// single curve from it.
///
/// Mirrors `path_curves` in the runtime: anchors sit at vertices 1, 4, 7, ...
/// with two handles between each pair, so an open path needs vertices 1..=4,
/// five in all. A closed path always gets its wrapping curve from the last two
/// vertices and the first two, which needs three.
fn path_min_points(closed: bool) -> usize {
    if closed {
        3
    } else {
        5
    }
}

/// A path constraint does nothing unless its target slot can hold a path the
/// runtime can follow.
///
/// A single skin may leave the path off, which is legitimate. No skin putting
/// a path there at all means the constraint can never act, which is reported,
/// as is a path too short to cut a curve from, which the runtime also reports
/// when it meets one.
fn check_path_coverage(ir: &SpineIr, c: &PathConstraint, report: &mut LoadReport) {
    let mut has_path = false;
    for skin in &ir.skins {
        for entry in skin.entries.iter().filter(|e| e.slot == c.target_slot) {
            let Some(attachment) = ir.attachments.get(entry.attachment.index()) else { continue };
            let AttachmentKind::Path(path) = &attachment.kind else { continue };
            has_path = true;
            let points = path.vertices.vertex_count();
            let needed = path_min_points(path.closed);
            if points < needed {
                report.warn(Degradation::UnsupportedConstraint {
                    name: c.name.clone(),
                    kind: format!(
                        "path (attachment {:?} has {points} control points; {} path needs at least {needed})",
                        entry.name,
                        if path.closed { "a closed" } else { "an open" }
                    ),
                });
            }
        }
    }
    if !has_path {
        report.warn(Degradation::MissingReference {
            kind: "path attachment in any skin for the target slot of path constraint".into(),
            name: c.name.clone(),
        });
    }
}

/// Whether the runtime applies a timeline of this type.
///
/// Every timeline the IR can hold is applied today. The match is exhaustive on
/// purpose: a timeline type added to the IR does not compile here until someone
/// decides whether the runtime plays it, so `validate` cannot fall silent about
/// a type it has never heard of.
fn runtime_applies(timeline: &Timeline) -> bool {
    match timeline {
        Timeline::BoneRotate { .. }
        | Timeline::BoneTranslate { .. }
        | Timeline::BoneScale { .. }
        | Timeline::BoneShear { .. }
        | Timeline::SlotColor { .. }
        | Timeline::SlotTwoColor { .. }
        | Timeline::SlotAlpha { .. }
        | Timeline::SlotAttachment { .. }
        | Timeline::Deform { .. }
        | Timeline::DrawOrder { .. }
        | Timeline::Event { .. }
        | Timeline::IkConstraint { .. }
        | Timeline::TransformConstraint { .. }
        | Timeline::PathPosition { .. }
        | Timeline::PathSpacing { .. }
        | Timeline::PathMix { .. } => true,
    }
}

/// Unsupported timeline types, and timelines aimed at something that does
/// not exist.
///
/// An animation with no timelines is not reported: rigs do carry empty
/// animations (a "stop" that keys nothing), and nothing about one was dropped
/// or approximated.
fn check_timelines(ir: &SpineIr, report: &mut LoadReport) {
    for animation in &ir.animations {
        for timeline in &animation.timelines {
            if !runtime_applies(timeline) {
                report.warn(Degradation::UnsupportedTimeline {
                    animation: animation.name.clone(),
                    kind: timeline.type_name().to_string(),
                });
            }
            check_timeline_targets(ir, &animation.name, timeline, report);
        }
    }
}

/// Range-checks what a timeline animates.
///
/// Playback skips a timeline whose target is out of range without a word, so
/// this is the only place the gap is said out loud. A package read from disk
/// does not range-check timelines, and the binary decoders take timeline
/// targets as raw indices, so an out-of-range one can reach here.
fn check_timeline_targets(ir: &SpineIr, animation: &str, timeline: &Timeline, report: &mut LoadReport) {
    let mut check = |what: &str, index: usize, len: usize| {
        if index >= len {
            report.warn(Degradation::MissingReference {
                kind: format!("{what} for {} timeline", timeline.type_name()),
                name: format!("{index} (in animation {animation:?})"),
            });
        }
    };
    match timeline {
        Timeline::BoneRotate { bone, .. }
        | Timeline::BoneTranslate { bone, .. }
        | Timeline::BoneScale { bone, .. }
        | Timeline::BoneShear { bone, .. } => check("bone", bone.index(), ir.bones.len()),
        Timeline::SlotColor { slot, .. }
        | Timeline::SlotTwoColor { slot, .. }
        | Timeline::SlotAlpha { slot, .. }
        | Timeline::SlotAttachment { slot, .. } => check("slot", slot.index(), ir.slots.len()),
        Timeline::Deform { slot, skin, attachment, .. } => {
            check("slot", slot.index(), ir.slots.len());
            check("skin", skin.index(), ir.skins.len());
            check("attachment", attachment.index(), ir.attachments.len());
        }
        Timeline::DrawOrder { keys } => {
            for slot in keys.iter().filter_map(|k| k.order.as_ref()).flatten() {
                check("slot", slot.index(), ir.slots.len());
            }
        }
        Timeline::Event { keys } => {
            for key in keys {
                check("event", key.event.index(), ir.events.len());
            }
        }
        Timeline::IkConstraint { constraint, .. } => {
            check("ik constraint", constraint.index(), ir.ik_constraints.len())
        }
        Timeline::TransformConstraint { constraint, .. } => {
            check("transform constraint", constraint.index(), ir.transform_constraints.len())
        }
        Timeline::PathPosition { constraint, .. }
        | Timeline::PathSpacing { constraint, .. }
        | Timeline::PathMix { constraint, .. } => {
            check("path constraint", constraint.index(), ir.path_constraints.len())
        }
    }
}

/// Checks a Cubism model the way [`validate_spine`] checks a skeleton.
///
/// The structural indices are already range-checked when `model.bin` is read,
/// so what is left here is what a package can be *coherently* missing: a
/// texture page with no file, a model with nothing to draw, and the tracks a
/// drawable needs to be posed at all.
pub fn validate_cubism(ir: &CubismIr, format_version: u32, textures: &[TextureFile], report: &mut LoadReport) {
    for entry in ir.drawables.iter().map(|d| d.texture).collect::<std::collections::BTreeSet<_>>() {
        if textures.get(entry as usize).is_none() {
            report.warn(Degradation::MissingReference { kind: "texture page".into(), name: format!("page {entry}") });
        }
    }
    if ir.drawables.is_empty() {
        report.warn(Degradation::Note("the model has no drawables, so nothing will be seen".into()));
    }
    if ir.parameters.is_empty() {
        report.warn(Degradation::Note("the model has no parameters, so it cannot be animated".into()));
    }

    // A keyform pool that cannot cover its own drawables poses nothing: the
    // blend falls back to zeroed coordinates and the mesh collapses.
    for d in &ir.drawables {
        let needed = d.keyform_begin as usize + d.keyform_count as usize;
        if needed > ir.keyforms.drawable_offsets.len() {
            report.warn(Degradation::MissingReference { kind: "drawable keyforms".into(), name: d.id.clone() });
        }
    }

    // Cubism hides a part by taking it to zero opacity, so a model with no
    // opacity track draws everything -- including what should be hidden.
    if ir.drawable_keyform_opacities.is_empty() {
        report.warn(Degradation::Note("no drawable opacity track; every drawable will be painted opaque".into()));
    }
    if ir.draw_order.is_empty() {
        report.warn(Degradation::Note("no draw-order object list; ties between drawables fall to model order".into()));
    }
    // Draw order decides paint order, and its resting value is 500 for every
    // drawable, so without the track the object list alone decides.
    if ir.drawable_keyform_draw_orders.is_empty() && !ir.drawables.is_empty() {
        report.warn(Degradation::Note(
            "no drawable draw-order track; drawables are painted in object-list order and cannot be reordered by motions"
                .into(),
        ));
    }

    // Glue stitches split meshes back together. A package written before glue
    // was stored names its glues but cannot say what they join, so the seams
    // open as the model moves; only a fresh import can put that right.
    if !ir.glue_ids.is_empty() && ir.glues.is_empty() {
        report.warn(Degradation::Note(format!(
            "this package predates stored glue: {} glue constraints are not applied and split meshes may show gaps; \
             re-import the source to include them",
            ir.glue_ids.len()
        )));
    }

    // A package written before warp opacities were stored (format 3) cannot say
    // which deformers the artist faded; a fresh import reads them. An empty
    // track in a current package means the source had none, which the import
    // already reported, so only the old format is told to re-import.
    if format_version < 4 && ir.warp_keyform_opacities.is_empty() && !ir.keyforms.warp_offsets.is_empty() {
        report.warn(Degradation::Note(
            "this package predates stored warp opacities (format 4): pieces hidden by fading a warp deformer are \
             drawn; re-import the source to include them"
                .into(),
        ));
    }

    // A package written before physics was stored cannot say whether its source
    // had any. Absence is legitimate in a current package, so only an older
    // format is told to re-import.
    if ir.physics.is_none() && format_version < 5 {
        report.warn(Degradation::Note(
            "this package predates stored physics (format 5): if the source model had physics it is not included; \
             re-import the source to recover it"
                .into(),
        ));
    }

    // Same for repeat flags: an older package cannot say whether its source
    // marked any parameter as repeating. Playback then infers them from motions.
    if ir.parameter_repeats.is_none() && format_version < 6 {
        report.warn(Degradation::Note(
            "this package predates stored repeat flags (format 6): repeating parameters are inferred from motions; \
             re-import the source to read them"
                .into(),
        ));
    }

    // Same for the default motion and handovers: an older package cannot say
    // whether its source scripted any. Only a package that has motions has
    // anything to be told about, and a current one with none stored is a
    // source that carried none.
    if format_version < 7 && !ir.motions.is_empty() && ir.default_motion.is_none() && ir.handovers.is_empty() {
        report.warn(Degradation::Note(
            "this package predates stored default motion and handovers (format 7): the standing motion is guessed from motion names and no motion hands back to it by itself; re-import the source to read them"
                .into(),
        ));
    }

    // Physics naming a parameter the model lacks is skipped at playback;
    // validation reports it for the same reason as for motions below.
    for degradation in ir.unresolved_physics_targets() {
        report.warn(degradation);
    }

    // Playback skips a curve aimed at a parameter the model lacks. Package
    // validation never builds the model, so it has to say so itself.
    for degradation in ir.unresolved_motion_targets() {
        report.warn(degradation);
    }

    // Playback wraps the parameters flagged as repeating, or, with no flag
    // stored, those the motions key by whole turns. Say where the two disagree
    // or what was inferred.
    for degradation in ir.repeat_degradations() {
        report.warn(degradation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, AtlasRegionId, AttachmentId, BoneId, SlotId};
    use a2d_core::ir::spine::*;
    use a2d_core::{Interpolation, Rgba, Vec2};

    fn healthy() -> SpineIr {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None), Bone::new("torso", Some(BoneId(0)))],
            slots: vec![Slot { setup_attachment: Some("body".into()), ..Slot::new("body", BoneId(1)) }],
            skins: vec![Skin::new("default")],
            attachments: vec![Attachment {
                name: "body".into(),
                kind: AttachmentKind::Region(RegionAttachment {
                    path: "body".into(),
                    region: Some(AtlasRegionId(0)),
                    position: Vec2::ZERO,
                    rotation: 0.0,
                    scale: Vec2::ONE,
                    size: Vec2::new(10.0, 10.0),
                    color: Rgba::WHITE,
                    sequence: None,
                }),
            }],
            animations: vec![Animation {
                name: "idle".into(),
                duration: 1.0,
                timelines: vec![Timeline::BoneRotate { bone: BoneId(1), keys: vec![] }],
            }],
            atlas: Atlas {
                pages: vec![AtlasPage { size: Some((64, 64)), ..AtlasPage::new("hero.png") }],
                regions: vec![AtlasRegion {
                    name: "body".into(),
                    page: AtlasPageId(0),
                    xy: (0, 0),
                    size: (10, 10),
                    rotate_deg: 0,
                    offset: (0, 0),
                    original_size: (10, 10),
                    index: -1,
                    splits: None,
                    pads: None,
                }],
            },
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        ir
    }

    fn page_file() -> Vec<TextureFile> {
        vec![TextureFile { file: "hero.png".into(), bytes: vec![0u8; 4] }]
    }

    fn run(ir: &SpineIr, textures: &[TextureFile]) -> String {
        let mut report = LoadReport::new();
        validate_spine(ir, crate::FORMAT_VERSION, textures, &mut report);
        report.to_string()
    }

    fn with_rotate(first: f32, second: f32) -> SpineIr {
        let mut ir = healthy();
        let key = |time: f32, value: f32| ScalarKey { time, value, interp: Interpolation::Linear };
        let mut anim = Animation::new("idle");
        anim.timelines.push(Timeline::BoneRotate { bone: BoneId(1), keys: vec![key(0.0, first), key(1.0, second)] });
        anim.duration = anim.max_key_time();
        ir.animations.push(anim);
        ir
    }

    fn notes_for(ir: &SpineIr, format_version: u32) -> String {
        let mut report = LoadReport::new();
        validate_spine(ir, format_version, &page_file(), &mut report);
        report.to_string()
    }

    #[test]
    fn an_old_package_with_a_long_rotate_jump_is_noted() {
        let text = notes_for(&with_rotate(10.0, 350.0), 9);
        assert!(
            text.contains(
                "imported before shortest-arc rotate keys (package format 10): some bones may spin the long way; \
             re-import the source"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_current_package_with_the_same_keys_is_not_noted() {
        let text = notes_for(&with_rotate(10.0, 350.0), 10);
        assert!(!text.contains("shortest-arc"), "{text}");
    }

    #[test]
    fn an_old_package_without_a_long_rotate_jump_is_not_noted() {
        let text = notes_for(&with_rotate(10.0, 120.0), 9);
        assert!(!text.contains("shortest-arc"), "{text}");
        let text = notes_for(&with_rotate(180.0, 0.0), 9);
        assert!(!text.contains("shortest-arc"), "an exact -180 step is not wrapped: {text}");
    }

    #[test]
    fn an_exact_plus_180_step_is_noted() {
        let text = notes_for(&with_rotate(0.0, 180.0), 9);
        assert!(text.contains("shortest-arc"), "{text}");
    }

    #[test]
    fn a_healthy_package_validates_cleanly() {
        let mut report = LoadReport::new();
        validate_spine(&healthy(), crate::FORMAT_VERSION, &page_file(), &mut report);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_missing_texture_page_is_reported() {
        let text = run(&healthy(), &[]);
        assert!(text.contains("texture page"), "{text}");
        assert!(text.contains("hero.png"), "{text}");
    }

    #[test]
    fn a_page_without_a_size_is_reported_because_uvs_need_it() {
        let mut ir = healthy();
        ir.atlas.pages[0].size = None;
        let text = run(&ir, &page_file());
        assert!(text.contains("size"), "{text}");
    }

    #[test]
    fn an_unresolved_attachment_is_reported() {
        let mut ir = healthy();
        if let AttachmentKind::Region(r) = &mut ir.attachments[0].kind {
            r.region = None;
        }
        let text = run(&ir, &page_file());
        assert!(text.contains("atlas region for attachment"), "{text}");
    }

    #[test]
    fn a_region_pointing_at_a_missing_page_is_reported() {
        let mut ir = healthy();
        ir.atlas.regions[0].page = AtlasPageId(9);
        let text = run(&ir, &page_file());
        assert!(text.contains("atlas page for region"), "{text}");
    }

    #[test]
    fn a_zero_area_region_is_reported() {
        let mut ir = healthy();
        ir.atlas.regions[0].size = (0, 10);
        let text = run(&ir, &page_file());
        assert!(text.contains("zero area"), "{text}");
    }

    #[test]
    fn an_invalid_bone_parent_is_reported() {
        let mut ir = healthy();
        ir.bones[1].parent = Some(BoneId(42));
        let text = run(&ir, &page_file());
        assert!(text.contains("parent bone"), "{text}");
    }

    #[test]
    fn a_bone_parented_after_itself_is_reported() {
        let mut ir = healthy();
        ir.bones[0].parent = Some(BoneId(1));
        let text = run(&ir, &page_file());
        assert!(text.contains("not stored before"), "{text}");
    }

    #[test]
    fn an_invalid_slot_bone_is_reported() {
        let mut ir = healthy();
        ir.slots[0].bone = BoneId(42);
        let text = run(&ir, &page_file());
        assert!(text.contains("slot target bone"), "{text}");
    }

    #[test]
    fn a_setup_attachment_that_no_skin_provides_is_reported() {
        let mut ir = healthy();
        ir.slots[0].setup_attachment = Some("ghost".into());
        let text = run(&ir, &page_file());
        assert!(text.contains("setup attachment"), "{text}");
    }

    #[test]
    fn a_dangling_skin_entry_is_reported() {
        let mut ir = healthy();
        ir.skins[0].entries[0].attachment = AttachmentId(99);
        let text = run(&ir, &page_file());
        assert!(text.contains("skin entry attachment"), "{text}");
    }

    #[test]
    fn a_mesh_with_mismatched_uvs_is_reported() {
        let mut ir = healthy();
        ir.attachments[0].kind = AttachmentKind::Mesh(MeshAttachment {
            path: "body".into(),
            region: Some(AtlasRegionId(0)),
            uvs: vec![Vec2::ZERO],
            triangles: vec![],
            vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::ONE]),
            hull_length: 0,
            edges: vec![],
            size: Vec2::ZERO,
            color: Rgba::WHITE,
            linked_to: None,
            sequence: None,
        });
        let text = run(&ir, &page_file());
        assert!(text.contains("1 UVs for 2 vertices"), "{text}");
    }

    #[test]
    fn an_out_of_range_triangle_index_is_reported() {
        let mut ir = healthy();
        ir.attachments[0].kind = AttachmentKind::Mesh(MeshAttachment {
            path: "body".into(),
            region: Some(AtlasRegionId(0)),
            uvs: vec![Vec2::ZERO, Vec2::ONE],
            triangles: vec![0, 1, 9],
            vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::ONE]),
            hull_length: 0,
            edges: vec![],
            size: Vec2::ZERO,
            color: Rgba::WHITE,
            linked_to: None,
            sequence: None,
        });
        let text = run(&ir, &page_file());
        assert!(text.contains("index 9 exceeds"), "{text}");
    }

    #[test]
    fn an_unresolved_linked_mesh_is_reported() {
        let mut ir = healthy();
        ir.attachments[0].kind = AttachmentKind::Mesh(MeshAttachment {
            path: "body".into(),
            region: Some(AtlasRegionId(0)),
            uvs: vec![],
            triangles: vec![],
            vertices: VertexData::Rigid(vec![]),
            hull_length: 0,
            edges: vec![],
            size: Vec2::ZERO,
            color: Rgba::WHITE,
            linked_to: Some(LinkedMesh {
                skin: None,
                slot: SlotId(0),
                parent: "missing".into(),
                inherit_timelines: true,
                resolved: None,
            }),
            sequence: None,
        });
        let text = run(&ir, &page_file());
        assert!(text.contains("linked mesh parent"), "{text}");
    }

    /// Adds slot 1 ("rail") holding a path attachment with `points` control
    /// points, bound in the default skin.
    fn with_path_slot(mut ir: SpineIr, points: usize, closed: bool) -> SpineIr {
        ir.slots.push(Slot { setup_attachment: Some("rail".into()), ..Slot::new("rail", BoneId(0)) });
        ir.attachments.push(Attachment {
            name: "rail".into(),
            kind: AttachmentKind::Path(PathAttachment {
                closed,
                constant_speed: true,
                lengths: vec![],
                vertices: VertexData::Rigid((0..points).map(|i| Vec2::new(i as f32 * 10.0, 0.0)).collect()),
                color: Rgba::WHITE,
            }),
        });
        let attachment = AttachmentId(ir.attachments.len() as u32 - 1);
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "rail".into(), attachment });
        ir.rebuild_derived();
        ir
    }

    fn path_constraint(bones: Vec<BoneId>, target_slot: SlotId) -> PathConstraint {
        PathConstraint {
            name: "pc".into(),
            order: 0,
            skin_required: false,
            bones,
            target_slot,
            position_mode: PathPositionMode::default(),
            spacing_mode: PathSpacingMode::default(),
            rotate_mode: PathRotateMode::default(),
            offset_rotation: 0.0,
            position: 0.0,
            spacing: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
        }
    }

    fn transform_constraint(local: bool, relative: bool) -> TransformConstraint {
        TransformConstraint {
            name: format!("tc-local-{local}-relative-{relative}"),
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
            relative,
            local,
        }
    }

    fn long_ik_chain() -> IkConstraint {
        IkConstraint {
            name: "spine".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(0), BoneId(1), BoneId(0)],
            target: BoneId(1),
            mix: 1.0,
            softness: 0.0,
            bend_positive: true,
            compress: false,
            stretch: false,
            uniform: false,
        }
    }

    /// Everything the runtime evaluates and `validate` once called
    /// unsupported: a path constraint in every position, spacing and rotate
    /// mode, all three path timelines, transform constraints in all four
    /// modes, and an empty animation.
    fn constrained() -> SpineIr {
        use a2d_core::ir::ids::PathConstraintId;
        let mut ir = with_path_slot(healthy(), 6, false);
        for position_mode in [PathPositionMode::Fixed, PathPositionMode::Percent] {
            for spacing_mode in [
                PathSpacingMode::Length,
                PathSpacingMode::Fixed,
                PathSpacingMode::Percent,
                PathSpacingMode::Proportional,
            ] {
                for rotate_mode in [PathRotateMode::Tangent, PathRotateMode::Chain, PathRotateMode::ChainScale] {
                    ir.path_constraints.push(PathConstraint {
                        name: format!("pc-{}", ir.path_constraints.len()),
                        position_mode,
                        spacing_mode,
                        rotate_mode,
                        ..path_constraint(vec![BoneId(1)], SlotId(1))
                    });
                }
            }
        }
        for (local, relative) in [(false, false), (false, true), (true, false), (true, true)] {
            ir.transform_constraints.push(transform_constraint(local, relative));
        }
        let pc = PathConstraintId(0);
        ir.animations[0].timelines.extend([
            Timeline::PathPosition { constraint: pc, keys: vec![] },
            Timeline::PathSpacing { constraint: pc, keys: vec![] },
            Timeline::PathMix { constraint: pc, keys: vec![] },
        ]);
        ir.animations.push(Animation { name: "Stop".into(), duration: 0.0, timelines: vec![] });
        ir.rebuild_derived();
        ir
    }

    #[test]
    fn what_the_runtime_evaluates_validates_cleanly() {
        let ir = constrained();
        let mut report = LoadReport::new();
        validate_spine(&ir, crate::FORMAT_VERSION, &page_file(), &mut report);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn an_empty_animation_is_not_a_degradation() {
        let mut ir = healthy();
        ir.animations.push(Animation { name: "Stop".into(), duration: 0.0, timelines: vec![] });
        let text = run(&ir, &page_file());
        assert_eq!(text, "Loaded cleanly.", "nothing was clamped or dropped");
    }

    #[test]
    fn a_path_constraint_whose_slot_no_skin_gives_a_path_is_reported() {
        // Slot 0 only ever holds a region, so the constraint can never act.
        let mut ir = healthy();
        ir.path_constraints.push(path_constraint(vec![BoneId(1)], SlotId(0)));
        ir.rebuild_derived();
        let text = run(&ir, &page_file());
        assert!(text.contains("path attachment in any skin for the target slot of path constraint: pc"), "{text}");
    }

    #[test]
    fn a_path_with_too_few_control_points_for_the_runtime_is_reported() {
        // The runtime cuts its first open curve from vertices 1..=4 and its
        // closed wrap from the last two and the first two.
        for (points, closed, reported) in
            [(4, false, true), (5, false, false), (2, true, true), (3, true, false), (0, false, true)]
        {
            let mut ir = with_path_slot(healthy(), points, closed);
            ir.path_constraints.push(path_constraint(vec![BoneId(1)], SlotId(1)));
            ir.rebuild_derived();
            let text = run(&ir, &page_file());
            let expected = format!("has {points} control points");
            assert_eq!(text.contains(&expected), reported, "{points} points, closed {closed}: {text}");
            if reported {
                assert!(text.contains("constraint \"pc\" unsupported"), "{text}");
            } else {
                assert_eq!(text, "Loaded cleanly.");
            }
        }
    }

    #[test]
    fn timelines_aimed_at_missing_targets_are_reported() {
        use a2d_core::ir::ids::{EventId, IkConstraintId, PathConstraintId, SkinId, TransformConstraintId};
        let far = 77u16;
        let cases: Vec<(Timeline, &str)> = vec![
            (Timeline::BoneRotate { bone: BoneId(far), keys: vec![] }, "missing bone for bone rotate timeline"),
            (Timeline::SlotAlpha { slot: SlotId(far), keys: vec![] }, "missing slot for slot alpha timeline"),
            (
                Timeline::Deform { slot: SlotId(0), skin: SkinId(far), attachment: AttachmentId(0), keys: vec![] },
                "missing skin for deform timeline",
            ),
            (
                Timeline::Deform {
                    slot: SlotId(0),
                    skin: SkinId(0),
                    attachment: AttachmentId(u32::from(far)),
                    keys: vec![],
                },
                "missing attachment for deform timeline",
            ),
            (
                Timeline::DrawOrder { keys: vec![DrawOrderKey { time: 0.0, order: Some(vec![SlotId(far)]) }] },
                "missing slot for draw order timeline",
            ),
            (
                Timeline::Event {
                    keys: vec![EventKey {
                        time: 0.0,
                        event: EventId(far),
                        int_value: 0,
                        float_value: 0.0,
                        string_value: None,
                        volume: 1.0,
                        balance: 0.0,
                    }],
                },
                "missing event for event timeline",
            ),
            (
                Timeline::IkConstraint { constraint: IkConstraintId(far), keys: vec![] },
                "missing ik constraint for ik constraint timeline",
            ),
            (
                Timeline::TransformConstraint { constraint: TransformConstraintId(far), keys: vec![] },
                "missing transform constraint for transform constraint timeline",
            ),
            (
                Timeline::PathSpacing { constraint: PathConstraintId(far), keys: vec![] },
                "missing path constraint for path spacing timeline",
            ),
        ];
        for (timeline, expected) in cases {
            let mut ir = healthy();
            ir.animations[0].timelines.push(timeline);
            let text = run(&ir, &page_file());
            assert!(text.contains(expected), "expected {expected:?} in:\n{text}");
            assert!(text.contains("77 (in animation \"idle\")"), "{text}");
        }
    }

    #[test]
    fn a_long_ik_chain_is_reported() {
        let mut ir = healthy();
        ir.ik_constraints.push(long_ik_chain());
        ir.rebuild_derived();
        let text = run(&ir, &page_file());
        assert!(text.contains("3-bone chain"), "{text}");
    }

    #[test]
    fn a_long_ik_chain_is_still_reported_alongside_supported_constraints() {
        let mut ir = constrained();
        ir.ik_constraints.push(long_ik_chain());
        ir.rebuild_derived();
        let mut report = LoadReport::new();
        validate_spine(&ir, crate::FORMAT_VERSION, &page_file(), &mut report);
        assert_eq!(report.len(), 1, "{report}");
        assert!(report.to_string().contains("ik with a 3-bone chain constraint \"spine\" unsupported"), "{report}");
    }

    #[test]
    fn constraints_naming_missing_bones_or_slots_are_reported() {
        let mut ir = healthy();
        ir.path_constraints.push(path_constraint(vec![BoneId(42)], SlotId(9)));
        ir.transform_constraints.push(TransformConstraint { target: BoneId(42), ..transform_constraint(false, false) });
        let text = run(&ir, &page_file());
        assert!(text.contains("missing path constraint bone: pc"), "{text}");
        assert!(text.contains("missing path constraint target slot: pc"), "{text}");
        assert!(text.contains("missing transform constraint bone"), "{text}");
    }

    #[test]
    fn a_repeating_cubism_parameter_is_reported_once() {
        use a2d_core::ir::cubism::{CubismIr, CurveKey, Motion, MotionCurve, Parameter};
        let key = |t: f32, v: f32| CurveKey { time: t, value: v, in_slope: 0.0, out_slope: 0.0, stepped: false };
        let motion = |name: &str| Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamSpin".into(),
                keys: vec![key(0.0, 360.0), key(1.0, 180.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let ir = CubismIr {
            parameters: vec![Parameter { id: "ParamSpin".into(), minimum: -30.0, maximum: 30.0, default: 0.0 }],
            motions: vec![motion("a"), motion("b")],
            ..CubismIr::default()
        };
        let mut report = LoadReport::new();
        validate_cubism(&ir, crate::FORMAT_VERSION, &[], &mut report);
        let hits: Vec<String> = report
            .warnings()
            .iter()
            .map(|w| w.to_string())
            .filter(|w| w.contains("ParamSpin") && w.contains("repeating"))
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    #[test]
    fn with_stored_flags_validation_reports_only_a_disagreement_and_only_once() {
        use a2d_core::ir::cubism::{CubismIr, CurveKey, Motion, MotionCurve, Parameter};
        let key = |t: f32, v: f32| CurveKey { time: t, value: v, in_slope: 0.0, out_slope: 0.0, stepped: false };
        let motion = |name: &str| Motion {
            name: name.into(),
            duration: 1.0,
            curves: vec![MotionCurve {
                target: "ParamSpin".into(),
                keys: vec![key(0.0, 360.0), key(1.0, 180.0)],
                ..MotionCurve::default()
            }],
            ..Motion::default()
        };
        let mut ir = CubismIr {
            parameters: vec![Parameter { id: "ParamSpin".into(), minimum: -30.0, maximum: 30.0, default: 0.0 }],
            motions: vec![motion("a"), motion("b")],
            ..CubismIr::default()
        };
        let notes = |ir: &CubismIr| {
            let mut report = LoadReport::new();
            validate_cubism(ir, crate::FORMAT_VERSION, &[], &mut report);
            report.warnings().iter().map(|w| w.to_string()).filter(|w| w.contains("ParamSpin")).collect::<Vec<_>>()
        };
        // The source does not mark it: clamped, and said once however many motions show it.
        ir.parameter_repeats = Some(vec![false]);
        let hits = notes(&ir);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].contains("does not mark it repeating"), "{hits:?}");
        // Marked and agreeing with the motions: nothing to say.
        ir.parameter_repeats = Some(vec![true]);
        assert!(notes(&ir).is_empty());
    }
}
