//! Turning a posed Cubism model into renderer-neutral primitives.
//!
//! Nothing here knows about a GPU. It produces world-space triangles, texture
//! coordinates and a draw order; `a2d-render` draws them, exactly as it draws
//! the Spine side, and never learns which of the two it is looking at.

use a2d_core::{MaskId, RenderList, Rgba, TextureId, Vec2};

use crate::eval::Pose;
use crate::moc3::CubismIr;

/// The draw order a drawable with no usable value is painted at, which is also
/// where an unanimated drawable rests.
const DEFAULT_DRAW_ORDER: i32 = 500;

/// The drawables in the order they are painted, back to front.
///
/// `draw_orders` holds each drawable's blended draw order; `tie_order` is the
/// model's draw-order object list, naming drawables. Drawables sort ascending
/// on the draw order *rounded to the nearest integer* -- blended weights sum to
/// one only within float error, so 500 can arrive as 499.99997, and truncating
/// that would invent a reorder. Equal draw orders are settled by position in
/// `tie_order`: the earlier entry paints first, further back.
///
/// A non-finite draw order counts as 500. A drawable `tie_order` does not name
/// goes after every one it does, but the draw orders still apply among the
/// unlisted ones (ties between them fall to index order), so an empty list
/// still animates; an entry naming no drawable, or repeating one, is ignored
/// (the first position wins). The result is always a permutation of
/// `0..draw_orders.len()`.
pub fn paint_sequence(draw_orders: &[f32], tie_order: &[u32]) -> Vec<usize> {
    paint_sequence_of(draw_orders.len(), draw_orders, tie_order)
}

/// [`paint_sequence`] for exactly `count` drawables, however many draw orders
/// there are: a missing one rests at 500 and an extra one is ignored, so a
/// pose that disagrees with the model in length neither panics nor drops
/// drawables.
pub fn paint_sequence_of(count: usize, draw_orders: &[f32], tie_order: &[u32]) -> Vec<usize> {
    let mut position = vec![u32::MAX; count];
    for (at, index) in tie_order.iter().enumerate() {
        if let Some(slot) = position.get_mut(*index as usize) {
            if *slot == u32::MAX {
                *slot = at as u32;
            }
        }
    }
    let key = |i: usize| {
        let order = draw_orders.get(i).copied().unwrap_or(DEFAULT_DRAW_ORDER as f32);
        let rounded = if order.is_finite() { order.round() as i32 } else { DEFAULT_DRAW_ORDER };
        match position.get(i).copied() {
            Some(at) if at != u32::MAX => (false, rounded, at),
            // Unlisted: after every listed drawable, ordered among themselves
            // by draw order and then by index.
            _ => (true, rounded, i as u32),
        }
    };
    let mut sequence: Vec<usize> = (0..count).collect();
    sequence.sort_by_key(|i| key(*i));
    sequence
}

/// Turning a pose into renderer-neutral meshes.
///
/// A trait for the same reason as [`CubismEval`](crate::CubismEval): the model
/// is defined in `a2d-core` so that a package can store it.
pub trait CubismEmit {
    /// Appends one mesh per visible drawable, in paint order.
    fn emit(&self, pose: &Pose, texture: TextureId, out: &mut RenderList) {
        self.emit_scaled(pose, texture, Vec2::ONE, out);
    }

    /// As [`CubismEmit::emit`], with every emitted position (meshes and the
    /// clip shapes alike) multiplied by `scale` about the model-space origin. A
    /// negative component mirrors. The renderer does not cull, so the flipped
    /// winding that a mirror causes needs no handling.
    fn emit_scaled(&self, pose: &Pose, texture: TextureId, scale: Vec2, out: &mut RenderList);

    /// The posed geometry's bounds, or `None` when nothing was posed.
    fn bounds(&self, pose: &Pose) -> Option<(Vec2, Vec2)>;
}

impl CubismEmit for CubismIr {
    /// Appends a posed model to `out`.
    ///
    /// `texture` is the page every drawable samples. Cubism models in these
    /// bundles ship a single atlas; a model with more would need the per
    /// drawable texture index, which is not decoded yet, so passing one page is
    /// honest rather than limiting.
    ///
    /// Coordinates come out in canvas units — the space the canvas size is
    /// expressed in once divided by its pixels per unit — with `y` upwards.
    fn emit_scaled(&self, pose: &Pose, texture: TextureId, scale: Vec2, out: &mut RenderList) {
        // Paint order is decided per frame: each drawable's blended draw order
        // (which motions animate) picks its layer, and the model's draw-order
        // object list breaks ties. Neither is a sequence on its own; the
        // list alone is wrong at rest on three of the four models checked.
        // Masks are shape sources looked up by index below, so none of this
        // changes what they clip.
        let order = paint_sequence_of(self.drawables.len(), &pose.draw_orders, &self.draw_order);

        // Masks are shared: a face's worth of drawables usually clip to the
        // same one or two meshes, so identical lists are registered once.
        let mut masks: Vec<(Vec<u32>, MaskId)> = Vec::new();

        for (z, index) in order.into_iter().enumerate() {
            let Some(drawable) = self.drawables.get(index) else { continue };
            let Some(points) = pose.drawables.get(index) else { continue };
            if points.len() != drawable.uvs.len() || drawable.indices.len() < 3 {
                continue;
            }

            // A fully transparent drawable is not drawn at all. Cubism hides
            // parts this way -- a closed-eye variant sits on top of the open one
            // at zero opacity -- so painting them opaque covers what should show
            // through, which is worse than drawing nothing.
            let opacity = pose.opacities.get(index).copied().unwrap_or(1.0);
            if opacity <= 1.0 / 512.0 {
                continue;
            }

            let clip = if drawable.masks.is_empty() {
                None
            } else {
                match masks.iter().find(|(list, _)| *list == drawable.masks) {
                    Some((_, id)) => Some(*id),
                    None => {
                        let mut shape = out.take_mask();
                        for source in &drawable.masks {
                            let at = *source as usize;
                            let (Some(points), Some(mask)) = (pose.drawables.get(at), self.drawables.get(at)) else {
                                continue;
                            };
                            if points.len() != mask.uvs.len() {
                                continue;
                            }
                            let corners: Vec<Vec2> =
                                points.iter().map(|(x, y)| Vec2::new(*x * scale.x, *y * scale.y)).collect();
                            shape.push_mesh(&corners, &mask.indices);
                        }
                        // An empty shape would clip everything away, so a mask
                        // that produced no triangles is treated as no mask.
                        if shape.indices.len() < 3 {
                            None
                        } else {
                            let id = out.push_mask_shape(shape);
                            masks.push((drawable.masks.clone(), id));
                            Some(id)
                        }
                    }
                }
            };

            let mut mesh = out.take_mesh();
            mesh.vertices.extend(points.iter().map(|(x, y)| Vec2::new(*x * scale.x, *y * scale.y)));
            mesh.uvs.extend(drawable.uvs.iter().map(|(u, v)| Vec2::new(*u, *v)));
            mesh.indices.extend_from_slice(&drawable.indices);
            mesh.texture = texture;
            mesh.color = Rgba::new(1.0, 1.0, 1.0, opacity);
            mesh.blend_mode = drawable.blend_mode();
            mesh.clipping_mask = clip;
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
    fn bounds(&self, pose: &Pose) -> Option<(Vec2, Vec2)> {
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
    use crate::{CubismEval, Moc3};

    #[test]
    fn meshes_are_painted_in_the_sequence_the_model_names() {
        // With every draw order equal, the object list (section 87) settles
        // everything: slot k names the drawable painted k-th. Read the other
        // way round it is the inverse permutation, which is plausible enough
        // to have shipped -- it paints a face's skin over its eyes. The
        // fixture uses a rotation rather than a reversal because a reversal is
        // its own inverse and could not tell the two apart.
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().drawables(3).build()).expect("should parse");
        assert_eq!(moc.draw_order, [1, 2, 0], "the table is read as a sequence, not as a per-drawable key");

        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert_eq!(list.meshes().len(), 3);

        // Each drawable carries its own `u`, so the first uv names its source.
        let painted: Vec<usize> = list.meshes().iter().map(|m| (m.uvs[0].x / 0.125).round() as usize).collect();
        assert_eq!(painted, [1, 2, 0], "emitted in the model's own order; the inverse would be [2, 0, 1]");
    }

    #[test]
    fn a_drawable_the_sequence_never_names_is_still_painted() {
        // A hand-built or damaged list can repeat an entry rather than
        // permuting cleanly (the decoder never produces one, but a package
        // could carry it). The drawables left unnamed must still be drawn, or
        // part of the character silently disappears.
        let mut moc = Moc3::parse(&crate::moc3::tests::Builder::new().drawables(3).build()).expect("should parse");
        moc.draw_order = vec![1, 1, 1];
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert_eq!(list.meshes().len(), 3, "every drawable is painted exactly once");
        let painted: Vec<usize> = list.meshes().iter().map(|m| (m.uvs[0].x / 0.125).round() as usize).collect();
        assert_eq!(painted, [1, 0, 2], "the named one first, the rest in model order");
    }

    /// The drawable each emitted mesh came from, read from its tagged `u`.
    fn painted(list: &RenderList) -> Vec<usize> {
        list.meshes().iter().map(|m| (m.uvs[0].x / 0.125).round() as usize).collect()
    }

    #[test]
    fn the_blended_draw_order_picks_the_layer_and_the_list_breaks_ties() {
        // Orders [500, 500, 400, 500], list [1, 3, 2, 0]. Drawable 2 is lowest,
        // so it goes first; the rest tie at 500 and follow the list: 1, 3, 0.
        // The alternatives each give a different answer, so this pins both the
        // key and the tie direction:
        //   the list alone (ignoring draw order)  -> [1, 3, 2, 0]
        //   ties by drawable index                -> [2, 0, 1, 3]
        //   ties by the list reversed             -> [2, 0, 3, 1]
        assert_eq!(paint_sequence(&[500.0, 500.0, 400.0, 500.0], &[1, 3, 2, 0]), [2, 1, 3, 0]);
    }

    #[test]
    fn draw_orders_are_rounded_not_truncated() {
        // A weighted sum of 500s can come out a hair under 500. Truncated that
        // is 499 and would put drawable 0 in front of 1 against the list
        // [1, 0]; rounded it ties at 500 and the list puts 1 first.
        assert_eq!(paint_sequence(&[499.99997, 500.0], &[1, 0]), [1, 0]);
        // A genuine half-step still separates them: 500.4 rounds to 500, 500.6 to 501.
        assert_eq!(paint_sequence(&[500.6, 500.4], &[0, 1]), [1, 0]);
    }

    #[test]
    fn a_non_finite_draw_order_counts_as_500() {
        // NaN and infinities rest at 500, so against a 499 and a 501 they sit between.
        let orders = [f32::NAN, 499.0, 501.0, f32::INFINITY, f32::NEG_INFINITY];
        assert_eq!(paint_sequence(&orders, &[0, 1, 2, 3, 4]), [1, 0, 3, 4, 2]);
    }

    #[test]
    fn an_unlisted_drawable_goes_last_and_a_duplicate_entry_is_ignored() {
        // Drawables 1 and 3 are never named, so both follow every listed
        // drawable even though their draw orders are the lowest; between
        // themselves the draw orders still apply, so 3 (0) precedes 1 (100).
        assert_eq!(paint_sequence(&[500.0, 100.0, 500.0, 0.0], &[2, 0]), [2, 0, 3, 1]);
        // The repeated 2 and the entry naming no drawable change nothing: the
        // first position of each wins.
        assert_eq!(paint_sequence(&[500.0, 500.0, 500.0], &[2, 9, 0, 2, 1]), [2, 0, 1]);
        // No list at all: everything is unlisted, so draw order decides and
        // equal ones fall to index order.
        assert_eq!(paint_sequence(&[3.0, 2.0, 1.0], &[]), [2, 1, 0]);
        assert_eq!(paint_sequence(&[500.0, 500.0], &[]), [0, 1]);
        assert!(paint_sequence(&[], &[1]).is_empty());
    }

    #[test]
    fn an_empty_list_still_animates() {
        assert_eq!(paint_sequence(&[600.0, 400.0], &[]), [1, 0]);
    }

    #[test]
    fn a_pose_of_the_wrong_length_neither_panics_nor_paints_nothing() {
        let bytes = crate::moc3::tests::Builder::new().drawables(3).build();
        let moc = Moc3::parse(&bytes).expect("should parse");
        let ir = moc.model;
        let full = ir.pose(&[]);
        for orders in [vec![], vec![400.0], vec![500.0; 7]] {
            let pose = Pose { draw_orders: orders.clone(), ..full.clone() };
            let mut list = RenderList::new();
            ir.emit(&pose, TextureId(0), &mut list);
            assert_eq!(list.meshes().len(), 3, "{} draw orders", orders.len());
        }
        // Missing entries rest at 500, extra ones are ignored.
        assert_eq!(paint_sequence_of(3, &[600.0], &[]), [1, 2, 0]);
        assert_eq!(paint_sequence_of(2, &[500.0, 400.0, 0.0, 0.0], &[]), [1, 0]);
    }

    #[test]
    fn an_animated_draw_order_reorders_the_emitted_meshes() {
        // Drawable 0's draw order is 400 / 400 / 600 at the keys -30 / 0 / 30
        // of the first parameter, so with p the fraction of the way from 0 to
        // 30 it is 400 + 200p. The others are keyed 500 throughout, so the
        // tie falls at p = 0.5.
        let bytes = crate::moc3::tests::Builder::new()
            .drawables(3)
            .tie_order(&[1, 2, 0])
            .draw_orders(&[400.0, 400.0, 600.0, 500.0, 500.0, 500.0, 500.0, 500.0, 500.0])
            .build();
        let moc = Moc3::parse(&bytes).expect("should parse");
        assert!(moc.notes.is_empty(), "{:?}", moc.notes);
        assert_eq!(moc.draw_order, [1, 2, 0]);
        let order_at = |p: f32| {
            let pose = moc.pose(&[30.0 * p, 1.0]);
            let mut list = RenderList::new();
            moc.emit(&pose, TextureId(0), &mut list);
            (painted(&list), list.meshes().iter().map(|m| m.z_order).collect::<Vec<_>>())
        };
        // 400 and 450 are below the others' 500: drawable 0 is furthest back.
        assert_eq!(order_at(0.0).0, [0, 1, 2]);
        assert_eq!(order_at(0.25).0, [0, 1, 2]);
        // 500 ties, and the list puts 0 after 1 and 2. 600 is simply in front.
        assert_eq!(order_at(0.5).0, [1, 2, 0]);
        let (order, z) = order_at(1.0);
        assert_eq!(order, [1, 2, 0]);
        assert_eq!(z, [0, 1, 2], "z follows the paint sequence");
    }

    #[test]
    fn a_masked_drawable_carries_a_clip_and_its_blend_mode() {
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().masked().build()).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);

        assert_eq!(list.masks().len(), 1, "one mask, shared rather than one per drawable");
        let clipped: Vec<_> = list.meshes().iter().filter(|m| m.clipping_mask.is_some()).collect();
        assert_eq!(clipped.len(), 1);
        assert_eq!(clipped[0].blend_mode, a2d_core::BlendMode::Multiply);
        // The mask carries the masking drawable's own triangles, not a bounding box.
        let mask = list.mask(clipped[0].clipping_mask.expect("present")).expect("registered");
        assert_eq!(mask.indices.len(), moc.drawables[0].indices.len());
    }

    #[test]
    fn a_drawable_hidden_by_its_deformer_is_not_drawn_but_still_masks() {
        // Drawable 0 hangs off the warp, whose every keyform is transparent, and
        // is the mask of drawable 1, which hangs off nothing. Hiding the mask's
        // own drawable must not stop it clipping the one that shows.
        let bytes = crate::moc3::tests::Builder::new()
            .masked()
            .drawable_parents(&[1, u32::MAX])
            .warp_opacities(&[0.0, 0.0, 0.0])
            .build();
        let moc = Moc3::parse(&bytes).expect("should parse");
        let pose = moc.pose(&[]);
        assert_eq!(pose.opacities, [0.0, 1.0]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert_eq!(list.meshes().len(), 1, "only the drawable whose deformer is opaque is drawn");
        let id = list.meshes()[0].clipping_mask.expect("the survivor is still clipped");
        let mask = list.mask(id).expect("registered");
        assert_eq!(mask.indices.len(), moc.drawables[0].indices.len());
    }

    #[test]
    fn a_drawable_with_no_vertices_is_posed_and_skipped_without_harm() {
        // Real models contain empty placeholder art meshes: zero vertices, zero
        // indices, one keyform. Posing must give them an empty point list rather
        // than panic or invent geometry, emit must skip them, and the drawables
        // around them must be unaffected.
        let bytes = crate::moc3::tests::Builder::new().drawables(3).vertex_counts(&[3, 0, 3]).build();
        let moc = Moc3::parse(&bytes).expect("should parse");

        let pose = moc.pose(&[]);
        assert_eq!(pose.drawables.len(), 3);
        assert!(pose.drawables[1].is_empty(), "an empty drawable poses to no points");
        assert!(pose.drawables[0].iter().chain(&pose.drawables[2]).all(|(x, y)| x.is_finite() && y.is_finite()));

        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert_eq!(list.meshes().len(), 2, "the empty drawable is skipped, its neighbours are drawn");
        assert!(moc.bounds(&pose).is_some());
    }

    #[test]
    fn an_unmasked_model_registers_no_masks() {
        let moc = Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert!(list.masks().is_empty());
        assert!(list.meshes().iter().all(|m| m.clipping_mask.is_none()));
    }

    #[test]
    fn opacity_reaches_the_mesh_and_a_transparent_drawable_is_not_drawn() {
        // Cubism hides a part by taking it to zero opacity rather than by
        // removing it, so drawing one opaque covers what should show through.
        let bytes = crate::moc3::tests::Builder::new().opacities(&[0.5, 0.5, 0.5]).build();
        let moc = Moc3::parse(&bytes).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert_eq!(list.meshes().len(), 1);
        assert!((list.meshes()[0].color.a - 0.5).abs() < 1e-5, "{:?}", list.meshes()[0].color);

        let bytes = crate::moc3::tests::Builder::new().opacities(&[0.0, 0.0, 0.0]).build();
        let moc = Moc3::parse(&bytes).expect("should parse");
        let pose = moc.pose(&[]);
        let mut list = RenderList::new();
        moc.emit(&pose, TextureId(0), &mut list);
        assert!(list.meshes().is_empty(), "a fully transparent drawable should not be emitted");
    }

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
