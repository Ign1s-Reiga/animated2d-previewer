//! The normalized Cubism model.
//!
//! Separate from the Spine IR on purpose: spec §5 forbids forcing the two into
//! one low-level deformation model, and they share nothing below
//! [`AnimatedModel`](crate::model::AnimatedModel) and
//! [`RenderMesh`](crate::render::RenderMesh).
//!
//! These types live here rather than beside the MOC3 reader because a package
//! stores them (spec §9) and `a2d-pack` may not depend on a format crate. What
//! stays with the reader is the container: the format version, the section
//! table, and the code that turns bytes into this.
//!
//! # Nothing here is a resting pose
//!
//! A drawable's coordinates are not stored. They come from blending that
//! drawable's keyforms by the current parameter values, and the result is in
//! the space of whatever deforms it. Evaluating that is `a2d-cubism`'s job;
//! this module is the data it works from.

use crate::BlendMode;

/// How many of each element the model declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub parts: u32,
    pub deformers: u32,
    pub warp_deformers: u32,
    pub rotation_deformers: u32,
    pub drawables: u32,
    pub parameters: u32,
    pub glues: u32,
}

/// The drawing area the model was authored in.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Canvas {
    /// Pixels per unit of model space.
    pub pixels_per_unit: f32,
    /// Origin within the canvas, in pixels.
    pub origin: (f32, f32),
    /// Canvas size in pixels.
    pub size: (f32, f32),
}

/// One animatable parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub id: String,
    pub minimum: f32,
    pub maximum: f32,
    pub default: f32,
}

impl Parameter {
    /// Clamps a value into this parameter's range.
    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(self.minimum, self.maximum)
    }

    /// Folds a value into `[minimum, maximum)` by whole turns of the range, for
    /// a parameter that repeats (see [`CubismIr::repeating_parameters`]).
    ///
    /// The maximum maps onto the minimum, since on a loop they are the same
    /// point. A range with no usable width, or a non-finite value or offset,
    /// falls back to [`Parameter::clamp`] so the result never leaves the range.
    pub fn wrap(&self, value: f32) -> f32 {
        let span = self.maximum - self.minimum;
        if !span.is_finite() || span <= 0.0 {
            return self.clamp(value);
        }
        let turn = (value - self.minimum).rem_euclid(span);
        if !turn.is_finite() {
            // The offset overflowed (a range of astronomic width): the clamped
            // value, with the maximum still read as the minimum.
            let clamped = self.clamp(value);
            return if clamped >= self.maximum { self.minimum } else { clamped };
        }
        let wrapped = self.minimum + turn;
        // `rem_euclid` can round up to `span`, and adding the minimum back can
        // round up to the maximum; the maximum is the minimum on a loop.
        if turn >= span || wrapped >= self.maximum {
            return self.minimum;
        }
        self.clamp(wrapped)
    }
}

/// One drawable: a textured triangle mesh, in Cubism terms an art mesh.
///
/// The mesh's *shape* is here; its *positions* are not. Cubism does not store a
/// resting pose — a drawable's coordinates come from blending its keyforms
/// according to the current parameter values, so there is nothing to read until
/// that evaluation exists. What is fixed per vertex, and is here, is the
/// texture coordinate and the triangle list.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawable {
    pub id: String,
    /// Deformer that moves this mesh, or `None` when nothing does.
    ///
    /// A drawable may be parented straight to the model root rather than to a
    /// deformer, and the format spells that `0xFFFFFFFF` -- the same sentinel
    /// [`Deformer::parent`] and [`Drawable::part`] use. Read as an index it is
    /// four billion, so a model that uses it fails to load outright rather
    /// than deforming oddly.
    pub parent_deformer: Option<u32>,
    /// Texture coordinates, one per vertex.
    pub uvs: Vec<(f32, f32)>,
    /// Triangle indices, local to this drawable's own vertices.
    pub indices: Vec<u16>,
    /// This drawable's own keyforms, as a range in the drawable keyform list.
    pub keyform_begin: u32,
    pub keyform_count: u32,
    pub keyform_binding: u32,
    /// Drawables this one is clipped to. Empty when it is not masked.
    ///
    /// The stencil is even-odd, so masks overlapping each other cancel where
    /// they do; a model's masks are normally disjoint.
    pub masks: Vec<u32>,
    /// Constant flags: bit 0 additive, bit 1 multiply, bit 2 double sided,
    /// bit 3 inverted mask.
    pub flags: u8,
    /// The part this drawable belongs to, where it names one.
    pub part: Option<u32>,
    /// Texture page this drawable samples.
    pub texture: u32,
}

impl Drawable {
    /// How this drawable is composited.
    pub fn blend_mode(&self) -> BlendMode {
        // Additive is checked first: no model seen sets both, and if one did,
        // treating it as additive is the less destructive reading.
        if self.flags & 0b0000_0001 != 0 {
            BlendMode::Additive
        } else if self.flags & 0b0000_0010 != 0 {
            BlendMode::Multiply
        } else {
            BlendMode::Normal
        }
    }

    pub fn vertex_count(&self) -> usize {
        self.uvs.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A parameter and the values at which that parameter has keyforms.
///
/// One parameter appears in as many bindings as there are elements driven by
/// different key sets, which is why there are more bindings than parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterBinding {
    pub parameter: u32,
    /// Strictly increasing, and always inside the parameter's own range.
    pub keys: Vec<f32>,
}

/// The parameters that drive one element's keyforms.
///
/// An element's keyforms form a grid with one axis per binding, so the number
/// of keyforms is the product of the bindings' key counts. That identity is
/// checked on parse for every element.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyformBinding {
    /// Indices into [`Moc3::parameter_bindings`], one per axis.
    pub axes: Vec<u32>,
}

/// Which kind of deformer, and where to find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeformerKind {
    /// Bends its children through a grid of control points.
    Warp(u32),
    /// Moves, turns and scales its children rigidly.
    Rotation(u32),
}

/// One node of the deformation tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Deformer {
    pub id: String,
    /// `None` at the root of a tree.
    pub parent: Option<u32>,
    pub kind: DeformerKind,
}

/// A rotation deformer: a rigid frame driven by its keyforms.
#[derive(Debug, Clone, PartialEq)]
pub struct RotationDeformer {
    pub id: String,
    pub keyform_binding: u32,
    pub keyform_begin: u32,
    pub keyform_count: u32,
    /// A constant angle, in degrees, that the keyforms are measured *from*.
    ///
    /// The frame's actual angle is this plus whatever the keyforms blend to.
    /// It is not a formality: it is non-zero on 26 to 215 of the rotation
    /// deformers in every real model checked, and leaving it out poses one of
    /// them with the character a quarter turn over and outside her own canvas.
    pub base_angle: f32,
}

/// One rotation keyform: where the frame sits and how it is turned.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RotationKeyform {
    pub origin: (f32, f32),
    /// Degrees.
    pub angle: f32,
    pub scale: f32,
    pub opacity: f32,
}

/// A warp deformer: a grid of control points that bends whatever hangs off it.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpDeformer {
    pub id: String,
    /// Grid divisions: `.0` down the grid's y axis, `.1` across its x axis.
    ///
    /// The stored lattice is row-major with `divisions.1 + 1` points to a row,
    /// and x runs along a row. The layout alone could not tell the two counts
    /// apart; reading every non-square grid in six real models both ways did.
    /// Read this way, 713 of 729 such grids are perfectly monotone — x
    /// increasing along every row, y along every column — and most of the rest
    /// are coherently mirrored rather than scrambled.
    pub divisions: (u32, u32),
    /// Control point count, always `(a + 1) * (b + 1)`.
    pub point_count: u32,
    /// This deformer's own keyforms, as a range in the warp keyform list.
    pub keyform_begin: u32,
    pub keyform_count: u32,
    pub keyform_binding: u32,
}

/// A glue: a constraint that pulls vertices of two meshes together.
///
/// Authoring tools split one surface into several meshes (a torso skinned in
/// two pieces, a cloth cut along a seam) and stitch the cut back together with
/// a glue. The pieces hang off unrelated deformer chains, so without the glue
/// they drift apart as parameters move and a gap opens along the seam.
///
/// Applying one is the last step of posing, after every drawable has been
/// carried to canvas space. With `I` the glue's blended intensity, and both
/// positions read before either is written,
///
/// ```text
/// A' = A + (B - A) * weight_a * I
/// B' = B + (A - B) * weight_b * I
/// ```
///
/// Each vertex moves towards the other in proportion to its own weight. When
/// the two weights of a pair sum to one and `I` is one, both land on the same
/// point. Otherwise the pair closes by `I * (wa + wb)` of its gap, leaving
/// `|1 - I(wa + wb)|` of it (a sum above one crosses over). Sums below one occur in real models, where the editor lets an
/// artist partly or fully release glued vertices
/// (<https://docs.live2d.com/en/cubism-editor-manual/glue>). A vertex may also
/// be named by more than one glue; they apply in file order, which is
/// unverified against Cubism.
#[derive(Debug, Clone, PartialEq)]
pub struct Glue {
    /// The two meshes, as indices into [`CubismIr::drawables`].
    pub mesh_a: u32,
    pub mesh_b: u32,
    /// Which keyform grid drives the intensity, as an index into
    /// [`CubismIr::keyform_bindings`].
    pub keyform_binding: u32,
    /// This glue's intensity keyforms, as a range in
    /// [`CubismIr::glue_intensities`].
    pub keyform_begin: u32,
    pub keyform_count: u32,
    /// The stitched vertex pairs.
    pub pairs: Vec<GluePair>,
}

/// One stitched pair of vertices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GluePair {
    /// A vertex of [`Glue::mesh_a`], local to that drawable.
    pub vertex_a: u32,
    /// A vertex of [`Glue::mesh_b`], local to that drawable.
    pub vertex_b: u32,
    /// How far the vertex on mesh A moves towards mesh B's, at full intensity.
    /// Which weight belongs to which mesh is inferred from seam geometry, not
    /// from a reference runtime.
    pub weight_a: f32,
    /// How far the vertex on mesh B moves towards mesh A's, at full intensity.
    pub weight_b: f32,
}

/// The keyform coordinate pool.
///
/// Warp deformer keyforms come first, then drawable keyforms, each element's
/// keyforms contiguous and each keyform padded as above. Only the warp offsets
/// are stored in the file; the drawable ones follow the same rule and are
/// derived here.
///
/// # These are not world coordinates
///
/// A keyform holds coordinates in the space of whatever deforms the element —
/// a warp deformer's grid, or a rotation deformer's frame — so they cannot be
/// drawn as they stand. Turning them into a pose means walking the deformer
/// chain, which this crate does not do yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyforms {
    /// Every keyform's coordinates, end to end, as `x, y` pairs.
    pub positions: Vec<f32>,
    /// Offset of each warp keyform, in floats.
    pub warp_offsets: Vec<u32>,
    /// Offset of each drawable keyform, in floats.
    pub drawable_offsets: Vec<u32>,
}

impl Keyforms {
    pub fn len(&self) -> usize {
        self.warp_offsets.len() + self.drawable_offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One warp keyform's control points.
    pub fn warp(&self, index: usize, point_count: usize) -> Option<&[f32]> {
        let start = *self.warp_offsets.get(index)? as usize;
        self.positions.get(start..start + point_count * 2)
    }

    /// One drawable keyform's vertex coordinates.
    pub fn drawable(&self, index: usize, vertex_count: usize) -> Option<&[f32]> {
        let start = *self.drawable_offsets.get(index)? as usize;
        self.positions.get(start..start + vertex_count * 2)
    }
}

/// A whole Cubism model, normalized: everything needed to pose and draw it,
/// and nothing about the container it arrived in.
///
/// The element tables are parallel arrays indexed by the ids that name them,
/// which is how the format itself addresses them and what keeps a package a
/// faithful record rather than a re-interpretation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CubismIr {
    pub canvas: Canvas,
    pub parameters: Vec<Parameter>,
    /// The editor's per-parameter "repeat" switch, one entry per parameter, as
    /// the MOC3 stores it. `None` when the source carries none (or a package
    /// written before it was stored is read): repeating parameters are then
    /// inferred from motions, see [`CubismIr::repeating_parameters`].
    pub parameter_repeats: Option<Vec<bool>>,
    pub part_ids: Vec<String>,
    pub deformer_ids: Vec<String>,
    pub drawable_ids: Vec<String>,
    pub glue_ids: Vec<String>,
    /// Stitching between meshes. Empty when the source declared no glue, or
    /// when a package written before glue was stored is read: `glue_ids` can
    /// then be longer than this, which `validate` reports.
    pub glues: Vec<Glue>,
    /// Every glue keyform's intensity, end to end; each glue owns the range
    /// its `keyform_begin` and `keyform_count` name.
    pub glue_intensities: Vec<f32>,
    pub drawables: Vec<Drawable>,
    pub warp_deformers: Vec<WarpDeformer>,
    pub rotation_deformers: Vec<RotationDeformer>,
    pub rotation_keyforms: Vec<RotationKeyform>,
    pub deformers: Vec<Deformer>,
    pub parameter_bindings: Vec<ParameterBinding>,
    pub keyform_bindings: Vec<KeyformBinding>,
    /// The keyform coordinate pool, undivided.
    pub keyforms: Keyforms,
    /// The tie-break order for painting: every drawable exactly once, as the
    /// model's draw-order group lists them. It is **not** the paint sequence.
    /// Drawables are painted back to front by their blended draw order (see
    /// [`CubismIr::drawable_keyform_draw_orders`]) rounded to an integer, and
    /// where two are equal the one listed earlier here is painted first.
    pub draw_order: Vec<u32>,
    /// Opacity of each drawable keyform, in the same order as
    /// [`Keyforms::drawable_offsets`]. Empty when the source carried none, in
    /// which case the model draws fully opaque.
    pub drawable_keyform_opacities: Vec<f32>,
    /// Opacity of each warp deformer keyform, in the same order as
    /// [`Keyforms::warp_offsets`]. Empty when the source carried none (or a
    /// package written before this was stored is read), in which case no warp
    /// takes anything away from the drawables below it. A rotation's opacity is
    /// stored in its [`RotationKeyform`].
    pub warp_keyform_opacities: Vec<f32>,
    /// Draw order of each drawable keyform, in the same order.
    pub drawable_keyform_draw_orders: Vec<f32>,
    /// The model's own motions. Empty when the source supplied none.
    pub motions: Vec<Motion>,
    /// Secondary motion (hair, cloth, ornaments) driven by the parameters.
    /// `None` when the source carried none, or when a package written before
    /// this was stored is read. Nothing applies it yet: it is recovered and
    /// kept, and playback ignores it.
    pub physics: Option<Physics>,
}

/// A model's physics rig: groups of pendulum chains that read some parameters
/// and write others.
///
/// Groups are evaluated in order and write straight into the parameter values,
/// so a later group sees what an earlier one wrote.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Physics {
    /// Acceleration applied to every chain. `(0, -1)` is ordinary downward gravity.
    pub gravity: (f32, f32),
    /// Constant push applied to every chain.
    pub wind: (f32, f32),
    pub groups: Vec<PhysicsGroup>,
}

/// One chain of particles together with the parameters that sway it and the
/// parameters it moves in return.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhysicsGroup {
    /// Parameters read to move the chain's root.
    pub inputs: Vec<PhysicsInput>,
    /// Parameters written from the chain's motion.
    pub outputs: Vec<PhysicsOutput>,
    /// The chain, root first. The root is pinned to the input; each later
    /// particle hangs a fixed `radius` from the one before. Starting positions
    /// are not stored: they follow from the radii.
    pub particles: Vec<PhysicsParticle>,
    /// How input parameter values map onto the chain's translation and angle.
    pub normalization: PhysicsNormalization,
}

/// Which quantity of a chain an input feeds or an output reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhysicsComponent {
    /// Horizontal translation.
    #[default]
    X,
    /// Vertical translation.
    Y,
    /// Rotation.
    Angle,
}

/// A parameter that moves a chain.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhysicsInput {
    /// Identifier of the parameter read. It may name nothing in the model;
    /// evaluation ignores such an input and reports it instead.
    pub source: String,
    pub component: PhysicsComponent,
    /// Share of the input that counts, in `0..=100`.
    pub weight: f32,
    /// The source's "reflect" switch for this input; it decides whether the
    /// input's sign is flipped on the way in.
    pub reflect: bool,
}

/// A parameter that a chain's motion writes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhysicsOutput {
    /// Identifier of the parameter written. It may name nothing in the model.
    pub destination: String,
    /// The particle whose motion relative to the one before it is measured.
    /// Always at least 1 (the root has no predecessor) and below the group's
    /// particle count.
    pub particle: u32,
    pub component: PhysicsComponent,
    /// Multiplier from the measured quantity to the parameter value, already
    /// the one that belongs to `component`.
    pub scale: f32,
    /// How much of the result replaces the parameter, in `0..=100`; the rest of
    /// the parameter's current value is kept.
    pub weight: f32,
    pub reflect: bool,
}

impl PhysicsOutput {
    /// Whether this output reads a particle a chain of `chain_len` has and that
    /// has a predecessor to be measured against: `1..chain_len`.
    pub fn reads_particle_in(&self, chain_len: usize) -> bool {
        self.particle >= 1 && (self.particle as usize) < chain_len
    }
}

/// One point of a chain.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicsParticle {
    /// How much of its velocity the particle keeps.
    pub mobility: f32,
    /// How slowly it follows its predecessor; larger lags more.
    pub delay: f32,
    /// Strength with which gravity and wind pull it.
    pub acceleration: f32,
    /// Distance from the previous particle. Not negative.
    pub radius: f32,
}

/// The ranges an input parameter is mapped onto.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicsNormalization {
    /// For translation inputs.
    pub position: NormRange,
    /// For angle inputs.
    pub angle: NormRange,
}

/// The target range of a normalization: where the parameter's lowest value,
/// its midpoint and its highest value land.
///
/// `minimum <= maximum` always, whichever order the source listed them in.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NormRange {
    pub minimum: f32,
    pub maximum: f32,
    /// Where the parameter's midpoint lands.
    pub default: f32,
}

impl NormRange {
    /// A range from two ends in either order.
    pub fn new(a: f32, b: f32, default: f32) -> NormRange {
        NormRange { minimum: a.min(b), maximum: a.max(b), default }
    }
}

impl PhysicsGroup {
    /// The first reason this group cannot be used, if any.
    ///
    /// One rule set for every layer: an importer drops a group it names, and
    /// the package writer and reader refuse one. Non-finite numbers, weights
    /// outside `0..=100`, a negative radius, a normalization range turned
    /// inside out, an empty chain, and an output reading a particle that is not in the chain.
    pub fn problem(&self) -> Option<String> {
        let finite = |what: &str, v: f32| (!v.is_finite()).then(|| format!("{what} is not finite ({v})"));
        let weight = |what: &str, v: f32| {
            (!(v.is_finite() && (0.0..=100.0).contains(&v))).then(|| format!("{what} weight {v} is outside 0..=100"))
        };
        if self.particles.is_empty() {
            return Some("the chain has no particles".to_string());
        }
        for i in &self.inputs {
            if let Some(p) = weight("an input", i.weight) {
                return Some(p);
            }
        }
        for o in &self.outputs {
            if let Some(p) = weight("an output", o.weight).or_else(|| finite("an output scale", o.scale)) {
                return Some(p);
            }
            if !o.reads_particle_in(self.particles.len()) {
                return Some(format!(
                    "an output reads particle {} of a chain of {}; only 1.. is a valid choice",
                    o.particle,
                    self.particles.len()
                ));
            }
        }
        for p in &self.particles {
            for (what, v) in [
                ("a particle mobility", p.mobility),
                ("a particle delay", p.delay),
                ("a particle acceleration", p.acceleration),
                ("a particle radius", p.radius),
            ] {
                if let Some(problem) = finite(what, v) {
                    return Some(problem);
                }
            }
            if p.radius < 0.0 {
                return Some(format!("a particle radius {} is negative", p.radius));
            }
        }
        for (what, r) in [("position", self.normalization.position), ("angle", self.normalization.angle)] {
            for (name, v) in [("minimum", r.minimum), ("maximum", r.maximum), ("default", r.default)] {
                if let Some(problem) = finite(&format!("the {what} normalization {name}"), v) {
                    return Some(problem);
                }
            }
            if r.minimum > r.maximum {
                return Some(format!("the {what} normalization has minimum {} above maximum {}", r.minimum, r.maximum));
            }
        }
        None
    }
}

impl Physics {
    /// The first reason these physics cannot be used, if any: a non-finite
    /// gravity or wind, or the first group's [`PhysicsGroup::problem`].
    pub fn problem(&self) -> Option<String> {
        if !self.forces_finite() {
            return Some(format!("gravity {:?} or wind {:?} is not finite", self.gravity, self.wind));
        }
        self.groups.iter().enumerate().find_map(|(i, g)| g.problem().map(|p| format!("group {i}: {p}")))
    }

    /// Whether gravity and wind are both finite.
    pub fn forces_finite(&self) -> bool {
        [self.gravity.0, self.gravity.1, self.wind.0, self.wind.1].iter().all(|v| v.is_finite())
    }
}

/// One authored motion: parameter curves over a fixed length.
///
/// Source-neutral on purpose. Whatever container the curves came from, they
/// arrive here as keyed Hermite segments in seconds.
///
/// The fade fields are stored so a package can keep what the source said, but
/// nothing applies them: playback crossfades between motions over the length
/// the host asks for (`PlayOptions::mix_duration`), because no asset this was
/// built against carries authored fades, and never ramps a curve in and out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Motion {
    pub name: String,
    /// Length of one pass, in seconds. Looping wraps at this, not at the last key.
    pub duration: f32,
    /// Seconds to fade in. Stored, not applied.
    pub fade_in: f32,
    /// Seconds to fade out. Stored, not applied.
    pub fade_out: f32,
    pub curves: Vec<MotionCurve>,
}

/// A curve driving one target, by identifier.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MotionCurve {
    /// The parameter identifier this curve drives. It may name nothing in the
    /// model; evaluation ignores such a curve and reports it instead.
    pub target: String,
    /// Per-curve fade override in seconds; `None` means use the motion's. Stored,
    /// not applied.
    pub fade_in: Option<f32>,
    pub fade_out: Option<f32>,
    /// Ordered by time, non-decreasing.
    pub keys: Vec<CurveKey>,
}

/// One keyframe of a [`MotionCurve`].
///
/// Segments are cubic Hermite. Slopes are in value per second, and the segment
/// between keys `k0` and `k1` uses `k0.out_slope` and `k1.in_slope`. A linear
/// segment is one whose slopes both equal the chord, `(v1 - v0) / (t1 - t0)`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CurveKey {
    pub time: f32,
    pub value: f32,
    pub in_slope: f32,
    pub out_slope: f32,
    /// The segment *leaving* this key holds `value` until the next key. An
    /// explicit flag rather than an infinite slope, which no arithmetic survives.
    pub stepped: bool,
}

/// Why a parameter was taken to repeat.
#[derive(Debug, Clone, Copy)]
struct RepeatEvidence {
    parameter: usize,
    motion: usize,
    key: f32,
}

impl CubismIr {
    /// A parameter by identifier.
    pub fn parameter(&self, id: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.id == id)
    }

    /// The parameter index a motion curve's target names, if any.
    ///
    /// The one place targets are resolved: playback binds curves with it and
    /// validation reports through it, so what is applied and what is reported
    /// as unresolved cannot disagree.
    pub fn motion_target(&self, id: &str) -> Option<usize> {
        self.parameters.iter().position(|p| p.id == id)
    }

    /// One degradation per motion curve whose target names no parameter.
    ///
    /// Playback skips such a curve; this is how it is reported instead of being
    /// lost silently. Both the model and package validation use it, so the
    /// wording is identical.
    pub fn unresolved_motion_targets(&self) -> Vec<crate::Degradation> {
        let mut out = Vec::new();
        for m in &self.motions {
            for c in &m.curves {
                if self.motion_target(&c.target).is_none() {
                    out.push(crate::Degradation::MissingReference {
                        kind: format!("parameter targeted by motion {:?}", m.name),
                        name: c.target.clone(),
                    });
                }
            }
        }
        out
    }

    /// Parameters that loop: their two ends join, so a value past the maximum
    /// wraps round to the minimum instead of being clamped. Ascending indices.
    ///
    /// The editor's per-parameter "repeat" switch is stored in the MOC3
    /// ([`CubismIr::parameter_repeats`]) and, when the model carries it, it is
    /// the answer: exactly the flagged parameters. Only a model with no flags
    /// (a source or an older package without them) falls back to
    /// an inference from the motions, described on the private helper behind it.
    pub fn repeating_parameters(&self) -> Vec<usize> {
        match &self.parameter_repeats {
            Some(flags) => {
                flags.iter().enumerate().filter(|(i, on)| **on && *i < self.parameters.len()).map(|(i, _)| i).collect()
            }
            None => self.inferred_repeating_parameters(),
        }
    }

    /// Parameters that loop, found from the motions that key them: the
    /// fallback for a model that carries no repeat flags (an older package, or
    /// a source without the section).
    ///
    /// A parameter is taken to repeat when some motion curve driving it has
    /// every key on the same side of `[minimum, maximum]`, at least half the
    /// range beyond that end. Such a curve would sit frozen at one end of the
    /// range if clamped, which is not something anyone authors, while on a loop
    /// it is a perfectly good value (a whole number of turns, or a rest phase).
    /// A curve with keys on both sides (-90 to 90 on [-30, 30]) is a deliberate
    /// sweep, not evidence; neither is one with a key inside the range.
    ///
    /// Deliberately narrow. A ramp that merely passes beyond the range on its
    /// way somewhere (it has a key inside the range) does not count: that is
    /// just as likely a value the author expected to be clamped. A key that
    /// overshoots by a little, as a Hermite segment can, never counts. Keys
    /// that are not finite, and curves whose target names no parameter, are
    /// ignored. Ascending parameter indices.
    ///
    /// Measured against the stored flags on real models this finds a subset of
    /// them: it misses a parameter whose motions never leave the range.
    fn inferred_repeating_parameters(&self) -> Vec<usize> {
        self.repeat_evidence().into_iter().map(|e| e.parameter).collect()
    }

    /// Notes on repeating parameters, reported by the model, the importer and
    /// package validation alike so a guess that may be wrong stays visible.
    ///
    /// With stored flags the flags decide, so there is nothing to say about the
    /// parameters they mark. The one thing worth saying is a parameter the
    /// motions suggest should repeat but the source does not mark: it is
    /// clamped, and the note names the evidence so the disagreement is visible.
    /// Without flags, one note per parameter inferred to repeat.
    pub fn repeat_degradations(&self) -> Vec<crate::Degradation> {
        self.repeat_evidence()
            .into_iter()
            .filter_map(|e| {
                let p = self.parameters.get(e.parameter)?;
                let motion = self.motions.get(e.motion)?;
                let evidence = format!(
                    "parameter {} is keyed at least half its range beyond one end of [{}, {}] for a whole curve (motion {:?} holds {})",
                    p.id, p.minimum, p.maximum, motion.name, e.key
                );
                match &self.parameter_repeats {
                    Some(flags) if flags.get(e.parameter) == Some(&true) => None,
                    Some(_) => Some(crate::Degradation::Note(format!(
                        "{evidence}, but the source does not mark it repeating; clamped"
                    ))),
                    None => Some(crate::Degradation::Note(format!(
                        "{evidence}; treated as repeating, because no repeat flags are stored"
                    ))),
                }
            })
            .collect()
    }

    /// The first qualifying curve of each repeating parameter, in parameter order.
    fn repeat_evidence(&self) -> Vec<RepeatEvidence> {
        let mut out: Vec<Option<RepeatEvidence>> = vec![None; self.parameters.len()];
        for (mi, m) in self.motions.iter().enumerate() {
            for c in &m.curves {
                let Some(pi) = self.motion_target(&c.target) else { continue };
                if out[pi].is_some() {
                    continue;
                }
                let p = &self.parameters[pi];
                let span = p.maximum - p.minimum;
                if !span.is_finite() || span <= 0.0 || c.keys.is_empty() {
                    continue;
                }
                let half = span * 0.5;
                let above = |v: f32| v.is_finite() && v >= p.maximum + half;
                let below = |v: f32| v.is_finite() && v <= p.minimum - half;
                if c.keys.iter().all(|k| above(k.value)) || c.keys.iter().all(|k| below(k.value)) {
                    out[pi] = Some(RepeatEvidence { parameter: pi, motion: mi, key: c.keys[0].value });
                }
            }
        }
        out.into_iter().flatten().collect()
    }

    /// One degradation per physics input or output whose parameter id names
    /// nothing in the model.
    ///
    /// Evaluation skips such an entry; this is how it is reported instead of
    /// being lost silently, by the importer and by package validation alike.
    pub fn unresolved_physics_targets(&self) -> Vec<crate::Degradation> {
        let mut out = Vec::new();
        let Some(physics) = &self.physics else { return out };
        for (g, group) in physics.groups.iter().enumerate() {
            let inputs = group.inputs.iter().map(|i| ("read by", &i.source));
            let outputs = group.outputs.iter().map(|o| ("written by", &o.destination));
            for (verb, id) in inputs.chain(outputs) {
                if self.motion_target(id).is_none() {
                    out.push(crate::Degradation::MissingReference {
                        kind: format!("parameter {verb} physics group {g}"),
                        name: id.clone(),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(id: &str) -> Parameter {
        Parameter { id: id.into(), minimum: -30.0, maximum: 30.0, default: 0.0 }
    }

    fn key(time: f32, value: f32) -> CurveKey {
        CurveKey { time, value, in_slope: 0.0, out_slope: 0.0, stepped: false }
    }

    fn ir_with(curves: Vec<(&str, Vec<f32>)>) -> CubismIr {
        let motion = Motion {
            name: "m".into(),
            duration: 4.0,
            curves: curves
                .into_iter()
                .map(|(target, values)| MotionCurve {
                    target: target.into(),
                    keys: values.into_iter().enumerate().map(|(i, v)| key(i as f32, v)).collect(),
                    ..MotionCurve::default()
                })
                .collect(),
            ..Motion::default()
        };
        CubismIr { parameters: vec![param("A"), param("B")], motions: vec![motion], ..CubismIr::default() }
    }

    #[test]
    fn wrap_folds_into_a_half_open_range() {
        let p = param("A");
        assert!((p.wrap(45.0) - -15.0).abs() < 1e-4);
        assert!((p.wrap(135.0) - 15.0).abs() < 1e-4);
        assert!((p.wrap(-45.0) - 15.0).abs() < 1e-4);
        assert!((p.wrap(180.0) - 0.0).abs() < 1e-4);
        assert!((p.wrap(10.0) - 10.0).abs() < 1e-6);
        // The maximum is the same point as the minimum.
        assert_eq!(p.wrap(30.0), -30.0);
        assert_eq!(p.wrap(-30.0), -30.0);
    }

    #[test]
    fn wrap_never_leaves_the_range_and_survives_bad_input() {
        let p = param("A");
        assert!(p.wrap(f32::NAN).is_nan(), "same as clamp, which lets NaN through");
        assert_eq!(p.wrap(f32::INFINITY), -30.0, "clamped to the maximum, which reads as the minimum");
        let tiny = p.wrap(-30.0 - 1e-6);
        assert!((-30.0..30.0).contains(&tiny), "{tiny}");
        let flat = Parameter { id: "F".into(), minimum: 1.0, maximum: 1.0, default: 1.0 };
        assert_eq!(flat.wrap(5.0), 1.0);
    }

    #[test]
    fn a_curve_wholly_far_outside_the_range_marks_the_parameter_repeating() {
        let ir = ir_with(vec![("A", vec![360.0, 180.0]), ("B", vec![0.0, 10.0])]);
        assert_eq!(ir.repeating_parameters(), vec![0]);
        let notes = ir.repeat_degradations();
        assert_eq!(notes.len(), 1);
        let text = notes[0].to_string();
        assert!(text.contains("parameter A") && text.contains("repeating") && text.contains("\"m\""), "{text}");
    }

    #[test]
    fn without_flags_the_note_says_the_source_stores_none() {
        let ir = ir_with(vec![("A", vec![360.0, 180.0])]);
        let text = ir.repeat_degradations()[0].to_string();
        assert!(text.contains("treated as repeating") && text.contains("no repeat flags are stored"), "{text}");
    }

    #[test]
    fn a_stored_flag_makes_a_parameter_repeat_with_no_motion_evidence() {
        let mut ir = ir_with(vec![("A", vec![0.0, 10.0])]);
        ir.parameter_repeats = Some(vec![false, true]);
        assert_eq!(ir.repeating_parameters(), vec![1]);
        assert!(ir.inferred_repeating_parameters().is_empty());
        // A flag the motions do not contradict needs no note.
        assert!(ir.repeat_degradations().is_empty());
    }

    #[test]
    fn a_cleared_flag_overrides_motion_evidence_and_the_disagreement_is_reported_once() {
        let mut ir = ir_with(vec![("A", vec![360.0, 180.0]), ("A", vec![200.0])]);
        ir.parameter_repeats = Some(vec![false, false]);
        assert!(ir.repeating_parameters().is_empty());
        assert_eq!(ir.inferred_repeating_parameters(), vec![0]);
        let notes = ir.repeat_degradations();
        assert_eq!(notes.len(), 1, "{notes:?}");
        let text = notes[0].to_string();
        assert!(text.contains("parameter A") && text.contains("does not mark it repeating"), "{text}");
        assert!(text.contains("clamped") && !text.contains("treated as repeating"), "{text}");
    }

    #[test]
    fn a_flag_that_agrees_with_the_motions_is_silent_and_a_flag_alone_is_silent() {
        let mut ir = ir_with(vec![("A", vec![360.0, 180.0])]);
        ir.parameter_repeats = Some(vec![true, true]);
        assert_eq!(ir.repeating_parameters(), vec![0, 1]);
        assert!(ir.repeat_degradations().is_empty(), "{:?}", ir.repeat_degradations());
    }

    #[test]
    fn each_repeating_parameter_is_reported_once_however_many_curves_show_it() {
        let mut ir = ir_with(vec![("A", vec![180.0]), ("A", vec![-90.0, -120.0])]);
        let again = ir.motions[0].clone();
        ir.motions.push(again);
        assert_eq!(ir.repeating_parameters(), vec![0]);
        assert_eq!(ir.repeat_degradations().len(), 1);
    }

    #[test]
    fn hermite_overshoot_and_ramps_that_visit_the_range_do_not_repeat() {
        // 30.5 on [-30, 30] is the size of overshoot a Hermite segment makes.
        assert!(ir_with(vec![("A", vec![30.5, 30.5])]).repeating_parameters().is_empty());
        assert!(ir_with(vec![("A", vec![0.0, 40.0, 29.0])]).repeating_parameters().is_empty());
        // A curve with an in-range key could be authored to clamp.
        assert!(ir_with(vec![("A", vec![0.0, 180.0, 0.0])]).repeating_parameters().is_empty());
        // Just under half a span (30) beyond the maximum.
        assert!(ir_with(vec![("A", vec![59.0])]).repeating_parameters().is_empty());
        assert_eq!(ir_with(vec![("A", vec![60.0])]).repeating_parameters(), vec![0]);
    }

    #[test]
    fn a_curve_straddling_the_range_is_a_sweep_not_a_repeat() {
        assert!(ir_with(vec![("A", vec![-90.0, 90.0])]).repeating_parameters().is_empty());
        assert!(ir_with(vec![("A", vec![90.0, -90.0])]).repeating_parameters().is_empty());
        // Same side on both ends still flags, whichever side.
        assert_eq!(ir_with(vec![("A", vec![-90.0, -75.0])]).repeating_parameters(), vec![0]);
    }

    #[test]
    fn wrap_keeps_the_half_open_range_where_rounding_would_reach_the_maximum() {
        let p = Parameter { id: "P".into(), minimum: 1000.0, maximum: 1060.0, default: 1000.0 };
        for v in [1059.9999, 1060.0, 1060.001, 1119.9999, 2060.0, 940.00006, 939.9999] {
            let w = p.wrap(v);
            assert!((1000.0..1060.0).contains(&w), "{v} -> {w}");
        }
        let wide = Parameter { id: "W".into(), minimum: -1e38, maximum: 1e38, default: 0.0 };
        for v in [0.0, 5e37, -5e37, 1e38, -1e38, 3e38, f32::MAX] {
            let w = wide.wrap(v);
            assert!(w.is_finite() && (-1e38..1e38).contains(&w), "{v} -> {w}");
        }
    }

    #[test]
    fn non_finite_keys_unresolved_targets_and_empty_curves_never_flag() {
        assert!(ir_with(vec![("A", vec![f32::NAN, 400.0])]).repeating_parameters().is_empty());
        assert!(ir_with(vec![("A", vec![f32::INFINITY])]).repeating_parameters().is_empty());
        assert!(ir_with(vec![("Missing", vec![400.0])]).repeating_parameters().is_empty());
        assert!(ir_with(vec![("A", vec![])]).repeating_parameters().is_empty());
        let mut flat = ir_with(vec![("A", vec![400.0])]);
        flat.parameters[0].maximum = flat.parameters[0].minimum;
        assert!(flat.repeating_parameters().is_empty());
    }
}
