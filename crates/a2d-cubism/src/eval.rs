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
//! # How deformers compose
//!
//! Deformers are resolved into canvas space **parents first**, and a drawable
//! is then carried through its one resolved parent only. Keyform blending is
//! unchanged; what changed is how a deformer is placed under its parent.
//!
//! * A **warp** carries its own grid through its parent, so its lookup lands in
//!   canvas space. It adds no scale of its own: its total scale is its
//!   parent's, and one at the root.
//! * A **rotation under a rotation** sends its origin through the parent,
//!   adds the angles, and multiplies the scales.
//! * A **rotation under a warp** takes only a *position* and an *angle* from
//!   the warp. The origin goes through the warp. The turn is read from a short
//!   probe along the warp's own up axis, compared with where an unbent warp
//!   would point it. The total scale is the warp's (its parent's) times the
//!   rotation's own, so **a warp lends no scale at all** and everything below
//!   such a rotation stays rigid.
//!   If the warp is squashed so flat that no probe has a direction, the rotation
//!   keeps its origin and its own angle and the model keeps posing.
//!
//! A point in a rotation's frame lands at `origin + R(angle) * (p * total
//! scale)`. A rotation's total scale is the product of every rotation scale
//! from the root down to it, with warps contributing one.
//!
//! ## What established it
//!
//! The Cubism Editor manual says of a rotation under a warp that moving the
//! warp's control points makes the rotation and its children "move and rotate
//! rather than deform". That is the rule above, stated by the authoring tool.
//! Two measurements on real models agree with it and disagree with the rule
//! this replaced:
//!
//! * **Texture proportions.** Every rigid mesh -- one keyform, hung off a
//!   rotation -- is an exact similarity of its own texture layout. Posed under
//!   the rule above it stays one, to within `1e-5` on three real models. Under
//!   the old rule a hem panel came out 4.2:1, and fabric strands stretched and
//!   contracted by up to seven times during a motion.
//! * **Twin meshes.** Some models carry two copies of the same local mesh on
//!   independent chains, one through several warps and rotations and one under
//!   a root warp. Their posed sizes agree exactly under this rule and differ by
//!   up to 14% under the old one.
//!
//! The old rule converted units: a rotation under a warp measured the warp's
//! grid extent per axis, and pushed its children through the warp's bilinear
//! lookup. It was found by reading one model's deformer tree, where a rotation
//! sits at `(0.501, 0.500)` -- the centre of a unit square -- while its child
//! is back at pixel coordinates. That observation is still right and is
//! explained by this rule: the rotation's origin is a position *in the warp*
//! and its children are in the rotation's own space. What was wrong was the
//! conclusion that the child space is rescaled by the warp. It was
//! measurable only against the texture, not against canvas fit.
//!
//! ## What is not verified
//!
//! * The probe length. The direction vector is read at `0.1` of the warp's unit
//!   square, retried from the other side and then ten times shorter, up to ten
//!   times, which is recalled from the Cubism 2 SDK and not re-read. Moving it
//!   to `0.01` changes a vertex by at most `1.1e-3` canvas units on the worst
//!   model measured and `1e-6` on the others.
//! * Warp inside warp. The child's grid is carried through the parent (the SDK's
//!   structure as recalled) rather than each point being walked through both
//!   warps. They differ by up to `0.02` canvas units at rest and `0.06` in
//!   motion on the worst model. Texture agreement leans weakly towards carrying
//!   the grid, and glue closure at rest is identical.
//! * Glue that has a gap at rest. A few glues do not coincide at rest (the
//!   widest is `0.3` canvas units on one model) and no choice of parameters
//!   closes the worst one. Glue pulls such a pair together, which can still
//!   stretch a mesh. Whether Cubism shows the same pull needs a reference.
//! * Glue toward a hidden mesh. A glue is applied whatever the opacity of
//!   either mesh. On one model that drags a visible mesh 0.6 canvas units
//!   towards a fully transparent, static partner. Whether Cubism skips it is
//!   unknown.
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
//! # Glue
//!
//! Meshes that were one surface before the artist split them hang off
//! unrelated deformer chains, so as parameters move the pieces drift apart and
//! a transparent gap opens along the seam. A *glue* stitches chosen vertex
//! pairs back together, and it is applied last, after every drawable has been
//! carried up its chain and before the y flip: see [`CubismEval::pose`] and
//! the rule in `a2d_core::ir::cubism::Glue`. Found by reproducing the gap from
//! parameters alone, with no motion involved, and by the pairs coinciding
//! exactly at the default pose on every glue of a real model. The rule closes
//! the gap in rendered output; it has not been compared with a reference
//! runtime, and only an intensity of one has been seen in real data.
//!
//! A pair closes by `I * (wa + wb)` of its gap, not always all of it. Weights
//! summing below one are real data (seen on two of three real models): the
//! Cubism Editor lets an artist partly or fully release glued vertices
//! (<https://docs.live2d.com/en/cubism-editor-manual/glue>), so such a pair is
//! left open on purpose and is not a decoding error. One vertex was also seen
//! in two glues; file order is applied and is unverified. Which weight belongs
//! to which mesh is supported by seam geometry (a pair's share `wa / (wa + wb)`
//! rises towards mesh B in all 282 glues whose weights vary, and swapping the
//! weights raises triangle flips) but not by a reference runtime. Open: a glue
//! can pull a visible mesh towards a fully transparent partner, and it is not
//! known whether Cubism skips such a glue, as this code does not.
//!
//! # What is still unverified
//!
//! **No reference runtime has been compared against.** The models now render
//! as coherent scenes, which is much stronger than landing in the right frame,
//! but a picture that looks right cannot show that a particular warp bends the
//! way Live2D bends it, or that blending weights are right *between* keys
//! rather than only on them.
//!
//! **Rotation composition** is the rule at the top of this module. Its
//! open points are listed there: the probe length, warp-in-warp order, and
//! glue that has a gap at rest.

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
    fn resolve_deformers(&self, warps: &[Vec<(f32, f32)>], rotations: &[RotationKeyform]) -> Vec<Resolved>;
    fn apply_glue(&self, drawables: &mut [Vec<(f32, f32)>], unstable: &[usize], values: &[f32]);
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

        let resolved_deformers = self.resolve_deformers(&warps, &rotations);

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
            let parent = d.parent_deformer.and_then(|p| resolved_deformers.get(p as usize));
            if let Some(parent) = parent {
                for point in points.iter_mut() {
                    *point = parent.map(*point);
                }
            }
            if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
                points = local;
                unstable.push(index);
            }

            drawables.push(points);
        }

        // Glue runs last of everything that moves a vertex: it joins positions
        // that only exist once every drawable has been carried up its own
        // chain, and it is what closes the seams those chains open.
        self.apply_glue(&mut drawables, &unstable, &resolved);

        // A MOC3 stores y running down the canvas, the way an image's rows
        // do; everything downstream of `formats/` takes y as running up.
        // Converting here rather than at emit keeps `bounds` and `emit`
        // agreeing, and keeps the whole chain above -- deformer grids,
        // rotation frames, keyform blending -- working in the file's own
        // space, where the numbers mean what the format says they mean.
        for points in &mut drawables {
            for point in points {
                point.1 = -point.1;
            }
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

    /// Pulls each glue's vertex pairs together.
    ///
    /// The intensity is blended through the glue's own keyform binding, with
    /// the same weights machinery as any other keyform, then for every pair
    /// `A' = A + (B - A) * wa * I` and `B' = B + (A - B) * wb * I`, both read
    /// before either is written. Glues run in file order, each seeing the
    /// result of the ones before it.
    ///
    /// A glue is skipped when either mesh is unstable: such a mesh holds its
    /// own un-deformed coordinates, which are in the wrong space to join to.
    fn apply_glue(&self, drawables: &mut [Vec<(f32, f32)>], unstable: &[usize], values: &[f32]) {
        for glue in &self.glues {
            let (a, b) = (glue.mesh_a as usize, glue.mesh_b as usize);
            if unstable.contains(&a) || unstable.contains(&b) {
                continue;
            }
            let mut intensity = 0.0f32;
            for (keyform, weight) in self.weights_for(glue.keyform_binding, values) {
                if let Some(i) = self.glue_intensities.get(glue.keyform_begin as usize + keyform) {
                    intensity += i * weight;
                }
            }
            if intensity == 0.0 || !intensity.is_finite() {
                continue;
            }
            for pair in &glue.pairs {
                let (va, vb) = (pair.vertex_a as usize, pair.vertex_b as usize);
                let (Some(pa), Some(pb)) = (
                    drawables.get(a).and_then(|m| m.get(va)).copied(),
                    drawables.get(b).and_then(|m| m.get(vb)).copied(),
                ) else {
                    continue;
                };
                let (ka, kb) = (pair.weight_a * intensity, pair.weight_b * intensity);
                let moved_a = (pa.0 + (pb.0 - pa.0) * ka, pa.1 + (pb.1 - pa.1) * ka);
                let moved_b = (pb.0 + (pa.0 - pb.0) * kb, pb.1 + (pa.1 - pb.1) * kb);
                // Weights are only required to be finite, so a huge one can
                // overflow here. A pair that would produce a non-finite vertex
                // is left where it was rather than handed on to emit.
                let finite = |p: (f32, f32)| p.0.is_finite() && p.1.is_finite();
                if !finite(moved_a) || !finite(moved_b) {
                    continue;
                }
                if let Some(slot) = drawables.get_mut(a).and_then(|m| m.get_mut(va)) {
                    *slot = moved_a;
                }
                if let Some(slot) = drawables.get_mut(b).and_then(|m| m.get_mut(vb)) {
                    *slot = moved_b;
                }
            }
        }
    }

    /// Resolves every deformer into canvas space, parents first.
    ///
    /// See the module doc for the rule. The result is indexed like
    /// `self.deformers`. A deformer whose parent index points nowhere is
    /// resolved as a root, the same as one with no parent.
    fn resolve_deformers(&self, warps: &[Vec<(f32, f32)>], rotations: &[RotationKeyform]) -> Vec<Resolved> {
        let count = self.deformers.len();
        let mut done: Vec<Option<Resolved>> = (0..count).map(|_| None).collect();
        for start in 0..count {
            if done[start].is_some() {
                continue;
            }
            // Walk up to the first resolved ancestor (or the root), then
            // resolve back down. The tree is checked acyclic on parse; the
            // length cap is belt and braces against that check going away.
            let mut chain = Vec::new();
            let mut at = Some(start);
            while let Some(i) = at {
                if i >= count || done[i].is_some() || chain.len() > count {
                    break;
                }
                chain.push(i);
                at = self.deformers[i].parent.map(|p| p as usize);
            }
            for &i in chain.iter().rev() {
                let deformer = &self.deformers[i];
                let parent = deformer.parent.and_then(|p| done.get(p as usize)).and_then(|p| p.as_ref());
                let resolved = match deformer.kind {
                    DeformerKind::Warp(w) => {
                        let shape = self.warp_deformers.get(w as usize);
                        resolve_warp(warps.get(w as usize), shape.map(|s| s.divisions), parent)
                    }
                    DeformerKind::Rotation(r) => resolve_rotation(rotations.get(r as usize), parent),
                };
                done[i] = Some(resolved);
            }
        }
        done.into_iter().map(|r| r.unwrap_or_else(Resolved::identity)).collect()
    }
}

/// A deformer resolved into canvas space.
#[derive(Debug, Clone)]
struct Resolved {
    frame: Frame,
    /// The product of every rotation's scale from the root down to here. A warp
    /// adds none of its own: it passes its parent's through.
    total_scale: f32,
}

#[derive(Debug, Clone)]
enum Frame {
    /// A warp, with its grid already carried into canvas space.
    Warp { grid: Vec<(f32, f32)>, columns: usize, rows: usize },
    /// A rotation, with its origin in canvas space and its angle in degrees,
    /// the sum of everything above it.
    Rotation { origin: (f32, f32), angle: f32 },
    /// Maps a point to itself: a deformer that could not be found.
    Identity,
}

impl Resolved {
    fn identity() -> Self {
        Self { frame: Frame::Identity, total_scale: 1.0 }
    }

    /// Carries a point from this deformer's child space into canvas space.
    fn map(&self, point: (f32, f32)) -> (f32, f32) {
        match &self.frame {
            Frame::Warp { grid, columns, rows } => warp_point(grid, *columns, *rows, point),
            Frame::Rotation { origin, angle } => rotate_point(*origin, *angle, self.total_scale, point),
            Frame::Identity => point,
        }
    }
}

fn resolve_warp(grid: Option<&Vec<(f32, f32)>>, divisions: Option<(u32, u32)>, parent: Option<&Resolved>) -> Resolved {
    // The second division count is the one that runs along a stored row (see
    // `WarpDeformer::divisions`), so it is the number of columns.
    let (rows, columns) = divisions.unwrap_or((0, 0));
    let local: &[(f32, f32)] = grid.map(|g| g.as_slice()).unwrap_or(&[]);
    let (grid, total_scale) = match parent {
        Some(p) => (local.iter().map(|g| p.map(*g)).collect(), p.total_scale),
        None => (local.to_vec(), 1.0),
    };
    Resolved { frame: Frame::Warp { grid, columns: columns as usize, rows: rows as usize }, total_scale }
}

/// How far along a warp's own up axis the probe that reads a rotation's turn
/// reaches at first, in the warp's unit square.
const TURN_PROBE: f32 = 0.1;

fn resolve_rotation(key: Option<&RotationKeyform>, parent: Option<&Resolved>) -> Resolved {
    let Some(key) = key else { return Resolved::identity() };
    let Some(p) = parent else {
        return Resolved { frame: Frame::Rotation { origin: key.origin, angle: key.angle }, total_scale: key.scale };
    };
    let (origin, angle) = match &p.frame {
        Frame::Rotation { angle: parent_angle, .. } => (p.map(key.origin), key.angle + parent_angle),
        Frame::Warp { .. } => {
            let origin = p.map(key.origin);
            // Only the warp's position and orientation reach a rotation below
            // it, so the turn is read off a short probe along the warp's own
            // up axis and compared with where that probe would point in an
            // unbent warp. A probe that collapses onto the origin is retried
            // from the other side, then shorter.
            let mut reach = TURN_PROBE;
            let mut found = None;
            for _ in 0..10 {
                let forward = p.map((key.origin.0, key.origin.1 - reach));
                let d = (forward.0 - origin.0, forward.1 - origin.1);
                if d != (0.0, 0.0) {
                    found = Some(d);
                    break;
                }
                let back = p.map((key.origin.0, key.origin.1 + reach));
                let d = (origin.0 - back.0, origin.1 - back.1);
                if d != (0.0, 0.0) {
                    found = Some(d);
                    break;
                }
                reach *= 0.1;
            }
            // A warp squashed flat has no direction to offer. The rotation then
            // keeps its place and its own angle and the model keeps posing, as
            // the SDK does; non-finite results are caught per drawable.
            match found {
                Some(d) => (origin, key.angle + d.1.atan2(d.0).to_degrees() + 90.0),
                None => (origin, key.angle),
            }
        }
        Frame::Identity => (key.origin, key.angle),
    };
    Resolved { frame: Frame::Rotation { origin, angle }, total_scale: p.total_scale * key.scale }
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

/// Places a point in a rotation deformer's frame: `origin + R(angle) * (p * scale)`.
///
/// `scale` is the deformer's total scale, the product down the rotation chain.
fn rotate_point(origin: (f32, f32), angle: f32, scale: f32, point: (f32, f32)) -> (f32, f32) {
    let (sin, cos) = angle.to_radians().sin_cos();
    let x = point.0 * scale;
    let y = point.1 * scale;
    (origin.0 + x * cos - y * sin, origin.1 + x * sin + y * cos)
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

    /// Two triangles sharing a seam. Drawable 0 hangs off the model root and
    /// never moves; drawable 1 hangs off a rotation deformer that ParamAngleX
    /// turns by exactly that many degrees (keys -30, 0, 30). So at zero the
    /// two coincide, and anywhere else the second swings away from the first.
    ///
    /// The glue stitches vertex 1 to vertex 1 with weights 0.25 and 0.75, and
    /// vertex 2 to vertex 2 with 0.5 and 0.5, at the given intensity per key.
    fn glued_pair(intensities: &[f32]) -> crate::Moc3 {
        let bytes = crate::moc3::tests::Builder::new()
            .glued(intensities)
            .rotation_angles(&[-30.0, 0.0, 30.0])
            .drawable_parents(&[u32::MAX, 0])
            .build();
        crate::Moc3::parse(&bytes).expect("should parse")
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn glued_vertices_that_coincide_at_rest_stay_together() {
        let moc = glued_pair(&[1.0; 3]);
        let pose = moc.pose(&[0.0]);
        assert!(close(pose.drawables[0][1], pose.drawables[1][1]), "{:?}", pose.drawables);
        assert!(close(pose.drawables[0][2], pose.drawables[1][2]), "{:?}", pose.drawables);
    }

    #[test]
    fn glue_closes_the_gap_a_turned_deformer_opens() {
        let mut moc = glued_pair(&[1.0; 3]);
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());

        // Without the glue the second triangle has swung off the first: the
        // premise of the test, asserted rather than assumed.
        let glued = moc.glues.clone();
        moc.glues.clear();
        let raw = moc.pose(&[30.0]);
        let gap = |p: &Pose, v: usize| {
            let (a, b) = (p.drawables[0][v], p.drawables[1][v]);
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        };
        assert!(gap(&raw, 1) > 0.1 && gap(&raw, 2) > 0.1, "{:?}", raw.drawables);

        // With it, both ends of each pair land on the weighted point. Pair one
        // has weights 0.25 for A and 0.75 for B, so A' = A + 0.25 (B - A) and
        // B' = B + 0.75 (A - B), which are the same point, 0.75 A + 0.25 B.
        // In the file's space A = (1, 0) and B = (cos 30, sin 30); the pose
        // then flips y.
        moc.glues = glued;
        let pose = moc.pose(&[30.0]);
        let expect1 = (0.75 * 1.0 + 0.25 * c, -(0.25 * s));
        assert!(close(pose.drawables[0][1], expect1), "{:?} vs {expect1:?}", pose.drawables[0][1]);
        assert!(close(pose.drawables[1][1], expect1), "{:?} vs {expect1:?}", pose.drawables[1][1]);
        // Pair two is 0.5 and 0.5: the midpoint of (0, 1) and (-sin 30, cos 30).
        let expect2 = (0.5 * (0.0 - s), -(0.5 * (1.0 + c)));
        assert!(close(pose.drawables[0][2], expect2), "{:?} vs {expect2:?}", pose.drawables[0][2]);
        assert!(close(pose.drawables[1][2], expect2), "{:?} vs {expect2:?}", pose.drawables[1][2]);

        // Vertices no pair names are left alone.
        assert_eq!(pose.drawables[0][0], raw.drawables[0][0]);
        assert_eq!(pose.drawables[1][0], raw.drawables[1][0]);
    }

    #[test]
    fn a_half_intensity_glue_pulls_half_as_far() {
        // The same pose at intensity one and at one half, from the same raw
        // positions: the pull of every glued vertex must halve.
        let full = glued_pair(&[1.0, 1.0, 1.0]).pose(&[30.0]);
        let half_moc = glued_pair(&[1.0, 1.0, 0.5]);
        let half = half_moc.pose(&[30.0]);
        let mut raw_moc = glued_pair(&[1.0; 3]);
        raw_moc.glues.clear();
        let raw = raw_moc.pose(&[30.0]);

        for (mesh, vertex) in [(0, 1), (1, 1), (0, 2), (1, 2)] {
            let pull = |p: &Pose| {
                let (a, b) = (p.drawables[mesh][vertex], raw.drawables[mesh][vertex]);
                (a.0 - b.0, a.1 - b.1)
            };
            let (f, h) = (pull(&full), pull(&half));
            assert!(f.0.abs() + f.1.abs() > 0.01, "the premise is a glue that moves something");
            assert!(close((h.0 * 2.0, h.1 * 2.0), f), "mesh {mesh} vertex {vertex}: {h:?} vs {f:?}");
        }
        // The two meshes no longer coincide at half intensity.
        assert!(!close(half.drawables[0][1], half.drawables[1][1]));

        // Pair one at intensity one half, worked by hand: A = (1, 0) and
        // B = (cos 30, sin 30) before the flip, so
        // A' = A + 0.25 * 0.5 * (B - A) and B' = B + 0.75 * 0.5 * (A - B).
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
        let a = (1.0 + 0.125 * (c - 1.0), -(0.125 * s));
        let b = (c + 0.375 * (1.0 - c), -(s + 0.375 * (0.0 - s)));
        assert!(close(half.drawables[0][1], a), "{:?} vs {a:?}", half.drawables[0][1]);
        assert!(close(half.drawables[1][1], b), "{:?} vs {b:?}", half.drawables[1][1]);
    }

    #[test]
    fn intensity_blends_between_keys_like_any_other_keyform() {
        // Keys at 0 and 30 carry intensities 1 and 0, so 15 degrees sits
        // halfway and pulls half as far as the full-intensity pose would.
        let moc = glued_pair(&[1.0, 1.0, 0.0]);
        let mut raw_moc = glued_pair(&[1.0; 3]);
        raw_moc.glues.clear();
        let at_30 = moc.pose(&[30.0]);
        let raw_30 = raw_moc.pose(&[30.0]);
        assert_eq!(at_30.drawables, raw_30.drawables, "intensity zero is no glue at all");
        let mid = moc.pose(&[15.0]);
        let mid_raw = raw_moc.pose(&[15.0]);
        // At 15 the intensity is 0.5 and the pair-one weights apply to it.
        let a = (
            mid_raw.drawables[0][1].0 + (mid_raw.drawables[1][1].0 - mid_raw.drawables[0][1].0) * 0.25 * 0.5,
            mid_raw.drawables[0][1].1 + (mid_raw.drawables[1][1].1 - mid_raw.drawables[0][1].1) * 0.25 * 0.5,
        );
        assert!(close(mid.drawables[0][1], a), "{:?} vs {a:?}", mid.drawables[0][1]);
    }

    /// The glue-cleared pose at 30 degrees, for the tests below.
    fn raw_at_30() -> Pose {
        let mut raw_moc = glued_pair(&[1.0; 3]);
        raw_moc.glues.clear();
        raw_moc.pose(&[30.0])
    }

    #[test]
    fn a_partly_released_pair_closes_only_its_share() {
        // Weights summing below one are real data (the editor lets an artist
        // release glued vertices), so the pair must close by exactly
        // I * (wa + wb) of its gap. Pair one gets 0.3 and 0.2 here.
        let mut moc = glued_pair(&[1.0; 3]);
        moc.glues[0].pairs[0].weight_a = 0.3;
        moc.glues[0].pairs[0].weight_b = 0.2;
        let raw = raw_at_30();
        let pose = moc.pose(&[30.0]);
        // A = (1, 0) and B = (cos 30, sin 30) in the file's space; y flips.
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
        let (a, b) = ((1.0, 0.0f32), (c, -s));
        let expect_a = (a.0 + 0.3 * (b.0 - a.0), a.1 + 0.3 * (b.1 - a.1));
        let expect_b = (b.0 + 0.2 * (a.0 - b.0), b.1 + 0.2 * (a.1 - b.1));
        assert!(close(pose.drawables[0][1], expect_a), "{:?} vs {expect_a:?}", pose.drawables[0][1]);
        assert!(close(pose.drawables[1][1], expect_b), "{:?} vs {expect_b:?}", pose.drawables[1][1]);
        // The gap left is (1 - 0.5) of the one the pose had without glue.
        let before = dist(raw.drawables[0][1], raw.drawables[1][1]);
        let after = dist(pose.drawables[0][1], pose.drawables[1][1]);
        assert!(before > 0.1, "the premise is a gap");
        assert!((after - 0.5 * before).abs() < 1e-5, "{after} vs half of {before}");
    }

    #[test]
    fn a_fully_released_pair_stays_where_its_chains_put_it() {
        let mut moc = glued_pair(&[1.0; 3]);
        moc.glues[0].pairs[0].weight_a = 0.0;
        moc.glues[0].pairs[0].weight_b = 0.0;
        let raw = raw_at_30();
        let pose = moc.pose(&[30.0]);
        assert_eq!(pose.drawables[0][1], raw.drawables[0][1]);
        assert_eq!(pose.drawables[1][1], raw.drawables[1][1]);
        // The other pair of the same glue still closes.
        assert!(close(pose.drawables[0][2], pose.drawables[1][2]));
    }

    #[test]
    fn one_sided_weights_move_only_one_mesh() {
        let mut moc = glued_pair(&[1.0; 3]);
        moc.glues[0].pairs[0].weight_a = 1.0;
        moc.glues[0].pairs[0].weight_b = 0.0;
        let raw = raw_at_30();
        let pose = moc.pose(&[30.0]);
        // A lands exactly on B's un-glued position and B does not move.
        assert!(close(pose.drawables[0][1], raw.drawables[1][1]), "{:?}", pose.drawables);
        assert_eq!(pose.drawables[1][1], raw.drawables[1][1]);
    }

    #[test]
    fn a_vertex_in_two_glues_sees_the_earlier_glues_result() {
        // UNVERIFIED against Cubism: this pins the order the code chose (file
        // order, each glue reading what the ones before it wrote). One real
        // model names a single vertex twice and no one-pass order closes both
        // of its glues, so the rule is a guess; change this test with it.
        let mut moc = glued_pair(&[1.0; 3]);
        let mut second = moc.glues[0].clone();
        // Mesh 1 vertex 1 is already in the first glue; pull it fully onto
        // mesh 0 vertex 2, which is also already named there.
        second.mesh_a = 1;
        second.mesh_b = 0;
        second.pairs = vec![a2d_core::ir::cubism::GluePair { vertex_a: 1, vertex_b: 2, weight_a: 1.0, weight_b: 0.0 }];
        moc.glues.push(second);
        let raw = raw_at_30();
        let pose = moc.pose(&[30.0]);
        // The first glue puts pair two (0.5, 0.5) on the midpoint of its raw
        // positions and pair one on 0.75 A + 0.25 B.
        let (a1, b1) = (raw.drawables[0][1], raw.drawables[1][1]);
        let (a2, b2) = (raw.drawables[0][2], raw.drawables[1][2]);
        let mid2 = (0.5 * (a2.0 + b2.0), 0.5 * (a2.1 + b2.1));
        let p1 = (0.75 * a1.0 + 0.25 * b1.0, 0.75 * a1.1 + 0.25 * b1.1);
        // The second then moves mesh 1 vertex 1 onto what mesh 0 vertex 2 is
        // by then: the midpoint, not its raw position.
        assert!(close(pose.drawables[0][1], p1), "{:?}", pose.drawables);
        assert!(close(pose.drawables[1][1], mid2), "{:?} vs {mid2:?}", pose.drawables[1][1]);
        assert!(close(pose.drawables[0][2], mid2));
        assert!(close(pose.drawables[1][2], mid2));
    }

    #[test]
    fn a_pair_whose_pull_would_overflow_is_left_where_it_was() {
        let mut moc = glued_pair(&[4.0; 3]);
        let mut raw_moc = glued_pair(&[4.0; 3]);
        raw_moc.glues.clear();
        moc.glues[0].pairs[0].weight_a = f32::MAX;
        let pose = moc.pose(&[30.0]);
        let raw = raw_moc.pose(&[30.0]);
        assert!(pose.drawables.iter().flatten().all(|(x, y)| x.is_finite() && y.is_finite()));
        // The overflowing pair is untouched; the healthy one still applies.
        assert_eq!(pose.drawables[0][1], raw.drawables[0][1]);
        assert_eq!(pose.drawables[1][1], raw.drawables[1][1]);
        assert_ne!(pose.drawables[0][2], raw.drawables[0][2]);
    }

    #[test]
    fn a_glue_joining_a_mesh_to_itself_is_applied() {
        let mut moc = glued_pair(&[1.0; 3]);
        moc.glues[0].mesh_b = 0;
        moc.glues[0].pairs = vec![crate::moc3::GluePair { vertex_a: 1, vertex_b: 2, weight_a: 0.25, weight_b: 0.75 }];
        // Vertices (1, 0) and (0, 1) of mesh 0: both read before either is
        // written, so both land on 0.75 A + 0.25 B = (0.75, 0.25), then y flips.
        let pose = moc.pose(&[0.0]);
        assert!(close(pose.drawables[0][1], (0.75, -0.25)), "{:?}", pose.drawables[0]);
        assert!(close(pose.drawables[0][2], (0.75, -0.25)), "{:?}", pose.drawables[0]);
    }

    #[test]
    fn a_glue_leaves_an_unstable_mesh_alone() {
        let moc = glued_pair(&[1.0; 3]);
        let start = vec![vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], vec![(5.0, 5.0), (6.0, 5.0), (5.0, 6.0)]];
        let mut stable = start.clone();
        moc.apply_glue(&mut stable, &[], &[0.0]);
        assert_ne!(stable, start, "the premise is that this glue moves these");
        let mut skipped = start.clone();
        moc.apply_glue(&mut skipped, &[1], &[0.0]);
        assert_eq!(skipped, start);
    }

    #[test]
    fn a_model_with_no_glue_poses_as_it_did() {
        let moc = crate::Moc3::parse(&crate::moc3::tests::Builder::new().build()).expect("should parse");
        assert!(moc.glues.is_empty());
        assert_eq!(moc.pose(&[15.0]).drawables, vec![posed(15.0)]);
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
        let out = rotate_point((100.0, 50.0), 90.0, 2.0, (1.0, 0.0));
        // Two units along x, turned a quarter turn, from the origin.
        assert!((out.0 - 100.0).abs() < 1e-4, "{out:?}");
        assert!((out.1 - 52.0).abs() < 1e-4, "{out:?}");
    }

    #[test]
    fn an_identity_frame_leaves_a_point_where_it_was() {
        let out = rotate_point((0.0, 0.0), 0.0, 1.0, (3.0, -4.0));
        assert!((out.0 - 3.0).abs() < 1e-6 && (out.1 + 4.0).abs() < 1e-6, "{out:?}");
    }

    // ---- Hand-built deformer trees -------------------------------------------

    enum Node {
        Warp { parent: Option<u32>, divisions: (u32, u32), grid: Vec<(f32, f32)> },
        Rotation { parent: Option<u32>, origin: (f32, f32), angle: f32, scale: f32 },
    }

    /// A model with the given deformer nodes (indexed in order) and one drawable
    /// carrying `points` under `drawable_parent`. No parameters, so every
    /// element has exactly its one keyform.
    fn hand_model(nodes: &[Node], drawable_parent: Option<u32>, points: &[(f32, f32)]) -> CubismIr {
        use crate::moc3::{Drawable, RotationDeformer, WarpDeformer};
        let mut moc = CubismIr::default();
        let mut floats: Vec<f32> = Vec::new();
        let store = |floats: &mut Vec<f32>, data: &[(f32, f32)]| -> u32 {
            let offset = floats.len() as u32;
            for (x, y) in data {
                floats.push(*x);
                floats.push(*y);
            }
            offset
        };
        for node in nodes {
            let id = format!("d{}", moc.deformers.len());
            match node {
                Node::Warp { parent, divisions, grid } => {
                    let index = moc.warp_deformers.len() as u32;
                    moc.warp_deformers.push(WarpDeformer {
                        id: format!("warp{index}"),
                        divisions: *divisions,
                        point_count: grid.len() as u32,
                        keyform_begin: moc.keyforms.warp_offsets.len() as u32,
                        keyform_count: 1,
                        keyform_binding: 0,
                    });
                    moc.keyforms.warp_offsets.push(store(&mut floats, grid));
                    moc.deformers.push(Deformer { id, parent: *parent, kind: DeformerKind::Warp(index) });
                }
                Node::Rotation { parent, origin, angle, scale } => {
                    let index = moc.rotation_deformers.len() as u32;
                    moc.rotation_deformers.push(RotationDeformer {
                        id: format!("rot{index}"),
                        keyform_binding: 0,
                        keyform_begin: moc.rotation_keyforms.len() as u32,
                        keyform_count: 1,
                        base_angle: 0.0,
                    });
                    moc.rotation_keyforms.push(RotationKeyform {
                        origin: *origin,
                        angle: *angle,
                        scale: *scale,
                        opacity: 1.0,
                    });
                    moc.deformers.push(Deformer { id, parent: *parent, kind: DeformerKind::Rotation(index) });
                }
            }
        }
        moc.keyforms.drawable_offsets.push(store(&mut floats, points));
        moc.keyforms.positions = floats;
        moc.drawables.push(Drawable {
            id: "mesh".into(),
            parent_deformer: drawable_parent,
            uvs: vec![(0.0, 0.0); points.len()],
            indices: vec![],
            keyform_begin: 0,
            keyform_count: 1,
            keyform_binding: 0,
            masks: vec![],
            flags: 0,
            part: None,
            texture: 0,
        });
        moc
    }

    fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }

    /// A one-cell warp, a rotation at the cell's centre under it, and a
    /// right-angled corner with 40-unit edges under that.
    fn rotation_under_warp(grid: [(f32, f32); 4], scale: f32) -> Pose {
        let nodes = [
            Node::Warp { parent: None, divisions: (1, 1), grid: grid.to_vec() },
            Node::Rotation { parent: Some(0), origin: (0.5, 0.5), angle: 0.0, scale },
        ];
        hand_model(&nodes, Some(1), &[(0.0, 0.0), (40.0, 0.0), (0.0, -40.0)]).pose(&[])
    }

    #[test]
    fn a_rotation_under_a_warp_moves_and_turns_but_does_not_stretch() {
        let grid = [(0.0, 0.0), (200.0, 0.0), (0.0, 100.0), (260.0, 140.0)];
        let pose = rotation_under_warp(grid, 1.0);
        assert!(pose.is_stable());

        // Pose flips y; work in the file's space.
        let p: Vec<(f32, f32)> = pose.drawables[0].iter().map(|(x, y)| (*x, -*y)).collect();
        // The warp maps its centre to the mean of its corners.
        assert!(close(p[0], (115.0, 60.0)), "{:?}", p[0]);
        // Edges stay exactly 40 and stay perpendicular.
        assert!((dist(p[1], p[0]) - 40.0).abs() < 1e-3, "{}", dist(p[1], p[0]));
        assert!((dist(p[2], p[0]) - 40.0).abs() < 1e-3, "{}", dist(p[2], p[0]));
        let (e1, e2) = ((p[1].0 - p[0].0, p[1].1 - p[0].1), (p[2].0 - p[0].0, p[2].1 - p[0].1));
        assert!((e1.0 * e2.0 + e1.1 * e2.1).abs() < 1e-2, "{e1:?} {e2:?}");

        // The turn is the warp's local up axis against an unbent one. By hand
        // the probe from (0.5, 0.4) lands on (112, 48), so it points along
        // (-3, -12) from the origin.
        let turn = (-12.0f32).atan2(-3.0).to_degrees() + 90.0;
        let (s, c) = turn.to_radians().sin_cos();
        assert!((e1.0 - 40.0 * c).abs() < 1e-2 && (e1.1 - 40.0 * s).abs() < 1e-2, "{e1:?} vs turn {turn}");
    }

    #[test]
    fn a_warp_lends_a_rotation_no_scale() {
        // A 100 by 300 rectangle turned a quarter so its u axis runs down y.
        // Under the old rule the 40 by 40 square came out 9:1 (180 by 20).
        let grid = [(0.0, 0.0), (0.0, 300.0), (100.0, 0.0), (100.0, 300.0)];
        let pose = rotation_under_warp(grid, 1.5);
        let p = &pose.drawables[0];
        assert!((dist(p[1], p[0]) - 60.0).abs() < 1e-3, "{}", dist(p[1], p[0]));
        assert!((dist(p[2], p[0]) - 60.0).abs() < 1e-3, "{}", dist(p[2], p[0]));
        assert!(close(p[0], (50.0, -150.0)), "{:?}", p[0]);
    }

    #[test]
    fn rotation_scales_multiply_down_a_chain_through_warps() {
        // The warp is a rectangle three times taller than wide, turned a
        // quarter so its u axis runs down y. An axis-aligned one would let the
        // old per-axis unit cancel against the bilinear lookup and hide the
        // difference; a turned one leaves it 9:1.
        for size in [10.0f32, 1000.0] {
            let nodes = [
                Node::Rotation { parent: None, origin: (0.0, 0.0), angle: 0.0, scale: 0.5 },
                Node::Warp {
                    parent: Some(0),
                    divisions: (1, 1),
                    grid: vec![(0.0, 0.0), (0.0, 3.0 * size), (size, 0.0), (size, 3.0 * size)],
                },
                Node::Rotation { parent: Some(1), origin: (0.5, 0.5), angle: 0.0, scale: 2.0 },
            ];
            let pose = hand_model(&nodes, Some(2), &[(0.0, 0.0), (40.0, 0.0), (0.0, -40.0)]).pose(&[]);
            let p = &pose.drawables[0];
            assert!((dist(p[1], p[0]) - 40.0).abs() < 1e-2, "grid {size}: {}", dist(p[1], p[0]));
            assert!((dist(p[2], p[0]) - 40.0).abs() < 1e-2, "grid {size}: {}", dist(p[2], p[0]));
        }
    }

    #[test]
    fn a_rotation_under_a_rotation_composes_as_before() {
        let nodes = [
            Node::Rotation { parent: None, origin: (100.0, 50.0), angle: 90.0, scale: 2.0 },
            Node::Rotation { parent: Some(0), origin: (10.0, 0.0), angle: 30.0, scale: 3.0 },
        ];
        let pose = hand_model(&nodes, Some(1), &[(1.0, 0.0)]).pose(&[]);
        // Origin: the parent's frame sends (10, 0) to (100, 50) + 2 * (0, 10).
        // Angles add to 120 degrees and scales multiply to 6.
        let (s, c) = 120f32.to_radians().sin_cos();
        let expect = (100.0 + 6.0 * c, -(70.0 + 6.0 * s));
        assert!(close(pose.drawables[0][0], expect), "{:?} vs {expect:?}", pose.drawables[0][0]);
    }

    #[test]
    fn a_warp_squashed_flat_leaves_its_rotation_children_posed_at_its_origin() {
        // Every control point equal: no probe has a direction. The rotation
        // keeps its origin and its own angle, and the model stays stable.
        let nodes = [
            Node::Warp { parent: None, divisions: (1, 1), grid: vec![(5.0, 5.0); 4] },
            Node::Rotation { parent: Some(0), origin: (0.5, 0.5), angle: 0.0, scale: 1.0 },
        ];
        let pose = hand_model(&nodes, Some(1), &[(0.0, 0.0), (40.0, 0.0), (0.0, -40.0)]).pose(&[]);
        assert!(pose.is_stable(), "{:?}", pose.unstable);
        let expect = [(5.0, -5.0), (45.0, -5.0), (5.0, 35.0)];
        for (got, want) in pose.drawables[0].iter().zip(expect) {
            assert!(close(*got, want), "{:?}", pose.drawables[0]);
        }
    }

    #[test]
    fn a_warp_that_goes_non_finite_reports_what_hangs_below_it_unstable() {
        let nodes = [
            Node::Warp { parent: None, divisions: (1, 1), grid: vec![(f32::NAN, 0.0); 4] },
            Node::Rotation { parent: Some(0), origin: (0.5, 0.5), angle: 0.0, scale: 1.0 },
        ];
        let pose = hand_model(&nodes, Some(1), &[(0.0, 0.0), (40.0, 0.0), (0.0, -40.0)]).pose(&[]);
        assert_eq!(pose.unstable, vec![0]);
        assert!(pose.drawables.iter().flatten().all(|(x, y)| x.is_finite() && y.is_finite()), "{:?}", pose.drawables);
    }

    #[test]
    fn a_collapsed_forward_probe_is_read_from_the_other_side() {
        // Two cells stacked in v (two row divisions, one column). The lower
        // cell is squashed flat, the upper one is skewed to the right. The
        // rotation sits on the seam at (0.5, 0.5), so the forward probe, at
        // v = 0.4, lands in the flat cell and returns the origin itself.
        let grid = vec![(0.0, 100.0), (100.0, 100.0), (0.0, 100.0), (100.0, 100.0), (30.0, 200.0), (130.0, 200.0)];
        let nodes = [
            Node::Warp { parent: None, divisions: (2, 1), grid },
            Node::Rotation { parent: Some(0), origin: (0.5, 0.5), angle: 0.0, scale: 1.0 },
        ];
        let pose = hand_model(&nodes, Some(1), &[(0.0, 0.0), (40.0, 0.0), (0.0, -40.0)]).pose(&[]);
        let p: Vec<(f32, f32)> = pose.drawables[0].iter().map(|(x, y)| (*x, -*y)).collect();
        assert!(close(p[0], (50.0, 100.0)), "{:?}", p[0]);
        // The back probe at v = 0.6 is 20% up the skewed cell: (56, 120). The
        // direction is origin - back = (-6, -20), not its negation, so the turn
        // is atan2(-20, -6) + 90 = -16.7 degrees. A sign slip would give 163.3.
        let turn = (-20.0f32).atan2(-6.0).to_degrees() + 90.0;
        assert!((turn + 16.699).abs() < 1e-2, "the expectation itself: {turn}");
        let (s, c) = turn.to_radians().sin_cos();
        let e1 = (p[1].0 - p[0].0, p[1].1 - p[0].1);
        assert!((e1.0 - 40.0 * c).abs() < 1e-2 && (e1.1 - 40.0 * s).abs() < 1e-2, "{e1:?}");
    }

    #[test]
    fn a_warp_inside_a_warp_carries_its_grid_through_the_parent() {
        // The parent is not affine (its far corner is pushed out), and the
        // child is a distorted patch of its unit square. Carrying the child's
        // corners through the parent -- (0,0), (50,0), (0,50) and (131.25,
        // 131.25) -- and interpolating gives (45.3125, 45.3125) for the
        // child's centre; walking the point through both warps in turn gives
        // (41.015625, 41.015625). The documented rule is the first.
        let parent = vec![(0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (200.0, 200.0)];
        let child = vec![(0.0, 0.0), (0.5, 0.0), (0.0, 0.5), (0.75, 0.75)];
        let nodes = [
            Node::Warp { parent: None, divisions: (1, 1), grid: parent },
            Node::Warp { parent: Some(0), divisions: (1, 1), grid: child },
        ];
        let pose = hand_model(&nodes, Some(1), &[(0.5, 0.5)]).pose(&[]);
        assert!(close(pose.drawables[0][0], (45.3125, -45.3125)), "{:?}", pose.drawables[0]);
    }

    #[test]
    fn a_warp_under_a_rotation_lives_in_the_rotations_turned_and_scaled_space() {
        let nodes = [
            Node::Rotation { parent: None, origin: (10.0, 20.0), angle: 90.0, scale: 2.0 },
            Node::Warp {
                parent: Some(0),
                divisions: (1, 1),
                grid: vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)],
            },
        ];
        let pose = hand_model(&nodes, Some(1), &[(0.5, 0.5), (1.0, 0.0)]).pose(&[]);
        // (0.5, 0.5) doubles to (1, 1), a quarter turn makes it (-1, 1).
        assert!(close(pose.drawables[0][0], (9.0, -21.0)), "{:?}", pose.drawables[0]);
        // (1, 0) doubles to (2, 0), a quarter turn makes it (0, 2).
        assert!(close(pose.drawables[0][1], (10.0, -22.0)), "{:?}", pose.drawables[0]);
    }
}
