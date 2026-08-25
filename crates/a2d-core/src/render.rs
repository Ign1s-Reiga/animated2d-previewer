//! Renderer-neutral draw primitives.
//!
//! This is the whole contract between `runtime/` and `renderer/`. Models emit
//! these; the renderer draws them. Nothing here may mention a source format or a
//! game — that is what keeps the renderer neutral (spec §2, §11).

use crate::math::{Aabb, Rgb, Rgba, Vec2};

/// Handle to a texture page owned by the renderer's texture cache.
///
/// The index is assigned when a package's texture pages are uploaded, in
/// manifest order, so it is stable for a given package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TextureId(pub u32);

/// Handle to a clipping mask registered in the current [`RenderList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaskId(pub u32);

/// Identifies an interactive region of a model, for [`AnimatedModel::hit_test`].
///
/// [`AnimatedModel::hit_test`]: crate::model::AnimatedModel::hit_test
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HitAreaId(pub String);

/// Compositing modes both source ecosystems share.
///
/// Spine's `screen` and Cubism's multiply/additive drawable flags both land
/// here. A mode with no entry is reported as a degradation by the decoder and
/// falls back to [`BlendMode::Normal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Additive,
    Multiply,
    Screen,
}

impl BlendMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Additive => "additive",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
        }
    }
}

/// One textured triangle mesh ready to draw.
#[derive(Debug, Clone, Default)]
pub struct RenderMesh {
    /// World-space positions, already deformed and skinned.
    pub vertices: Vec<Vec2>,
    /// Texture coordinates, one per vertex, already mapped into atlas page space.
    pub uvs: Vec<Vec2>,
    /// Triangle list indices into `vertices`.
    pub indices: Vec<u16>,
    pub texture: TextureId,
    /// Non-premultiplied tint: slot colour times model alpha.
    pub color: Rgba,
    /// Spine two-colour tint. `None` means single-colour tinting.
    pub dark_color: Option<Rgb>,
    pub blend_mode: BlendMode,
    pub clipping_mask: Option<MaskId>,
    /// Painter-order index; lower draws first.
    pub z_order: u32,
}

impl RenderMesh {
    /// Drops the geometry but keeps the allocations, for reuse across frames.
    fn reset(&mut self) {
        self.vertices.clear();
        self.uvs.clear();
        self.indices.clear();
        self.texture = TextureId(0);
        self.color = Rgba::WHITE;
        self.dark_color = None;
        self.blend_mode = BlendMode::Normal;
        self.clipping_mask = None;
        self.z_order = 0;
    }

    /// True when the mesh has consistent array lengths and in-range indices.
    ///
    /// Used by tests and `validate`; the hot path trusts the emitter.
    pub fn is_well_formed(&self) -> bool {
        if self.vertices.len() != self.uvs.len() {
            return false;
        }
        if self.indices.len() % 3 != 0 {
            return false;
        }
        let n = self.vertices.len();
        self.indices.iter().all(|&i| (i as usize) < n)
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.extend(*v);
        }
        b
    }
}

/// A clipping shape in world space, as triangles.
///
/// Triangles rather than an outline because the two source families disagree:
/// a Spine clipping attachment is a polygon, while a Cubism mask is an ordinary
/// drawable's mesh. Triangles hold both -- a polygon fans into them -- and the
/// renderer stays neutral between the two, which is the whole point of this
/// layer.
#[derive(Debug, Clone, Default)]
pub struct RenderMask {
    pub vertices: Vec<Vec2>,
    pub indices: Vec<u32>,
}

impl RenderMask {
    fn reset(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }

    /// Adds a polygon as a fan from its first vertex.
    ///
    /// The mask pipeline inverts stencil rather than writing it, so overlapping
    /// fan triangles cancel and a concave outline still fills correctly without
    /// being triangulated properly.
    pub fn push_polygon(&mut self, polygon: &[Vec2]) {
        if polygon.len() < 3 {
            return;
        }
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(polygon);
        for i in 1..polygon.len() as u32 - 1 {
            self.indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    }

    /// Adds a triangle mesh, with `indices` local to `vertices`.
    ///
    /// Beware that the stencil is even-odd, so two shapes added to the *same*
    /// mask cancel where they overlap instead of uniting. Disjoint shapes --
    /// which is what a model's masks normally are -- combine correctly.
    pub fn push_mesh(&mut self, vertices: &[Vec2], indices: &[u16]) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(vertices);
        self.indices.extend(indices.iter().map(|i| base + *i as u32));
    }
}

/// One frame's worth of draw primitives.
///
/// The list recycles its [`RenderMesh`] allocations between frames, so emitting
/// every frame does not churn the allocator even though the public shape is a
/// plain `Vec<Vec2>` per mesh.
#[derive(Debug, Default)]
pub struct RenderList {
    meshes: Vec<RenderMesh>,
    masks: Vec<RenderMask>,
    /// Meshes and masks retired by `clear`, kept for their capacity.
    mesh_pool: Vec<RenderMesh>,
    mask_pool: Vec<RenderMask>,
}

impl RenderList {
    pub fn new() -> Self {
        RenderList::default()
    }

    /// Empties the list, retaining allocations.
    pub fn clear(&mut self) {
        for mut m in self.meshes.drain(..) {
            m.reset();
            self.mesh_pool.push(m);
        }
        for mut m in self.masks.drain(..) {
            m.reset();
            self.mask_pool.push(m);
        }
    }

    /// Takes a recycled mesh to fill in. Commit it with [`RenderList::push_mesh`].
    pub fn take_mesh(&mut self) -> RenderMesh {
        self.mesh_pool.pop().unwrap_or_default()
    }

    pub fn push_mesh(&mut self, mesh: RenderMesh) {
        debug_assert!(mesh.is_well_formed(), "emitter produced a malformed mesh");
        self.meshes.push(mesh);
    }

    /// Registers a clipping polygon and returns the handle to reference it with.
    pub fn push_mask(&mut self, polygon: Vec<Vec2>) -> MaskId {
        let mut mask = self.take_mask();
        mask.push_polygon(&polygon);
        self.push_mask_shape(mask)
    }

    /// Takes a recycled mask to fill in. Commit it with
    /// [`RenderList::push_mask_shape`].
    pub fn take_mask(&mut self) -> RenderMask {
        let mut mask = self.mask_pool.pop().unwrap_or_default();
        mask.reset();
        mask
    }

    /// Registers a mask built by hand and returns its handle.
    pub fn push_mask_shape(&mut self, mask: RenderMask) -> MaskId {
        let id = MaskId(self.masks.len() as u32);
        self.masks.push(mask);
        id
    }

    pub fn meshes(&self) -> &[RenderMesh] {
        &self.meshes
    }

    pub fn masks(&self) -> &[RenderMask] {
        &self.masks
    }

    pub fn mask(&self, id: MaskId) -> Option<&RenderMask> {
        self.masks.get(id.0 as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    /// Sorts meshes by `z_order`, stably, so equal orders keep emission order.
    pub fn sort_by_z(&mut self) {
        self.meshes.sort_by_key(|m| m.z_order);
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for m in &self.meshes {
            b.union(&m.bounds());
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(z: u32) -> RenderMesh {
        RenderMesh {
            vertices: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)],
            uvs: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)],
            indices: vec![0, 1, 2, 2, 3, 0],
            z_order: z,
            ..Default::default()
        }
    }

    #[test]
    fn a_well_formed_quad_validates() {
        assert!(quad(0).is_well_formed());
    }

    #[test]
    fn mismatched_uv_count_is_not_well_formed() {
        let mut m = quad(0);
        m.uvs.pop();
        assert!(!m.is_well_formed());
    }

    #[test]
    fn out_of_range_index_is_not_well_formed() {
        let mut m = quad(0);
        m.indices[0] = 99;
        assert!(!m.is_well_formed());
    }

    #[test]
    fn partial_triangle_is_not_well_formed() {
        let mut m = quad(0);
        m.indices.pop();
        assert!(!m.is_well_formed());
    }

    #[test]
    fn clear_recycles_allocations() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0));
        let cap_before = list.meshes()[0].vertices.capacity();
        list.clear();
        assert!(list.is_empty());
        let recycled = list.take_mesh();
        assert_eq!(recycled.vertices.capacity(), cap_before);
        assert!(recycled.vertices.is_empty());
    }

    #[test]
    fn recycled_mesh_carries_no_stale_state() {
        let mut list = RenderList::new();
        let mut m = quad(0);
        m.z_order = 7;
        m.blend_mode = BlendMode::Additive;
        m.dark_color = Some(Rgb::BLACK);
        list.push_mesh(m);
        list.clear();
        let recycled = list.take_mesh();
        assert_eq!(recycled.z_order, 0);
        assert_eq!(recycled.blend_mode, BlendMode::Normal);
        assert_eq!(recycled.dark_color, None);
        assert_eq!(recycled.color, Rgba::WHITE);
    }

    #[test]
    fn sort_by_z_is_stable_for_equal_orders() {
        let mut list = RenderList::new();
        let mut first = quad(5);
        first.texture = TextureId(1);
        let mut second = quad(5);
        second.texture = TextureId(2);
        list.push_mesh(quad(9));
        list.push_mesh(first);
        list.push_mesh(second);
        list.sort_by_z();
        assert_eq!(list.meshes()[0].texture, TextureId(1));
        assert_eq!(list.meshes()[1].texture, TextureId(2));
        assert_eq!(list.meshes()[2].z_order, 9);
    }

    #[test]
    fn mask_handles_resolve_to_their_polygon() {
        let mut list = RenderList::new();
        let id = list.push_mask(vec![Vec2::ZERO, Vec2::ONE, Vec2::new(0.0, 1.0)]);
        assert_eq!(list.mask(id).unwrap().vertices.len(), 3);
        assert_eq!(list.mask(id).unwrap().indices, [0, 1, 2]);
        assert!(list.mask(MaskId(99)).is_none());
    }

    #[test]
    fn list_bounds_union_every_mesh() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0));
        let mut far = quad(0);
        far.vertices = vec![Vec2::new(10.0, 10.0), Vec2::new(11.0, 10.0), Vec2::new(10.0, 11.0)];
        far.uvs = vec![Vec2::ZERO; 3];
        far.indices = vec![0, 1, 2];
        list.push_mesh(far);
        let b = list.bounds();
        assert_eq!(b.min, Vec2::ZERO);
        assert_eq!(b.max, Vec2::new(11.0, 11.0));
    }
}
