//! Posing a MOC3 model: parameter values in, vertex positions out.
//!
//! # Status: it poses, and the result has been looked at
//!
//! Everything the [`crate::moc3`] reader exposes was checked against an
//! independent source before being believed. This was checked for a long time
//! only by whether the result *landed* where a character should, and that
//! turned out to be much weaker than it reads: a model can put every drawable
//! at the right size in a plausible frame and still be a quarter turn over.
//!
//! What the chain is now checked by is drawing it with its own texture and
//! looking. Three characters from one source come out as coherent scenes —
//! each figure whole, correctly textured, and placed against its own scenery
//! inside its own canvas. That is not a reference render, but it is the first
//! criterion here that a wrong pose cannot quietly pass.
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
//! # The grid orientation, and how it was found
//!
//! A warp's two division counts arrive with nothing in the layout to say which
//! is rows and which is columns, and for a square grid it cannot matter — the
//! lookup is identical either way. Reading every non-square grid in the same
//! six models both ways settled it: with `divisions.1 + 1` points to a stored
//! row, 713 of 729 such grids are perfectly monotone lattices and most of the
//! rest are coherently mirrored; with `divisions.0 + 1`, the rows interleave
//! and not one grid is monotone.
//!
//! # A correction, and what a determinant can and cannot show
//!
//! These coordinates were once read the other way round — point pairs swapped,
//! rotation angles negated, the lattice transposed — because a drawable's posed
//! geometry appeared *mirrored* against its own texture coordinates. The
//! measurement was sound as far as it went: a mesh is the same shape in uv
//! space as in canvas space, so only a mirror flips the sign of the map between
//! them, and the sign was consistent across three models.
//!
//! The expectation was what was wrong. It assumed one mirror must be present,
//! texture rows running down against `y` running up. But [`a2d_unity`] **flips
//! Unity's bottom-up rows** when it decodes a `Texture2D`, and that flip is
//! what makes the two agree: a correct model fits *positive*, and the reading
//! that looked broken was right all along.
//!
//! Two things are worth keeping from it. A determinant detects a mirror and
//! **nothing else** — it is blind to rotation, so it can never show that a
//! model is the right way up, and treating it as though it could is what turned
//! a correct decoder into a wrong one. And a convention reasoned out from first
//! principles is worth less than the same reasoning checked against the code
//! that runs: the row flip was in a neighbouring crate, had been read earlier
//! the same day, and was still forgotten.
//!
//! What none of this settles is whether a character stands up. The model these
//! were measured on is drawn reclining, head to the right, which is why every
//! attempt to assert that its face should be level produced a wrong answer.
//!
//! # How well it works, measured
//!
//! Landing inside the canvas is now known to be a weak criterion — see the
//! base angle below, where a rule that improved canvas fit on every model
//! visibly wrecked a pose that had been right — so it is reported here as a
//! smoke test rather than as evidence. On six models from one source every
//! drawable poses inside its own canvas, and no chain produces anything
//! unusable, so [`Pose::unstable`] is empty on all six.
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
//! # The base angle, and why aggregates could not find it
//!
//! A rotation deformer carries a constant angle of its own, beside the angle
//! its keyforms blend to; the frame's real angle is the two added. Leaving it
//! out poses every model subtly wrong and one of six catastrophically — its
//! character came out a quarter turn over and outside her own canvas while the
//! scenery beside her, which hangs off the model root rather than off a
//! deformer, stayed exactly where it belonged.
//!
//! That split is what makes the defect findable at all, and every aggregate
//! measure missed it. Drawables came out the right *size* — the map from a
//! mesh's texture coordinates to its posed positions had the same scale in the
//! broken model as in the sound ones — and they landed in a plausible frame,
//! so both of the criteria this module had been trusting passed. What settled
//! it was drawing the model with its own texture and looking: a figure lying
//! sideways in mid-air beside an upright dressing table is not a pose any
//! artist composed.
//!
//! Two criteria were tried on the way and are recorded here because they do
//! *not* work. Fitting a similarity from a mesh's uvs to its posed positions
//! and reading off the **angle** says nothing: across all six models those
//! angles have a mean resultant length of 0.08 to 0.46, so they are barely
//! concentrated at all — Cubism packs art meshes into the atlas at whatever
//! rotation fits. And **canvas fit** is far weaker than it looks: a wrong rule
//! tried here brought every model inside its canvas while visibly wrecking the
//! pose of a model that had been correct.
//!
//! # Keyform ordering
//!
//! An element's keyforms form a grid with one axis per parameter, and the
//! **first** axis varies fastest.
//!
//! Established by fitting each element's keyform values with an additive
//! model, `v[i][j] ~ mu + r_i + c_j`, and comparing the residual between the
//! competing reshapes. A rig's keyform grid is close to additively separable —
//! each parameter contributes its own offset — and the comparison is fair
//! because an `a x b` additive model and a `b x a` one have the same number of
//! free parameters over the same number of samples. That is what an earlier
//! smoothness argument got wrong: it counted neighbour pairs, and unequal axes
//! give the two reshapes different numbers of them.
//!
//! Across six models, 122 of the 128 decidable two-axis grids and all six
//! decidable three-axis grids come out first-axis-fastest, with a mean margin
//! of 0.38 against 0.06 for the exceptions. Grids whose axes are all the same
//! length cannot decide it either way — the two reshapes are transposes and
//! the fit is identical — so only unequal axes count.
//!
//! # What is still unverified
//!
//! **No reference runtime has been compared against.** The models now render
//! as coherent scenes, which is much stronger than landing in the right frame,
//! but a picture that looks right cannot show that a particular warp bends the
//! way Live2D bends it, or that blending weights are right *between* keys
//! rather than only on them.
//!
//! **Rotation composition** is applied as
//! `origin + rotate(base + angle) * scale * p / unit`. The unit conversion is
//! the least pinned part: it takes a warp's posed bounding box per axis, which
//! is right for an axis-aligned lattice and approximate for a turned one.
//! Reading the setup grid instead, or forcing the rate isotropic, changes no
//! model measurably, so nothing available here distinguishes them.

use crate::moc3::{CubismIr, Deformer, DeformerKind, KeyformBinding, RotationKeyform};

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

/// Posing, on the normalized model.
///
/// A trait rather than inherent methods, because [`CubismIr`] belongs to
/// `a2d-core` -- a package stores it, and `a2d-pack` may not depend on a
/// format crate -- while the evaluation belongs here, beside the decoder the
/// model was derived from. The orphan rule reserves inherent impls for the
/// crate that defines a type, so this is what "methods on the IR" has to look
/// like. [`Moc3`] derefs to the model, so `moc.pose(..)` still reads the same.
pub trait CubismEval {
    /// Poses the model at the given parameter values.
    ///
    /// `values` is indexed by parameter, in the model's own order; anything
    /// short is filled from the parameters' defaults, and every value is
    /// clamped into its parameter's range before use.
    fn pose(&self, values: &[f32]) -> Pose;

    /// The deformer chain above an element, root last.
    fn deformer_chain(&self, from: u32) -> Vec<&Deformer>;
}

/// The parts of posing that are not API.
///
/// Kept off [`CubismEval`] so that having the model does not mean publishing
/// how it is walked; a trait only because the helpers call each other through
/// `self` and the type is foreign.
trait EvalInternals {
    fn resolve_values(&self, values: &[f32]) -> Vec<f32>;
    fn weights_for(&self, binding: u32, values: &[f32]) -> Vec<(usize, f32)>;
    fn carry_up(
        &self,
        from: Option<u32>,
        points: &mut [(f32, f32)],
        warps: &[Vec<(f32, f32)>],
        rotations: &[RotationKeyform],
        units: &[(f32, f32)],
    );
    fn warp_units(&self, grids: &[Vec<(f32, f32)>]) -> Vec<(f32, f32)>;
}

impl CubismEval for CubismIr {
    /// Poses the model at the given parameter values.
    ///
    /// `values` is indexed by parameter, in the model's own order; anything
    /// short is filled from the parameters' defaults, and every value is
    /// clamped into its parameter's range before use.
    fn pose(&self, values: &[f32]) -> Pose {
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
                // The keyforms are measured from the deformer's base angle, so
                // the frame's real angle is the two together. The base is a
                // constant of the deformer, not of a keyform, so it is added
                // once rather than blended.
                out.angle += r.base_angle;
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
            // The artist's animated value, which rests at 500 where a model
            // carries no track for it.
            let mut order = if self.drawable_keyform_draw_orders.is_empty() { 500.0 } else { 0.0 };
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

            // A MOC3 stores y running down the canvas, the way an image's rows
            // do; everything downstream of `formats/` takes y as running up.
            // Converting here rather than at emit keeps `bounds` and `emit`
            // agreeing, and keeps the whole chain above -- deformer grids,
            // rotation frames, keyform blending -- working in the file's own
            // space, where the numbers mean what the format says they mean.
            for point in &mut points {
                point.1 = -point.1;
            }
            drawables.push(points);
        }

        Pose { drawables, opacities, draw_orders, unstable }
    }
    /// The deformer chain above an element, root last.
    fn deformer_chain(&self, from: u32) -> Vec<&Deformer> {
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

impl EvalInternals for CubismIr {
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

        // The first axis varies fastest, so its stride is one.
        let mut strides = vec![1usize; sizes.len()];
        for i in 1..sizes.len() {
            strides[i] = strides[i - 1] * sizes[i - 1];
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
        from: Option<u32>,
        points: &mut [(f32, f32)],
        warps: &[Vec<(f32, f32)>],
        rotations: &[RotationKeyform],
        units: &[(f32, f32)],
    ) {
        // `None` is not a special case to handle: a drawable with no parent
        // deformer is already in model space, so the walk simply does not run.
        let mut at = from;
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
                        let (rows, columns) = shape.divisions;
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
    /// Converts an expectation written in the file's own space.
    ///
    /// The fixture's keyforms are authored the way a MOC3 stores them, with y
    /// running down; `pose` hands back the upward y everything downstream uses.
    /// Keeping the expectations in the authored space and converting them here
    /// means each test still reads against the numbers the fixture contains.
    fn up(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
        points.iter().map(|(x, y)| (*x, -*y)).collect()
    }

    fn posed(angle: f32) -> Vec<(f32, f32)> {
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        moc.pose(&[angle]).drawables.into_iter().next().expect("one drawable")
    }

    #[test]
    fn a_parameter_on_a_key_poses_that_keyform_outright() {
        // At zero the grid is the unit quad, so the drawable's corners land on
        // it: (0,0), (1,0) and (0,1) of the square map to the quad's corners.
        let points = posed(0.0);
        assert_eq!(points, up(&[(0.0, 0.0), (10.0, 0.0), (0.0, 20.0)]));
    }

    #[test]
    fn a_parameter_between_keys_blends_the_keyforms_around_it() {
        // Half way from key 0 to key 30 blends a unit quad with a doubled one.
        let points = posed(15.0);
        assert_eq!(points, up(&[(0.0, 0.0), (15.0, 0.0), (0.0, 30.0)]));
    }

    #[test]
    fn a_parameter_past_its_range_is_clamped_before_it_is_used() {
        // The parameter's own maximum is 30, so anything beyond poses as 30.
        assert_eq!(posed(999.0), posed(30.0));
        assert_eq!(posed(30.0), up(&[(0.0, 0.0), (20.0, 0.0), (0.0, 40.0)]));
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

    /// The two-axis fixture, posed at exact key values, with the drawable
    /// parented straight to the model root so nothing but the keyform blend
    /// stands between the stored numbers and the result.
    ///
    /// Its axes are a two-key one first and a three-key one second, and its
    /// keyform `k` reaches `k + 1` along each side of the triangle, so the
    /// vertex that comes back names the keyform the blend chose.
    fn keyform_reach(first: f32, second: f32) -> f32 {
        let bytes = crate::moc3::tests::Builder::new().two_axis().drawable_parents(&[u32::MAX]).build();
        let moc = crate::Moc3::parse(&bytes).expect("should parse");
        let points = moc.pose(&[first, second]).drawables.into_iter().next().expect("one drawable");
        points[1].0
    }

    #[test]
    fn the_first_axis_of_a_keyform_grid_is_the_one_that_varies_fastest() {
        // Axis lengths 2 and 3, so index = i0 + i1 * 2 with the first axis
        // fastest, and i0 * 3 + i1 with the last. Reach is the keyform index
        // plus one, so the two readings are distinguishable at every corner
        // where the indices differ.
        assert_eq!(keyform_reach(-30.0, 0.0), 1.0, "keyform 0 is the near corner either way");
        // i0 = 1, i1 = 0: first-fastest picks keyform 1, last-fastest 3.
        assert_eq!(keyform_reach(30.0, 0.0), 2.0);
        // i0 = 0, i1 = 2: first-fastest picks keyform 4, last-fastest 2.
        assert_eq!(keyform_reach(-30.0, 1.2), 5.0);
        // i0 = 1, i1 = 2: the far corner, which both readings agree on.
        assert_eq!(keyform_reach(30.0, 1.2), 6.0);
    }

    #[test]
    fn a_rotation_deformer_turns_by_its_base_angle_as_well_as_its_keyforms() {
        // The fixture's rotation deformer sits at the root with an identity
        // frame, so its base angle is the only rotation in the chain and the
        // quad it carries has to come back turned by exactly that much.
        let turned = |degrees: f32| {
            let bytes = crate::moc3::tests::Builder::new().rotation_base_angle(degrees).build();
            let moc = crate::Moc3::parse(&bytes).expect("should parse");
            moc.pose(&[0.0]).drawables.into_iter().next().expect("one drawable")
        };
        // Without one, the fixture poses as every other test here expects.
        assert_eq!(turned(0.0), up(&[(0.0, 0.0), (10.0, 0.0), (0.0, 20.0)]));
        // A quarter turn in the file's downward-y space sends +x to +y.
        let quarter = turned(90.0);
        assert!((quarter[1].0 - 0.0).abs() < 1e-4, "{quarter:?}");
        assert!((quarter[1].1 + 10.0).abs() < 1e-4, "{quarter:?}");
        assert!((quarter[2].0 + 20.0).abs() < 1e-4, "{quarter:?}");
        assert!((quarter[2].1 - 0.0).abs() < 1e-3, "{quarter:?}");
    }

    #[test]
    fn a_model_that_carries_no_base_angle_section_poses_as_it_always_did() {
        // The section is read optionally, so a layout without it must leave
        // every frame exactly where it was rather than refusing the model.
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        assert!(moc.rotation_deformers.iter().all(|r| r.base_angle == 0.0));
        assert_eq!(moc.pose(&[0.0]).drawables, vec![posed(0.0)]);
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
        assert_eq!(points, up(&[(0.0, 0.0), (10.0, 0.0), (0.0, 20.0)]));
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
