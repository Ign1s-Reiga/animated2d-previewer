//! Turning a posed Cubism model into renderer-neutral primitives.
//!
//! Nothing here knows about a GPU. It produces world-space triangles, texture
//! coordinates and a draw order; `a2d-render` draws them, exactly as it draws
//! the Spine side, and never learns which of the two it is looking at.

use a2d_core::{RenderList, Rgba, TextureId, Vec2};

use crate::eval::Pose;
use crate::moc3::Moc3;

impl Moc3 {
    /// Appends a posed model to `out`.
    ///
    /// `texture` is the page every drawable samples. Cubism models in these
    /// bundles ship a single atlas; a model with more would need the per
    /// drawable texture index, which is not decoded yet, so passing one page is
    /// honest rather than limiting.
    ///
    /// Coordinates come out in canvas units — the space the canvas size is
    /// expressed in once divided by its pixels per unit — with `y` upwards.
    pub fn emit(&self, pose: &Pose, texture: TextureId, out: &mut RenderList) {
        // Drawables are listed in model order but drawn in draw order, so a
        // stable sort of one against the other gives the sequence to emit.
        let mut order: Vec<usize> = (0..self.drawables.len()).collect();
        order.sort_by_key(|i| self.drawables[*i].draw_order);

        for (z, index) in order.into_iter().enumerate() {
            let drawable = &self.drawables[index];
            let Some(points) = pose.drawables.get(index) else { continue };
            if points.len() != drawable.uvs.len() || drawable.indices.len() < 3 {
                continue;
            }

            let mut mesh = out.take_mesh();
            mesh.vertices.extend(points.iter().map(|(x, y)| Vec2::new(*x, *y)));
            mesh.uvs.extend(drawable.uvs.iter().map(|(u, v)| Vec2::new(*u, *v)));
            mesh.indices.extend_from_slice(&drawable.indices);
            mesh.texture = texture;
            mesh.color = Rgba::WHITE;
            mesh.z_order = z as u32;
            if mesh.is_well_formed() {
                out.push_mesh(mesh);
            } else {
                // Recycling keeps the allocation; dropping it would not.
                mesh.vertices.clear();
                mesh.uvs.clear();
                mesh.indices.clear();
            }
        }
    }

    /// Where the posed model sits, in canvas units.
    pub fn bounds(&self, pose: &Pose) -> Option<(Vec2, Vec2)> {
        let mut lo = Vec2::new(f32::MAX, f32::MAX);
        let mut hi = Vec2::new(f32::MIN, f32::MIN);
        let mut any = false;
        for points in &pose.drawables {
            for (x, y) in points {
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                lo = Vec2::new(lo.x.min(*x), lo.y.min(*y));
                hi = Vec2::new(hi.x.max(*x), hi.y.max(*y));
                any = true;
            }
        }
        any.then_some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posed_model_emits_one_mesh_per_drawable() {
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);

        assert_eq!(list.meshes().len(), moc.drawables.len());
        let mesh = &list.meshes()[0];
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.uvs.len(), 3);
        assert_eq!(mesh.indices, [0, 1, 2]);
        assert_eq!(mesh.texture, TextureId(0));
    }

    #[test]
    fn meshes_come_out_in_draw_order() {
        // Draw order is a permutation of the drawables, not their index order,
        // so emitting must sort rather than assume.
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        let z: Vec<u32> = list.meshes().iter().map(|m| m.z_order).collect();
        let mut sorted = z.clone();
        sorted.sort_unstable();
        assert_eq!(z, sorted, "z order must increase as meshes are emitted");
    }

    #[test]
    fn bounds_cover_every_posed_vertex() {
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        let pose = moc.pose(&[]);
        let (lo, hi) = moc.bounds(&pose).expect("a posed model has bounds");
        for points in &pose.drawables {
            for (x, y) in points {
                assert!(*x >= lo.x && *x <= hi.x && *y >= lo.y && *y <= hi.y);
            }
        }
    }
}
