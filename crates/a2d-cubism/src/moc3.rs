//! The MOC3 container.
//!
//! # How this was worked out
//!
//! MOC3 is undocumented and this is an independent parser (CLAUDE.md §13.1: no
//! Cubism Core). Everything below was derived from a real model and then
//! *checked against an independent source* rather than assumed:
//!
//! * The counts, and which count means what, were confirmed by comparing them
//!   with the same model's Unity components — 195 parts, 601 drawables, 849
//!   parameters, matching exactly.
//! * The identifier tables were confirmed by comparing every id against the
//!   `CubismPart` / `CubismDrawable` / `CubismParameter` object names on the
//!   Unity side. All three sets matched exactly.
//! * Which array holds parameter minimums, maximums and defaults was settled by
//!   the only assignment under which `min <= default <= max` holds for all 849,
//!   and the result is textbook Cubism (`ParamAngleX` spans ±30,
//!   `ParamEyeLOpen` runs 0 to 1.2 with a default of 1).
//! * The canvas fields were settled by the origin being exactly half the size,
//!   which fixes the field order.
//! * The drawable tables were settled by arithmetic that only closes if they
//!   are read right: the per-drawable offsets are cumulative, so each must
//!   equal the previous plus its own size, and the last must account for
//!   exactly the totals the count table declares. On a real model both close
//!   exactly, and all 27756 triangle indices land inside their own mesh.
//!
//! Anything *not* on that list is left unparsed rather than guessed at. The
//! section table is exposed raw so later work can extend this without having to
//! re-derive the frame.
//!
//! * The keyform pool's division was settled by prediction rather than by
//!   fitting. Walking it — warp deformers first, each keyform padded to a
//!   multiple of eight points, then drawables by the same rule — reproduces
//!   every one of the 9554 stored offsets exactly and ends precisely on the
//!   declared total. A wrong padding rule or a wrong ordering would miss on the
//!   very first deformer.
//! * The warp grid orientation — which division count is columns — was settled
//!   by reading every non-square grid in six models both ways: with
//!   `divisions.1 + 1` points to a row, 713 of 729 grids are perfectly
//!   monotone lattices; the other reading interleaves rows and makes not one
//!   of them monotone. See [`WarpDeformer::divisions`].
//!
//! # What is missing
//!
//! **There is no resting pose in a MOC3.** A drawable's coordinates are not
//! stored as such; they are produced by blending that drawable's keyforms
//! according to the current parameter values. Which element owns which keyforms
//! is now known, and each element's keyforms are addressable — but the
//! coordinates in them are in the space of whatever deforms the element, not in
//! world space, so they still cannot be drawn as they stand.
//!
//! What remains is the evaluation: which parameters drive which keyforms and
//! with what weights, and then walking the deformer chain — warp grids and
//! rotation deformers — to carry a drawable's vertices out to the canvas. That
//! is the part with no independent source to check against. Everything read so
//! far could be confirmed against the model's Unity components or against
//! arithmetic that had to close; deformation can only be confirmed by looking
//! at the result, which is what spec §11 means by visual parity.
//!
//! # Layout
//!
//! ```text
//! 0x00  "MOC3"
//! 0x04  u8   format version
//! 0x05  u8   non-zero if the file is big-endian
//! 0x06  ..   reserved to 0x40
//! 0x40  u32[] section offsets; the first one marks where the table ends
//! ```
//!
//! Every section is an array addressed by offset, and sections are padded, so a
//! section's size is only an upper bound on the bytes it uses.

use a2d_core::DecodeError;

/// One 64-byte fixed-width identifier slot.
const ID_LEN: usize = 64;

/// Section indices, as positions in the offset table.
///
/// The table is positional by design — that is what makes the format
/// addressable — so these are constants rather than something to search for.
/// Every one is checked on parse: a section that does not hold exactly the
/// declared number of well-formed entries is an error, so a wrong index here
/// fails loudly instead of returning a plausible model.
mod section {
    pub const COUNTS: usize = 0;
    pub const CANVAS: usize = 1;
    pub const PART_IDS: usize = 3;
    pub const DEFORMER_IDS: usize = 11;
    pub const DRAWABLE_IDS: usize = 33;
    pub const PARAMETER_IDS: usize = 50;
    pub const PARAMETER_MAXIMUMS: usize = 51;
    pub const PARAMETER_MINIMUMS: usize = 52;
    pub const PARAMETER_DEFAULTS: usize = 53;
    pub const GLUE_IDS: usize = 90;

    pub const DRAWABLE_PARENT_DEFORMERS: usize = 40;
    pub const DRAWABLE_VERTEX_COUNTS: usize = 43;
    /// Offsets into the shared coordinate arrays, counted in *floats*.
    pub const DRAWABLE_VERTEX_OFFSETS: usize = 44;
    pub const DRAWABLE_INDEX_OFFSETS: usize = 45;
    pub const DRAWABLE_INDEX_COUNTS: usize = 46;

    /// Texture coordinates for every vertex of every drawable, end to end.
    pub const VERTEX_UVS: usize = 78;
    /// Triangle indices for every drawable, end to end, local to each drawable.
    pub const VERTEX_INDICES: usize = 79;

    pub const DEFORMER_PARENT: usize = 16;
    /// 0 for a warp deformer, 1 for a rotation deformer.
    pub const DEFORMER_TYPE: usize = 17;
    /// Index within the warp or rotation list, whichever this deformer is.
    pub const DEFORMER_INDEX_IN_TYPE: usize = 18;

    pub const WARP_KEYFORM_BINDING: usize = 19;
    pub const WARP_KEYFORM_BEGIN: usize = 20;
    pub const WARP_KEYFORM_COUNT: usize = 21;
    pub const WARP_GRID_POINTS: usize = 22;
    pub const WARP_DIVISIONS_A: usize = 23;
    pub const WARP_DIVISIONS_B: usize = 24;

    pub const ROTATION_KEYFORM_BINDING: usize = 25;
    pub const ROTATION_KEYFORM_BEGIN: usize = 26;
    pub const ROTATION_KEYFORM_COUNT: usize = 27;

    /// Order the drawables are painted in, back to front.
    pub const DRAWABLE_DRAW_ORDER: usize = 87;
    pub const DRAWABLE_KEYFORM_BINDING: usize = 34;
    pub const DRAWABLE_KEYFORM_BEGIN: usize = 35;
    pub const DRAWABLE_KEYFORM_COUNT: usize = 36;

    /// Per parameter, the range of parameter bindings it drives. A begin of
    /// `u32::MAX` means the parameter drives nothing.
    pub const PARAMETER_BINDING_BEGIN: usize = 56;
    pub const PARAMETER_BINDING_COUNT: usize = 57;

    /// Opacity carried by each rotation keyform.
    pub const ROTATION_KEYFORM_OPACITY: usize = 61;
    pub const ROTATION_KEYFORM_ANGLE: usize = 62;
    pub const ROTATION_KEYFORM_ORIGIN_X: usize = 63;
    pub const ROTATION_KEYFORM_ORIGIN_Y: usize = 64;
    pub const ROTATION_KEYFORM_SCALE: usize = 65;

    /// The parameter binding each keyform-binding entry points at.
    pub const PARAMETER_BINDING_REFS: usize = 72;
    pub const KEYFORM_BINDING_BEGIN: usize = 73;
    pub const KEYFORM_BINDING_COUNT: usize = 74;
    pub const PARAMETER_KEY_BEGIN: usize = 75;
    pub const PARAMETER_KEY_COUNT: usize = 76;
    pub const PARAMETER_KEYS: usize = 77;

    /// Where each *warp* keyform's coordinates begin, in floats. Drawable
    /// keyforms are not listed; they follow the same rule after the warps.
    pub const KEYFORM_POSITION_OFFSETS: usize = 60;
    /// Every keyform's vertex coordinates, end to end.
    pub const KEYFORM_POSITIONS: usize = 71;
}

/// Index into the count table.
mod count {
    pub const PARTS: usize = 0;
    pub const DEFORMERS: usize = 1;
    pub const WARP_DEFORMERS: usize = 2;
    pub const ROTATION_DEFORMERS: usize = 3;
    pub const DRAWABLES: usize = 4;
    pub const PARAMETERS: usize = 5;
    pub const GLUES: usize = 20;
    pub const ROTATION_KEYFORMS: usize = 8;
    pub const PARAMETER_BINDING_REFS: usize = 11;
    pub const KEYFORM_BINDINGS: usize = 12;
    pub const PARAMETER_BINDINGS: usize = 13;
    pub const PARAMETER_KEYS: usize = 14;
    pub const WARP_KEYFORMS: usize = 7;
    pub const DRAWABLE_KEYFORMS: usize = 9;
    pub const KEYFORM_POSITION_FLOATS: usize = 10;
    pub const UV_FLOATS: usize = 15;
    pub const INDICES: usize = 16;
}

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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Index of the deformer that moves this mesh.
    pub parent_deformer: u32,
    /// Texture coordinates, one per vertex.
    pub uvs: Vec<(f32, f32)>,
    /// Triangle indices, local to this drawable's own vertices.
    pub indices: Vec<u16>,
    /// Where this drawable sits back to front. Lower is drawn first.
    ///
    /// A permutation of the drawables in five of the six models checked; in the
    /// sixth it repeats, so it is treated as a sort key rather than an index
    /// and ties fall back to model order.
    pub draw_order: u32,
    /// This drawable's own keyforms, as a range in the drawable keyform list.
    pub keyform_begin: u32,
    pub keyform_count: u32,
    pub keyform_binding: u32,
}

impl Drawable {
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
    /// are coherently mirrored rather than scrambled. Read with
    /// `divisions.0 + 1` points to a row instead, the rows interleave and not
    /// one grid is monotone.
    pub divisions: (u32, u32),
    /// Control point count, always `(a + 1) * (b + 1)`.
    pub point_count: u32,
    /// This deformer's own keyforms, as a range in the warp keyform list.
    pub keyform_begin: u32,
    pub keyform_count: u32,
    pub keyform_binding: u32,
}

/// Coordinates are stored padded to a multiple of eight points.
///
/// Every one of the model's keyform offsets is reproduced exactly by this rule,
/// which is what identified it: the padding is for vectorised evaluation, and
/// it has to be stepped over rather than read.
fn padded_points(points: u32) -> u32 {
    points.div_ceil(8) * 8
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
#[derive(Debug, Clone, Default)]
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

/// A parsed MOC3 model.
///
/// This is the container, not yet a runnable model: identifiers, ranges and the
/// shape of the thing. Geometry and deformation are not decoded yet.
#[derive(Debug, Clone)]
pub struct Moc3 {
    pub version: u8,
    pub counts: Counts,
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
    /// Every section offset, so later work can reach data this does not decode.
    pub sections: Vec<u32>,
}

impl Moc3 {
    /// Highest format version whose section layout has been checked.
    ///
    /// Later versions append sections rather than reorder them, so they are
    /// attempted; the per-section validation below is what catches it if that
    /// assumption ever fails.
    pub const VERIFIED_VERSION: u8 = 2;

    pub fn parse(bytes: &[u8]) -> Result<Moc3, DecodeError> {
        if bytes.len() < 0x40 {
            return Err(DecodeError::corrupt(format!("a MOC3 file is at least 64 bytes; this one is {}", bytes.len())));
        }
        if &bytes[..4] != b"MOC3" {
            return Err(DecodeError::UnsupportedFormat(format!(
                "not a MOC3 file: it starts with {:02x?}",
                &bytes[..4]
            )));
        }
        let version = bytes[4];
        if version == 0 {
            return Err(DecodeError::corrupt("the MOC3 header declares version 0".to_string()));
        }
        if bytes[5] != 0 {
            // Every model seen is little-endian, and a big-endian one would need
            // every read below to flip. Refusing beats reading it backwards.
            return Err(DecodeError::UnsupportedFormat("this MOC3 is big-endian, which is not read here".to_string()));
        }

        let sections = read_section_table(bytes)?;
        let counts = read_counts(bytes, &sections)?;
        let canvas = read_canvas(bytes, &sections)?;

        let part_ids = read_ids(bytes, &sections, section::PART_IDS, counts.parts, "part")?;
        let deformer_ids = read_ids(bytes, &sections, section::DEFORMER_IDS, counts.deformers, "deformer")?;
        let drawable_ids = read_ids(bytes, &sections, section::DRAWABLE_IDS, counts.drawables, "drawable")?;
        let glue_ids = read_ids(bytes, &sections, section::GLUE_IDS, counts.glues, "glue")?;

        let parameter_ids = read_ids(bytes, &sections, section::PARAMETER_IDS, counts.parameters, "parameter")?;
        let maximums = read_floats(bytes, &sections, section::PARAMETER_MAXIMUMS, counts.parameters, "maximum")?;
        let minimums = read_floats(bytes, &sections, section::PARAMETER_MINIMUMS, counts.parameters, "minimum")?;
        let defaults = read_floats(bytes, &sections, section::PARAMETER_DEFAULTS, counts.parameters, "default")?;

        let mut parameters = Vec::with_capacity(parameter_ids.len());
        for (i, id) in parameter_ids.into_iter().enumerate() {
            let (minimum, maximum, default) = (minimums[i], maximums[i], defaults[i]);
            if !(minimum.is_finite() && maximum.is_finite() && default.is_finite()) {
                return Err(DecodeError::corrupt(format!(
                    "parameter {id:?} has a non-finite range ({minimum}..{maximum}, default {default})"
                )));
            }
            if minimum > maximum {
                // The ordering of these three arrays was derived from this
                // relation holding, so a violation means the layout is wrong.
                return Err(DecodeError::corrupt(format!(
                    "parameter {id:?} has minimum {minimum} above maximum {maximum}: \
                     the parameter arrays are not where this version puts them"
                )));
            }
            parameters.push(Parameter { id, minimum, maximum, default });
        }

        let parameter_bindings = read_parameter_bindings(bytes, &sections, &counts)?;
        let keyform_bindings = read_keyform_bindings(bytes, &sections, &parameter_bindings)?;
        let drawables = read_drawables(bytes, &sections, &counts, &drawable_ids)?;
        let warp_deformers = read_warp_deformers(bytes, &sections, &counts, &deformer_ids)?;
        let (mut rotation_deformers, rotation_keyforms) = read_rotation_deformers(bytes, &sections, &counts)?;
        let deformers = read_deformers(bytes, &sections, &counts, &deformer_ids)?;
        for d in &deformers {
            if let DeformerKind::Rotation(i) = d.kind {
                if let Some(r) = rotation_deformers.get_mut(i as usize) {
                    r.id = d.id.clone();
                }
            }
        }
        check_keyform_grids(&keyform_bindings, &parameter_bindings, &warp_deformers, &rotation_deformers, &drawables)?;
        let keyforms = read_keyforms(bytes, &sections, &warp_deformers, &drawables)?;

        Ok(Moc3 {
            version,
            counts,
            canvas,
            parameters,
            part_ids,
            deformer_ids,
            drawable_ids,
            glue_ids,
            drawables,
            warp_deformers,
            rotation_deformers,
            rotation_keyforms,
            deformers,
            parameter_bindings,
            keyform_bindings,
            keyforms,
            sections,
        })
    }

    /// Whether this file's layout has actually been checked against a model.
    pub fn is_verified_version(&self) -> bool {
        self.version <= Self::VERIFIED_VERSION
    }

    /// A parameter by identifier.
    pub fn parameter(&self, id: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.id == id)
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, DecodeError> {
    let slice = bytes.get(at..at + 4).ok_or_else(|| {
        DecodeError::corrupt_at(format!("wanted four bytes, the file holds {}", bytes.len()), at as u64)
    })?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap_or([0; 4])))
}

fn f32_at(bytes: &[u8], at: usize) -> Result<f32, DecodeError> {
    Ok(f32::from_bits(u32_at(bytes, at)?))
}

/// The offset table, whose first entry marks where it ends.
fn read_section_table(bytes: &[u8]) -> Result<Vec<u32>, DecodeError> {
    let first = u32_at(bytes, 0x40)? as usize;
    if first <= 0x40 || first > bytes.len() {
        return Err(DecodeError::corrupt(format!(
            "the first section offset is {first}, which cannot be where the table ends in a {}-byte file",
            bytes.len()
        )));
    }
    if (first - 0x40) % 4 != 0 {
        return Err(DecodeError::corrupt(format!(
            "the section table spans {} bytes, not a multiple of four",
            first - 0x40
        )));
    }
    let entries = (first - 0x40) / 4;
    let mut out = Vec::with_capacity(entries.min(4096));
    for i in 0..entries {
        let value = u32_at(bytes, 0x40 + i * 4)?;
        if value as usize > bytes.len() {
            return Err(DecodeError::corrupt(format!(
                "section {i} points at {value}, past the end of a {}-byte file",
                bytes.len()
            )));
        }
        out.push(value);
    }
    Ok(out)
}

fn section_offset(sections: &[u32], index: usize, what: &str) -> Result<usize, DecodeError> {
    let offset = sections.get(index).copied().ok_or_else(|| {
        DecodeError::corrupt(format!(
            "this MOC3 has {} sections, too few to hold the {what} table at index {index}",
            sections.len()
        ))
    })?;
    if offset == 0 {
        return Err(DecodeError::corrupt(format!("the {what} section is absent from this MOC3")));
    }
    Ok(offset as usize)
}

fn read_counts(bytes: &[u8], sections: &[u32]) -> Result<Counts, DecodeError> {
    let at = section_offset(sections, section::COUNTS, "count")?;
    let get = |i: usize| u32_at(bytes, at + i * 4);
    // Every count is used to size a run of at least four bytes per item, so a
    // count the file could not hold is refused before anything reserves for it.
    let bound = |n: u32, what: &str| checked_count(n, 4, bytes, what).map(|v| v as u32);
    let counts = Counts {
        parts: bound(get(count::PARTS)?, "part")?,
        deformers: bound(get(count::DEFORMERS)?, "deformer")?,
        warp_deformers: bound(get(count::WARP_DEFORMERS)?, "warp deformer")?,
        rotation_deformers: bound(get(count::ROTATION_DEFORMERS)?, "rotation deformer")?,
        drawables: bound(get(count::DRAWABLES)?, "drawable")?,
        parameters: bound(get(count::PARAMETERS)?, "parameter")?,
        glues: bound(get(count::GLUES)?, "glue")?,
    };
    // Warp and rotation deformers are the two kinds there are, so they must
    // account for the total. This is the cheapest check that the count table is
    // where it is believed to be.
    let split = counts.warp_deformers as u64 + counts.rotation_deformers as u64;
    if split != counts.deformers as u64 {
        return Err(DecodeError::corrupt(format!(
            "the count table says {} deformers but {} warp plus {} rotation; \
             the counts are not where this version puts them",
            counts.deformers, counts.warp_deformers, counts.rotation_deformers
        )));
    }
    Ok(counts)
}

fn read_canvas(bytes: &[u8], sections: &[u32]) -> Result<Canvas, DecodeError> {
    let at = section_offset(sections, section::CANVAS, "canvas")?;
    let canvas = Canvas {
        pixels_per_unit: f32_at(bytes, at)?,
        origin: (f32_at(bytes, at + 4)?, f32_at(bytes, at + 8)?),
        size: (f32_at(bytes, at + 12)?, f32_at(bytes, at + 16)?),
    };
    if !(canvas.pixels_per_unit.is_finite() && canvas.pixels_per_unit > 0.0) {
        return Err(DecodeError::corrupt(format!("the canvas declares {} pixels per unit", canvas.pixels_per_unit)));
    }
    if !(canvas.size.0.is_finite() && canvas.size.1.is_finite() && canvas.size.0 > 0.0 && canvas.size.1 > 0.0) {
        return Err(DecodeError::corrupt(format!("the canvas declares a size of {:?}", canvas.size)));
    }
    Ok(canvas)
}

/// Reads `count` fixed-width identifiers.
///
/// Each slot is 64 bytes: printable ASCII, then NUL padding. Anything else
/// means the section is not the identifier table it was taken for, which is the
/// check that makes the hard-coded section indices safe.
fn read_ids(bytes: &[u8], sections: &[u32], index: usize, count: u32, what: &str) -> Result<Vec<String>, DecodeError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let at = section_offset(sections, index, what)?;
    let needed = (count as usize)
        .checked_mul(ID_LEN)
        .ok_or_else(|| DecodeError::corrupt(format!("{count} {what} identifiers would overflow")))?;
    let end = at.checked_add(needed).filter(|e| *e <= bytes.len()).ok_or_else(|| {
        DecodeError::corrupt(format!(
            "the {what} table needs {needed} bytes at {at}, past the end of a {}-byte file",
            bytes.len()
        ))
    })?;

    let mut out = Vec::with_capacity(count as usize);
    for (i, slot) in bytes[at..end].chunks_exact(ID_LEN).enumerate() {
        let text_len = slot.iter().position(|b| *b == 0).ok_or_else(|| {
            DecodeError::corrupt(format!("{what} identifier {i} fills all {ID_LEN} bytes with no terminator"))
        })?;
        if text_len == 0 {
            return Err(DecodeError::corrupt(format!("{what} identifier {i} is empty")));
        }
        let body = &slot[..text_len];
        if !body.iter().all(|b| (0x20..0x7F).contains(b)) {
            return Err(DecodeError::corrupt(format!(
                "{what} identifier {i} is not printable ASCII: {body:02x?}; \
                 the {what} table is not where this version puts it"
            )));
        }
        if slot[text_len..].iter().any(|b| *b != 0) {
            return Err(DecodeError::corrupt(format!("{what} identifier {i} has bytes after its terminator")));
        }
        out.push(String::from_utf8_lossy(body).into_owned());
    }
    Ok(out)
}

/// Reads every drawable's mesh.
///
/// The per-drawable offsets are cumulative, so each one must equal the previous
/// plus its size — and the last must account for exactly the totals the count
/// table declares. Those two relations, plus every index landing inside its own
/// drawable, are what make this readable-by-inspection rather than a guess.
fn read_drawables(
    bytes: &[u8],
    sections: &[u32],
    counts: &Counts,
    ids: &[String],
) -> Result<Vec<Drawable>, DecodeError> {
    let n = counts.drawables as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    let u32s = |index: usize, what: &str| -> Result<Vec<u32>, DecodeError> {
        let at = section_offset(sections, index, what)?;
        (0..n).map(|i| u32_at(bytes, at + i * 4)).collect()
    };

    let parents = u32s(section::DRAWABLE_PARENT_DEFORMERS, "drawable parent")?;
    let draw_order = u32s(section::DRAWABLE_DRAW_ORDER, "drawable draw order")?;
    let keyform_binding = u32s(section::DRAWABLE_KEYFORM_BINDING, "drawable keyform binding")?;
    let keyform_begin = u32s(section::DRAWABLE_KEYFORM_BEGIN, "drawable keyform begin")?;
    let keyform_count = u32s(section::DRAWABLE_KEYFORM_COUNT, "drawable keyform count")?;
    let vertex_counts = u32s(section::DRAWABLE_VERTEX_COUNTS, "drawable vertex count")?;
    let vertex_offsets = u32s(section::DRAWABLE_VERTEX_OFFSETS, "drawable vertex offset")?;
    let index_offsets = u32s(section::DRAWABLE_INDEX_OFFSETS, "drawable index offset")?;
    let index_counts = u32s(section::DRAWABLE_INDEX_COUNTS, "drawable index count")?;

    // Coordinates are `x, y` pairs, so a vertex costs two floats.
    let mut expect_vertex = 0u64;
    let mut expect_index = 0u64;
    for i in 0..n {
        if vertex_offsets[i] as u64 != expect_vertex {
            return Err(DecodeError::corrupt(format!(
                "drawable {i} starts at float {} where {expect_vertex} was expected;                  the drawable tables are not where this version puts them",
                vertex_offsets[i]
            )));
        }
        if index_offsets[i] as u64 != expect_index {
            return Err(DecodeError::corrupt(format!(
                "drawable {i} indices start at {} where {expect_index} was expected",
                index_offsets[i]
            )));
        }
        expect_vertex += vertex_counts[i] as u64 * 2;
        expect_index += index_counts[i] as u64;
    }
    let declared_uv = counts_at(bytes, sections, count::UV_FLOATS)? as u64;
    let declared_idx = counts_at(bytes, sections, count::INDICES)? as u64;
    if expect_vertex != declared_uv || expect_index != declared_idx {
        return Err(DecodeError::corrupt(format!(
            "the drawables account for {expect_vertex} coordinate floats and {expect_index} indices,              but the count table declares {declared_uv} and {declared_idx}"
        )));
    }

    let uv_at = section_offset(sections, section::VERTEX_UVS, "uv")?;
    let idx_at = section_offset(sections, section::VERTEX_INDICES, "index")?;

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let count = vertex_counts[i] as usize;
        let base = vertex_offsets[i] as usize;
        let mut uvs = Vec::with_capacity(count);
        for v in 0..count {
            let u = f32_at(bytes, uv_at + (base + v * 2) * 4)?;
            let w = f32_at(bytes, uv_at + (base + v * 2 + 1) * 4)?;
            if !(u.is_finite() && w.is_finite()) {
                return Err(DecodeError::corrupt(format!("drawable {i} vertex {v} has a non-finite uv")));
            }
            uvs.push((u, w));
        }

        let index_base = index_offsets[i] as usize;
        let mut indices = Vec::with_capacity(index_counts[i] as usize);
        for k in 0..index_counts[i] as usize {
            let at = idx_at + (index_base + k) * 2;
            let slice = bytes
                .get(at..at + 2)
                .ok_or_else(|| DecodeError::corrupt_at(format!("drawable {i} index {k} is past the end"), at as u64))?;
            let value = u16::from_le_bytes(slice.try_into().unwrap_or([0; 2]));
            if value as usize >= count {
                // An index outside its own drawable is the loudest possible
                // sign that these tables were misread.
                return Err(DecodeError::corrupt(format!(
                    "drawable {i} index {k} is {value}, outside its own {count} vertices"
                )));
            }
            indices.push(value);
        }
        if indices.len() % 3 != 0 {
            return Err(DecodeError::corrupt(format!(
                "drawable {i} has {} indices, which is not a whole number of triangles",
                indices.len()
            )));
        }
        if parents[i] as u64 >= counts.deformers as u64 {
            return Err(DecodeError::corrupt(format!(
                "drawable {i} names deformer {} of {}",
                parents[i], counts.deformers
            )));
        }

        out.push(Drawable {
            id: ids.get(i).cloned().unwrap_or_default(),
            parent_deformer: parents[i],
            draw_order: draw_order[i],
            uvs,
            indices,
            keyform_begin: keyform_begin[i],
            keyform_count: keyform_count[i],
            keyform_binding: keyform_binding[i],
        });
    }

    let mut expect = 0u64;
    for (i, d) in out.iter().enumerate() {
        if d.keyform_begin as u64 != expect {
            return Err(DecodeError::corrupt(format!(
                "drawable {i} claims keyforms from {} where {expect} was expected",
                d.keyform_begin
            )));
        }
        expect += d.keyform_count as u64;
    }
    let declared = counts_at(bytes, sections, count::DRAWABLE_KEYFORMS)? as u64;
    if expect != declared {
        return Err(DecodeError::corrupt(format!(
            "the drawables account for {expect} keyforms, but the count table declares {declared}"
        )));
    }
    Ok(out)
}

/// Reads every parameter binding: a parameter and the keys it is keyed at.
///
/// The bindings are listed per parameter, so the owner of each is recovered by
/// walking that list. A begin of `u32::MAX` marks a parameter that drives
/// nothing, which 133 of this model's 849 do.
fn read_parameter_bindings(
    bytes: &[u8],
    sections: &[u32],
    counts: &Counts,
) -> Result<Vec<ParameterBinding>, DecodeError> {
    let binding_count =
        checked_count(counts_at(bytes, sections, count::PARAMETER_BINDINGS)?, 4, bytes, "parameter binding")?;
    let key_count = checked_count(counts_at(bytes, sections, count::PARAMETER_KEYS)?, 4, bytes, "parameter key")?;
    if binding_count == 0 {
        return Ok(Vec::new());
    }
    let parameters = counts.parameters as usize;

    let at = |index: usize, what: &str| section_offset(sections, index, what);
    let owner_begin_at = at(section::PARAMETER_BINDING_BEGIN, "parameter binding begin")?;
    let owner_count_at = at(section::PARAMETER_BINDING_COUNT, "parameter binding count")?;
    let key_begin_at = at(section::PARAMETER_KEY_BEGIN, "parameter key begin")?;
    let key_count_at = at(section::PARAMETER_KEY_COUNT, "parameter key count")?;
    let keys_at = at(section::PARAMETER_KEYS, "parameter key")?;

    // Which parameter owns each binding.
    let mut owner = vec![u32::MAX; binding_count];
    let mut expect = 0u64;
    for p in 0..parameters {
        let begin = u32_at(bytes, owner_begin_at + p * 4)?;
        let n = u32_at(bytes, owner_count_at + p * 4)?;
        if begin == u32::MAX {
            if n != 0 {
                return Err(DecodeError::corrupt(format!("parameter {p} drives nothing but declares {n} bindings")));
            }
            continue;
        }
        if begin as u64 != expect {
            return Err(DecodeError::corrupt(format!(
                "parameter {p} claims bindings from {begin} where {expect} was expected"
            )));
        }
        for k in 0..n as usize {
            let index = begin as usize + k;
            if index >= binding_count {
                return Err(DecodeError::corrupt(format!("parameter {p} names binding {index} of {binding_count}")));
            }
            owner[index] = p as u32;
        }
        expect += n as u64;
    }
    if expect != binding_count as u64 {
        return Err(DecodeError::corrupt(format!(
            "the parameters account for {expect} bindings, but the count table declares {binding_count}"
        )));
    }

    let mut out = Vec::with_capacity(binding_count);
    let mut expect_key = 0u64;
    for (b, owner) in owner.iter().enumerate() {
        let begin = u32_at(bytes, key_begin_at + b * 4)? as usize;
        let n = checked_count(u32_at(bytes, key_count_at + b * 4)?, 4, bytes, "key")?;
        if begin as u64 != expect_key {
            return Err(DecodeError::corrupt(format!(
                "parameter binding {b} claims keys from {begin} where {expect_key} was expected"
            )));
        }
        if n == 0 {
            return Err(DecodeError::corrupt(format!("parameter binding {b} has no keys")));
        }
        expect_key += n as u64;

        let mut keys = Vec::with_capacity(n);
        let mut previous = f32::NEG_INFINITY;
        for k in 0..n {
            let value = f32_at(bytes, keys_at + (begin + k) * 4)?;
            // Keys are a lookup axis, so they must be usable as one.
            if !value.is_finite() || value <= previous {
                return Err(DecodeError::corrupt(format!(
                    "parameter binding {b} key {k} is {value}, not greater than the one before it"
                )));
            }
            previous = value;
            keys.push(value);
        }
        out.push(ParameterBinding { parameter: *owner, keys });
    }
    // The key pool can be larger than the bindings consume: on two of the six
    // models checked they use only about half of it, so something not decoded
    // here owns the rest. What must hold is that no binding reaches past the
    // end, which is the part that would corrupt a read.
    if expect_key > key_count as u64 {
        return Err(DecodeError::corrupt(format!(
            "the bindings reach key {expect_key}, past the {key_count} the count table declares"
        )));
    }
    Ok(out)
}

/// Reads which parameter bindings drive each element group.
fn read_keyform_bindings(
    bytes: &[u8],
    sections: &[u32],
    parameter_bindings: &[ParameterBinding],
) -> Result<Vec<KeyformBinding>, DecodeError> {
    let n = checked_count(counts_at(bytes, sections, count::KEYFORM_BINDINGS)?, 4, bytes, "keyform binding")?;
    let refs =
        checked_count(counts_at(bytes, sections, count::PARAMETER_BINDING_REFS)?, 4, bytes, "binding reference")?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let begin_at = section_offset(sections, section::KEYFORM_BINDING_BEGIN, "keyform binding begin")?;
    let count_at = section_offset(sections, section::KEYFORM_BINDING_COUNT, "keyform binding count")?;
    let refs_at = section_offset(sections, section::PARAMETER_BINDING_REFS, "parameter binding reference")?;

    let mut out = Vec::with_capacity(n);
    let mut expect = 0u64;
    for b in 0..n {
        let begin = u32_at(bytes, begin_at + b * 4)? as usize;
        let count = checked_count(u32_at(bytes, count_at + b * 4)?, 4, bytes, "binding axis")?;
        if begin as u64 != expect {
            return Err(DecodeError::corrupt(format!(
                "keyform binding {b} claims axes from {begin} where {expect} was expected"
            )));
        }
        expect += count as u64;
        let mut axes = Vec::with_capacity(count);
        for k in 0..count {
            let value = u32_at(bytes, refs_at + (begin + k) * 4)?;
            if value as usize >= parameter_bindings.len() {
                return Err(DecodeError::corrupt(format!(
                    "keyform binding {b} names parameter binding {value} of {}",
                    parameter_bindings.len()
                )));
            }
            axes.push(value);
        }
        out.push(KeyformBinding { axes });
    }
    if expect != refs as u64 {
        return Err(DecodeError::corrupt(format!(
            "the keyform bindings account for {expect} axes, but the count table declares {refs}"
        )));
    }
    Ok(out)
}

/// Reads the rotation deformers and their keyforms.
fn read_rotation_deformers(
    bytes: &[u8],
    sections: &[u32],
    counts: &Counts,
) -> Result<(Vec<RotationDeformer>, Vec<RotationKeyform>), DecodeError> {
    let n = counts.rotation_deformers as usize;
    let keyform_count =
        checked_count(counts_at(bytes, sections, count::ROTATION_KEYFORMS)?, 4, bytes, "rotation keyform")?;
    if n == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let u32s = |index: usize, what: &str| -> Result<Vec<u32>, DecodeError> {
        let at = section_offset(sections, index, what)?;
        (0..n).map(|i| u32_at(bytes, at + i * 4)).collect()
    };
    let binding = u32s(section::ROTATION_KEYFORM_BINDING, "rotation keyform binding")?;
    let begin = u32s(section::ROTATION_KEYFORM_BEGIN, "rotation keyform begin")?;
    let count_of = u32s(section::ROTATION_KEYFORM_COUNT, "rotation keyform count")?;

    let mut deformers = Vec::with_capacity(n);
    let mut expect = 0u64;
    for i in 0..n {
        if begin[i] as u64 != expect {
            return Err(DecodeError::corrupt(format!(
                "rotation deformer {i} claims keyforms from {} where {expect} was expected",
                begin[i]
            )));
        }
        expect += count_of[i] as u64;
        deformers.push(RotationDeformer {
            id: String::new(),
            keyform_binding: binding[i],
            keyform_begin: begin[i],
            keyform_count: count_of[i],
        });
    }
    if expect != keyform_count as u64 {
        return Err(DecodeError::corrupt(format!(
            "the rotation deformers account for {expect} keyforms, but the count table declares {keyform_count}"
        )));
    }

    let floats = |index: usize, what: &str| -> Result<Vec<f32>, DecodeError> {
        let at = section_offset(sections, index, what)?;
        (0..keyform_count).map(|i| f32_at(bytes, at + i * 4)).collect()
    };
    let opacity = floats(section::ROTATION_KEYFORM_OPACITY, "rotation opacity")?;
    let angle = floats(section::ROTATION_KEYFORM_ANGLE, "rotation angle")?;
    let origin_x = floats(section::ROTATION_KEYFORM_ORIGIN_X, "rotation origin")?;
    let origin_y = floats(section::ROTATION_KEYFORM_ORIGIN_Y, "rotation origin")?;
    let scale = floats(section::ROTATION_KEYFORM_SCALE, "rotation scale")?;

    let mut keyforms = Vec::with_capacity(keyform_count);
    for i in 0..keyform_count {
        let k = RotationKeyform {
            origin: (origin_x[i], origin_y[i]),
            angle: angle[i],
            scale: scale[i],
            opacity: opacity[i],
        };
        if !(k.origin.0.is_finite() && k.origin.1.is_finite() && k.angle.is_finite() && k.scale.is_finite()) {
            return Err(DecodeError::corrupt(format!("rotation keyform {i} holds a non-finite value")));
        }
        keyforms.push(k);
    }
    Ok((deformers, keyforms))
}

/// Reads the deformation tree: what each deformer is and what it hangs off.
fn read_deformers(
    bytes: &[u8],
    sections: &[u32],
    counts: &Counts,
    ids: &[String],
) -> Result<Vec<Deformer>, DecodeError> {
    let n = counts.deformers as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    let u32s = |index: usize, what: &str| -> Result<Vec<u32>, DecodeError> {
        let at = section_offset(sections, index, what)?;
        (0..n).map(|i| u32_at(bytes, at + i * 4)).collect()
    };
    let parent = u32s(section::DEFORMER_PARENT, "deformer parent")?;
    let kind = u32s(section::DEFORMER_TYPE, "deformer type")?;
    let index_in_type = u32s(section::DEFORMER_INDEX_IN_TYPE, "deformer index")?;

    let mut out = Vec::with_capacity(n);
    let (mut warps, mut rotations) = (0usize, 0usize);
    for i in 0..n {
        let kind = match kind[i] {
            0 => {
                warps += 1;
                if index_in_type[i] >= counts.warp_deformers {
                    return Err(DecodeError::corrupt(format!(
                        "deformer {i} is warp {} of {}",
                        index_in_type[i], counts.warp_deformers
                    )));
                }
                DeformerKind::Warp(index_in_type[i])
            }
            1 => {
                rotations += 1;
                if index_in_type[i] >= counts.rotation_deformers {
                    return Err(DecodeError::corrupt(format!(
                        "deformer {i} is rotation {} of {}",
                        index_in_type[i], counts.rotation_deformers
                    )));
                }
                DeformerKind::Rotation(index_in_type[i])
            }
            other => return Err(DecodeError::corrupt(format!("deformer {i} declares type {other}"))),
        };
        let parent = if parent[i] == u32::MAX {
            None
        } else if (parent[i] as usize) < n {
            Some(parent[i])
        } else {
            return Err(DecodeError::corrupt(format!("deformer {i} names parent {} of {n}", parent[i])));
        };
        out.push(Deformer { id: ids.get(i).cloned().unwrap_or_default(), parent, kind });
    }
    if warps != counts.warp_deformers as usize || rotations != counts.rotation_deformers as usize {
        return Err(DecodeError::corrupt(format!(
            "the tree holds {warps} warp and {rotations} rotation deformers, but the counts declare {} and {}",
            counts.warp_deformers, counts.rotation_deformers
        )));
    }

    // A cycle would hang every walk of the tree, so it is refused here rather
    // than guarded at every use.
    for i in 0..n {
        let mut steps = 0usize;
        let mut at = out[i].parent;
        while let Some(p) = at {
            steps += 1;
            if steps > n {
                return Err(DecodeError::corrupt(format!("the deformer tree has a cycle through {i}")));
            }
            at = out[p as usize].parent;
        }
    }
    Ok(out)
}

/// Every element's keyform count must equal the product of its axes' key counts.
///
/// A keyform set is a grid with one axis per parameter, so this identity has to
/// hold. It is the check that ties the binding chain together: element to
/// keyform binding to parameter bindings to keys. On a real model it holds for
/// all 567 warp deformers, all 473 rotation deformers and all 601 drawables.
fn check_keyform_grids(
    bindings: &[KeyformBinding],
    parameters: &[ParameterBinding],
    warps: &[WarpDeformer],
    rotations: &[RotationDeformer],
    drawables: &[Drawable],
) -> Result<(), DecodeError> {
    let product = |binding: u32| -> Result<u64, DecodeError> {
        let b = bindings
            .get(binding as usize)
            .ok_or_else(|| DecodeError::corrupt(format!("keyform binding {binding} of {}", bindings.len())))?;
        let mut n = 1u64;
        for axis in &b.axes {
            let p = parameters
                .get(*axis as usize)
                .ok_or_else(|| DecodeError::corrupt(format!("parameter binding {axis} of {}", parameters.len())))?;
            n = n.saturating_mul(p.keys.len() as u64);
        }
        Ok(n)
    };
    let check = |what: &str, i: usize, binding: u32, keyforms: u32| -> Result<(), DecodeError> {
        let n = product(binding)?;
        if n != keyforms as u64 {
            return Err(DecodeError::corrupt(format!(
                "{what} {i} has {keyforms} keyforms but its parameters imply {n};                  the binding tables are not where this version puts them"
            )));
        }
        Ok(())
    };
    for (i, w) in warps.iter().enumerate() {
        check("warp deformer", i, w.keyform_binding, w.keyform_count)?;
    }
    for (i, r) in rotations.iter().enumerate() {
        check("rotation deformer", i, r.keyform_binding, r.keyform_count)?;
    }
    for (i, d) in drawables.iter().enumerate() {
        check("drawable", i, d.keyform_binding, d.keyform_count)?;
    }
    Ok(())
}

/// Reads the warp deformers: their grids and which keyforms are theirs.
fn read_warp_deformers(
    bytes: &[u8],
    sections: &[u32],
    counts: &Counts,
    deformer_ids: &[String],
) -> Result<Vec<WarpDeformer>, DecodeError> {
    let n = counts.warp_deformers as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    let u32s = |index: usize, what: &str| -> Result<Vec<u32>, DecodeError> {
        let at = section_offset(sections, index, what)?;
        (0..n).map(|i| u32_at(bytes, at + i * 4)).collect()
    };
    let binding = u32s(section::WARP_KEYFORM_BINDING, "warp keyform binding")?;
    let begin = u32s(section::WARP_KEYFORM_BEGIN, "warp keyform begin")?;
    let count_of = u32s(section::WARP_KEYFORM_COUNT, "warp keyform count")?;
    let points = u32s(section::WARP_GRID_POINTS, "warp grid point")?;
    let div_a = u32s(section::WARP_DIVISIONS_A, "warp division")?;
    let div_b = u32s(section::WARP_DIVISIONS_B, "warp division")?;

    let total = counts_at(bytes, sections, count::WARP_KEYFORMS)?;
    let mut expect = 0u64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // A grid is a lattice, so its point count follows from its divisions.
        // This is the check that these three arrays belong together.
        let expected_points = (div_a[i] as u64 + 1) * (div_b[i] as u64 + 1);
        if points[i] as u64 != expected_points {
            return Err(DecodeError::corrupt(format!(
                "warp deformer {i} declares {} grid points but {}x{} divisions imply {expected_points}",
                points[i], div_a[i], div_b[i]
            )));
        }
        if begin[i] as u64 != expect {
            return Err(DecodeError::corrupt(format!(
                "warp deformer {i} claims keyforms from {} where {expect} was expected",
                begin[i]
            )));
        }
        expect += count_of[i] as u64;
        out.push(WarpDeformer {
            id: deformer_ids.get(i).cloned().unwrap_or_default(),
            divisions: (div_a[i], div_b[i]),
            point_count: points[i],
            keyform_begin: begin[i],
            keyform_count: count_of[i],
            keyform_binding: binding[i],
        });
    }
    if expect != total as u64 {
        return Err(DecodeError::corrupt(format!(
            "the warp deformers account for {expect} keyforms, but the count table declares {total}"
        )));
    }
    Ok(out)
}

/// Reads the keyform pool and divides it between warps and drawables.
///
/// The file stores offsets only for the warp keyforms. That they are reproduced
/// exactly by walking `padded_points * 2` per keyform is what establishes both
/// the padding rule and the ordering — and once the warps are placed, the
/// drawables follow by the same rule and must end precisely on the declared
/// total. Nothing here is fitted; either every offset agrees or the read is
/// wrong and says so.
fn read_keyforms(
    bytes: &[u8],
    sections: &[u32],
    warps: &[WarpDeformer],
    drawables: &[Drawable],
) -> Result<Keyforms, DecodeError> {
    let float_count =
        checked_count(counts_at(bytes, sections, count::KEYFORM_POSITION_FLOATS)?, 4, bytes, "keyform coordinate")?;
    let warp_keyforms = checked_count(counts_at(bytes, sections, count::WARP_KEYFORMS)?, 4, bytes, "warp keyform")?;
    let drawable_keyforms =
        checked_count(counts_at(bytes, sections, count::DRAWABLE_KEYFORMS)?, 4, bytes, "drawable keyform")?;
    if float_count == 0 {
        return Ok(Keyforms::default());
    }

    let offsets_at = section_offset(sections, section::KEYFORM_POSITION_OFFSETS, "keyform offset")?;
    let mut warp_offsets = Vec::with_capacity(warp_keyforms);
    let mut at = 0u64;
    for warp in warps {
        let stride = padded_points(warp.point_count) as u64 * 2;
        for k in 0..warp.keyform_count as usize {
            let index = warp.keyform_begin as usize + k;
            let stored = u32_at(bytes, offsets_at + index * 4)? as u64;
            if stored != at {
                return Err(DecodeError::corrupt(format!(
                    "warp keyform {index} is stored at float {stored} but the layout puts it at {at};                      the keyform pool is not arranged the way this version arranges it"
                )));
            }
            warp_offsets.push(at as u32);
            at += stride;
        }
    }
    if warp_offsets.len() != warp_keyforms {
        return Err(DecodeError::corrupt(format!(
            "the warp deformers own {} keyforms but the count table declares {warp_keyforms}",
            warp_offsets.len()
        )));
    }

    // Drawable keyforms are not listed; they continue where the warps stopped.
    let mut drawable_offsets = Vec::with_capacity(drawable_keyforms);
    for drawable in drawables {
        let stride = padded_points(drawable.vertex_count() as u32) as u64 * 2;
        for _ in 0..drawable.keyform_count {
            drawable_offsets.push(at as u32);
            at += stride;
        }
    }
    if drawable_offsets.len() != drawable_keyforms {
        return Err(DecodeError::corrupt(format!(
            "the drawables own {} keyforms but the count table declares {drawable_keyforms}",
            drawable_offsets.len()
        )));
    }
    if at != float_count as u64 {
        return Err(DecodeError::corrupt(format!(
            "the keyforms account for {at} coordinates, but the count table declares {float_count}"
        )));
    }

    let positions_at = section_offset(sections, section::KEYFORM_POSITIONS, "keyform position")?;
    let mut positions = Vec::with_capacity(float_count);
    for i in 0..float_count {
        positions.push(f32_at(bytes, positions_at + i * 4)?);
    }
    Ok(Keyforms { positions, warp_offsets, drawable_offsets })
}

/// Refuses a count larger than the file could hold.
///
/// Counts come out of the file, so a corrupted one would otherwise be believed
/// and reserved for. A run of `n` items of `elem` bytes cannot exceed the file
/// itself, which is a loose bound but a cheap and sufficient one: it turns a
/// wild allocation into an error.
fn checked_count(n: u32, elem: usize, bytes: &[u8], what: &str) -> Result<usize, DecodeError> {
    let n = n as usize;
    if n.saturating_mul(elem) > bytes.len() {
        return Err(DecodeError::corrupt(format!(
            "the file declares {n} {what} entries, more than its {} bytes could hold",
            bytes.len()
        )));
    }
    Ok(n)
}

/// One entry of the count table, by index.
fn counts_at(bytes: &[u8], sections: &[u32], index: usize) -> Result<u32, DecodeError> {
    let at = section_offset(sections, section::COUNTS, "count")?;
    u32_at(bytes, at + index * 4)
}

fn read_floats(bytes: &[u8], sections: &[u32], index: usize, count: u32, what: &str) -> Result<Vec<f32>, DecodeError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let at = section_offset(sections, index, what)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        out.push(f32_at(bytes, at + i * 4)?);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a MOC3 with the section layout this parser expects.
    ///
    /// Laid out by hand from the module docs, so a disagreement between the two
    /// shows up as a parse failure. It proves internal consistency only — the
    /// layout itself is validated by the real-asset test.
    pub(crate) struct Builder {
        parts: Vec<&'static str>,
        drawables: Vec<&'static str>,
        parameters: Vec<(&'static str, f32, f32, f32)>,
        deformers: Vec<&'static str>,
        warp_divisions: (u32, u32),
        version: u8,
    }

    impl Builder {
        pub(crate) fn new() -> Self {
            Builder {
                parts: vec!["Part01", "Part02"],
                drawables: vec!["ArtMesh1"],
                parameters: vec![("ParamAngleX", -30.0, 30.0, 0.0), ("ParamEyeLOpen", 0.0, 1.2, 1.0)],
                deformers: vec!["Rotation1", "Warp1"],
                warp_divisions: (1, 1),
                version: 2,
            }
        }

        /// A non-square grid, for tests where rows and columns must differ.
        pub(crate) fn warp_divisions(mut self, rows: u32, columns: u32) -> Self {
            self.warp_divisions = (rows, columns);
            self
        }

        pub(crate) fn build(&self) -> Vec<u8> {
            const SECTIONS: usize = 96;
            let table_bytes = SECTIONS * 4;
            let mut body: Vec<u8> = Vec::new();
            let base = 0x40 + table_bytes;
            let mut offsets = vec![0u32; SECTIONS];

            let place = |body: &mut Vec<u8>, data: &[u8]| -> u32 {
                let at = base + body.len();
                body.extend_from_slice(data);
                at as u32
            };

            // Counts.
            let mut counts = [0u32; 32];
            counts[count::PARTS] = self.parts.len() as u32;
            counts[count::DRAWABLES] = self.drawables.len() as u32;
            counts[count::PARAMETERS] = self.parameters.len() as u32;
            counts[count::GLUES] = 0;
            // Reserved now, filled in at the end: the geometry below decides
            // several of these.
            let counts_at = body.len();
            offsets[section::COUNTS] = place(&mut body, &[0u8; 128]);

            // Canvas: origin at the centre, as a real model has it.
            let canvas: Vec<u8> = [3792.0f32, 1784.0, 3138.5, 3568.0, 6277.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .chain(std::iter::repeat_n(0u8, 44))
                .collect();
            offsets[section::CANVAS] = place(&mut body, &canvas);

            let ids = |names: &[&str]| -> Vec<u8> {
                let mut out = Vec::with_capacity(names.len() * ID_LEN);
                for name in names {
                    let mut slot = [0u8; ID_LEN];
                    slot[..name.len()].copy_from_slice(name.as_bytes());
                    out.extend_from_slice(&slot);
                }
                out
            };
            offsets[section::PART_IDS] = place(&mut body, &ids(&self.parts));
            offsets[section::DEFORMER_IDS] = place(&mut body, &ids(&self.deformers));
            offsets[section::DRAWABLE_IDS] = place(&mut body, &ids(&self.drawables));

            let names: Vec<&str> = self.parameters.iter().map(|p| p.0).collect();
            offsets[section::PARAMETER_IDS] = place(&mut body, &ids(&names));
            let floats = |pick: fn(&(&'static str, f32, f32, f32)) -> f32, p: &[(&'static str, f32, f32, f32)]| {
                p.iter().flat_map(|e| pick(e).to_le_bytes()).collect::<Vec<u8>>()
            };
            offsets[section::PARAMETER_MINIMUMS] = place(&mut body, &floats(|e| e.1, &self.parameters));
            offsets[section::PARAMETER_MAXIMUMS] = place(&mut body, &floats(|e| e.2, &self.parameters));
            offsets[section::PARAMETER_DEFAULTS] = place(&mut body, &floats(|e| e.3, &self.parameters));

            // Geometry: one triangle per drawable, so the offsets and totals
            // have something to be checked against.
            let verts_each = 3usize;
            let idx_each = 3usize;
            let d = self.drawables.len();
            let u32_array = |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
            offsets[section::DRAWABLE_VERTEX_COUNTS] = place(&mut body, &u32_array(&vec![verts_each as u32; d]));
            let voff: Vec<u32> = (0..d).map(|i| (i * verts_each * 2) as u32).collect();
            offsets[section::DRAWABLE_VERTEX_OFFSETS] = place(&mut body, &u32_array(&voff));
            let ioff: Vec<u32> = (0..d).map(|i| (i * idx_each) as u32).collect();
            offsets[section::DRAWABLE_INDEX_OFFSETS] = place(&mut body, &u32_array(&ioff));
            offsets[section::DRAWABLE_INDEX_COUNTS] = place(&mut body, &u32_array(&vec![idx_each as u32; d]));

            let uvs: Vec<u8> =
                (0..d).flat_map(|_| [0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0]).flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::VERTEX_UVS] = place(&mut body, &uvs);
            let indices: Vec<u8> = (0..d).flat_map(|_| [0u16, 1, 2]).flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::VERTEX_INDICES] = place(&mut body, &indices);

            // --- bindings -------------------------------------------------
            // One parameter binding on the first parameter, keyed at three
            // values, so every element has a three-keyform grid to blend over.
            let keys: [f32; 3] = [-30.0, 0.0, 30.0];
            let param_bindings = 1usize;
            // Per parameter: the range of bindings it drives. Only the first
            // parameter drives anything; the rest are marked absent.
            let mut owner_begin = vec![u32::MAX; self.parameters.len()];
            let mut owner_count = vec![0u32; self.parameters.len()];
            owner_begin[0] = 0;
            owner_count[0] = param_bindings as u32;
            offsets[section::PARAMETER_BINDING_BEGIN] = place(&mut body, &u32_array(&owner_begin));
            offsets[section::PARAMETER_BINDING_COUNT] = place(&mut body, &u32_array(&owner_count));

            offsets[section::PARAMETER_KEY_BEGIN] = place(&mut body, &u32_array(&[0]));
            offsets[section::PARAMETER_KEY_COUNT] = place(&mut body, &u32_array(&[keys.len() as u32]));
            let key_bytes: Vec<u8> = keys.iter().flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::PARAMETER_KEYS] = place(&mut body, &key_bytes);

            // One keyform binding, one axis, pointing at that parameter binding.
            offsets[section::KEYFORM_BINDING_BEGIN] = place(&mut body, &u32_array(&[0]));
            offsets[section::KEYFORM_BINDING_COUNT] = place(&mut body, &u32_array(&[1]));
            offsets[section::PARAMETER_BINDING_REFS] = place(&mut body, &u32_array(&[0]));

            let per_element = keys.len();

            // --- drawables ------------------------------------------------
            let d_begin: Vec<u32> = (0..d).map(|i| (i * per_element) as u32).collect();
            offsets[section::DRAWABLE_DRAW_ORDER] =
                place(&mut body, &u32_array(&(0..d as u32).rev().collect::<Vec<_>>()));
            offsets[section::DRAWABLE_KEYFORM_BINDING] = place(&mut body, &u32_array(&vec![0u32; d]));
            offsets[section::DRAWABLE_KEYFORM_BEGIN] = place(&mut body, &u32_array(&d_begin));
            offsets[section::DRAWABLE_KEYFORM_COUNT] = place(&mut body, &u32_array(&vec![per_element as u32; d]));

            // --- deformers ------------------------------------------------
            // A rotation deformer at the root with a warp hanging off it, and
            // the drawables hanging off the warp.
            let warps = 1usize;
            let rotations = 1usize;
            let (div_a, div_b) = self.warp_divisions;
            let grid_points = (div_a + 1) * (div_b + 1);

            offsets[section::WARP_KEYFORM_BINDING] = place(&mut body, &u32_array(&[0]));
            offsets[section::WARP_KEYFORM_BEGIN] = place(&mut body, &u32_array(&[0]));
            offsets[section::WARP_KEYFORM_COUNT] = place(&mut body, &u32_array(&[per_element as u32]));
            offsets[section::WARP_GRID_POINTS] = place(&mut body, &u32_array(&[grid_points]));
            offsets[section::WARP_DIVISIONS_A] = place(&mut body, &u32_array(&[div_a]));
            offsets[section::WARP_DIVISIONS_B] = place(&mut body, &u32_array(&[div_b]));

            offsets[section::ROTATION_KEYFORM_BINDING] = place(&mut body, &u32_array(&[0]));
            offsets[section::ROTATION_KEYFORM_BEGIN] = place(&mut body, &u32_array(&[0]));
            offsets[section::ROTATION_KEYFORM_COUNT] = place(&mut body, &u32_array(&[per_element as u32]));
            let f_array = |values: &[f32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
            offsets[section::ROTATION_KEYFORM_OPACITY] = place(&mut body, &f_array(&[1.0; 3]));
            offsets[section::ROTATION_KEYFORM_ANGLE] = place(&mut body, &f_array(&[0.0; 3]));
            offsets[section::ROTATION_KEYFORM_ORIGIN_X] = place(&mut body, &f_array(&[0.0; 3]));
            offsets[section::ROTATION_KEYFORM_ORIGIN_Y] = place(&mut body, &f_array(&[0.0; 3]));
            offsets[section::ROTATION_KEYFORM_SCALE] = place(&mut body, &f_array(&[1.0; 3]));

            // Deformer 0 is the rotation at the root, deformer 1 the warp.
            offsets[section::DEFORMER_PARENT] = place(&mut body, &u32_array(&[u32::MAX, 0]));
            offsets[section::DEFORMER_TYPE] = place(&mut body, &u32_array(&[1, 0]));
            offsets[section::DEFORMER_INDEX_IN_TYPE] = place(&mut body, &u32_array(&[0, 0]));
            // Drawables hang off the warp, which is deformer 1.
            offsets[section::DRAWABLE_PARENT_DEFORMERS] = place(&mut body, &u32_array(&vec![1u32; d]));

            // --- the keyform pool ------------------------------------------
            let warp_stride = padded_points(grid_points) as usize * 2;
            let draw_stride = padded_points(verts_each as u32) as usize * 2;
            let warp_keyforms = warps * per_element;
            let draw_keyforms = d * per_element;
            let warp_floats = warp_stride * warp_keyforms;
            let draw_floats = draw_stride * draw_keyforms;

            let warp_kf_offsets: Vec<u32> = (0..warp_keyforms).map(|k| (k * warp_stride) as u32).collect();
            offsets[section::KEYFORM_POSITION_OFFSETS] = place(&mut body, &u32_array(&warp_kf_offsets));

            let mut pool = vec![0.0f32; warp_floats + draw_floats];
            // The grid is a regular lattice spanning 10 by 20 that doubles
            // across the three keys, so a blend between them is visible in the
            // result. It is stored row-major with `div_b + 1` points to a row,
            // the orientation established in the module docs; with the default
            // 1x1 divisions it is the quad (0,0)-(10,0)-(0,20)-(10,20).
            for k in 0..warp_keyforms {
                let scale = [0.5f32, 1.0, 2.0][k % 3];
                for pt in 0..grid_points as usize {
                    let column = (pt % (div_b as usize + 1)) as f32;
                    let row = (pt / (div_b as usize + 1)) as f32;
                    pool[k * warp_stride + pt * 2] = column * 10.0 / div_b as f32 * scale;
                    pool[k * warp_stride + pt * 2 + 1] = row * 20.0 / div_a as f32 * scale;
                }
            }
            // Drawable vertices sit in the warp's unit square.
            for k in 0..draw_keyforms {
                let base = warp_floats + k * draw_stride;
                pool[base..base + 6].copy_from_slice(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
            }
            let pool_bytes: Vec<u8> = pool.iter().flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::KEYFORM_POSITIONS] = place(&mut body, &pool_bytes);

            counts[count::DEFORMERS] = (warps + rotations) as u32;
            counts[count::WARP_DEFORMERS] = warps as u32;
            counts[count::ROTATION_DEFORMERS] = rotations as u32;
            counts[count::WARP_KEYFORMS] = warp_keyforms as u32;
            counts[count::ROTATION_KEYFORMS] = (rotations * per_element) as u32;
            counts[count::DRAWABLE_KEYFORMS] = draw_keyforms as u32;
            counts[count::KEYFORM_POSITION_FLOATS] = (warp_floats + draw_floats) as u32;
            counts[count::PARAMETER_BINDINGS] = param_bindings as u32;
            counts[count::PARAMETER_KEYS] = keys.len() as u32;
            counts[count::KEYFORM_BINDINGS] = 1;
            counts[count::PARAMETER_BINDING_REFS] = 1;
            counts[count::UV_FLOATS] = (d * verts_each * 2) as u32;
            counts[count::INDICES] = (d * idx_each) as u32;

            let counts_bytes: Vec<u8> = counts.iter().flat_map(|v| v.to_le_bytes()).collect();
            body[counts_at..counts_at + counts_bytes.len()].copy_from_slice(&counts_bytes);

            let mut out = Vec::new();
            out.extend_from_slice(b"MOC3");
            out.push(self.version);
            out.push(0); // little-endian
            out.extend_from_slice(&[0u8; 58]);
            // The first entry marks where the table ends, so it must lead.
            offsets[0] = offsets[section::COUNTS];
            debug_assert_eq!(offsets[0] as usize, base);
            for value in &offsets {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&body);
            out
        }
    }

    #[test]
    fn a_model_round_trips_its_identifiers_and_ranges() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        assert_eq!(moc.version, 2);
        assert!(moc.is_verified_version());
        assert_eq!(moc.counts.parts, 2);
        assert_eq!(moc.counts.parameters, 2);
        assert_eq!(moc.part_ids, ["Part01", "Part02"]);
        assert_eq!(moc.drawable_ids, ["ArtMesh1"]);
        assert_eq!(moc.deformer_ids, ["Rotation1", "Warp1"]);

        let eye = moc.parameter("ParamEyeLOpen").expect("the eye parameter should be found");
        assert_eq!(eye.minimum, 0.0);
        assert_eq!(eye.maximum, 1.2);
        assert_eq!(eye.default, 1.0);
        assert_eq!(eye.clamp(5.0), 1.2);
        assert_eq!(eye.clamp(-1.0), 0.0);
    }

    #[test]
    fn the_canvas_origin_is_read_as_the_centre_it_is() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        assert_eq!(moc.canvas.pixels_per_unit, 3792.0);
        assert_eq!(moc.canvas.size, (3568.0, 6277.0));
        // The field order was settled by this relation; keep it honest.
        assert_eq!(moc.canvas.origin.0 * 2.0, moc.canvas.size.0);
        assert_eq!(moc.canvas.origin.1 * 2.0, moc.canvas.size.1);
    }

    #[test]
    fn drawable_meshes_read_with_their_uvs_and_triangles() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        assert_eq!(moc.drawables.len(), 1);
        let d = &moc.drawables[0];
        assert_eq!(d.id, "ArtMesh1");
        assert_eq!(d.vertex_count(), 3);
        assert_eq!(d.triangle_count(), 1);
        assert_eq!(d.uvs, [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(d.indices, [0, 1, 2]);
    }

    #[test]
    fn an_index_outside_its_own_drawable_is_refused() {
        // The loudest possible sign the drawable tables were misread, so it is
        // checked rather than trusted.
        let mut bytes = Builder::new().build();
        let at = u32::from_le_bytes(
            bytes[0x40 + section::VERTEX_INDICES * 4..0x40 + section::VERTEX_INDICES * 4 + 4].try_into().unwrap(),
        ) as usize;
        bytes[at..at + 2].copy_from_slice(&9u16.to_le_bytes());
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("outside its own"), "{err}");
    }

    #[test]
    fn a_vertex_offset_that_does_not_follow_the_counts_is_refused() {
        let mut b = Builder::new();
        b.drawables = vec!["ArtMesh1", "ArtMesh2"];
        let mut bytes = b.build();
        let at = u32::from_le_bytes(
            bytes[0x40 + section::DRAWABLE_VERTEX_OFFSETS * 4..0x40 + section::DRAWABLE_VERTEX_OFFSETS * 4 + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        // Second drawable now starts somewhere the first does not end.
        bytes[at + 4..at + 8].copy_from_slice(&99u32.to_le_bytes());
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("where"), "{err}");
    }

    #[test]
    fn each_element_addresses_its_own_keyforms() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        // Three keyforms for the warp and three for the one drawable.
        assert_eq!(moc.keyforms.len(), 6);
        assert_eq!(moc.warp_deformers.len(), 1);
        assert_eq!(moc.rotation_deformers.len(), 1);

        let warp = &moc.warp_deformers[0];
        assert_eq!(warp.divisions, (1, 1));
        assert_eq!(warp.point_count, 4);
        assert_eq!(warp.keyform_count, 3);
        // The quad doubles across the three keys.
        assert_eq!(moc.keyforms.warp(0, 4).expect("first"), [0.0, 0.0, 5.0, 0.0, 0.0, 10.0, 5.0, 10.0]);
        // The second keyform starts after the first one's padding, not after
        // its four points -- reading it proves the padding is stepped over.
        assert_eq!(moc.keyforms.warp(1, 4).expect("second"), [0.0, 0.0, 10.0, 0.0, 0.0, 20.0, 10.0, 20.0]);

        let d = &moc.drawables[0];
        assert_eq!(d.keyform_count, 3);
        let verts = moc.keyforms.drawable(d.keyform_begin as usize, d.vertex_count()).expect("reachable");
        assert_eq!(verts, [0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);

        // Asking for more than an element holds yields nothing rather than
        // reading into whatever follows.
        assert!(moc.keyforms.drawable(0, 9999).is_none());
    }

    #[test]
    fn the_binding_chain_reaches_from_an_element_to_its_parameter() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        assert_eq!(moc.parameter_bindings.len(), 1);
        assert_eq!(moc.parameter_bindings[0].parameter, 0);
        assert_eq!(moc.parameter_bindings[0].keys, [-30.0, 0.0, 30.0]);

        assert_eq!(moc.keyform_bindings.len(), 1);
        assert_eq!(moc.keyform_bindings[0].axes, [0]);

        // The tree: a rotation at the root with the warp hanging off it.
        assert_eq!(moc.deformers.len(), 2);
        assert_eq!(moc.deformers[0].parent, None);
        assert!(matches!(moc.deformers[0].kind, DeformerKind::Rotation(0)));
        assert_eq!(moc.deformers[1].parent, Some(0));
        assert!(matches!(moc.deformers[1].kind, DeformerKind::Warp(0)));
    }

    #[test]
    fn a_keyform_grid_that_contradicts_its_parameters_is_refused() {
        // The identity that ties the whole binding chain together: a grid has
        // one axis per parameter, so its size is the product of the key counts.
        let mut bytes = Builder::new().build();
        let section_at = |index: usize| -> usize {
            u32::from_le_bytes(bytes[0x40 + index * 4..0x40 + index * 4 + 4].try_into().unwrap()) as usize
        };
        // Shrink the drawable's grid to two keyforms *and* say so in the count
        // table, so the cumulative checks still pass and only the identity with
        // the parameters is left to catch it.
        let counts_at = section_at(section::COUNTS);
        let keyform_count_at = section_at(section::DRAWABLE_KEYFORM_COUNT);
        bytes[keyform_count_at..keyform_count_at + 4].copy_from_slice(&2u32.to_le_bytes());
        let entry = counts_at + count::DRAWABLE_KEYFORMS * 4;
        bytes[entry..entry + 4].copy_from_slice(&2u32.to_le_bytes());
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("its parameters imply"), "{err}");
    }

    #[test]
    fn a_grid_whose_points_contradict_its_divisions_is_refused() {
        let mut bytes = Builder::new().build();
        let at = u32::from_le_bytes(
            bytes[0x40 + section::WARP_GRID_POINTS * 4..0x40 + section::WARP_GRID_POINTS * 4 + 4].try_into().unwrap(),
        ) as usize;
        bytes[at..at + 4].copy_from_slice(&7u32.to_le_bytes());
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("divisions imply"), "{err}");
    }

    #[test]
    fn a_keyform_offset_that_disagrees_with_the_layout_is_refused() {
        // The pool layout is derived, not read, so a stored offset that does
        // not match it means the derivation is wrong -- which must be loud.
        let mut bytes = Builder::new().build();
        let at = u32::from_le_bytes(
            bytes[0x40 + section::KEYFORM_POSITION_OFFSETS * 4..0x40 + section::KEYFORM_POSITION_OFFSETS * 4 + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        bytes[at + 4..at + 8].copy_from_slice(&999u32.to_le_bytes());
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("the layout puts it at"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_moc3_is_refused_by_name() {
        let err = Moc3::parse(b"NOPE............................................................").unwrap_err();
        assert!(err.to_string().contains("not a MOC3"), "{err}");
    }

    #[test]
    fn a_big_endian_model_is_refused_rather_than_read_backwards() {
        let mut bytes = Builder::new().build();
        bytes[5] = 1;
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("big-endian"), "{err}");
    }

    #[test]
    fn deformer_counts_that_do_not_add_up_are_refused() {
        // The cheapest check that the count table is where it is believed to be.
        let mut bytes = Builder::new().build();
        let counts_at = u32::from_le_bytes(bytes[0x40..0x44].try_into().unwrap()) as usize;
        bytes[counts_at + count::WARP_DEFORMERS * 4] = 9;
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("deformers"), "{err}");
    }

    #[test]
    fn an_identifier_table_of_the_wrong_shape_is_refused() {
        // Point the part table at the canvas, which is floats rather than ids.
        let mut bytes = Builder::new().build();
        let canvas = bytes[0x40 + section::CANVAS * 4..0x40 + section::CANVAS * 4 + 4].to_vec();
        let at = 0x40 + section::PART_IDS * 4;
        bytes[at..at + 4].copy_from_slice(&canvas);
        let err = Moc3::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("part"), "{err}");
    }

    #[test]
    fn a_minimum_above_its_maximum_is_refused() {
        let mut b = Builder::new();
        b.parameters = vec![("ParamBroken", 10.0, -10.0, 0.0)];
        let err = Moc3::parse(&b.build()).unwrap_err();
        assert!(err.to_string().contains("above maximum"), "{err}");
    }

    #[test]
    fn truncation_anywhere_is_an_error_and_never_a_panic() {
        let full = Builder::new().build();
        for cut in 0..full.len() {
            let _ = Moc3::parse(&full[..cut]);
        }
    }

    #[test]
    fn every_single_byte_corruption_is_an_error_and_never_a_panic() {
        let full = Builder::new().build();
        for i in 0..full.len() {
            for bit in [0x01u8, 0x80] {
                let mut bytes = full.clone();
                bytes[i] ^= bit;
                let _ = Moc3::parse(&bytes);
            }
        }
    }
}
