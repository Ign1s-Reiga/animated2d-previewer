//! Posing a MOC3 model: parameter values in, vertex positions out.
//!
//! # Status: unvalidated against a reference
//!
//! Everything the [`crate::moc3`] reader exposes was checked against an
//! independent source before being believed. **This is not.** Blending weights
//! and deformer application produce finite coordinates in plausible ranges
//! whether or not they are right, so structural checks cannot tell a correct
//! pose from a subtly wrong one. The only thing that can is comparing the
//! result against a known-good viewer, which is what spec §11 calls visual
//! parity, and no reference render exists yet.
//!
//! It is built anyway, and the assumptions are named so the first render can
//! confirm or refute each one individually rather than as a lump:
//!
//! 1. **Keyform ordering.** An element's keyforms form a grid with one axis per
//!    parameter. This assumes the *last* axis varies fastest, as a row-major
//!    array does. If the convention is the other way round, models driven by a
//!    single parameter still pose correctly and multi-parameter ones do not —
//!    which is a distinctive symptom to look for.
//! 2. **Grid orientation.** A warp deformer's divisions are stored as two
//!    numbers with nothing to say which is rows and which is columns. This
//!    takes the first as the axis that `x` runs along. Getting it backwards
//!    transposes the deformation.
//! 3. **Child space.** A deformer's own geometry is expressed in its *parent's*
//!    local space, and a warp's local space is the unit square. This one is no
//!    longer a guess: a warp parented to another warp has a grid inside
//!    `[0, 1]`, while a warp parented to a rotation or to the root has one in
//!    model units running to hundreds. A rotation parented to a warp likewise
//!    has its origin inside the unit square. The model holds.
//! 4. **Rotation composition.** Applied as `origin + rotate(angle) * scale * p`.
//!
//! # The one that is known to be wrong
//!
//! **How a rotation deformer's children are scaled into its frame is not
//! solved.** A drawable under one carries coordinates in the tens — one mesh
//! spans ±23 — and the rotation's own scale is near 1, so composing them
//! overshoots the unit square its parent warp expects by an order of magnitude.
//! Every further warp then extrapolates, and on a deep chain the result runs
//! away.
//!
//! The consequence is visible rather than hidden: [`Pose::unstable`] lists the
//! drawables whose chain produced something unusable, and those keep their
//! un-deformed coordinates instead. On the model this was built against that is
//! 1 drawable of 601 — but the other 600 are not thereby *right*, only finite.
//! The posed extent comes out around ten thousand units against a canvas of
//! under two, which is the same error showing up quietly instead of loudly.
//!
//! So this is machinery, not a working pose. What it gives is somewhere for the
//! missing scale rule to be dropped in once a reference render exists to
//! measure it against.
//!
//! Where a model exercises none of the ambiguous cases — one parameter, one
//! deformer — the result follows from the data alone and the assumptions do not
//! arise. The unit tests below are all of that kind, so they check the maths
//! rather than the conventions.

use crate::moc3::{Deformer, DeformerKind, KeyformBinding, Moc3, RotationKeyform};

/// A posed model: where every drawable's vertices ended up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose {
    /// One entry per drawable, in the model's own order, holding `x, y` pairs
    /// in canvas units.
    pub drawables: Vec<Vec<(f32, f32)>>,
    /// Drawables whose deformer chain produced something unusable, and which
    /// therefore hold their own coordinates rather than a posed result.
    ///
    /// This is not a formality: on a real model it is currently non-zero, and
    /// what it counts is the assumption named in the module docs that is still
    /// wrong. A caller should treat a non-zero count as "this pose is not
    /// finished" rather than as a rounding detail.
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

        let mut unstable = Vec::new();
        let mut drawables = Vec::with_capacity(self.drawables.len());
        for (index, d) in self.drawables.iter().enumerate() {
            let weights = self.weights_for(d.keyform_binding, &resolved);
            let mut points = vec![(0.0f32, 0.0f32); d.vertex_count()];
            for (keyform, weight) in &weights {
                let Some(source) = self.keyforms.drawable(d.keyform_begin as usize + keyform, points.len()) else {
                    continue;
                };
                for (out, pair) in points.iter_mut().zip(source.chunks_exact(2)) {
                    out.0 += pair[0] * weight;
                    out.1 += pair[1] * weight;
                }
            }

            // Keep the un-deformed coordinates: if the chain produces something
            // unusable they are what is reported, so a caller never receives a
            // non-finite vertex to hand to a renderer.
            let local = points.clone();
            self.carry_up(d.parent_deformer, &mut points, &warps, &rotations);
            if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
                points = local;
                unstable.push(index);
            }
            drawables.push(points);
        }

        Pose { drawables, unstable }
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
    fn carry_up(&self, from: u32, points: &mut [(f32, f32)], warps: &[Vec<(f32, f32)>], rotations: &[RotationKeyform]) {
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
                        let (a, b) = shape.divisions;
                        for point in points.iter_mut() {
                            *point = warp_point(grid, a as usize, b as usize, *point);
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
                        for point in points.iter_mut() {
                            *point = rotate_point(frame, *point);
                        }
                    }
                }
            }
            at = deformer.parent;
        }
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

    let corner = |cx: usize, cy: usize| grid[cy * cols + cx];
    let (a, b, c, d) = (corner(ix, iy), corner(ix + 1, iy), corner(ix, iy + 1), corner(ix + 1, iy + 1));
    let top = (a.0 + (b.0 - a.0) * tx, a.1 + (b.1 - a.1) * tx);
    let bottom = (c.0 + (d.0 - c.0) * tx, c.1 + (d.1 - c.1) * tx);
    (top.0 + (bottom.0 - top.0) * ty, top.1 + (bottom.1 - top.1) * ty)
}

/// Places a point in a rotation deformer's frame.
fn rotate_point(frame: &RotationKeyform, point: (f32, f32)) -> (f32, f32) {
    let (sin, cos) = frame.angle.to_radians().sin_cos();
    let x = point.0 * frame.scale;
    let y = point.1 * frame.scale;
    (frame.origin.0 + x * cos - y * sin, frame.origin.1 + x * sin + y * cos)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_warp_grid_reproduces_its_own_corners() {
        // A 1x1 grid is a quad; looking it up at the corners must return them.
        let grid = vec![(0.0, 0.0), (10.0, 0.0), (0.0, 20.0), (10.0, 20.0)];
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
        let grid = vec![(0.0, 0.0), (10.0, 0.0), (0.0, 20.0), (10.0, 20.0)];
        let out = warp_point(&grid, 1, 1, (2.0, 0.0));
        assert_eq!(out, (20.0, 0.0), "the cell should be extended, not clamped to the border");
    }

    #[test]
    fn a_rotation_frame_places_turns_and_scales() {
        let frame = RotationKeyform { origin: (100.0, 50.0), angle: 90.0, scale: 2.0, opacity: 1.0 };
        let out = rotate_point(&frame, (1.0, 0.0));
        // Two units along x, turned a quarter turn, from the origin.
        assert!((out.0 - 100.0).abs() < 1e-4, "{out:?}");
        assert!((out.1 - 52.0).abs() < 1e-4, "{out:?}");
    }

    #[test]
    fn an_identity_frame_leaves_a_point_where_it_was() {
        let frame = RotationKeyform { origin: (0.0, 0.0), angle: 0.0, scale: 1.0, opacity: 1.0 };
        let out = rotate_point(&frame, (3.0, -4.0));
        assert!((out.0 - 3.0).abs() < 1e-6 && (out.1 + 4.0).abs() < 1e-6, "{out:?}");
    }
}
