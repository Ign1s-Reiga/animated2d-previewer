//! Package validation — the checks spec §14 requires of `animated2d validate`.
//!
//! Validation never fails the load. It reports, so a partially-broken package
//! can still be opened and looked at while the gap is diagnosed.

use a2d_core::ir::cubism::CubismIr;
use a2d_core::ir::spine::{AttachmentKind, ConstraintKind, SpineIr, Timeline};
use a2d_core::{Degradation, LoadReport};

use crate::package::TextureFile;

/// Runs every Spine-side check against a decoded package.
pub fn validate_spine(ir: &SpineIr, textures: &[TextureFile], report: &mut LoadReport) {
    check_textures(ir, textures, report);
    check_atlas_references(ir, report);
    check_attachments(ir, report);
    check_bone_parents(ir, report);
    check_slot_references(ir, report);
    check_constraints(ir, report);
    check_timelines(ir, report);
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

/// Unsupported constraints.
fn check_constraints(ir: &SpineIr, report: &mut LoadReport) {
    for entry in &ir.constraint_order {
        if entry.kind == ConstraintKind::Path {
            if let Some(c) = ir.path_constraints.get(entry.index as usize) {
                report.warn(Degradation::UnsupportedConstraint { name: c.name.clone(), kind: "path".into() });
            }
        }
    }
    for c in &ir.ik_constraints {
        if c.bones.len() > 2 {
            report.warn(Degradation::UnsupportedConstraint {
                name: c.name.clone(),
                kind: format!("ik with a {}-bone chain", c.bones.len()),
            });
        }
        if c.bones.iter().any(|b| b.index() >= ir.bones.len()) || c.target.index() >= ir.bones.len() {
            report.warn(Degradation::MissingReference { kind: "ik constraint bone".into(), name: c.name.clone() });
        }
    }
    for c in &ir.transform_constraints {
        if c.local || c.relative {
            report.warn(Degradation::UnsupportedConstraint {
                name: c.name.clone(),
                kind: format!("transform constraint in {} mode", if c.local { "local" } else { "relative" }),
            });
        }
    }
}

/// Unsupported timeline types.
fn check_timelines(ir: &SpineIr, report: &mut LoadReport) {
    for animation in &ir.animations {
        for timeline in &animation.timelines {
            let unsupported = matches!(
                timeline,
                Timeline::PathPosition { .. } | Timeline::PathSpacing { .. } | Timeline::PathMix { .. }
            );
            if unsupported {
                report.warn(Degradation::UnsupportedTimeline {
                    animation: animation.name.clone(),
                    kind: timeline.type_name().to_string(),
                });
            }
        }
        if animation.timelines.is_empty() {
            report.warn(Degradation::ClampedValue {
                context: format!("animation {:?}", animation.name),
                field: "timelines".into(),
                detail: "animation has no timelines".into(),
            });
        }
    }
}

/// Checks a Cubism model the way [`validate_spine`] checks a skeleton.
///
/// The structural indices are already range-checked when `model.bin` is read,
/// so what is left here is what a package can be *coherently* missing: a
/// texture page with no file, a model with nothing to draw, and the tracks a
/// drawable needs to be posed at all.
pub fn validate_cubism(ir: &CubismIr, textures: &[TextureFile], report: &mut LoadReport) {
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
        report.warn(Degradation::Note("no paint sequence; drawables will be painted in model order".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, AtlasRegionId, AttachmentId, BoneId, SlotId};
    use a2d_core::ir::spine::*;
    use a2d_core::{Rgba, Vec2};

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
        validate_spine(ir, textures, &mut report);
        report.to_string()
    }

    #[test]
    fn a_healthy_package_validates_cleanly() {
        let mut report = LoadReport::new();
        validate_spine(&healthy(), &page_file(), &mut report);
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

    #[test]
    fn path_constraints_and_their_timelines_are_reported_as_unsupported() {
        let mut ir = healthy();
        ir.path_constraints.push(PathConstraint {
            name: "pc".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1)],
            target_slot: SlotId(0),
            position_mode: PathPositionMode::default(),
            spacing_mode: PathSpacingMode::default(),
            rotate_mode: PathRotateMode::default(),
            offset_rotation: 0.0,
            position: 0.0,
            spacing: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
        });
        ir.animations[0]
            .timelines
            .push(Timeline::PathMix { constraint: a2d_core::ir::ids::PathConstraintId(0), keys: vec![] });
        ir.rebuild_derived();
        let text = run(&ir, &page_file());
        assert!(text.contains("path constraint"), "{text}");
        assert!(text.contains("path mix timeline"), "{text}");
    }

    #[test]
    fn a_long_ik_chain_is_reported() {
        let mut ir = healthy();
        ir.ik_constraints.push(IkConstraint {
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
        });
        ir.rebuild_derived();
        let text = run(&ir, &page_file());
        assert!(text.contains("3-bone chain"), "{text}");
    }

    #[test]
    fn unsupported_transform_constraint_modes_are_reported() {
        for (local, relative, expected) in [(true, false, "local"), (false, true, "relative")] {
            let mut ir = healthy();
            ir.transform_constraints.push(TransformConstraint {
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
                relative,
                local,
            });
            ir.rebuild_derived();
            let text = run(&ir, &page_file());
            assert!(text.contains(expected), "expected {expected} in:\n{text}");
        }
    }

    #[test]
    fn an_animation_with_no_timelines_is_reported() {
        let mut ir = healthy();
        ir.animations[0].timelines.clear();
        let text = run(&ir, &page_file());
        assert!(text.contains("no timelines"), "{text}");
    }
}
