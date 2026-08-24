//! Post-decode normalisation shared by the JSON and binary decoders.
//!
//! Both decoders produce an IR with two kinds of dangling reference: linked
//! meshes that borrow geometry from another attachment, and attachments that
//! name an atlas region by string. Resolving them here rather than in each
//! decoder is what keeps the two decoders honest about producing the same shape.

use a2d_core::ir::ids::AttachmentId;
use a2d_core::ir::spine::{AttachmentKind, SpineIr, DEFAULT_SKIN};
use a2d_core::{Degradation, LoadReport};

/// Resolves linked meshes and atlas regions, then rebuilds sorted invariants.
pub fn finish(ir: &mut SpineIr, report: &mut LoadReport) {
    ir.rebuild_derived();
    resolve_linked_meshes(ir, report);
    resolve_regions(ir, report);
    ir.rebuild_derived();
}

/// Copies geometry from each linked mesh's parent.
///
/// A linked mesh stores only its own colour, path and deform timelines; its
/// vertices, UVs and triangles belong to the mesh it points at. Resolution runs
/// in two passes so the source and destination borrows never overlap.
fn resolve_linked_meshes(ir: &mut SpineIr, report: &mut LoadReport) {
    let mut jobs: Vec<(usize, Option<AttachmentId>)> = Vec::new();
    for (i, att) in ir.attachments.iter().enumerate() {
        let AttachmentKind::Mesh(m) = &att.kind else { continue };
        let Some(link) = &m.linked_to else { continue };
        if link.resolved.is_some() {
            continue;
        }
        let skin = match link.skin.as_deref() {
            None => Some(DEFAULT_SKIN),
            Some(name) => ir.skin_by_name(name),
        };
        let parent = skin.and_then(|s| ir.resolve_attachment(s, link.slot, &link.parent));
        jobs.push((i, parent));
    }

    for (i, parent) in jobs {
        let Some(parent_id) = parent else {
            let name = ir.attachments[i].name.clone();
            report.warn(Degradation::MissingReference { kind: "linked mesh parent".into(), name });
            continue;
        };
        if parent_id.index() == i {
            let name = ir.attachments[i].name.clone();
            report.warn(Degradation::MissingReference { kind: "linked mesh parent (self-reference)".into(), name });
            continue;
        }
        // Take a copy rather than a borrow so the destination can be mutated.
        let source = match &ir.attachments[parent_id.index()].kind {
            AttachmentKind::Mesh(m) => {
                Some((m.uvs.clone(), m.triangles.clone(), m.vertices.clone(), m.hull_length, m.edges.clone()))
            }
            _ => None,
        };
        let Some((uvs, triangles, vertices, hull_length, edges)) = source else {
            let name = ir.attachments[i].name.clone();
            report.warn(Degradation::MissingReference { kind: "linked mesh parent is not a mesh".into(), name });
            continue;
        };
        if let AttachmentKind::Mesh(m) = &mut ir.attachments[i].kind {
            m.uvs = uvs;
            m.triangles = triangles;
            m.vertices = vertices;
            m.hull_length = hull_length;
            m.edges = edges;
            if let Some(link) = &mut m.linked_to {
                link.resolved = Some(parent_id);
            }
        }
    }
}

/// Binds every textured attachment to its atlas region.
///
/// A missing region is a degradation, not a load failure: the rest of the model
/// still draws, and `validate` surfaces the gap.
fn resolve_regions(ir: &mut SpineIr, report: &mut LoadReport) {
    if ir.atlas.is_empty() {
        return;
    }
    let mut missing: Vec<String> = Vec::new();
    for att in &mut ir.attachments {
        let path = match &att.kind {
            AttachmentKind::Region(r) => r.path.clone(),
            AttachmentKind::Mesh(m) => m.path.clone(),
            _ => continue,
        };
        let found = ir.atlas.find(&path);
        if found.is_none() {
            missing.push(path);
        }
        match &mut att.kind {
            AttachmentKind::Region(r) => r.region = found,
            AttachmentKind::Mesh(m) => m.region = found,
            _ => {}
        }
    }
    for name in missing {
        report.warn(Degradation::MissingReference { kind: "atlas region".into(), name });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, BoneId, SkinId, SlotId};
    use a2d_core::ir::spine::{
        Attachment, LinkedMesh, MeshAttachment, RegionAttachment, Skin, SkinEntry, Slot, VertexData,
    };
    use a2d_core::{Rgba, Vec2};

    fn mesh(name: &str, path: &str, link: Option<LinkedMesh>) -> Attachment {
        Attachment {
            name: name.into(),
            kind: AttachmentKind::Mesh(MeshAttachment {
                path: path.into(),
                region: None,
                uvs: if link.is_some() { vec![] } else { vec![Vec2::ZERO, Vec2::ONE] },
                triangles: if link.is_some() { vec![] } else { vec![0, 1, 0] },
                vertices: VertexData::Rigid(if link.is_some() {
                    vec![]
                } else {
                    vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]
                }),
                hull_length: if link.is_some() { 0 } else { 2 },
                edges: vec![],
                size: Vec2::ZERO,
                color: Rgba::WHITE,
                linked_to: link,
                sequence: None,
            }),
        }
    }

    fn ir_with_link(link: LinkedMesh) -> SpineIr {
        let mut ir = SpineIr {
            bones: vec![a2d_core::ir::spine::Bone::new("root", None)],
            slots: vec![Slot::new("body", BoneId(0))],
            skins: vec![Skin::new("default"), Skin::new("blue")],
            attachments: vec![mesh("body", "body", None), mesh("body-blue", "body_blue", Some(link))],
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.skins[1].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(1) });
        ir
    }

    fn geometry(ir: &SpineIr, id: AttachmentId) -> (usize, usize, usize) {
        match &ir.attachment(id).unwrap().kind {
            AttachmentKind::Mesh(m) => (m.uvs.len(), m.triangles.len(), m.vertices.vertex_count()),
            other => panic!("expected a mesh, got {other:?}"),
        }
    }

    #[test]
    fn a_linked_mesh_inherits_its_parents_geometry() {
        let link =
            LinkedMesh { skin: None, slot: SlotId(0), parent: "body".into(), inherit_timelines: true, resolved: None };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);

        assert_eq!(geometry(&ir, AttachmentId(1)), (2, 3, 2));
        match &ir.attachment(AttachmentId(1)).unwrap().kind {
            AttachmentKind::Mesh(m) => {
                assert_eq!(m.linked_to.as_ref().unwrap().resolved, Some(AttachmentId(0)));
                // Its own path is kept; only geometry is inherited.
                assert_eq!(m.path, "body_blue");
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_linked_mesh_can_name_the_skin_holding_its_parent() {
        let link = LinkedMesh {
            skin: Some("default".into()),
            slot: SlotId(0),
            parent: "body".into(),
            inherit_timelines: true,
            resolved: None,
        };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert_eq!(geometry(&ir, AttachmentId(1)), (2, 3, 2));
    }

    #[test]
    fn an_unresolvable_linked_mesh_is_reported_rather_than_fatal() {
        let link = LinkedMesh {
            skin: None,
            slot: SlotId(0),
            parent: "nonexistent".into(),
            inherit_timelines: true,
            resolved: None,
        };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert_eq!(geometry(&ir, AttachmentId(1)), (0, 0, 0));
        assert!(report.to_string().contains("linked mesh parent"), "{report}");
    }

    #[test]
    fn a_linked_mesh_naming_an_unknown_skin_is_reported() {
        let link = LinkedMesh {
            skin: Some("ghost".into()),
            slot: SlotId(0),
            parent: "body".into(),
            inherit_timelines: true,
            resolved: None,
        };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert!(report.to_string().contains("linked mesh parent"), "{report}");
    }

    #[test]
    fn resolution_is_idempotent() {
        let link =
            LinkedMesh { skin: None, slot: SlotId(0), parent: "body".into(), inherit_timelines: true, resolved: None };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        let snapshot = ir.clone();
        finish(&mut ir, &mut report);
        assert_eq!(ir, snapshot);
    }

    fn atlas_with(names: &[&str]) -> Atlas {
        let mut atlas = Atlas {
            pages: vec![AtlasPage { size: Some((64, 64)), ..AtlasPage::new("p.png") }],
            regions: names
                .iter()
                .map(|n| AtlasRegion {
                    name: (*n).into(),
                    page: AtlasPageId(0),
                    xy: (0, 0),
                    size: (8, 8),
                    rotate_deg: 0,
                    offset: (0, 0),
                    original_size: (8, 8),
                    index: -1,
                    splits: None,
                    pads: None,
                })
                .collect(),
        };
        atlas.sort_regions();
        atlas
    }

    #[test]
    fn attachments_bind_to_their_atlas_region() {
        let mut ir = SpineIr {
            attachments: vec![Attachment {
                name: "head".into(),
                kind: AttachmentKind::Region(RegionAttachment {
                    path: "head".into(),
                    region: None,
                    position: Vec2::ZERO,
                    rotation: 0.0,
                    scale: Vec2::ONE,
                    size: Vec2::ZERO,
                    color: Rgba::WHITE,
                    sequence: None,
                }),
            }],
            atlas: atlas_with(&["head", "torso"]),
            ..Default::default()
        };
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert!(ir.attachments[0].kind.region().is_some());
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_missing_atlas_region_is_reported_and_leaves_the_binding_empty() {
        let mut ir = SpineIr {
            attachments: vec![mesh("body", "not_in_atlas", None)],
            atlas: atlas_with(&["head"]),
            ..Default::default()
        };
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert!(ir.attachments[0].kind.region().is_none());
        assert!(report.to_string().contains("not_in_atlas"), "{report}");
    }

    #[test]
    fn region_resolution_is_skipped_when_no_atlas_was_supplied() {
        let mut ir = SpineIr { attachments: vec![mesh("body", "body", None)], ..Default::default() };
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        // No atlas means no claim either way, so nothing is reported.
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_self_referential_linked_mesh_is_reported_rather_than_looping() {
        let mut ir = SpineIr {
            slots: vec![Slot::new("body", BoneId(0))],
            skins: vec![Skin::new("default")],
            attachments: vec![mesh(
                "body",
                "body",
                Some(LinkedMesh {
                    skin: None,
                    slot: SlotId(0),
                    parent: "body".into(),
                    inherit_timelines: true,
                    resolved: None,
                }),
            )],
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert!(report.to_string().contains("self-reference"), "{report}");
    }

    #[test]
    fn skin_lookup_by_id_still_works_after_normalisation() {
        let link =
            LinkedMesh { skin: None, slot: SlotId(0), parent: "body".into(), inherit_timelines: true, resolved: None };
        let mut ir = ir_with_link(link);
        let mut report = LoadReport::new();
        finish(&mut ir, &mut report);
        assert_eq!(ir.resolve_attachment(SkinId(1), SlotId(0), "body"), Some(AttachmentId(1)));
    }
}
