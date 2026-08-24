//! Attachment types.
//!
//! Priority order per spec §6.3: region, mesh, weighted mesh, clipping,
//! bounding box; point and path afterwards. All six are represented here so a
//! model containing them still *loads* — the runtime decides what it can draw
//! and reports the rest (spec §6.5, §16).

use crate::ir::ids::{AtlasRegionId, AttachmentId, BoneId, SlotId};
use crate::math::{Rgba, Vec2};

/// Vertex positions, either rigid or bound to bones.
///
/// Both cases are stored so that a deform timeline can address them uniformly:
/// see [`VertexData::deform_len`].
#[derive(Debug, Clone, PartialEq)]
pub enum VertexData {
    /// Positions in the slot bone's local space, one per vertex.
    Rigid(Vec<Vec2>),
    /// Positions expressed as weighted offsets from one or more bones.
    Weighted(WeightedVertices),
}

impl VertexData {
    pub fn vertex_count(&self) -> usize {
        match self {
            VertexData::Rigid(v) => v.len(),
            VertexData::Weighted(w) => w.vertex_count(),
        }
    }

    /// Number of floats a deform keyframe for this attachment must supply for a
    /// full-length key.
    ///
    /// Rigid meshes deform their vertices; weighted meshes deform each *bone
    /// influence* instead, which is why the two lengths differ. Getting this
    /// wrong silently corrupts deformation, so decoders assert against it.
    pub fn deform_len(&self) -> usize {
        match self {
            VertexData::Rigid(v) => v.len() * 2,
            VertexData::Weighted(w) => w.influences.len() * 2,
        }
    }

    pub fn is_weighted(&self) -> bool {
        matches!(self, VertexData::Weighted(_))
    }
}

/// One bone's pull on one vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VertexInfluence {
    pub bone: BoneId,
    /// Position in that bone's local space.
    pub position: Vec2,
    pub weight: f32,
}

/// Vertices bound to bones, in a flattened CSR-style layout.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedVertices {
    /// `vertex_count + 1` entries. Vertex `i` owns
    /// `influences[offsets[i]..offsets[i + 1]]`.
    pub offsets: Vec<u32>,
    pub influences: Vec<VertexInfluence>,
}

impl WeightedVertices {
    pub fn vertex_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn influences_for(&self, vertex: usize) -> &[VertexInfluence] {
        let (Some(&from), Some(&to)) = (self.offsets.get(vertex), self.offsets.get(vertex + 1)) else {
            return &[];
        };
        let (from, to) = (from as usize, to as usize);
        if from > to || to > self.influences.len() {
            return &[];
        }
        &self.influences[from..to]
    }

    /// Checks the CSR invariants. Decoders call this before handing data on.
    pub fn is_well_formed(&self) -> bool {
        if self.offsets.first() != Some(&0) {
            return false;
        }
        if self.offsets.last() != Some(&(self.influences.len() as u32)) {
            return false;
        }
        self.offsets.windows(2).all(|w| w[0] <= w[1])
    }
}

/// A single quad mapped to an atlas region.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionAttachment {
    /// Region name in the atlas. Defaults to the attachment name when the
    /// source omits an explicit path.
    pub path: String,
    /// Resolved atlas region, or `None` when the atlas had no such region. That
    /// is a reported degradation, not a load failure.
    pub region: Option<AtlasRegionId>,
    /// Offset from the slot bone, in bone-local space.
    pub position: Vec2,
    /// Degrees, counter-clockwise.
    pub rotation: f32,
    pub scale: Vec2,
    /// Untrimmed size the attachment should occupy.
    pub size: Vec2,
    pub color: Rgba,
    /// Sequence animation data, when the region is one frame of many.
    pub sequence: Option<Sequence>,
}

/// A deformable triangle mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshAttachment {
    pub path: String,
    pub region: Option<AtlasRegionId>,
    /// Texture coordinates in the attachment's own `0..1` space, before being
    /// mapped onto the atlas region.
    pub uvs: Vec<Vec2>,
    pub triangles: Vec<u16>,
    pub vertices: VertexData,
    /// Number of vertices forming the convex hull, for clipping and culling.
    pub hull_length: u32,
    /// Edge list kept for debug visualisation only.
    pub edges: Vec<u16>,
    pub size: Vec2,
    pub color: Rgba,
    /// Set when this mesh is a *linked* mesh: it borrows geometry from another
    /// attachment and supplies only its own colour, path and deform timeline.
    pub linked_to: Option<LinkedMesh>,
    pub sequence: Option<Sequence>,
}

/// A linked mesh's reference to the mesh it inherits geometry from.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkedMesh {
    /// The skin holding the source mesh, by name; `None` means the default skin.
    pub skin: Option<String>,
    pub slot: SlotId,
    /// Placeholder name of the source attachment.
    pub parent: String,
    /// Whether this mesh follows the parent's deform timelines.
    pub inherit_timelines: bool,
    /// Filled in during normalisation once the parent has been resolved.
    pub resolved: Option<AttachmentId>,
}

/// A polygon that clips everything drawn between its slot and `end_slot`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClippingAttachment {
    /// Clipping ends *after* this slot is drawn. `None` means it clips to the
    /// end of the draw order.
    pub end_slot: Option<SlotId>,
    pub vertices: VertexData,
    /// Debug-draw colour; not used for rendering the clip itself.
    pub color: Rgba,
}

/// A polygon used for hit testing, never drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundingBoxAttachment {
    pub vertices: VertexData,
    pub color: Rgba,
}

/// A single positioned, oriented point. Used for spawn locations and aiming.
#[derive(Debug, Clone, PartialEq)]
pub struct PointAttachment {
    pub position: Vec2,
    /// Degrees, counter-clockwise.
    pub rotation: f32,
    pub color: Rgba,
}

/// A spline that path constraints follow.
#[derive(Debug, Clone, PartialEq)]
pub struct PathAttachment {
    pub closed: bool,
    pub constant_speed: bool,
    /// Cumulative length at the end of each curve segment.
    pub lengths: Vec<f32>,
    pub vertices: VertexData,
    pub color: Rgba,
}

/// Frame-sequence playback for region and mesh attachments (Spine 4.1+).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sequence {
    pub count: u32,
    pub start: u32,
    pub digits: u32,
    pub setup_index: u32,
}

/// Any attachment, plus the name it was authored under.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// The attachment's own name. The name it is *bound* under lives on
    /// [`SkinEntry`](super::skeleton::SkinEntry) and may differ.
    pub name: String,
    pub kind: AttachmentKind,
}

/// The attachment payload.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentKind {
    Region(RegionAttachment),
    Mesh(MeshAttachment),
    Clipping(ClippingAttachment),
    BoundingBox(BoundingBoxAttachment),
    Point(PointAttachment),
    Path(PathAttachment),
}

impl AttachmentKind {
    /// Stable name used in reports and in `inspect` output.
    pub fn type_name(&self) -> &'static str {
        match self {
            AttachmentKind::Region(_) => "region",
            AttachmentKind::Mesh(m) if m.vertices.is_weighted() => "weighted mesh",
            AttachmentKind::Mesh(_) => "mesh",
            AttachmentKind::Clipping(_) => "clipping",
            AttachmentKind::BoundingBox(_) => "bounding box",
            AttachmentKind::Point(_) => "point",
            AttachmentKind::Path(_) => "path",
        }
    }

    /// Vertex data a deform timeline can target, when the type has any.
    pub fn deformable_vertices(&self) -> Option<&VertexData> {
        match self {
            AttachmentKind::Mesh(m) => Some(&m.vertices),
            AttachmentKind::Clipping(c) => Some(&c.vertices),
            AttachmentKind::BoundingBox(b) => Some(&b.vertices),
            AttachmentKind::Path(p) => Some(&p.vertices),
            AttachmentKind::Region(_) | AttachmentKind::Point(_) => None,
        }
    }

    /// Atlas region this attachment samples, when it samples one.
    pub fn region(&self) -> Option<AtlasRegionId> {
        match self {
            AttachmentKind::Region(r) => r.region,
            AttachmentKind::Mesh(m) => m.region,
            _ => None,
        }
    }

    /// Region name the attachment asked for, for missing-region reports.
    pub fn region_path(&self) -> Option<&str> {
        match self {
            AttachmentKind::Region(r) => Some(&r.path),
            AttachmentKind::Mesh(m) => Some(&m.path),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weighted() -> WeightedVertices {
        WeightedVertices {
            offsets: vec![0, 2, 3],
            influences: vec![
                VertexInfluence { bone: BoneId(0), position: Vec2::new(1.0, 0.0), weight: 0.5 },
                VertexInfluence { bone: BoneId(1), position: Vec2::new(0.0, 1.0), weight: 0.5 },
                VertexInfluence { bone: BoneId(1), position: Vec2::new(2.0, 2.0), weight: 1.0 },
            ],
        }
    }

    #[test]
    fn rigid_deform_length_is_two_floats_per_vertex() {
        let v = VertexData::Rigid(vec![Vec2::ZERO; 4]);
        assert_eq!(v.vertex_count(), 4);
        assert_eq!(v.deform_len(), 8);
    }

    #[test]
    fn weighted_deform_length_is_two_floats_per_influence_not_per_vertex() {
        let v = VertexData::Weighted(weighted());
        assert_eq!(v.vertex_count(), 2);
        assert_eq!(v.deform_len(), 6);
    }

    #[test]
    fn influences_are_sliced_per_vertex() {
        let w = weighted();
        assert_eq!(w.influences_for(0).len(), 2);
        assert_eq!(w.influences_for(1).len(), 1);
        assert_eq!(w.influences_for(1)[0].bone, BoneId(1));
    }

    #[test]
    fn out_of_range_vertex_yields_no_influences_rather_than_panicking() {
        let w = weighted();
        assert!(w.influences_for(9).is_empty());
    }

    #[test]
    fn csr_invariants_are_checked() {
        assert!(weighted().is_well_formed());

        let mut bad_start = weighted();
        bad_start.offsets[0] = 1;
        assert!(!bad_start.is_well_formed());

        let mut bad_end = weighted();
        bad_end.offsets.pop();
        assert!(!bad_end.is_well_formed());

        let mut non_monotone = weighted();
        non_monotone.offsets = vec![0, 3, 2];
        assert!(!non_monotone.is_well_formed());
    }

    #[test]
    fn malformed_offsets_do_not_panic_when_sliced() {
        let w = WeightedVertices { offsets: vec![0, 99], influences: vec![] };
        assert!(w.influences_for(0).is_empty());
    }

    fn mesh(vertices: VertexData) -> AttachmentKind {
        AttachmentKind::Mesh(MeshAttachment {
            path: "body".into(),
            region: None,
            uvs: vec![],
            triangles: vec![],
            vertices,
            hull_length: 0,
            edges: vec![],
            size: Vec2::ZERO,
            color: Rgba::WHITE,
            linked_to: None,
            sequence: None,
        })
    }

    #[test]
    fn type_name_distinguishes_weighted_meshes() {
        assert_eq!(mesh(VertexData::Rigid(vec![])).type_name(), "mesh");
        assert_eq!(mesh(VertexData::Weighted(weighted())).type_name(), "weighted mesh");
    }

    #[test]
    fn only_deformable_types_expose_vertices() {
        assert!(mesh(VertexData::Rigid(vec![])).deformable_vertices().is_some());
        let point = AttachmentKind::Point(PointAttachment { position: Vec2::ZERO, rotation: 0.0, color: Rgba::WHITE });
        assert!(point.deformable_vertices().is_none());
        assert!(point.region_path().is_none());
    }

    #[test]
    fn textured_types_expose_their_region_path() {
        assert_eq!(mesh(VertexData::Rigid(vec![])).region_path(), Some("body"));
    }
}
