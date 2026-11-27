//! Whether a point is on the character.
//!
//! Pure geometry over what a model emits, so it works for any model family and
//! can be tested without a model.

use a2d_core::{RenderList, Vec2};

/// Whether `point` lies inside a triangle of any visible mesh in `list`.
///
/// `point` is in the same model space the meshes are emitted in (the space
/// `AnimatedModel::hit_test` takes). A mesh whose tint is fully transparent is
/// not visible and does not count; clipping masks are not considered, so a
/// point inside a clipped-away part of a mesh still counts. A triangle on the
/// edge counts (a click exactly on the outline is on the character). Degenerate
/// triangles, indices out of range, and a non-finite point are never a hit.
pub fn point_on_character(list: &RenderList, point: Vec2) -> bool {
    if !point.is_finite() {
        return false;
    }
    list.meshes().iter().filter(|m| m.color.a > 0.0).any(|mesh| {
        mesh.indices.chunks_exact(3).any(|tri| {
            let corner = |i: u16| mesh.vertices.get(usize::from(i)).copied();
            match (corner(tri[0]), corner(tri[1]), corner(tri[2])) {
                (Some(a), Some(b), Some(c)) => in_triangle(point, a, b, c),
                _ => false,
            }
        })
    })
}

/// Edge function: positive on one side of `a -> b`, negative on the other.
fn side(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn in_triangle(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let (d1, d2, d3) = (side(p, a, b), side(p, b, c), side(p, c, a));
    if !(d1.is_finite() && d2.is_finite() && d3.is_finite()) {
        return false;
    }
    // Zero area has no inside; a sliver would otherwise pass for any point on
    // its line.
    if side(c, a, b) == 0.0 {
        return false;
    }
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::{RenderMesh, Rgba};

    fn mesh(vertices: Vec<Vec2>, indices: Vec<u16>, alpha: f32) -> RenderMesh {
        RenderMesh {
            uvs: vec![Vec2::ZERO; vertices.len()],
            vertices,
            indices,
            color: Rgba { r: 1.0, g: 1.0, b: 1.0, a: alpha },
            ..Default::default()
        }
    }

    fn list_of(meshes: Vec<RenderMesh>) -> RenderList {
        let mut list = RenderList::new();
        for m in meshes {
            list.push_mesh(m);
        }
        list
    }

    fn square() -> RenderMesh {
        mesh(
            vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)],
            vec![0, 1, 2, 2, 3, 0],
            1.0,
        )
    }

    #[test]
    fn a_point_inside_a_triangle_is_on_the_character() {
        let list = list_of(vec![square()]);
        assert!(point_on_character(&list, Vec2::new(5.0, 5.0)));
        assert!(point_on_character(&list, Vec2::new(9.0, 1.0)), "first triangle");
        assert!(point_on_character(&list, Vec2::new(1.0, 9.0)), "second triangle");
    }

    #[test]
    fn a_point_outside_every_triangle_is_not() {
        let list = list_of(vec![square()]);
        assert!(!point_on_character(&list, Vec2::new(11.0, 5.0)));
        assert!(!point_on_character(&list, Vec2::new(-0.5, 5.0)));
        assert!(!point_on_character(&RenderList::new(), Vec2::new(5.0, 5.0)));
    }

    #[test]
    fn the_outline_counts_and_the_winding_does_not_matter() {
        let list = list_of(vec![square()]);
        assert!(point_on_character(&list, Vec2::new(0.0, 5.0)));
        assert!(point_on_character(&list, Vec2::new(10.0, 10.0)));
        let reversed = mesh(vec![Vec2::new(0.0, 0.0), Vec2::new(0.0, 10.0), Vec2::new(10.0, 0.0)], vec![0, 1, 2], 1.0);
        assert!(point_on_character(&list_of(vec![reversed]), Vec2::new(2.0, 2.0)));
    }

    #[test]
    fn a_hole_between_meshes_is_not_a_hit() {
        let left = mesh(vec![Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0), Vec2::new(0.0, 2.0)], vec![0, 1, 2], 1.0);
        let right = mesh(vec![Vec2::new(8.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 2.0)], vec![0, 1, 2], 1.0);
        let list = list_of(vec![left, right]);
        assert!(point_on_character(&list, Vec2::new(0.5, 0.5)));
        assert!(point_on_character(&list, Vec2::new(9.5, 0.5)));
        assert!(!point_on_character(&list, Vec2::new(5.0, 1.0)));
    }

    #[test]
    fn a_transparent_mesh_does_not_count() {
        let list = list_of(vec![mesh(square().vertices, vec![0, 1, 2, 2, 3, 0], 0.0)]);
        assert!(!point_on_character(&list, Vec2::new(5.0, 5.0)));
    }

    #[test]
    fn a_flat_triangle_and_a_non_finite_point_are_never_a_hit() {
        let flat = mesh(vec![Vec2::new(0.0, 0.0), Vec2::new(5.0, 0.0), Vec2::new(10.0, 0.0)], vec![0, 1, 2], 1.0);
        assert!(!point_on_character(&list_of(vec![flat]), Vec2::new(5.0, 0.0)));
        assert!(!point_on_character(&list_of(vec![square()]), Vec2::new(f32::NAN, 1.0)));
    }
}
