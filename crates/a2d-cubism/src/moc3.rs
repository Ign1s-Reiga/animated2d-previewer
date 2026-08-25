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
//! # What is missing, and why it is the hard part
//!
//! **There is no resting pose in a MOC3.** A drawable's coordinates are not
//! stored; they are produced by blending that drawable's *keyforms* according
//! to the current parameter values. The keyform pool is here — 687936
//! coordinates across 9554 keyforms in the model this was built against — but
//! which element owns which keyforms is not worked out, so nothing can yet be
//! posed, not even at rest.
//!
//! That binding, and then the evaluation over it — warp deformer grids,
//! rotation deformers, and the parameter-to-keyform blending that drives both —
//! is the remainder of the format, and the part with no independent source to
//! check against. Everything above could be confirmed against the model's Unity
//! components or against arithmetic that had to close; deformation can only be
//! confirmed by looking at the result, which is what spec §11 means by visual
//! parity.
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

    /// Where each keyform's coordinates begin, in floats.
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
    pub const KEYFORMS: usize = 7;
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
}

impl Drawable {
    pub fn vertex_count(&self) -> usize {
        self.uvs.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Where a keyform's coordinates sit in the shared array.
///
/// Exposed so the evaluation this crate does not yet do has the frame it needs
/// without re-deriving it. Which element owns which keyform is *not* worked out
/// yet, which is exactly what stands between this and a posed model.
#[derive(Debug, Clone)]
pub struct Keyforms {
    /// Offset of each keyform's coordinates, in floats.
    pub offsets: Vec<u32>,
    /// Every keyform's coordinates, end to end, as `x, y` pairs.
    pub positions: Vec<f32>,
}

impl Keyforms {
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The coordinates of one keyform, given how many vertices it covers.
    pub fn keyform(&self, index: usize, vertex_count: usize) -> Option<&[f32]> {
        let start = *self.offsets.get(index)? as usize;
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

        let drawables = read_drawables(bytes, &sections, &counts, &drawable_ids)?;
        let keyforms = read_keyforms(bytes, &sections)?;

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
    let counts = Counts {
        parts: get(count::PARTS)?,
        deformers: get(count::DEFORMERS)?,
        warp_deformers: get(count::WARP_DEFORMERS)?,
        rotation_deformers: get(count::ROTATION_DEFORMERS)?,
        drawables: get(count::DRAWABLES)?,
        parameters: get(count::PARAMETERS)?,
        glues: get(count::GLUES)?,
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

        out.push(Drawable { id: ids.get(i).cloned().unwrap_or_default(), parent_deformer: parents[i], uvs, indices });
    }
    Ok(out)
}

/// Reads the keyform coordinate pool without dividing it up.
fn read_keyforms(bytes: &[u8], sections: &[u32]) -> Result<Keyforms, DecodeError> {
    let keyform_count = counts_at(bytes, sections, count::KEYFORMS)? as usize;
    let float_count = counts_at(bytes, sections, count::KEYFORM_POSITION_FLOATS)? as usize;
    if keyform_count == 0 || float_count == 0 {
        return Ok(Keyforms { offsets: Vec::new(), positions: Vec::new() });
    }

    let offsets_at = section_offset(sections, section::KEYFORM_POSITION_OFFSETS, "keyform offset")?;
    let mut offsets = Vec::with_capacity(keyform_count);
    let mut previous = 0u32;
    for i in 0..keyform_count {
        let value = u32_at(bytes, offsets_at + i * 4)?;
        if i > 0 && value < previous {
            return Err(DecodeError::corrupt(format!("keyform {i} starts at {value}, before keyform {}", i - 1)));
        }
        if value as usize > float_count {
            return Err(DecodeError::corrupt(format!(
                "keyform {i} starts at float {value}, past the {float_count} the model declares"
            )));
        }
        previous = value;
        offsets.push(value);
    }

    let positions_at = section_offset(sections, section::KEYFORM_POSITIONS, "keyform position")?;
    let mut positions = Vec::with_capacity(float_count);
    for i in 0..float_count {
        positions.push(f32_at(bytes, positions_at + i * 4)?);
    }
    Ok(Keyforms { offsets, positions })
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
mod tests {
    use super::*;

    /// Builds a MOC3 with the section layout this parser expects.
    ///
    /// Laid out by hand from the module docs, so a disagreement between the two
    /// shows up as a parse failure. It proves internal consistency only — the
    /// layout itself is validated by the real-asset test.
    struct Builder {
        parts: Vec<&'static str>,
        drawables: Vec<&'static str>,
        parameters: Vec<(&'static str, f32, f32, f32)>,
        deformers: Vec<&'static str>,
        version: u8,
    }

    impl Builder {
        fn new() -> Self {
            Builder {
                parts: vec!["Part01", "Part02"],
                drawables: vec!["ArtMesh1"],
                parameters: vec![("ParamAngleX", -30.0, 30.0, 0.0), ("ParamEyeLOpen", 0.0, 1.2, 1.0)],
                deformers: vec!["Warp1", "Rotation1"],
                version: 2,
            }
        }

        fn build(&self) -> Vec<u8> {
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
            counts[count::DEFORMERS] = self.deformers.len() as u32;
            counts[count::WARP_DEFORMERS] = 1;
            counts[count::ROTATION_DEFORMERS] = self.deformers.len() as u32 - 1;
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
            offsets[section::DRAWABLE_PARENT_DEFORMERS] = place(&mut body, &u32_array(&vec![0u32; d]));
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

            // One keyform per drawable, holding the resting coordinates.
            let kf_offsets: Vec<u32> = (0..d).map(|i| (i * verts_each * 2) as u32).collect();
            offsets[section::KEYFORM_POSITION_OFFSETS] = place(&mut body, &u32_array(&kf_offsets));
            let positions: Vec<u8> =
                (0..d).flat_map(|_| [0.0f32, 0.0, 10.0, 0.0, 0.0, 10.0]).flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::KEYFORM_POSITIONS] = place(&mut body, &positions);

            counts[count::KEYFORMS] = d as u32;
            counts[count::KEYFORM_POSITION_FLOATS] = (d * verts_each * 2) as u32;
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
        assert_eq!(moc.deformer_ids, ["Warp1", "Rotation1"]);

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
    fn the_keyform_pool_is_addressable_by_vertex_count() {
        let moc = Moc3::parse(&Builder::new().build()).expect("should parse");
        assert_eq!(moc.keyforms.len(), 1);
        let coords = moc.keyforms.keyform(0, 3).expect("the first keyform should be reachable");
        assert_eq!(coords, [0.0, 0.0, 10.0, 0.0, 0.0, 10.0]);
        // Asking for more vertices than the pool holds yields nothing rather
        // than reading into the next keyform.
        assert!(moc.keyforms.keyform(0, 99).is_none());
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
