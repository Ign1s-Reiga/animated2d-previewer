//! Posing a MOC3 model: parameter values in, vertex positions out.
//!
//! # Status: it poses; it is not yet compared against a reference
//!
//! Everything the [`crate::moc3`] reader exposes was checked against an
//! independent source before being believed. This is checked differently: by
//! whether the result lands where a character should. That is weaker than a
//! reference render but far from nothing, because the deformer chain is deep
//! enough that a wrong composition does not land anywhere plausible at all --
//! an earlier version was out by four orders of magnitude.
//!
//! # The scale rule, and how it was found
//!
//! Every space in a model is canvas pixels, with one exception: a warp
//! deformer's children live in its unit square. So a rotation deformer sitting
//! under a warp is the one place where units have to be converted, and that was
//! the missing piece.
//!
//! The exchange rate is the warp's own grid extent — the grid spans the
//! parent's space while the children span zero to one — and it **accumulates**:
//! a warp nested in another warp measures its grid in that warp's units, not in
//! pixels, so the rates multiply down the chain. Missing the accumulation is
//! what made the first attempt look almost right and still be wrong by a factor
//! of a thousand on deep chains.
//!
//! It was found by dumping one small model's deformer tree in full and reading
//! it. The giveaway was three lines: a warp with a grid spanning `-627..686`
//! pixels, a rotation under it sitting at `(0.501, 0.500)` — dead centre of a
//! unit square — and *that* rotation's own child back at `(305.6, -48.5)`,
//! pixels again. `0.501 + 305.6/1312` lands at `0.734`, inside the square. No
//! amount of staring at aggregate statistics produced that; one readable tree
//! did.
//!
//! # The axis order, and how it was found
//!
//! MOC3 writes the **vertical component of a point first**. That one fact fixes
//! four things at once: point pairs are swapped as they are read, the two
//! rotation-origin sections go into the opposite fields, rotation angles turn
//! the other way, and a warp's stored runs walk down a column rather than along
//! a row (see [`crate::WarpDeformer::divisions`]).
//!
//! Getting it wrong transposes the whole model — a mirror, not a turn — and the
//! error hides well, because reading the point pairs and the lattice in the
//! same wrong order transposes both. A census of monotone lattices scores the
//! two readings identically, and the parameters cannot witness against it
//! either, since they are carried through the very chain under test.
//!
//! What settled it was each drawable's own texture coordinates: the mesh is the
//! same shape in uv space as in canvas space, so the map between them is a
//! similarity, and only a mirror flips the sign of its determinant. Exactly one
//! mirror is expected, `v` running down the atlas against `y` running up the
//! canvas. Read the old way 1% of drawables matched their own texture; read
//! this way 99% do, on all three models measured.
//!
//! # How well it works, measured
//!
//! Across six models from the same source, **2130 of 2131 drawables pose
//! inside their own canvas**, and five of the six place every drawable. On the
//! model this was developed against the posed extent is 1.24 by 1.12 against a
//! canvas of 0.94 by 1.66 — a character occupying its frame rather than merely
//! a finite result. No chain produces anything unusable, so [`Pose::unstable`]
//! is empty on all six.
//!
//! The sixth model, whose drawables measured up to four and a half canvases
//! across, turned out not to be a chain defect either. Its root deformer is
//! driven by a zoom parameter running 0 to 10 whose *stored default is 8*,
//! which scales the whole model by about five. Wound back to 0 the model
//! measures 0.91 by 0.96 against a canvas of 1.00 by 1.35, and the sheet that
//! read as an impossible backdrop is an ordinary canvas-wide one. The lesson
//! generalises: **a MOC3's stored parameter defaults are not necessarily the
//! values it is displayed at.** The display values live on the Unity side, one
//! `CubismParameter` component per parameter, and recovering them belongs to
//! the importer.
//!
//! Two further things support the composition independently. A warp's child
//! space really is normalised: drawables under a warp carry coordinates near
//! one whether their parent's grid is in `[0, 1]` or in units running to 64.
//! And the root rotation's scale is 0.00026192, whose reciprocal is 3818
//! against a canvas of 3792 pixels per unit — that deformer is the
//! pixels-to-units bridge, exactly as the transform predicts.
//!
//! # What rendering showed
//!
//! Rendered at default parameters, the models come out as coherent scenes: a
//! reclining figure under a pine branch in one, a character on a swing in
//! another. Each drawable is well formed and correctly textured, and the parts
//! sit where a scene needs them.
//!
//! One measurement is worth more than the impression. Sweeping `ParamEyeBallX`
//! and `ParamEyeBallY` and taking the direction each moves the pupils gives two
//! axes that come out **perpendicular and right-handed** in every model, with
//! the responding drawables in total agreement. A chain that transposed a grid,
//! mirrored a warp or sheared a rotation could not do that. Their absolute
//! angle varies between models — 0, -60 and -95 degrees — which is head tilt in
//! the artwork rather than error, and is why the assertion is on the pair
//! rather than on either one alone.
//!
//! Two things about that are worth more than the picture itself. A model and
//! its own low-detail variant -- separately authored rigs, 101 drawables
//! against 601, 151 parameters against 849 -- render as the *same* scene. A
//! wrong chain would have to distort both identically to do that. And the
//! elements that read as scattered at first turned out to be scenery: a pine
//! branch hanging apart from the figure is the rig, not a fault.
//!
//! # What is still unverified
//!
//! **No reference render has been compared against.** Landing in the right
//! frame is strong evidence the chain composes correctly, but it cannot show
//! that a particular warp bends the way Live2D bends it, or that keyform
//! blending weights are right between keys rather than only on them.
//!
//! The assumptions that a render would settle, and their symptoms:
//!
//! 1. **Keyform ordering.** An element's keyforms form a grid with one axis per
//!    parameter, and this takes the last axis as varying fastest. Reversing it
//!    was tried and is worse: on the six models it drops 2130 drawables inside
//!    their canvas to 1990, so the current reading is not merely untested. It
//!    is not *confirmed* either — a smoothness argument over the keyform grid
//!    appeared to favour the reverse until it turned out to be confounded,
//!    because unequal axis lengths give the two reshapes different numbers of
//!    neighbour pairs and so incomparable totals.
//! 2. **Rotation composition.** Applied as
//!    `origin + rotate(angle) * scale * p / unit`.

use crate::moc3::{Deformer, DeformerKind, KeyformBinding, Moc3, RotationKeyform};

/// A posed model: where every drawable's vertices ended up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose {
    /// One entry per drawable, in the model's own order, holding `x, y` pairs
    /// in canvas units.
    pub drawables: Vec<Vec<(f32, f32)>>,
    /// One opacity per drawable, blended from its keyforms.
    ///
    /// A model that carries no opacity track leaves these at one, which draws
    /// exactly as it did before the track was read.
    pub opacities: Vec<f32>,
    /// One draw order per drawable, blended from its keyforms.
    ///
    /// Cubism animates draw order, so a hand passing in front of a face is a
    /// change in this rather than a change in geometry. Falls back to the fixed
    /// order when the model carries no track.
    pub draw_orders: Vec<f32>,
    /// Drawables whose deformer chain produced something unusable, and which
    /// therefore hold their own coordinates rather than a posed result.
    ///
    /// Empty on every model measured so far, but not a formality: a chain
    /// composed in the wrong units overflows within a few levels, and this is
    /// where that lands. A caller should treat a non-zero count as "this pose
    /// is not finished" rather than as a rounding detail.
    pub unstable: Vec<usize>,
}

impl Pose {
    /// Whether every drawable came through its deformer chain intact.
    pub fn is_stable(&self) -> bool {
        self.unstable.is_empty()
    }
}

/// Where one parameter's value falls among a binding's keys.
#[derive(Debug, Clone, Copy)]
struct AxisPosition {
    /// Index of the key at or below the value.
    lower: usize,
    /// How far towards the next key, in `0..=1`.
    fraction: f32,
}

impl Moc3 {
    /// Poses the model at the given parameter values.
    ///
    /// `values` is indexed by parameter, in the model's own order; anything
    /// short is filled from the parameters' defaults, and every value is
    /// clamped into its parameter's range before use.
    pub fn pose(&self, values: &[f32]) -> Pose {
        let resolved = self.resolve_values(values);

        // Each element's keyforms are blended first, then the tree is walked,
        // because a deformer's own shape has to exist before it can move a child.
        let warps: Vec<Vec<(f32, f32)>> = self
            .warp_deformers
            .iter()
            .map(|w| {
                let weights = self.weights_for(w.keyform_binding, &resolved);
                let mut grid = vec![(0.0f32, 0.0f32); w.point_count as usize];
                for (keyform, weight) in &weights {
                    let Some(source) = self.keyforms.warp(w.keyform_begin as usize + keyform, grid.len()) else {
                        continue;
                    };
                    for (out, pair) in grid.iter_mut().zip(source.chunks_exact(2)) {
                        out.0 += pair[0] * weight;
                        out.1 += pair[1] * weight;
                    }
                }
                grid
            })
            .collect();

        let rotations: Vec<RotationKeyform> = self
            .rotation_deformers
            .iter()
            .map(|r| {
                let weights = self.weights_for(r.keyform_binding, &resolved);
                let mut out = RotationKeyform::default();
                for (keyform, weight) in &weights {
                    let Some(k) = self.rotation_keyforms.get(r.keyform_begin as usize + keyform) else { continue };
                    out.origin.0 += k.origin.0 * weight;
                    out.origin.1 += k.origin.1 * weight;
                    out.angle += k.angle * weight;
                    out.scale += k.scale * weight;
                    out.opacity += k.opacity * weight;
                }
                out
            })
            .collect();

        let units = self.warp_units(&warps);

        let mut unstable = Vec::new();
        let mut drawables = Vec::with_capacity(self.drawables.len());
        let mut opacities = Vec::with_capacity(self.drawables.len());
        let mut draw_orders = Vec::with_capacity(self.drawables.len());
        for (index, d) in self.drawables.iter().enumerate() {
            let weights = self.weights_for(d.keyform_binding, &resolved);
            let mut points = vec![(0.0f32, 0.0f32); d.vertex_count()];
            let mut opacity = if self.drawable_keyform_opacities.is_empty() { 1.0 } else { 0.0 };
            let mut order = if self.drawable_keyform_draw_orders.is_empty() { d.draw_order as f32 } else { 0.0 };
            for (keyform, weight) in &weights {
                let at = d.keyform_begin as usize + keyform;
                if let Some(o) = self.drawable_keyform_opacities.get(at) {
                    opacity += o * weight;
                }
                if let Some(o) = self.drawable_keyform_draw_orders.get(at) {
                    order += o * weight;
                }
                let Some(source) = self.keyforms.drawable(at, points.len()) else {
                    continue;
                };
                for (out, pair) in points.iter_mut().zip(source.chunks_exact(2)) {
                    out.0 += pair[0] * weight;
                    out.1 += pair[1] * weight;
                }
            }
            opacities.push(opacity.clamp(0.0, 1.0));
            draw_orders.push(order);

            // Keep the un-deformed coordinates: if the chain produces something
            // unusable they are what is reported, so a caller never receives a
            // non-finite vertex to hand to a renderer.
            let local = points.clone();
            self.carry_up(d.parent_deformer, &mut points, &warps, &rotations, &units);
            if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
                points = local;
                unstable.push(index);
            }
            drawables.push(points);
        }

        Pose { drawables, opacities, draw_orders, unstable }
    }

    /// Parameter values, defaulted and clamped.
    fn resolve_values(&self, values: &[f32]) -> Vec<f32> {
        self.parameters
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let raw = values.get(i).copied().filter(|v| v.is_finite()).unwrap_or(p.default);
                p.clamp(raw)
            })
            .collect()
    }

    /// Blend weights over one element's keyform grid.
    ///
    /// The grid has one axis per parameter binding, so a value between two keys
    /// on every axis lands between `2^axes` keyforms, weighted multilinearly —
    /// the same rule as bilinear interpolation, in as many dimensions as there
    /// are parameters.
    fn weights_for(&self, binding: u32, values: &[f32]) -> Vec<(usize, f32)> {
        let Some(KeyformBinding { axes }) = self.keyform_bindings.get(binding as usize) else {
            return vec![(0, 1.0)];
        };
        if axes.is_empty() {
            return vec![(0, 1.0)];
        }

        let mut positions = Vec::with_capacity(axes.len());
        let mut sizes = Vec::with_capacity(axes.len());
        for axis in axes {
            let Some(pb) = self.parameter_bindings.get(*axis as usize) else { return vec![(0, 1.0)] };
            let value = pb.parameter.try_into().ok().and_then(|i: usize| values.get(i).copied()).unwrap_or(0.0);
            positions.push(locate(&pb.keys, value));
            sizes.push(pb.keys.len());
        }

        // The last axis varies fastest, so its stride is one.
        let mut strides = vec![1usize; sizes.len()];
        for i in (0..sizes.len().saturating_sub(1)).rev() {
            strides[i] = strides[i + 1] * sizes[i + 1];
        }

        let corners = 1usize << axes.len().min(16);
        let mut out = Vec::with_capacity(corners);
        for corner in 0..corners {
            let mut index = 0usize;
            let mut weight = 1.0f32;
            for (axis, position) in positions.iter().enumerate() {
                let upper = corner >> axis & 1 == 1;
                let key = if upper { (position.lower + 1).min(sizes[axis] - 1) } else { position.lower };
                weight *= if upper { position.fraction } else { 1.0 - position.fraction };
                index += key * strides[axis];
            }
            if weight > 0.0 {
                out.push((index, weight));
            }
        }
        if out.is_empty() {
            out.push((0, 1.0));
        }
        out
    }

    /// Carries points up the deformer chain until they reach canvas space.
    fn carry_up(
        &self,
        from: u32,
        points: &mut [(f32, f32)],
        warps: &[Vec<(f32, f32)>],
        rotations: &[RotationKeyform],
        units: &[(f32, f32)],
    ) {
        let mut at = Some(from);
        // The tree is checked acyclic on parse, so this terminates; the counter
        // is belt and braces against a future change that stops checking.
        let mut guard = 0usize;
        while let Some(index) = at {
            guard += 1;
            if guard > self.deformers.len() + 1 {
                return;
            }
            let Some(deformer) = self.deformers.get(index as usize) else { return };
            match deformer.kind {
                DeformerKind::Warp(i) => {
                    if let (Some(grid), Some(shape)) = (warps.get(i as usize), self.warp_deformers.get(i as usize)) {
                        // The second division count is the one that runs along
                        // a stored row (see `WarpDeformer::divisions`), so it
                        // is the x axis here.
                        let (columns, rows) = shape.divisions;
                        for point in points.iter_mut() {
                            *point = warp_point(grid, columns as usize, rows as usize, *point);
                        }
                    }
                    // A warp extrapolates outside its grid, so a child that
                    // arrives in the wrong units grows by a factor per level.
                    // Stopping here keeps that visible as one bad drawable
                    // rather than as infinities spreading through the pose.
                    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
                        return;
                    }
                }
                DeformerKind::Rotation(i) => {
                    if let Some(frame) = rotations.get(i as usize) {
                        // A rotation hands its result to its parent, so it has
                        // to arrive in the parent's units. Every space in the
                        // model is canvas pixels except a warp's, which is its
                        // own unit square, so a rotation sitting under a warp is
                        // the one place a conversion is needed.
                        let scale = match deformer.parent.and_then(|p| self.deformers.get(p as usize)) {
                            Some(Deformer { kind: DeformerKind::Warp(w), .. }) => {
                                units.get(*w as usize).copied().unwrap_or((1.0, 1.0))
                            }
                            _ => (1.0, 1.0),
                        };
                        for point in points.iter_mut() {
                            *point = rotate_point(frame, *point, scale);
                        }
                    }
                }
            }
            at = deformer.parent;
        }
    }

    /// How many canvas pixels one unit of each warp's child space spans.
    ///
    /// A warp's grid is drawn in its parent's space while its children live in
    /// a unit square, so the grid's extent is the exchange rate between the
    /// two. That extent is only in pixels when the parent is *not* itself a
    /// warp; nested warps measure in their parent's units, so the rates
    /// multiply down the chain.
    ///
    /// Rates are taken from the posed grids rather than setup ones, so a warp
    /// that stretches carries its children with it.
    fn warp_units(&self, grids: &[Vec<(f32, f32)>]) -> Vec<(f32, f32)> {
        let extent = |i: usize| -> Option<(f32, f32)> {
            let grid = grids.get(i)?;
            let mut bounds = (f32::MIN, f32::MIN, f32::MAX, f32::MAX);
            for (x, y) in grid {
                bounds.0 = bounds.0.max(*x);
                bounds.1 = bounds.1.max(*y);
                bounds.2 = bounds.2.min(*x);
                bounds.3 = bounds.3.min(*y);
            }
            let (w, h) = (bounds.0 - bounds.2, bounds.1 - bounds.3);
            // A degenerate grid has no exchange rate to offer.
            (w.is_finite() && h.is_finite() && w > f32::EPSILON && h > f32::EPSILON).then_some((w, h))
        };

        let mut out = vec![(1.0f32, 1.0f32); self.warp_deformers.len()];
        let mut done = vec![false; self.warp_deformers.len()];
        // Resolve each warp by walking up to the first non-warp ancestor, then
        // multiplying back down. Depth is bounded by the tree, which is checked
        // acyclic on parse.
        for (index, deformer) in self.deformers.iter().enumerate() {
            let DeformerKind::Warp(w) = deformer.kind else { continue };
            if done[w as usize] {
                continue;
            }
            let mut chain = Vec::new();
            let mut at = Some(index as u32);
            while let Some(i) = at {
                let Some(d) = self.deformers.get(i as usize) else { break };
                let DeformerKind::Warp(w) = d.kind else { break };
                chain.push(w as usize);
                if done[w as usize] {
                    break;
                }
                at = d.parent;
                if chain.len() > self.deformers.len() {
                    break;
                }
            }
            // The far end is either a resolved warp or open pixel space.
            let mut carry = match chain.last() {
                Some(&w) if done[w] => out[w],
                _ => (1.0, 1.0),
            };
            for &w in chain.iter().rev() {
                if done[w] {
                    carry = out[w];
                    continue;
                }
                let (ew, eh) = extent(w).unwrap_or((1.0, 1.0));
                carry = (ew * carry.0, eh * carry.1);
                out[w] = carry;
                done[w] = true;
            }
        }
        out
    }

    /// The deformer chain above an element, root last.
    pub fn deformer_chain(&self, from: u32) -> Vec<&Deformer> {
        let mut out = Vec::new();
        let mut at = Some(from);
        while let Some(index) = at {
            let Some(deformer) = self.deformers.get(index as usize) else { break };
            out.push(deformer);
            at = deformer.parent;
            if out.len() > self.deformers.len() {
                break;
            }
        }
        out
    }
}

/// Where a value falls among a sorted key list.
///
/// Outside the ends it clamps rather than extrapolating: a parameter is already
/// clamped to its own range, and a key list covers that range.
fn locate(keys: &[f32], value: f32) -> AxisPosition {
    if keys.len() < 2 {
        return AxisPosition { lower: 0, fraction: 0.0 };
    }
    if value <= keys[0] {
        return AxisPosition { lower: 0, fraction: 0.0 };
    }
    if value >= keys[keys.len() - 1] {
        return AxisPosition { lower: keys.len() - 2, fraction: 1.0 };
    }
    // Keys are strictly increasing, checked on parse.
    let upper = keys.partition_point(|k| *k <= value).max(1);
    let lower = upper - 1;
    let span = keys[upper] - keys[lower];
    let fraction = if span > f32::EPSILON { (value - keys[lower]) / span } else { 0.0 };
    AxisPosition { lower, fraction }
}

/// Bilinear lookup of a warp grid.
///
/// A child of a warp lives in the grid's unit square, so its coordinates are
/// the lookup. Outside the square the edge cells are extended rather than
/// clamped, so geometry that overhangs a deformer keeps its shape instead of
/// collapsing onto the border.
fn warp_point(grid: &[(f32, f32)], divisions_x: usize, divisions_y: usize, point: (f32, f32)) -> (f32, f32) {
    let (cols, rows) = (divisions_x + 1, divisions_y + 1);
    if cols < 2 || rows < 2 || grid.len() < cols * rows {
        return point;
    }
    let fx = point.0 * divisions_x as f32;
    let fy = point.1 * divisions_y as f32;
    let ix = (fx.floor() as isize).clamp(0, divisions_x as isize - 1) as usize;
    let iy = (fy.floor() as isize).clamp(0, divisions_y as isize - 1) as usize;
    let tx = fx - ix as f32;
    let ty = fy - iy as f32;

    // Column-major: a stored run walks *down* a column, because the file's
    // first axis is the vertical one (see `WarpDeformer::divisions`).
    let corner = |cx: usize, cy: usize| grid[cx * rows + cy];
    let (a, b, c, d) = (corner(ix, iy), corner(ix + 1, iy), corner(ix, iy + 1), corner(ix + 1, iy + 1));
    let top = (a.0 + (b.0 - a.0) * tx, a.1 + (b.1 - a.1) * tx);
    let bottom = (c.0 + (d.0 - c.0) * tx, c.1 + (d.1 - c.1) * tx);
    (top.0 + (bottom.0 - top.0) * ty, top.1 + (bottom.1 - top.1) * ty)
}

/// Places a point in a rotation deformer's frame.
///
/// `unit` converts the turned offset into the parent's units, and is `(1, 1)`
/// wherever the two already agree.
fn rotate_point(frame: &RotationKeyform, point: (f32, f32), unit: (f32, f32)) -> (f32, f32) {
    let (sin, cos) = frame.angle.to_radians().sin_cos();
    let x = point.0 * frame.scale;
    let y = point.1 * frame.scale;
    (frame.origin.0 + (x * cos - y * sin) / unit.0, frame.origin.1 + (x * sin + y * cos) / unit.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_without_an_opacity_track_poses_fully_opaque() {
        // Every model read before these sections were identified drew opaque,
        // and must keep doing so rather than turning invisible.
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        assert!(moc.drawable_keyform_opacities.is_empty());
        let pose = moc.pose(&[]);
        assert!(pose.opacities.iter().all(|o| *o == 1.0), "{:?}", pose.opacities);
    }

    #[test]
    fn opacity_blends_across_keyforms_like_position_does() {
        // The fixture's parameter has keys at -30, 0 and 30, so a value of 15
        // sits halfway between the second and third keyform.
        let bytes = crate::moc3::tests::Builder::new().opacities(&[1.0, 1.0, 0.0]).build();
        let moc = crate::Moc3::parse(&bytes).expect("should parse");
        assert_eq!(moc.drawable_keyform_opacities.len(), moc.keyforms.drawable_offsets.len());

        let at = |value: f32| {
            let mut values: Vec<f32> = moc.parameters.iter().map(|p| p.default).collect();
            values[0] = value;
            moc.pose(&values).opacities[0]
        };
        assert!((at(0.0) - 1.0).abs() < 1e-5, "{}", at(0.0));
        assert!((at(30.0) - 0.0).abs() < 1e-5, "{}", at(30.0));
        assert!((at(15.0) - 0.5).abs() < 1e-5, "halfway between keys should be halfway: {}", at(15.0));
    }

    fn keys(values: &[f32]) -> Vec<f32> {
        values.to_vec()
    }

    /// The fixture in `moc3` has one parameter with keys at -30, 0 and 30, a
    /// quad that doubles across them, and a drawable in the warp's unit square.
    fn posed(angle: f32) -> Vec<(f32, f32)> {
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        moc.pose(&[angle]).drawables.into_iter().next().expect("one drawable")
    }

    #[test]
    fn a_parameter_on_a_key_poses_that_keyform_outright() {
        // At zero the grid is the unit quad, so the drawable's corners land on
        // it: (0,0), (1,0) and (0,1) of the square map to the quad's corners.
        let points = posed(0.0);
        assert_eq!(points, [(0.0, 0.0), (10.0, 0.0), (0.0, 20.0)]);
    }

    #[test]
    fn a_parameter_between_keys_blends_the_keyforms_around_it() {
        // Half way from key 0 to key 30 blends a unit quad with a doubled one.
        let points = posed(15.0);
        assert_eq!(points, [(0.0, 0.0), (15.0, 0.0), (0.0, 30.0)]);
    }

    #[test]
    fn a_parameter_past_its_range_is_clamped_before_it_is_used() {
        // The parameter's own maximum is 30, so anything beyond poses as 30.
        assert_eq!(posed(999.0), posed(30.0));
        assert_eq!(posed(30.0), [(0.0, 0.0), (20.0, 0.0), (0.0, 40.0)]);
    }

    #[test]
    fn an_absent_value_falls_back_to_the_parameter_default() {
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        // ParamAngleX defaults to zero, so an empty slice poses the same.
        assert_eq!(moc.pose(&[]).drawables, moc.pose(&[0.0]).drawables);
    }

    #[test]
    fn a_value_between_two_keys_lands_proportionally_between_them() {
        let k = keys(&[-30.0, 0.0, 30.0]);
        let at = locate(&k, 15.0);
        assert_eq!(at.lower, 1);
        assert!((at.fraction - 0.5).abs() < 1e-6, "{at:?}");
    }

    #[test]
    fn a_value_on_a_key_takes_that_key_outright() {
        let k = keys(&[0.0, 1.0, 1.2]);
        let at = locate(&k, 1.0);
        assert_eq!(at.lower, 1);
        assert_eq!(at.fraction, 0.0);
    }

    #[test]
    fn a_value_past_either_end_clamps_rather_than_extrapolating() {
        let k = keys(&[-30.0, 0.0, 30.0]);
        let low = locate(&k, -999.0);
        assert_eq!((low.lower, low.fraction), (0, 0.0));
        let high = locate(&k, 999.0);
        assert_eq!((high.lower, high.fraction), (1, 1.0));
    }

    #[test]
    fn a_single_key_axis_has_nowhere_to_interpolate_to() {
        let at = locate(&keys(&[5.0]), 99.0);
        assert_eq!((at.lower, at.fraction), (0, 0.0));
    }

    #[test]
    fn a_non_square_grid_reads_its_columns_from_the_second_division() {
        // A regular 10-by-20 lattice with one row division and two column
        // divisions. Its corners only land back on themselves when the second
        // division count is read as the number of columns; read the other way
        // the rows interleave and the right edge collapses onto the middle
        // column, which is the defect this pins down.
        let bytes = crate::moc3::tests::Builder::new().warp_divisions(1, 2).build();
        let moc = crate::Moc3::parse(&bytes).expect("should parse");
        let points = moc.pose(&[0.0]).drawables.into_iter().next().expect("one drawable");
        assert_eq!(points, [(0.0, 0.0), (10.0, 0.0), (0.0, 20.0)]);
    }

    #[test]
    fn a_warp_grid_reproduces_its_own_corners() {
        // A 1x1 grid is a quad; looking it up at the corners must return them.
        // Stored column-major, so the second point is the one *below* the
        // first, not the one beside it.
        let grid = vec![(0.0, 0.0), (0.0, 20.0), (10.0, 0.0), (10.0, 20.0)];
        assert_eq!(warp_point(&grid, 1, 1, (0.0, 0.0)), (0.0, 0.0));
        assert_eq!(warp_point(&grid, 1, 1, (1.0, 0.0)), (10.0, 0.0));
        assert_eq!(warp_point(&grid, 1, 1, (0.0, 1.0)), (0.0, 20.0));
        assert_eq!(warp_point(&grid, 1, 1, (1.0, 1.0)), (10.0, 20.0));
        // And the middle is the middle.
        assert_eq!(warp_point(&grid, 1, 1, (0.5, 0.5)), (5.0, 10.0));
    }

    #[test]
    fn a_warp_extends_its_edge_cells_rather_than_clamping() {
        // Geometry that overhangs a deformer should keep its shape.
        let grid = vec![(0.0, 0.0), (0.0, 20.0), (10.0, 0.0), (10.0, 20.0)];
        let out = warp_point(&grid, 1, 1, (2.0, 0.0));
        assert_eq!(out, (20.0, 0.0), "the cell should be extended, not clamped to the border");
    }

    #[test]
    fn a_rotation_frame_places_turns_and_scales() {
        let frame = RotationKeyform { origin: (100.0, 50.0), angle: 90.0, scale: 2.0, opacity: 1.0 };
        let out = rotate_point(&frame, (1.0, 0.0), (1.0, 1.0));
        // Two units along x, turned a quarter turn, from the origin.
        assert!((out.0 - 100.0).abs() < 1e-4, "{out:?}");
        assert!((out.1 - 52.0).abs() < 1e-4, "{out:?}");
    }

    #[test]
    fn an_identity_frame_leaves_a_point_where_it_was() {
        let frame = RotationKeyform { origin: (0.0, 0.0), angle: 0.0, scale: 1.0, opacity: 1.0 };
        let out = rotate_point(&frame, (3.0, -4.0), (1.0, 1.0));
        assert!((out.0 - 3.0).abs() < 1e-6 && (out.1 + 4.0).abs() < 1e-6, "{out:?}");
    }
}
