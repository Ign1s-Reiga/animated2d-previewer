//! Turning a [`RenderList`] into GPU geometry and draw batches.
//!
//! This is the part of the renderer with no GPU in it, which is deliberate:
//! batching decisions and vertex layout are where correctness bugs actually
//! live, and here they can be tested on any machine.

use std::ops::Range;

use a2d_core::{BlendMode, MaskId, RenderList, RenderMesh, Rgba, TextureId, Vec2};

/// One vertex as the shader consumes it.
///
/// Tint travels per vertex rather than per draw call so that meshes differing
/// only in colour still batch together — which is the common case, since every
/// slot of a character carries its own tint.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub light: [f32; 4],
    /// Spine's second tint colour. All zeroes means "no dark tint", which the
    /// shader's formula handles without a branch.
    pub dark: [f32; 4],
}

impl Vertex {
    pub const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }

    fn new(position: Vec2, uv: Vec2, light: Rgba, dark: Option<a2d_core::Rgb>) -> Vertex {
        Vertex {
            position: [position.x, position.y],
            uv: [uv.x, uv.y],
            light: light.to_array(),
            // The alpha term carries the two-colour weight; without a dark
            // colour it stays zero and the formula collapses to a plain tint.
            dark: dark.map(|d| [d.r, d.g, d.b, 1.0]).unwrap_or([0.0; 4]),
        }
    }

    /// A position-only vertex, for mask outlines. The shader ignores the rest.
    fn position_only(position: Vec2) -> Vertex {
        Vertex { position: [position.x, position.y], uv: [0.0; 2], light: [0.0; 4], dark: [0.0; 4] }
    }
}

/// A run of geometry that can be drawn with one call.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawBatch {
    pub texture: TextureId,
    pub blend_mode: BlendMode,
    /// Clipping mask in force for this batch, if any.
    pub mask: Option<MaskId>,
    /// Range into [`FrameGeometry::indices`].
    pub indices: Range<u32>,
}

/// A clipping outline, stored as geometry alongside the meshes.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskShape {
    pub id: MaskId,
    pub indices: Range<u32>,
}

/// One frame's geometry, ready to upload.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FrameGeometry {
    pub vertices: Vec<Vertex>,
    /// 32-bit because a character's meshes combined routinely exceed the 65k a
    /// 16-bit index could address, even though each source mesh is 16-bit.
    pub indices: Vec<u32>,
    pub batches: Vec<DrawBatch>,
    /// Mask outlines, in the order the render list declared them.
    pub masks: Vec<MaskShape>,
    /// Meshes dropped for being malformed. Non-zero means an emitter bug.
    pub skipped_meshes: usize,
}

impl FrameGeometry {
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Total triangles across every batch.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.batches.clear();
        self.masks.clear();
        self.skipped_meshes = 0;
    }
}

/// Builds frame geometry from a render list, reusing `out`'s allocations.
///
/// Meshes are drawn in `z_order`, stably, so an emitter that pushed out of
/// order still composites correctly. Consecutive meshes sharing a texture,
/// blend mode and mask merge into one batch; that adjacency requirement is not
/// an optimisation detail but a correctness one, since a painter's-algorithm
/// renderer cannot reorder across a differing draw.
pub fn build(list: &RenderList, out: &mut FrameGeometry) {
    out.clear();

    // Masks first, so their geometry is in the buffer before any batch that
    // references one, and so mask ids index `out.masks` directly.
    for (i, mask) in list.masks().iter().enumerate() {
        let start = out.indices.len() as u32;
        append_mask(&mask.polygon, out);
        let end = out.indices.len() as u32;
        if end > start {
            out.masks.push(MaskShape { id: MaskId(i as u32), indices: start..end });
        }
    }

    let mut order: Vec<usize> = (0..list.meshes().len()).collect();
    order.sort_by_key(|i| list.meshes()[*i].z_order);

    for i in order {
        let mesh = &list.meshes()[i];
        if mesh.indices.is_empty() || mesh.vertices.is_empty() {
            continue;
        }
        if !mesh.is_well_formed() {
            // Emitting this would read out of bounds on the GPU. Drop it and
            // count it, so the caller can surface an emitter bug.
            out.skipped_meshes += 1;
            continue;
        }

        let base = out.vertices.len() as u32;
        append_mesh(mesh, out);
        let end = out.indices.len() as u32;

        match out.batches.last_mut() {
            // Extend the batch in progress when nothing about the draw state
            // changed. `indices.end == base_of_this_mesh` holds by construction
            // because meshes are appended contiguously.
            Some(last)
                if last.texture == mesh.texture
                    && last.blend_mode == mesh.blend_mode
                    && last.mask == mesh.clipping_mask =>
            {
                last.indices.end = end;
            }
            _ => out.batches.push(DrawBatch {
                texture: mesh.texture,
                blend_mode: mesh.blend_mode,
                mask: mesh.clipping_mask,
                indices: (end - mesh.indices.len() as u32)..end,
            }),
        }
        debug_assert_eq!(base as usize + mesh.vertices.len(), out.vertices.len());
    }
}

fn append_mesh(mesh: &RenderMesh, out: &mut FrameGeometry) {
    let base = out.vertices.len() as u32;
    for (position, uv) in mesh.vertices.iter().zip(&mesh.uvs) {
        out.vertices.push(Vertex::new(*position, *uv, mesh.color, mesh.dark_color));
    }
    out.indices.extend(mesh.indices.iter().map(|i| base + *i as u32));
}

/// Appends a polygon as a fan from its first vertex.
///
/// The mask pipeline inverts stencil rather than writing it, so overlapping
/// fan triangles cancel and the result is an even-odd fill. That is what lets a
/// concave outline work without being triangulated properly.
fn append_mask(polygon: &[Vec2], out: &mut FrameGeometry) {
    if polygon.len() < 3 {
        return;
    }
    let base = out.vertices.len() as u32;
    for point in polygon {
        out.vertices.push(Vertex::position_only(*point));
    }
    for i in 1..polygon.len() as u32 - 1 {
        out.indices.extend_from_slice(&[base, base + i, base + i + 1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::Rgb;

    fn quad(texture: u32, z: u32) -> RenderMesh {
        RenderMesh {
            vertices: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)],
            uvs: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)],
            indices: vec![0, 1, 2, 2, 3, 0],
            texture: TextureId(texture),
            z_order: z,
            ..Default::default()
        }
    }

    fn build_list(list: &RenderList) -> FrameGeometry {
        let mut geometry = FrameGeometry::default();
        build(list, &mut geometry);
        geometry
    }

    #[test]
    fn one_mesh_becomes_one_batch() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        let g = build_list(&list);
        assert_eq!(g.batches.len(), 1);
        assert_eq!(g.vertices.len(), 4);
        assert_eq!(g.indices, vec![0, 1, 2, 2, 3, 0]);
        assert_eq!(g.triangle_count(), 2);
        assert_eq!(g.skipped_meshes, 0);
    }

    #[test]
    fn indices_are_rebased_onto_the_shared_vertex_buffer() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        list.push_mesh(quad(0, 1));
        let g = build_list(&list);
        assert_eq!(g.vertices.len(), 8);
        // The second quad's indices are offset by its base vertex.
        assert_eq!(&g.indices[6..], &[4, 5, 6, 6, 7, 4]);
    }

    #[test]
    fn meshes_sharing_draw_state_merge_into_one_batch() {
        let mut list = RenderList::new();
        for z in 0..5 {
            list.push_mesh(quad(0, z));
        }
        let g = build_list(&list);
        assert_eq!(g.batches.len(), 1, "same texture, blend and mask should be one draw");
        assert_eq!(g.batches[0].indices, 0..30);
    }

    #[test]
    fn a_texture_change_splits_the_batch() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        list.push_mesh(quad(1, 1));
        list.push_mesh(quad(0, 2));
        let g = build_list(&list);
        assert_eq!(g.batches.len(), 3);
        let textures: Vec<u32> = g.batches.iter().map(|b| b.texture.0).collect();
        assert_eq!(textures, vec![0, 1, 0]);
    }

    #[test]
    fn a_blend_mode_change_splits_the_batch() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        let mut additive = quad(0, 1);
        additive.blend_mode = BlendMode::Additive;
        list.push_mesh(additive);
        let g = build_list(&list);
        assert_eq!(g.batches.len(), 2);
        assert_eq!(g.batches[1].blend_mode, BlendMode::Additive);
    }

    #[test]
    fn a_mask_change_splits_the_batch() {
        let mut list = RenderList::new();
        let mask = list.push_mask(vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(5.0, 10.0)]);
        let mut masked = quad(0, 0);
        masked.clipping_mask = Some(mask);
        list.push_mesh(masked);
        list.push_mesh(quad(0, 1));
        let g = build_list(&list);
        assert_eq!(g.batches.len(), 2);
        assert_eq!(g.batches[0].mask, Some(mask));
        assert_eq!(g.batches[1].mask, None);
    }

    #[test]
    fn batches_are_ordered_by_z_even_when_pushed_out_of_order() {
        let mut list = RenderList::new();
        list.push_mesh(quad(2, 20));
        list.push_mesh(quad(0, 0));
        list.push_mesh(quad(1, 10));
        let g = build_list(&list);
        let textures: Vec<u32> = g.batches.iter().map(|b| b.texture.0).collect();
        assert_eq!(textures, vec![0, 1, 2]);
    }

    #[test]
    fn equal_z_orders_keep_their_emission_order() {
        let mut list = RenderList::new();
        list.push_mesh(quad(7, 5));
        list.push_mesh(quad(8, 5));
        let g = build_list(&list);
        let textures: Vec<u32> = g.batches.iter().map(|b| b.texture.0).collect();
        assert_eq!(textures, vec![7, 8]);
    }

    #[test]
    fn tint_travels_per_vertex_so_differing_colours_still_batch() {
        let mut list = RenderList::new();
        let mut red = quad(0, 0);
        red.color = Rgba::new(1.0, 0.0, 0.0, 1.0);
        let mut blue = quad(0, 1);
        blue.color = Rgba::new(0.0, 0.0, 1.0, 0.5);
        list.push_mesh(red);
        list.push_mesh(blue);

        let g = build_list(&list);
        assert_eq!(g.batches.len(), 1, "colour alone must not split a batch");
        assert_eq!(g.vertices[0].light, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.vertices[4].light, [0.0, 0.0, 1.0, 0.5]);
    }

    #[test]
    fn a_mesh_without_a_dark_tint_carries_zeroes() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        let g = build_list(&list);
        assert_eq!(g.vertices[0].dark, [0.0; 4], "zero dark must collapse the tint formula");
    }

    #[test]
    fn a_dark_tint_is_carried_with_a_unit_weight() {
        let mut list = RenderList::new();
        let mut mesh = quad(0, 0);
        mesh.dark_color = Some(Rgb::new(0.25, 0.5, 0.75));
        list.push_mesh(mesh);
        let g = build_list(&list);
        assert_eq!(g.vertices[0].dark, [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn uvs_are_carried_through_untouched() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        let g = build_list(&list);
        assert_eq!(g.vertices[2].uv, [1.0, 1.0]);
    }

    #[test]
    fn a_mask_polygon_becomes_a_fan() {
        let mut list = RenderList::new();
        list.push_mask(vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)]);
        let g = build_list(&list);
        assert_eq!(g.masks.len(), 1);
        assert_eq!(g.masks[0].id, MaskId(0));
        // Four vertices fan into two triangles.
        assert_eq!(g.masks[0].indices, 0..6);
        assert_eq!(&g.indices[..6], &[0, 1, 2, 0, 2, 3]);
    }

    #[test]
    fn a_concave_mask_still_produces_a_fan_for_the_stencil_to_resolve() {
        // An arrowhead: the fan overlaps itself, which the invert op cancels.
        let mut list = RenderList::new();
        list.push_mask(vec![Vec2::new(0.0, 0.0), Vec2::new(4.0, 2.0), Vec2::new(0.0, 4.0), Vec2::new(1.0, 2.0)]);
        let g = build_list(&list);
        assert_eq!(g.masks[0].indices.len(), 6);
    }

    #[test]
    fn a_degenerate_mask_is_dropped_rather_than_emitted() {
        let mut list = RenderList::new();
        list.push_mask(vec![Vec2::ZERO, Vec2::ONE]);
        let g = build_list(&list);
        assert!(g.masks.is_empty(), "a two-point polygon has no area");
        assert!(g.indices.is_empty());
    }

    #[test]
    fn mask_geometry_precedes_mesh_geometry_in_the_buffer() {
        let mut list = RenderList::new();
        let mask = list.push_mask(vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)]);
        let mut masked = quad(0, 0);
        masked.clipping_mask = Some(mask);
        list.push_mesh(masked);
        let g = build_list(&list);
        assert!(g.masks[0].indices.end <= g.batches[0].indices.start);
    }

    #[test]
    fn an_empty_list_produces_empty_geometry() {
        let g = build_list(&RenderList::new());
        assert!(g.is_empty());
        assert_eq!(g.triangle_count(), 0);
    }

    #[test]
    fn an_empty_mesh_is_skipped_without_being_counted_as_malformed() {
        let mut list = RenderList::new();
        list.push_mesh(RenderMesh::default());
        let g = build_list(&list);
        assert!(g.is_empty());
        assert_eq!(g.skipped_meshes, 0, "an empty mesh is not a malformed one");
    }

    #[test]
    fn well_formed_meshes_are_never_skipped() {
        // A malformed mesh cannot be built through the public API: `push_mesh`
        // asserts on one. The `skipped_meshes` guard exists for release builds,
        // where that assertion is compiled out and an out-of-range index would
        // otherwise reach the GPU. What is testable here is the other half of
        // the contract — that valid geometry is never dropped by mistake.
        let mut list = RenderList::new();
        for z in 0..8 {
            list.push_mesh(quad(z % 3, z));
        }
        let g = build_list(&list);
        assert_eq!(g.skipped_meshes, 0);
        assert_eq!(g.triangle_count(), 16);
    }

    #[test]
    fn rebuilding_reuses_allocations_and_leaves_no_stale_state() {
        let mut list = RenderList::new();
        list.push_mesh(quad(0, 0));
        let mut g = FrameGeometry::default();
        build(&list, &mut g);
        let capacity = g.vertices.capacity();

        let empty = RenderList::new();
        build(&empty, &mut g);
        assert!(g.is_empty());
        assert!(g.masks.is_empty());
        assert_eq!(g.skipped_meshes, 0);
        assert_eq!(g.vertices.capacity(), capacity, "allocations should be retained");
    }

    #[test]
    fn the_vertex_layout_matches_the_struct() {
        let layout = Vertex::layout();
        assert_eq!(layout.array_stride, std::mem::size_of::<Vertex>() as u64);
        // 2 + 2 + 4 + 4 floats.
        assert_eq!(std::mem::size_of::<Vertex>(), 12 * 4);
        assert_eq!(layout.attributes.len(), 4);
        assert_eq!(layout.attributes[3].offset, 8 * 4);
    }

    #[test]
    fn a_large_list_exceeds_a_16_bit_index_space_without_overflowing() {
        // 20k quads is 80k vertices, past what a u16 index could address.
        let mut list = RenderList::new();
        for z in 0..20_000u32 {
            list.push_mesh(quad(0, z));
        }
        let g = build_list(&list);
        assert_eq!(g.vertices.len(), 80_000);
        assert!(g.indices.iter().copied().max().unwrap() > u16::MAX as u32);
        assert_eq!(g.batches.len(), 1);
    }
}
