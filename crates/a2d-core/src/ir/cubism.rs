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
    pub part_ids: Vec<String>,
    pub deformer_ids: Vec<String>,
    pub drawable_ids: Vec<String>,
    pub glue_ids: Vec<String>,
    pub drawables: Vec<Drawable>,
    pub warp_deformers: Vec<WarpDeformer>,
    pub rotation_deformers: Vec<RotationDeformer>,
    pub rotation_keyforms: Vec<RotationKeyform>,
    pub deformers: Vec<Deformer>,
    pub parameter_bindings: Vec<ParameterBinding>,
    pub keyform_bindings: Vec<KeyformBinding>,
    /// The keyform coordinate pool, undivided.
    pub keyforms: Keyforms,
    /// The drawables in the order they are painted, back to front: entry `k`
    /// is the index of the drawable drawn `k`-th.
    pub draw_order: Vec<u32>,
    /// Opacity of each drawable keyform, in the same order as
    /// [`Keyforms::drawable_offsets`]. Empty when the source carried none, in
    /// which case the model draws fully opaque.
    pub drawable_keyform_opacities: Vec<f32>,
    /// Draw order of each drawable keyform, in the same order.
    pub drawable_keyform_draw_orders: Vec<f32>,
}

impl CubismIr {
    /// A parameter by identifier.
    pub fn parameter(&self, id: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.id == id)
    }
}
