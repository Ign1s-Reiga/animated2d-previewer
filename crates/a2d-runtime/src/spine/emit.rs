//! Turning a posed skeleton into renderer-neutral primitives.
//!
//! Nothing here knows about a GPU. It produces world-space triangles, atlas
//! UVs, tints and blend modes; `a2d-render` draws them.

use a2d_core::ir::atlas::{Atlas, AtlasRegion};
use a2d_core::ir::ids::AtlasRegionId;
use a2d_core::ir::spine::{AttachmentKind, MeshAttachment, RegionAttachment, SpineIr, VertexData};
use a2d_core::{Aabb, Affine2, RenderList, RenderMesh, Rgba, TextureId, Vec2};

use crate::spine::pose::SkeletonPose;

/// Appends the pose's current frame to `out`.
///
/// `alpha` multiplies every slot's alpha, so a model can fade as a whole.
pub fn emit(pose: &SkeletonPose, alpha: f32, out: &mut RenderList) {
    let ir = pose.ir();
    // A clipping attachment clips everything from its own slot up to and
    // including its `end_slot`, in draw order.
    let mut active_mask: Option<(a2d_core::MaskId, usize)> = None;

    for (z, slot_id) in pose.draw_order.iter().enumerate() {
        let Some(slot_pose) = pose.slots.get(slot_id.index()) else { continue };
        let Some(slot_data) = ir.slot(*slot_id) else { continue };
        let Some(attachment_id) = slot_pose.attachment else { continue };
        let Some(attachment) = ir.attachment(attachment_id) else { continue };
        let Some(bone) = pose.bones.get(slot_data.bone.index()) else { continue };

        // Retire a mask once its end slot has been drawn.
        if let Some((_, end)) = active_mask {
            if z > end {
                active_mask = None;
            }
        }

        match &attachment.kind {
            AttachmentKind::Clipping(c) => {
                let polygon = deform_polygon(pose, &c.vertices, &slot_pose.deform, bone.world);
                if polygon.len() >= 3 {
                    let end = c
                        .end_slot
                        .and_then(|s| pose.draw_order.iter().position(|d| *d == s))
                        .unwrap_or(pose.draw_order.len());
                    active_mask = Some((out.push_mask(polygon), end));
                }
            }
            AttachmentKind::Region(r) => {
                if let Some(mesh) =
                    region_mesh(ir, r, bone.world, slot_data, slot_pose.color, slot_pose.dark_color, alpha, z, out)
                {
                    let mut mesh = mesh;
                    mesh.clipping_mask = active_mask.map(|(id, _)| id);
                    out.push_mesh(mesh);
                }
            }
            AttachmentKind::Mesh(m) => {
                if let Some(mesh) = skinned_mesh(
                    ir,
                    m,
                    pose,
                    bone.world,
                    slot_data,
                    &slot_pose.deform,
                    slot_pose.color,
                    slot_pose.dark_color,
                    alpha,
                    z,
                    out,
                ) {
                    let mut mesh = mesh;
                    mesh.clipping_mask = active_mask.map(|(id, _)| id);
                    out.push_mesh(mesh);
                }
            }
            // Bounding boxes, points and paths carry no drawable geometry.
            AttachmentKind::BoundingBox(_) | AttachmentKind::Point(_) | AttachmentKind::Path(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn region_mesh(
    ir: &SpineIr,
    r: &RegionAttachment,
    bone: Affine2,
    slot: &a2d_core::ir::spine::Slot,
    color: Rgba,
    dark: Option<a2d_core::Rgb>,
    alpha: f32,
    z: usize,
    out: &mut RenderList,
) -> Option<RenderMesh> {
    let region_id = r.region?;
    let region = ir.atlas.region(region_id)?;
    let uvs = region_uvs(&ir.atlas, region_id)?;

    let (ow, oh) = (region.original_size.0 as f32, region.original_size.1 as f32);
    if ow <= 0.0 || oh <= 0.0 {
        return None;
    }
    // The attachment's size is the *untrimmed* size, so the packed sub-image
    // has to be scaled and offset back into it.
    let scale_x = r.size.x / ow * r.scale.x;
    let scale_y = r.size.y / oh * r.scale.y;
    let (pw, ph) = region.unrotated_size();
    let local_x = -r.size.x / 2.0 * r.scale.x + region.offset.0 as f32 * scale_x;
    let local_y = -r.size.y / 2.0 * r.scale.y + region.offset.1 as f32 * scale_y;
    let local_x2 = local_x + pw as f32 * scale_x;
    let local_y2 = local_y + ph as f32 * scale_y;

    let (sin, cos) = r.rotation.to_radians().sin_cos();
    let corner = |lx: f32, ly: f32| {
        let rx = lx * cos - ly * sin + r.position.x;
        let ry = lx * sin + ly * cos + r.position.y;
        bone.transform_point(Vec2::new(rx, ry))
    };

    let mut mesh = out.take_mesh();
    // Corner order: bottom-left, bottom-right, top-right, top-left.
    mesh.vertices.extend_from_slice(&[
        corner(local_x, local_y),
        corner(local_x2, local_y),
        corner(local_x2, local_y2),
        corner(local_x, local_y2),
    ]);
    mesh.uvs.extend_from_slice(&uvs);
    mesh.indices.extend_from_slice(&[0, 1, 2, 2, 3, 0]);
    mesh.texture = TextureId(region.page.index() as u32);
    mesh.color = tint(color, r.color, alpha);
    mesh.dark_color = dark;
    mesh.blend_mode = slot.blend_mode;
    mesh.z_order = z as u32;
    Some(mesh)
}

#[allow(clippy::too_many_arguments)]
fn skinned_mesh(
    ir: &SpineIr,
    m: &MeshAttachment,
    pose: &SkeletonPose,
    slot_bone: Affine2,
    slot: &a2d_core::ir::spine::Slot,
    deform: &[f32],
    color: Rgba,
    dark: Option<a2d_core::Rgb>,
    alpha: f32,
    z: usize,
    out: &mut RenderList,
) -> Option<RenderMesh> {
    if m.triangles.is_empty() || m.uvs.is_empty() {
        return None;
    }
    let region_id = m.region?;
    let region = ir.atlas.region(region_id)?;
    let corners = region_uvs(&ir.atlas, region_id)?;

    let mut mesh = out.take_mesh();
    mesh.vertices.extend(deform_positions(pose, &m.vertices, deform, slot_bone));
    if mesh.vertices.len() != m.uvs.len() {
        // Vertex and UV counts disagree; emitting would produce a malformed
        // mesh, so drop it rather than draw garbage.
        mesh.vertices.clear();
        return None;
    }
    for uv in &m.uvs {
        mesh.uvs.push(map_uv(&corners, *uv));
    }
    mesh.indices.extend_from_slice(&m.triangles);
    mesh.texture = TextureId(region.page.index() as u32);
    mesh.color = tint(color, m.color, alpha);
    mesh.dark_color = dark;
    mesh.blend_mode = slot.blend_mode;
    mesh.z_order = z as u32;
    Some(mesh)
}

/// World positions for a vertex set, applying deform offsets and skinning.
///
/// Lives on the pose because path constraints need exactly the same geometry:
/// the path they follow is an attachment, skinned and deformed like any other.
fn deform_positions(pose: &SkeletonPose, vertices: &VertexData, deform: &[f32], slot_bone: Affine2) -> Vec<Vec2> {
    pose.world_vertices(vertices, deform, slot_bone)
}

/// World-space polygon for a clipping or bounding-box attachment.
fn deform_polygon(pose: &SkeletonPose, vertices: &VertexData, deform: &[f32], slot_bone: Affine2) -> Vec<Vec2> {
    deform_positions(pose, vertices, deform, slot_bone)
}

/// The four corner UVs of a region, in bottom-left, bottom-right, top-right,
/// top-left order.
fn region_uvs(atlas: &Atlas, id: AtlasRegionId) -> Option<[Vec2; 4]> {
    let region = atlas.region(id)?;
    let page = atlas.page(region.page)?;
    region.corner_uvs(page)
}

/// Maps an attachment-local UV into atlas page space.
///
/// Attachment UVs use image coordinates: `(0, 0)` is the region's top-left and
/// `(1, 1)` its bottom-right. Expressing the mapping as a basis over the corner
/// UVs makes it correct for every packing rotation without a special case.
fn map_uv(corners: &[Vec2; 4], uv: Vec2) -> Vec2 {
    let (bl, tr, tl) = (corners[0], corners[2], corners[3]);
    // Both axes are measured from the top-left corner, which is where an
    // attachment UV of (0, 0) sits.
    tl + (tr - tl) * uv.x + (bl - tl) * uv.y
}

fn tint(slot: Rgba, attachment: Rgba, alpha: f32) -> Rgba {
    let mut c = slot.modulate(attachment);
    c.a *= alpha;
    c
}

/// Bounds of everything the pose would draw, including bounding-box attachments.
pub fn pose_bounds(pose: &SkeletonPose) -> Aabb {
    let ir = pose.ir();
    let mut bounds = Aabb::EMPTY;
    for slot_id in &pose.draw_order {
        let Some(slot_pose) = pose.slots.get(slot_id.index()) else { continue };
        let Some(slot_data) = ir.slot(*slot_id) else { continue };
        let Some(attachment_id) = slot_pose.attachment else { continue };
        let Some(attachment) = ir.attachment(attachment_id) else { continue };
        let Some(bone) = pose.bones.get(slot_data.bone.index()) else { continue };

        match &attachment.kind {
            AttachmentKind::Region(r) => {
                if let Some(region) = r.region.and_then(|id| ir.atlas.region(id)) {
                    for v in region_corners(r, region, bone.world) {
                        bounds.extend(v);
                    }
                }
            }
            AttachmentKind::Mesh(m) => {
                for v in deform_positions(pose, &m.vertices, &slot_pose.deform, bone.world) {
                    bounds.extend(v);
                }
            }
            AttachmentKind::BoundingBox(b) => {
                for v in deform_positions(pose, &b.vertices, &slot_pose.deform, bone.world) {
                    bounds.extend(v);
                }
            }
            _ => {}
        }
    }
    if bounds.is_empty() {
        // Nothing drawable: fall back to where the bones are, so the viewer can
        // still frame the model.
        return pose.bone_bounds();
    }
    bounds
}

fn region_corners(r: &RegionAttachment, region: &AtlasRegion, bone: Affine2) -> [Vec2; 4] {
    let (ow, oh) = (region.original_size.0.max(1) as f32, region.original_size.1.max(1) as f32);
    let scale_x = r.size.x / ow * r.scale.x;
    let scale_y = r.size.y / oh * r.scale.y;
    let (pw, ph) = region.unrotated_size();
    let local_x = -r.size.x / 2.0 * r.scale.x + region.offset.0 as f32 * scale_x;
    let local_y = -r.size.y / 2.0 * r.scale.y + region.offset.1 as f32 * scale_y;
    let local_x2 = local_x + pw as f32 * scale_x;
    let local_y2 = local_y + ph as f32 * scale_y;
    let (sin, cos) = r.rotation.to_radians().sin_cos();
    let corner = |lx: f32, ly: f32| {
        bone.transform_point(Vec2::new(lx * cos - ly * sin + r.position.x, lx * sin + ly * cos + r.position.y))
    };
    [corner(local_x, local_y), corner(local_x2, local_y), corner(local_x2, local_y2), corner(local_x, local_y2)]
}

/// True when `point` is inside the polygon, by the even-odd rule.
pub fn point_in_polygon(polygon: &[Vec2], point: Vec2) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.y > point.y) != (b.y > point.y) {
            let dy = b.y - a.y;
            if dy != 0.0 && point.x < (b.x - a.x) * (point.y - a.y) / dy + a.x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// World-space polygon of a slot's bounding-box attachment, if it has one.
pub fn bounding_box_polygon(pose: &SkeletonPose, slot: a2d_core::ir::ids::SlotId) -> Option<Vec<Vec2>> {
    let ir = pose.ir();
    let slot_pose = pose.slots.get(slot.index())?;
    let slot_data = ir.slot(slot)?;
    let attachment = ir.attachment(slot_pose.attachment?)?;
    let AttachmentKind::BoundingBox(b) = &attachment.kind else { return None };
    let bone = pose.bones.get(slot_data.bone.index())?;
    Some(deform_positions(pose, &b.vertices, &slot_pose.deform, bone.world))
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::atlas::{AtlasPage, AtlasRegion};
    use a2d_core::ir::ids::{AtlasPageId, AttachmentId, BoneId, SlotId};
    use a2d_core::ir::spine::{
        Attachment, Bone, BoneLocal, BoundingBoxAttachment, ClippingAttachment, Skin, SkinEntry, Slot, VertexInfluence,
        WeightedVertices,
    };
    use std::sync::Arc;

    /// Trig-derived coordinates carry the same rounding the reference runtime
    /// has, so geometry is compared with a tolerance rather than exactly.
    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    fn assert_close2(a: Vec2, b: Vec2) {
        assert!(close(a, b), "{a:?} != {b:?}");
    }

    fn assert_all_close(a: &[Vec2], b: &[Vec2]) {
        assert_eq!(a.len(), b.len(), "{a:?} != {b:?}");
        assert!(a.iter().zip(b).all(|(x, y)| close(*x, *y)), "{a:?} != {b:?}");
    }

    fn region(name: &str, xy: (u32, u32), size: (u32, u32), rotate: u16) -> AtlasRegion {
        AtlasRegion {
            name: name.into(),
            page: AtlasPageId(0),
            xy,
            size,
            rotate_deg: rotate,
            offset: (0, 0),
            original_size: if rotate == 90 || rotate == 270 { (size.1, size.0) } else { size },
            index: -1,
            splits: None,
            pads: None,
        }
    }

    fn atlas(regions: Vec<AtlasRegion>) -> Atlas {
        let mut a = Atlas { pages: vec![AtlasPage { size: Some((100, 100)), ..AtlasPage::new("p.png") }], regions };
        a.sort_regions();
        a
    }

    fn region_attachment(size: Vec2) -> AttachmentKind {
        AttachmentKind::Region(RegionAttachment {
            path: "r".into(),
            region: Some(AtlasRegionId(0)),
            position: Vec2::ZERO,
            rotation: 0.0,
            scale: Vec2::ONE,
            size,
            color: Rgba::WHITE,
            sequence: None,
        })
    }

    fn build(attachment: AttachmentKind, atlas: Atlas) -> SkeletonPose {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            slots: vec![Slot { setup_attachment: Some("r".into()), ..Slot::new("body", BoneId(0)) }],
            skins: vec![Skin::new("default")],
            attachments: vec![Attachment { name: "r".into(), kind: attachment }],
            atlas,
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "r".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        SkeletonPose::new(Arc::new(ir))
    }

    #[test]
    fn a_region_attachment_emits_a_centred_quad() {
        let pose = build(region_attachment(Vec2::new(10.0, 20.0)), atlas(vec![region("r", (0, 0), (10, 20), 0)]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_eq!(list.meshes().len(), 1);
        let mesh = &list.meshes()[0];
        assert!(mesh.is_well_formed());
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, vec![0, 1, 2, 2, 3, 0]);
        // The quad is centred on the bone.
        let b = mesh.bounds();
        assert_close2(b.min, Vec2::new(-5.0, -10.0));
        assert_close2(b.max, Vec2::new(5.0, 10.0));
    }

    #[test]
    fn a_region_quad_follows_its_bone() {
        let mut pose = build(region_attachment(Vec2::new(10.0, 10.0)), atlas(vec![region("r", (0, 0), (10, 10), 0)]));
        pose.bones[0].local = BoneLocal { position: Vec2::new(100.0, 50.0), ..BoneLocal::default() };
        pose.update_world_transforms();
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_close2(list.meshes()[0].bounds().center(), Vec2::new(100.0, 50.0));
    }

    #[test]
    fn region_uvs_come_from_the_atlas() {
        let pose = build(region_attachment(Vec2::new(10.0, 10.0)), atlas(vec![region("r", (10, 20), (30, 40), 0)]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        let uvs = &list.meshes()[0].uvs;
        // x: 10..40 of 100, y: 20..60 of 100.
        assert_eq!(uvs[0], Vec2::new(0.10, 0.60), "bottom-left");
        assert_eq!(uvs[2], Vec2::new(0.40, 0.20), "top-right");
    }

    #[test]
    fn the_texture_id_is_the_atlas_page_index() {
        let pose = build(region_attachment(Vec2::new(10.0, 10.0)), atlas(vec![region("r", (0, 0), (10, 10), 0)]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_eq!(list.meshes()[0].texture, TextureId(0));
    }

    #[test]
    fn an_unresolved_region_emits_nothing_rather_than_a_broken_quad() {
        let mut kind = region_attachment(Vec2::new(10.0, 10.0));
        if let AttachmentKind::Region(r) = &mut kind {
            r.region = None;
        }
        let pose = build(kind, atlas(vec![]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert!(list.is_empty());
    }

    fn mesh_attachment(vertices: VertexData, uvs: Vec<Vec2>) -> AttachmentKind {
        AttachmentKind::Mesh(MeshAttachment {
            path: "r".into(),
            region: Some(AtlasRegionId(0)),
            uvs,
            triangles: vec![0, 1, 2],
            vertices,
            hull_length: 3,
            edges: vec![],
            size: Vec2::new(10.0, 10.0),
            color: Rgba::WHITE,
            linked_to: None,
            sequence: None,
        })
    }

    #[test]
    fn a_rigid_mesh_emits_its_vertices_in_bone_space() {
        let kind = mesh_attachment(
            VertexData::Rigid(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)]),
            vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        );
        let mut pose = build(kind, atlas(vec![region("r", (0, 0), (100, 100), 0)]));
        pose.bones[0].local = BoneLocal { position: Vec2::new(5.0, 5.0), ..BoneLocal::default() };
        pose.update_world_transforms();
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_all_close(
            &list.meshes()[0].vertices,
            &[Vec2::new(5.0, 5.0), Vec2::new(15.0, 5.0), Vec2::new(5.0, 15.0)],
        );
    }

    #[test]
    fn deform_offsets_move_rigid_vertices() {
        let kind = mesh_attachment(
            VertexData::Rigid(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)]),
            vec![Vec2::ZERO; 3],
        );
        let mut pose = build(kind, atlas(vec![region("r", (0, 0), (100, 100), 0)]));
        pose.slots[0].deform = vec![1.0, 2.0, 0.0, 0.0, 0.0, 0.0];
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_close2(list.meshes()[0].vertices[0], Vec2::new(1.0, 2.0));
        assert_close2(list.meshes()[0].vertices[1], Vec2::new(10.0, 0.0));
    }

    /// Two bones 100 apart, one vertex weighted half to each.
    fn weighted_pose() -> SkeletonPose {
        let weighted = VertexData::Weighted(WeightedVertices {
            offsets: vec![0, 2, 3, 4],
            influences: vec![
                VertexInfluence { bone: BoneId(0), position: Vec2::ZERO, weight: 0.5 },
                VertexInfluence { bone: BoneId(1), position: Vec2::ZERO, weight: 0.5 },
                VertexInfluence { bone: BoneId(0), position: Vec2::ZERO, weight: 1.0 },
                VertexInfluence { bone: BoneId(1), position: Vec2::ZERO, weight: 1.0 },
            ],
        });
        let kind = mesh_attachment(weighted, vec![Vec2::ZERO; 3]);
        let mut ir = SpineIr {
            bones: vec![
                Bone::new("root", None),
                Bone {
                    setup: BoneLocal { position: Vec2::new(100.0, 0.0), ..BoneLocal::default() },
                    ..Bone::new("b", Some(BoneId(0)))
                },
            ],
            slots: vec![Slot { setup_attachment: Some("r".into()), ..Slot::new("body", BoneId(0)) }],
            skins: vec![Skin::new("default")],
            attachments: vec![Attachment { name: "r".into(), kind }],
            atlas: atlas(vec![region("r", (0, 0), (100, 100), 0)]),
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "r".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        SkeletonPose::new(Arc::new(ir))
    }

    #[test]
    fn a_weighted_vertex_lands_between_its_bones() {
        let pose = weighted_pose();
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        let v = &list.meshes()[0].vertices;
        assert_close2(v[0], Vec2::new(50.0, 0.0));
        assert_close2(v[1], Vec2::ZERO);
        assert_close2(v[2], Vec2::new(100.0, 0.0));
    }

    #[test]
    fn a_weighted_vertex_follows_a_moving_bone() {
        let mut pose = weighted_pose();
        pose.bones[1].local.position = Vec2::new(200.0, 0.0);
        pose.update_world_transforms();
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_close2(list.meshes()[0].vertices[0], Vec2::new(100.0, 0.0));
    }

    #[test]
    fn deform_offsets_apply_per_influence_for_weighted_meshes() {
        let mut pose = weighted_pose();
        // Offsets address influences, not vertices: two floats per influence.
        pose.slots[0].deform = vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        // The first influence is weighted 0.5, so a 10-unit offset moves the
        // vertex by 5.
        assert_close2(list.meshes()[0].vertices[0], Vec2::new(55.0, 0.0));
    }

    #[test]
    fn slot_and_attachment_tints_multiply_with_the_model_alpha() {
        let mut kind = region_attachment(Vec2::new(10.0, 10.0));
        if let AttachmentKind::Region(r) = &mut kind {
            r.color = Rgba::new(0.5, 1.0, 1.0, 1.0);
        }
        let mut pose = build(kind, atlas(vec![region("r", (0, 0), (10, 10), 0)]));
        pose.slots[0].color = Rgba::new(1.0, 0.5, 1.0, 0.8);
        let mut list = RenderList::new();
        emit(&pose, 0.5, &mut list);
        let c = list.meshes()[0].color;
        assert_eq!(c.r, 0.5);
        assert_eq!(c.g, 0.5);
        assert!((c.a - 0.4).abs() < 1e-6);
    }

    #[test]
    fn draw_order_becomes_the_z_order() {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            slots: vec![
                Slot { setup_attachment: Some("r".into()), ..Slot::new("a", BoneId(0)) },
                Slot { setup_attachment: Some("r".into()), ..Slot::new("b", BoneId(0)) },
            ],
            skins: vec![Skin::new("default")],
            attachments: vec![Attachment { name: "r".into(), kind: region_attachment(Vec2::new(10.0, 10.0)) }],
            atlas: atlas(vec![region("r", (0, 0), (10, 10), 0)]),
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "r".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "r".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        let mut pose = SkeletonPose::new(Arc::new(ir));
        pose.draw_order = vec![SlotId(1), SlotId(0)];
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_eq!(list.meshes()[0].z_order, 0);
        assert_eq!(list.meshes()[1].z_order, 1);
    }

    #[test]
    fn a_clipping_attachment_registers_a_mask_for_the_slots_it_covers() {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            slots: vec![
                Slot { setup_attachment: Some("clip".into()), ..Slot::new("clipper", BoneId(0)) },
                Slot { setup_attachment: Some("r".into()), ..Slot::new("inside", BoneId(0)) },
                Slot { setup_attachment: Some("r".into()), ..Slot::new("outside", BoneId(0)) },
            ],
            skins: vec![Skin::new("default")],
            attachments: vec![
                Attachment {
                    name: "clip".into(),
                    kind: AttachmentKind::Clipping(ClippingAttachment {
                        end_slot: Some(SlotId(1)),
                        vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(5.0, 10.0)]),
                        color: Rgba::WHITE,
                    }),
                },
                Attachment { name: "r".into(), kind: region_attachment(Vec2::new(10.0, 10.0)) },
            ],
            atlas: atlas(vec![region("r", (0, 0), (10, 10), 0)]),
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "clip".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "r".into(), attachment: AttachmentId(1) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(2), name: "r".into(), attachment: AttachmentId(1) });
        ir.rebuild_derived();
        let pose = SkeletonPose::new(Arc::new(ir));

        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert_eq!(list.masks().len(), 1);
        assert_eq!(list.meshes().len(), 2);
        assert!(list.meshes()[0].clipping_mask.is_some(), "the slot inside the clip range is masked");
        assert!(list.meshes()[1].clipping_mask.is_none(), "the slot past the end slot is not");
    }

    #[test]
    fn bounds_cover_the_emitted_geometry() {
        let pose = build(region_attachment(Vec2::new(10.0, 20.0)), atlas(vec![region("r", (0, 0), (10, 20), 0)]));
        let b = pose_bounds(&pose);
        assert_close2(b.min, Vec2::new(-5.0, -10.0));
        assert_close2(b.max, Vec2::new(5.0, 10.0));
    }

    #[test]
    fn bounds_fall_back_to_the_bones_when_nothing_is_drawable() {
        let mut ir = SpineIr { bones: vec![Bone::new("root", None)], ..Default::default() };
        ir.bones[0].setup.position = Vec2::new(3.0, 4.0);
        ir.rebuild_derived();
        let pose = SkeletonPose::new(Arc::new(ir));
        assert_close2(pose_bounds(&pose).center(), Vec2::new(3.0, 4.0));
    }

    #[test]
    fn point_in_polygon_uses_the_even_odd_rule() {
        let square = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)];
        assert!(point_in_polygon(&square, Vec2::new(5.0, 5.0)));
        assert!(!point_in_polygon(&square, Vec2::new(15.0, 5.0)));
        assert!(!point_in_polygon(&square, Vec2::new(5.0, -1.0)));
    }

    #[test]
    fn point_in_polygon_rejects_degenerate_polygons() {
        assert!(!point_in_polygon(&[], Vec2::ZERO));
        assert!(!point_in_polygon(&[Vec2::ZERO, Vec2::ONE], Vec2::ZERO));
    }

    #[test]
    fn bounding_box_polygons_are_returned_in_world_space() {
        let kind = AttachmentKind::BoundingBox(BoundingBoxAttachment {
            vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)]),
            color: Rgba::WHITE,
        });
        let mut pose = build(kind, atlas(vec![]));
        pose.bones[0].local.position = Vec2::new(5.0, 5.0);
        pose.update_world_transforms();
        let polygon = bounding_box_polygon(&pose, SlotId(0)).expect("bounding box should be found");
        assert_close2(polygon[0], Vec2::new(5.0, 5.0));
        assert!(point_in_polygon(&polygon, Vec2::new(7.0, 7.0)));
    }

    #[test]
    fn a_bounding_box_emits_no_drawable_geometry() {
        let kind = AttachmentKind::BoundingBox(BoundingBoxAttachment {
            vertices: VertexData::Rigid(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)]),
            color: Rgba::WHITE,
        });
        let pose = build(kind, atlas(vec![]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        assert!(list.is_empty());
    }

    #[test]
    fn a_rotated_region_maps_uvs_consistently_with_its_corners() {
        let pose = build(region_attachment(Vec2::new(10.0, 20.0)), atlas(vec![region("r", (0, 0), (20, 10), 90)]));
        let mut list = RenderList::new();
        emit(&pose, 1.0, &mut list);
        let mesh = &list.meshes()[0];
        // A 90-degree packed region occupies 20x10 on the page but represents a
        // 10x20 image, so the emitted quad keeps the attachment's proportions.
        assert_close2(mesh.bounds().size(), Vec2::new(10.0, 20.0));
        assert_eq!(mesh.uvs.len(), 4);
        assert!(mesh.uvs.iter().all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));
    }
}
