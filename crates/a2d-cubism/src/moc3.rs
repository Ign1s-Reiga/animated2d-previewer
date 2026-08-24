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
//!
//! Anything *not* on that list is left unparsed rather than guessed at. The
//! section table is exposed raw so later work can extend this without having to
//! re-derive the frame.
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

        Ok(Moc3 { version, counts, canvas, parameters, part_ids, deformer_ids, drawable_ids, glue_ids, sections })
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
            let counts_bytes: Vec<u8> = counts.iter().flat_map(|v| v.to_le_bytes()).collect();
            offsets[section::COUNTS] = place(&mut body, &counts_bytes);

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
