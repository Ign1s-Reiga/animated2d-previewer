//! Deterministic encoding of the Generic Cubism model into `model.bin`.
//!
//! Field order here *is* the format. Adding, removing or reordering a field is
//! a layout change: bump [`crate::FORMAT_VERSION`] and handle the old layout in
//! the reader, or every existing package stops loading.
//!
//! # Versions
//!
//! * Version 1: everything up to and including the per-keyform draw orders.
//!   Motions did not exist in the IR, so a version 1 model reads with none.
//! * Version 2: appends the motion table.
//!
//! A motion is `name: str, duration: f32, fade_in: f32, fade_out: f32,
//! curves: seq`. A curve is `target: str, fade_in: opt f32, fade_out: opt f32,
//! keys: seq`. A key is `time, value, in_slope, out_slope: f32` followed by
//! `stepped: u8` (0 or 1). Motions are written in IR order and never sorted:
//! the order is the order a viewer lists them in.
//!
//! Durations and fades must be finite and not negative (`-0.0` passes). A
//! curve's fade is `None` to mean "use the motion's"; there is no sentinel
//! value. Cubism's own sources spell "unset" with a negative number, so an
//! importer must map that to `None` per curve, or to the default per motion,
//! before it reaches the IR. A negative fade is refused on read.
//!
//! # Why the model is stored rather than the MOC3
//!
//! A package could have carried the MOC3 bytes untouched, which would have
//! been shorter to write and lossless. It does not, for the reason spec §9
//! gives: `model.bin` holds the normalized IR and never a raw source-format
//! object graph. Storing the decoded model means loading a package needs no
//! MOC3 decoder, so a package stays readable if the format is ever revised,
//! and a package that loads is a package whose model already parsed.
//!
//! # What is checked on read
//!
//! A package is not hostile input, but it can be stale or hand-edited, and an
//! index that points outside its table would otherwise surface much later as
//! geometry in the wrong place. Every cross-reference is range-checked here,
//! where the error can still name the field.
//!
//! A drawable's blend mode is not written. It is derived from its constant
//! flags, and storing it too would let a package hold two answers to one
//! question.

use a2d_core::ir::cubism::*;
use a2d_core::DecodeError;

use crate::bin_io::{Reader, Writer};

/// Writes a whole model.
pub fn write(w: &mut Writer, ir: &CubismIr) -> Result<(), DecodeError> {
    // Refuse before the first byte, so a model that cannot be read back is
    // never half written.
    for m in &ir.motions {
        validate_motion(m)?;
    }
    write_canvas(w, &ir.canvas);
    w.seq(&ir.parameters, write_parameter);
    write_ids(w, &ir.part_ids);
    write_ids(w, &ir.deformer_ids);
    write_ids(w, &ir.drawable_ids);
    write_ids(w, &ir.glue_ids);
    w.seq(&ir.drawables, write_drawable);
    w.seq(&ir.warp_deformers, write_warp);
    w.seq(&ir.rotation_deformers, write_rotation);
    w.seq(&ir.rotation_keyforms, write_rotation_keyform);
    w.seq(&ir.deformers, write_deformer);
    w.seq(&ir.parameter_bindings, write_parameter_binding);
    w.seq(&ir.keyform_bindings, write_keyform_binding);
    write_keyforms(w, &ir.keyforms);
    w.u32_seq(&ir.draw_order);
    w.f32_seq(&ir.drawable_keyform_opacities);
    w.f32_seq(&ir.drawable_keyform_draw_orders);
    // Added in format version 2. Always written, so a motion-less model still
    // carries a zero count rather than ending early.
    w.seq(&ir.motions, write_motion);
    Ok(())
}

/// Reads a whole model and checks that its tables refer to each other.
///
/// `version` is the format version from the `model.bin` header. A version 1
/// model ends after the draw-order tracks and has no motions; version 2 adds
/// the motion table. The layout is chosen by the header, never inferred from
/// how many bytes happen to remain.
pub fn read(r: &mut Reader<'_>, version: u32) -> Result<CubismIr, DecodeError> {
    let mut ir = CubismIr {
        canvas: read_canvas(r)?,
        parameters: r.seq("parameter", 16, read_parameter)?,
        part_ids: read_ids(r, "part id")?,
        deformer_ids: read_ids(r, "deformer id")?,
        drawable_ids: read_ids(r, "drawable id")?,
        glue_ids: read_ids(r, "glue id")?,
        drawables: r.seq("drawable", 32, read_drawable)?,
        warp_deformers: r.seq("warp deformer", 24, read_warp)?,
        rotation_deformers: r.seq("rotation deformer", 20, read_rotation)?,
        rotation_keyforms: r.seq("rotation keyform", 20, read_rotation_keyform)?,
        deformers: r.seq("deformer", 12, read_deformer)?,
        parameter_bindings: r.seq("parameter binding", 8, read_parameter_binding)?,
        keyform_bindings: r.seq("keyform binding", 4, read_keyform_binding)?,
        keyforms: read_keyforms(r)?,
        draw_order: r.u32_seq()?,
        drawable_keyform_opacities: r.f32_seq()?,
        drawable_keyform_draw_orders: r.f32_seq()?,
        motions: Vec::new(),
    };
    if version >= 2 {
        ir.motions = r.seq("motion", 20, read_motion)?;
    }
    validate_references(&ir)?;
    Ok(ir)
}

/// Rejects indices that point outside the table they name.
fn validate_references(ir: &CubismIr) -> Result<(), DecodeError> {
    let out_of_range = |what: &str, index: usize, len: usize| DecodeError::corrupt(format!("{what} {index} of {len}"));

    for (i, d) in ir.drawables.iter().enumerate() {
        if let Some(parent) = d.parent_deformer {
            if parent as usize >= ir.deformers.len() {
                return Err(out_of_range("drawable parent deformer", parent as usize, ir.deformers.len()));
            }
        }
        if let Some(part) = d.part {
            if part as usize >= ir.part_ids.len() {
                return Err(out_of_range("drawable part", part as usize, ir.part_ids.len()));
            }
        }
        if d.keyform_binding as usize >= ir.keyform_bindings.len() {
            return Err(out_of_range(
                "drawable keyform binding",
                d.keyform_binding as usize,
                ir.keyform_bindings.len(),
            ));
        }
        for mask in &d.masks {
            if *mask as usize >= ir.drawables.len() {
                return Err(out_of_range("clipping mask", *mask as usize, ir.drawables.len()));
            }
        }
        // Every index must land inside the drawable's own vertices, which is
        // the check that stops a stale package drawing another mesh's shape.
        for index in &d.indices {
            if *index as usize >= d.uvs.len() {
                return Err(DecodeError::corrupt(format!(
                    "drawable {i} names vertex {index} of its own {}",
                    d.uvs.len()
                )));
            }
        }
    }

    for d in &ir.deformers {
        if let Some(parent) = d.parent {
            if parent as usize >= ir.deformers.len() {
                return Err(out_of_range("deformer parent", parent as usize, ir.deformers.len()));
            }
        }
        match d.kind {
            DeformerKind::Warp(w) if w as usize >= ir.warp_deformers.len() => {
                return Err(out_of_range("warp deformer", w as usize, ir.warp_deformers.len()))
            }
            DeformerKind::Rotation(x) if x as usize >= ir.rotation_deformers.len() => {
                return Err(out_of_range("rotation deformer", x as usize, ir.rotation_deformers.len()))
            }
            _ => {}
        }
    }

    for b in &ir.parameter_bindings {
        if b.parameter as usize >= ir.parameters.len() {
            return Err(out_of_range("bound parameter", b.parameter as usize, ir.parameters.len()));
        }
    }
    for b in &ir.keyform_bindings {
        for axis in &b.axes {
            if *axis as usize >= ir.parameter_bindings.len() {
                return Err(out_of_range("binding axis", *axis as usize, ir.parameter_bindings.len()));
            }
        }
    }
    for index in &ir.draw_order {
        if *index as usize >= ir.drawables.len() {
            return Err(out_of_range("painted drawable", *index as usize, ir.drawables.len()));
        }
    }
    Ok(())
}

/// Durations and fades share one rule: finite and not negative. `-0.0` passes.
fn is_valid_duration(v: f32) -> bool {
    v.is_finite() && v >= 0.0
}

/// The one set of rules for a motion, applied by the writer before it emits a
/// byte and by the reader after it parses one, so anything that writes also
/// reads and the two cannot drift apart.
///
/// Errors are `Corrupt` on both sides, matching `Manifest::to_json`'s refusal
/// of non-finite durations: the value is bad data, whichever direction it
/// travels.
fn validate_motion(m: &Motion) -> Result<(), DecodeError> {
    let length = |v: f32| !is_valid_duration(v);
    // Looping wraps at the duration, so a NaN or negative one would stall or
    // spin the playhead.
    if length(m.duration) {
        return Err(DecodeError::corrupt(format!(
            "motion {:?} has duration {}, which is not a finite non-negative length",
            m.name, m.duration
        )));
    }
    for (what, value) in [("fade in", m.fade_in), ("fade out", m.fade_out)] {
        if length(value) {
            return Err(DecodeError::corrupt(format!(
                "motion {:?} has {what} {value}, which is not a finite non-negative length",
                m.name
            )));
        }
    }
    for c in &m.curves {
        for (what, value) in [("fade in", c.fade_in), ("fade out", c.fade_out)] {
            if let Some(value) = value.filter(|v| length(*v)) {
                return Err(DecodeError::corrupt(format!(
                    "curve {:?} has {what} {value}, which is not a finite non-negative length",
                    c.target
                )));
            }
        }
        for k in &c.keys {
            for (what, value) in
                [("time", k.time), ("value", k.value), ("in slope", k.in_slope), ("out slope", k.out_slope)]
            {
                if !value.is_finite() {
                    return Err(DecodeError::corrupt(format!(
                        "curve {:?} key {what} is not finite ({value})",
                        c.target
                    )));
                }
            }
        }
        // The sampler binary-searches by time, so unsorted keys would silently
        // select the wrong segment.
        if let Some(pair) = c.keys.windows(2).find(|w| w[0].time > w[1].time) {
            return Err(DecodeError::corrupt(format!(
                "curve {:?} has key times that are not non-decreasing: {} then {}",
                c.target, pair[0].time, pair[1].time
            )));
        }
    }
    Ok(())
}

fn write_motion(w: &mut Writer, m: &Motion) {
    w.str(&m.name);
    w.f32(m.duration);
    w.f32(m.fade_in);
    w.f32(m.fade_out);
    w.seq(&m.curves, write_curve);
}

fn read_motion(r: &mut Reader<'_>) -> Result<Motion, DecodeError> {
    let m = Motion {
        name: r.str()?,
        duration: r.f32()?,
        fade_in: r.f32()?,
        fade_out: r.f32()?,
        curves: r.seq("motion curve", 10, read_curve)?,
    };
    validate_motion(&m)?;
    Ok(m)
}

fn write_curve(w: &mut Writer, c: &MotionCurve) {
    w.str(&c.target);
    w.opt(c.fade_in, |w, v| w.f32(v));
    w.opt(c.fade_out, |w, v| w.f32(v));
    w.seq(&c.keys, write_key);
}

fn read_curve(r: &mut Reader<'_>) -> Result<MotionCurve, DecodeError> {
    Ok(MotionCurve {
        target: r.str()?,
        fade_in: r.opt(|r| r.f32())?,
        fade_out: r.opt(|r| r.f32())?,
        keys: r.seq("curve key", 17, read_key)?,
    })
}

fn write_key(w: &mut Writer, k: &CurveKey) {
    w.f32(k.time);
    w.f32(k.value);
    w.f32(k.in_slope);
    w.f32(k.out_slope);
    w.bool(k.stepped);
}

fn read_key(r: &mut Reader<'_>) -> Result<CurveKey, DecodeError> {
    Ok(CurveKey {
        time: r.f32()?,
        value: r.f32()?,
        in_slope: r.f32()?,
        out_slope: r.f32()?,
        stepped: match r.u8()? {
            0 => false,
            1 => true,
            other => return Err(DecodeError::corrupt(format!("curve key stepped flag is {other}, not 0 or 1"))),
        },
    })
}

fn write_ids(w: &mut Writer, ids: &[String]) {
    w.seq(ids, |w, id| w.str(id));
}

fn read_ids(r: &mut Reader<'_>, what: &str) -> Result<Vec<String>, DecodeError> {
    r.seq(what, 4, |r| r.str())
}

fn write_canvas(w: &mut Writer, c: &Canvas) {
    w.f32(c.pixels_per_unit);
    w.f32(c.origin.0);
    w.f32(c.origin.1);
    w.f32(c.size.0);
    w.f32(c.size.1);
}

fn read_canvas(r: &mut Reader<'_>) -> Result<Canvas, DecodeError> {
    Ok(Canvas { pixels_per_unit: r.f32()?, origin: (r.f32()?, r.f32()?), size: (r.f32()?, r.f32()?) })
}

fn write_parameter(w: &mut Writer, p: &Parameter) {
    w.str(&p.id);
    w.f32(p.minimum);
    w.f32(p.maximum);
    w.f32(p.default);
}

fn read_parameter(r: &mut Reader<'_>) -> Result<Parameter, DecodeError> {
    let p = Parameter { id: r.str()?, minimum: r.f32()?, maximum: r.f32()?, default: r.f32()? };
    if !(p.minimum <= p.default && p.default <= p.maximum) {
        return Err(DecodeError::corrupt(format!(
            "parameter {:?} defaults to {} outside {}..{}",
            p.id, p.default, p.minimum, p.maximum
        )));
    }
    Ok(p)
}

fn write_drawable(w: &mut Writer, d: &Drawable) {
    w.str(&d.id);
    w.opt(d.parent_deformer, |w, v| w.u32(v));
    w.seq(&d.uvs, |w, uv| {
        w.f32(uv.0);
        w.f32(uv.1);
    });
    w.u16_seq(&d.indices);
    w.u32(d.keyform_begin);
    w.u32(d.keyform_count);
    w.u32(d.keyform_binding);
    w.u32_seq(&d.masks);
    w.u8(d.flags);
    w.opt(d.part, |w, v| w.u32(v));
    w.u32(d.texture);
}

fn read_drawable(r: &mut Reader<'_>) -> Result<Drawable, DecodeError> {
    Ok(Drawable {
        id: r.str()?,
        parent_deformer: r.opt(|r| r.u32())?,
        uvs: r.seq("uv", 8, |r| Ok((r.f32()?, r.f32()?)))?,
        indices: r.u16_seq()?,
        keyform_begin: r.u32()?,
        keyform_count: r.u32()?,
        keyform_binding: r.u32()?,
        masks: r.u32_seq()?,
        flags: r.u8()?,
        part: r.opt(|r| r.u32())?,
        texture: r.u32()?,
    })
}

fn write_warp(w: &mut Writer, d: &WarpDeformer) {
    w.str(&d.id);
    w.u32(d.divisions.0);
    w.u32(d.divisions.1);
    w.u32(d.point_count);
    w.u32(d.keyform_begin);
    w.u32(d.keyform_count);
    w.u32(d.keyform_binding);
}

fn read_warp(r: &mut Reader<'_>) -> Result<WarpDeformer, DecodeError> {
    let d = WarpDeformer {
        id: r.str()?,
        divisions: (r.u32()?, r.u32()?),
        point_count: r.u32()?,
        keyform_begin: r.u32()?,
        keyform_count: r.u32()?,
        keyform_binding: r.u32()?,
    };
    // The grid is a lattice, so its point count is fixed by its divisions. A
    // package disagreeing with itself here would index past the pool.
    let expected = (d.divisions.0 + 1).saturating_mul(d.divisions.1 + 1);
    if d.point_count != expected {
        return Err(DecodeError::corrupt(format!(
            "warp {:?} declares {} points for a {:?} grid, which needs {expected}",
            d.id, d.point_count, d.divisions
        )));
    }
    Ok(d)
}

fn write_rotation(w: &mut Writer, d: &RotationDeformer) {
    w.str(&d.id);
    w.u32(d.keyform_binding);
    w.u32(d.keyform_begin);
    w.u32(d.keyform_count);
    w.f32(d.base_angle);
}

fn read_rotation(r: &mut Reader<'_>) -> Result<RotationDeformer, DecodeError> {
    Ok(RotationDeformer {
        id: r.str()?,
        keyform_binding: r.u32()?,
        keyform_begin: r.u32()?,
        keyform_count: r.u32()?,
        base_angle: r.f32()?,
    })
}

fn write_rotation_keyform(w: &mut Writer, k: &RotationKeyform) {
    w.f32(k.origin.0);
    w.f32(k.origin.1);
    w.f32(k.angle);
    w.f32(k.scale);
    w.f32(k.opacity);
}

fn read_rotation_keyform(r: &mut Reader<'_>) -> Result<RotationKeyform, DecodeError> {
    Ok(RotationKeyform { origin: (r.f32()?, r.f32()?), angle: r.f32()?, scale: r.f32()?, opacity: r.f32()? })
}

fn write_deformer(w: &mut Writer, d: &Deformer) {
    w.str(&d.id);
    w.opt(d.parent, |w, v| w.u32(v));
    match d.kind {
        DeformerKind::Warp(i) => {
            w.u8(0);
            w.u32(i);
        }
        DeformerKind::Rotation(i) => {
            w.u8(1);
            w.u32(i);
        }
    }
}

fn read_deformer(r: &mut Reader<'_>) -> Result<Deformer, DecodeError> {
    let id = r.str()?;
    let parent = r.opt(|r| r.u32())?;
    let tag = r.u8()?;
    let index = r.u32()?;
    let kind = match tag {
        0 => DeformerKind::Warp(index),
        1 => DeformerKind::Rotation(index),
        other => return Err(DecodeError::corrupt(format!("deformer {id:?} has kind {other}, not 0 or 1"))),
    };
    Ok(Deformer { id, parent, kind })
}

fn write_parameter_binding(w: &mut Writer, b: &ParameterBinding) {
    w.u32(b.parameter);
    w.f32_seq(&b.keys);
}

fn read_parameter_binding(r: &mut Reader<'_>) -> Result<ParameterBinding, DecodeError> {
    let b = ParameterBinding { parameter: r.u32()?, keys: r.f32_seq()? };
    // Keys are a lookup axis, so they have to be usable as one.
    if b.keys.windows(2).any(|w| w[0] >= w[1] || !w[0].is_finite()) {
        return Err(DecodeError::corrupt(format!("parameter binding keys are not strictly increasing: {:?}", b.keys)));
    }
    Ok(b)
}

fn write_keyform_binding(w: &mut Writer, b: &KeyformBinding) {
    w.u32_seq(&b.axes);
}

fn read_keyform_binding(r: &mut Reader<'_>) -> Result<KeyformBinding, DecodeError> {
    Ok(KeyformBinding { axes: r.u32_seq()? })
}

fn write_keyforms(w: &mut Writer, k: &Keyforms) {
    w.f32_seq(&k.positions);
    w.u32_seq(&k.warp_offsets);
    w.u32_seq(&k.drawable_offsets);
}

fn read_keyforms(r: &mut Reader<'_>) -> Result<Keyforms, DecodeError> {
    let k = Keyforms { positions: r.f32_seq()?, warp_offsets: r.u32_seq()?, drawable_offsets: r.u32_seq()? };
    // An offset past the pool would read another keyform's coordinates, or
    // none, and only show up as geometry in the wrong place.
    for offset in k.warp_offsets.iter().chain(&k.drawable_offsets) {
        if *offset as usize > k.positions.len() {
            return Err(DecodeError::corrupt(format!(
                "keyform offset {offset} is past the {} coordinates stored",
                k.positions.len()
            )));
        }
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{Package, PackageModel};

    /// A small model exercising every table the format writes: two deformers of
    /// different kinds, a masked drawable, a two-axis binding, and both
    /// per-keyform tracks.
    fn model() -> CubismIr {
        CubismIr {
            canvas: Canvas { pixels_per_unit: 3500.0, origin: (1750.0, 3101.0), size: (3500.0, 6202.0) },
            parameters: vec![
                Parameter { id: "ParamAngleX".into(), minimum: -30.0, maximum: 30.0, default: 0.0 },
                Parameter { id: "ParamEyeLOpen".into(), minimum: 0.0, maximum: 1.2, default: 1.0 },
            ],
            part_ids: vec!["Part01".into()],
            deformer_ids: vec!["Rotation1".into(), "Warp1".into()],
            drawable_ids: vec!["ArtMesh1".into(), "ArtMesh2".into()],
            glue_ids: vec![],
            drawables: vec![
                Drawable {
                    id: "ArtMesh1".into(),
                    parent_deformer: Some(1),
                    uvs: vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)],
                    indices: vec![0, 1, 2],
                    keyform_begin: 0,
                    keyform_count: 3,
                    keyform_binding: 0,
                    masks: vec![],
                    flags: 4,
                    part: Some(0),
                    texture: 0,
                },
                Drawable {
                    id: "ArtMesh2".into(),
                    // Parented to the model root, which the sentinel spells.
                    parent_deformer: None,
                    uvs: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
                    indices: vec![0, 1, 2],
                    keyform_begin: 3,
                    keyform_count: 3,
                    keyform_binding: 1,
                    masks: vec![0],
                    flags: 6,
                    part: None,
                    texture: 0,
                },
            ],
            warp_deformers: vec![WarpDeformer {
                id: "Warp1".into(),
                divisions: (1, 2),
                point_count: 6,
                keyform_begin: 0,
                keyform_count: 3,
                keyform_binding: 0,
            }],
            rotation_deformers: vec![RotationDeformer {
                id: "Rotation1".into(),
                keyform_binding: 0,
                keyform_begin: 0,
                keyform_count: 3,
                base_angle: 90.0,
            }],
            rotation_keyforms: vec![
                RotationKeyform { origin: (0.0, 0.0), angle: 0.0, scale: 1.0, opacity: 1.0 },
                RotationKeyform { origin: (1.0, 2.0), angle: 15.0, scale: 1.5, opacity: 0.5 },
                RotationKeyform { origin: (2.0, 4.0), angle: 30.0, scale: 2.0, opacity: 0.0 },
            ],
            deformers: vec![
                Deformer { id: "Rotation1".into(), parent: None, kind: DeformerKind::Rotation(0) },
                Deformer { id: "Warp1".into(), parent: Some(0), kind: DeformerKind::Warp(0) },
            ],
            parameter_bindings: vec![
                ParameterBinding { parameter: 0, keys: vec![-30.0, 0.0, 30.0] },
                ParameterBinding { parameter: 1, keys: vec![0.0, 1.2] },
            ],
            keyform_bindings: vec![KeyformBinding { axes: vec![0] }, KeyformBinding { axes: vec![0, 1] }],
            keyforms: Keyforms {
                positions: (0..96).map(|i| i as f32 * 0.25).collect(),
                warp_offsets: vec![0, 16, 32],
                drawable_offsets: vec![48, 64, 80],
            },
            draw_order: vec![1, 0],
            drawable_keyform_opacities: vec![1.0, 1.0, 0.0, 1.0, 0.5, 0.25],
            drawable_keyform_draw_orders: vec![500.0; 6],
            motions: vec![],
        }
    }

    fn round_trip(ir: &CubismIr) -> CubismIr {
        let package = Package::from_cubism(ir.clone(), "test", "cubism-moc3-v2");
        let bytes = package.encode_model().unwrap();
        let PackageModel::Cubism(back) = Package::decode_model(&bytes).expect("should decode") else {
            panic!("a Cubism package must decode as one")
        };
        back
    }

    #[test]
    fn a_model_round_trips_through_model_bin() {
        let ir = model();
        assert_eq!(round_trip(&ir), ir);
    }

    #[test]
    fn encoding_is_deterministic() {
        // Golden tests compare bytes, so the same model must encode the same
        // way every time and in any process.
        let package = Package::from_cubism(model(), "test", "cubism-moc3-v2");
        assert_eq!(package.encode_model().unwrap(), package.encode_model().unwrap());
        let again = Package::from_cubism(model(), "test", "cubism-moc3-v2");
        assert_eq!(package.encode_model().unwrap(), again.encode_model().unwrap());
    }

    #[test]
    fn a_package_that_names_a_missing_table_entry_is_refused() {
        // An index past its table would otherwise surface far downstream as
        // geometry in the wrong place, or as nothing at all.
        let mut ir = model();
        ir.drawables[0].parent_deformer = Some(9);
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let err = Package::decode_model(&bytes).expect_err("an out-of-range parent should be refused");
        assert!(format!("{err}").contains("drawable parent deformer"), "{err}");
    }

    #[test]
    fn a_drawable_naming_a_vertex_it_does_not_have_is_refused() {
        let mut ir = model();
        ir.drawables[0].indices = vec![0, 1, 7];
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let err = Package::decode_model(&bytes).expect_err("an out-of-range index should be refused");
        assert!(format!("{err}").contains("vertex 7"), "{err}");
    }

    #[test]
    fn a_warp_whose_points_contradict_its_divisions_is_refused() {
        let mut ir = model();
        ir.warp_deformers[0].point_count = 7;
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let err = Package::decode_model(&bytes).expect_err("a mismatched grid should be refused");
        assert!(format!("{err}").contains("points for a"), "{err}");
    }

    #[test]
    fn keys_that_do_not_increase_are_refused() {
        let mut ir = model();
        ir.parameter_bindings[0].keys = vec![0.0, 0.0, 30.0];
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let err = Package::decode_model(&bytes).expect_err("a flat key axis should be refused");
        assert!(format!("{err}").contains("strictly increasing"), "{err}");
    }

    fn key(time: f32, value: f32) -> CurveKey {
        CurveKey { time, value, in_slope: 0.5, out_slope: -0.25, stepped: false }
    }

    fn with_motions() -> CubismIr {
        let mut ir = model();
        ir.motions = vec![
            Motion {
                // Deliberately not alphabetical: order is meaningful.
                name: "Walk".into(),
                duration: 2.5,
                fade_in: 0.25,
                fade_out: 0.5,
                curves: vec![
                    MotionCurve {
                        target: "ParamAngleX".into(),
                        fade_in: Some(0.1),
                        fade_out: Some(0.2),
                        keys: vec![
                            key(0.0, -10.0),
                            CurveKey { stepped: true, ..key(1.0, 10.0) },
                            // Equal times are allowed: non-decreasing.
                            key(1.0, 5.0),
                            key(2.5, 0.0),
                        ],
                    },
                    MotionCurve { target: "ParamEyeLOpen".into(), fade_in: None, fade_out: Some(0.0), keys: vec![] },
                    MotionCurve {
                        target: "NoSuchParam".into(),
                        fade_in: Some(0.3),
                        fade_out: None,
                        keys: vec![key(0.0, 1.0)],
                    },
                ],
            },
            Motion { name: "Idle".into(), duration: 4.0, fade_in: 0.0, fade_out: 0.0, curves: vec![] },
        ];
        ir
    }

    const SENTINEL: f32 = 913.25;

    /// Encodes `ir` after `set` has put [`SENTINEL`] into the field under test,
    /// then overwrites that one value in the bytes with `bad`. The writer
    /// refuses to produce such a value, so the reader's
    /// checks are reached by patching the bytes instead.
    fn refused(set: impl FnOnce(&mut CubismIr), bad: f32) -> String {
        let mut ir = with_motions();
        set(&mut ir);
        let mut bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let needle = SENTINEL.to_bits().to_le_bytes();
        let hits: Vec<usize> = (0..bytes.len() - 3).filter(|&i| bytes[i..i + 4] == needle).collect();
        assert_eq!(hits.len(), 1, "the sentinel must appear exactly once");
        bytes[hits[0]..hits[0] + 4].copy_from_slice(&bad.to_bits().to_le_bytes());
        let err = Package::decode_model(&bytes).expect_err("a corrupt motion should be refused");
        assert!(matches!(err, DecodeError::Corrupt { .. }), "{err:?}");
        err.to_string()
    }

    #[test]
    fn motions_round_trip_and_re_encode_to_identical_bytes() {
        let ir = with_motions();
        let first = Package::from_cubism(ir.clone(), "test", "x").encode_model().unwrap();
        let PackageModel::Cubism(back) = Package::decode_model(&first).expect("should decode") else {
            panic!("a Cubism package must decode as one")
        };
        assert_eq!(back, ir);
        assert_eq!(back.motions[0].name, "Walk", "motion order must be preserved");
        assert_eq!(Package::from_cubism(back, "test", "x").encode_model().unwrap(), first);
    }

    #[test]
    fn a_version_1_model_reads_with_no_motions() {
        // Generated by the version 1 writer from `model()`. It ends exactly where
        // the motion table now begins.
        let bytes = include_bytes!("../tests/fixtures/cubism_v1.model.bin");
        assert_eq!(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]), 1);
        let PackageModel::Cubism(ir) = Package::decode_model(bytes).expect("v1 must still load") else {
            panic!("a Cubism package must decode as one")
        };
        assert!(ir.motions.is_empty());
        assert_eq!(ir, model());
    }

    #[test]
    fn a_version_2_model_missing_its_motion_table_is_refused() {
        // The layout comes from the header, so a version 2 header over version 1
        // bytes is truncated rather than quietly accepted.
        let mut bytes = include_bytes!("../tests/fixtures/cubism_v1.model.bin").to_vec();
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(Package::decode_model(&bytes).is_err());
    }

    #[test]
    fn unsorted_key_times_are_refused() {
        // The writer refuses unsorted keys too, so the last key is patched
        // below its predecessor in the bytes.
        let msg = refused(|ir| ir.motions[0].curves[0].keys[3].time = SENTINEL, 0.5);
        assert!(msg.contains("non-decreasing"), "{msg}");
    }

    #[test]
    fn a_nan_key_time_is_refused() {
        let msg = refused(|ir| ir.motions[0].curves[0].keys[3].time = SENTINEL, f32::NAN);
        assert!(msg.contains("key time"), "{msg}");
    }

    #[test]
    fn a_non_finite_key_value_is_refused() {
        let msg = refused(|ir| ir.motions[0].curves[0].keys[0].value = SENTINEL, f32::NEG_INFINITY);
        assert!(msg.contains("key value"), "{msg}");
    }

    #[test]
    fn an_infinite_slope_is_refused() {
        let msg = refused(|ir| ir.motions[0].curves[0].keys[1].out_slope = SENTINEL, f32::INFINITY);
        assert!(msg.contains("out slope"), "{msg}");
        let msg = refused(|ir| ir.motions[0].curves[0].keys[1].in_slope = SENTINEL, f32::INFINITY);
        assert!(msg.contains("in slope"), "{msg}");
    }

    #[test]
    fn a_negative_or_nan_duration_is_refused() {
        for bad in [-1.0, f32::NAN, f32::INFINITY] {
            let msg = refused(|ir| ir.motions[0].duration = SENTINEL, bad);
            assert!(msg.contains("duration"), "{msg}");
        }
    }

    #[test]
    fn a_non_finite_or_negative_fade_is_refused() {
        for bad in [f32::NAN, f32::INFINITY, -0.5] {
            let msg = refused(|ir| ir.motions[0].fade_in = SENTINEL, bad);
            assert!(msg.contains("motion") && msg.contains("fade in"), "{msg}");
            let msg = refused(|ir| ir.motions[0].fade_out = SENTINEL, bad);
            assert!(msg.contains("motion") && msg.contains("fade out"), "{msg}");
            let msg = refused(|ir| ir.motions[0].curves[0].fade_in = Some(SENTINEL), bad);
            assert!(msg.contains("curve") && msg.contains("fade in"), "{msg}");
            let msg = refused(|ir| ir.motions[0].curves[0].fade_out = Some(SENTINEL), bad);
            assert!(msg.contains("curve") && msg.contains("fade out"), "{msg}");
        }
    }

    #[test]
    fn a_negative_zero_fade_and_duration_are_accepted() {
        let mut ir = with_motions();
        ir.motions[0].fade_in = -0.0;
        ir.motions[0].duration = -0.0;
        ir.motions[0].curves[0].fade_out = Some(-0.0);
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        assert!(Package::decode_model(&bytes).is_ok());
    }

    #[test]
    fn the_writer_refuses_what_the_reader_would_refuse() {
        // Release builds included: this is an error, not an assertion. Each
        // case names the field that is wrong.
        type Edit = fn(&mut CubismIr);
        let cases: [(&str, Edit); 12] = [
            ("duration", |ir| ir.motions[0].duration = -1.0),
            ("duration", |ir| ir.motions[0].duration = f32::NAN),
            ("fade in", |ir| ir.motions[0].fade_in = -0.5),
            ("fade out", |ir| ir.motions[0].fade_out = f32::INFINITY),
            ("fade in", |ir| ir.motions[0].curves[0].fade_in = Some(f32::NAN)),
            ("fade out", |ir| ir.motions[0].curves[0].fade_out = Some(-1.0)),
            ("key time", |ir| ir.motions[0].curves[0].keys[0].time = f32::NAN),
            ("key value", |ir| ir.motions[0].curves[0].keys[0].value = f32::INFINITY),
            ("in slope", |ir| ir.motions[0].curves[0].keys[1].in_slope = f32::INFINITY),
            ("out slope", |ir| ir.motions[0].curves[0].keys[1].out_slope = f32::NEG_INFINITY),
            ("non-decreasing", |ir| ir.motions[0].curves[0].keys = vec![key(1.0, 0.0), key(0.5, 0.0)]),
            ("fade out", |ir| ir.motions[1].fade_out = f32::NAN),
        ];
        for (field, edit) in cases {
            let mut ir = with_motions();
            edit(&mut ir);
            let err = Package::from_cubism(ir, "test", "x").encode_model().expect_err("the writer should refuse");
            assert!(matches!(err, DecodeError::Corrupt { .. }), "{err:?}");
            assert!(err.to_string().contains(field), "{field}: {err}");
        }
    }

    #[test]
    fn write_to_leaves_nothing_behind_when_the_model_is_refused() {
        let mut ir = with_motions();
        ir.motions[0].fade_in = -1.0;
        let dir = std::env::temp_dir().join(format!("a2d-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(Package::from_cubism(ir, "test", "x").write_to(&dir).is_err());
        assert!(!dir.join("manifest.json").exists() && !dir.join("model.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_curve_aimed_at_a_missing_parameter_is_reported_by_validate() {
        let package = Package::from_cubism(with_motions(), "test", "x");
        let text = package.validate().warnings().iter().map(|w| w.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("NoSuchParam") && text.contains("Walk"), "{text}");
        assert!(!text.contains("ParamAngleX"), "{text}");
        // And it survives a trip through model.bin.
        let bytes = package.encode_model().unwrap();
        let PackageModel::Cubism(back) = Package::decode_model(&bytes).expect("decode") else { panic!() };
        assert_eq!(back.unresolved_motion_targets().len(), 1);
    }

    #[test]
    fn an_implausible_motion_count_is_refused_before_allocating() {
        let mut bytes = Package::from_cubism(model(), "test", "x").encode_model().unwrap();
        let at = bytes.len() - 4;
        bytes[at..].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = Package::decode_model(&bytes).expect_err("a huge count should be refused");
        assert!(err.to_string().contains("motion count"), "{err}");
    }

    #[test]
    fn truncating_or_corrupting_motion_bytes_never_panics() {
        let full = Package::from_cubism(with_motions(), "test", "x").encode_model().unwrap();
        for cut in 0..full.len() {
            let _ = Package::decode_model(&full[..cut]);
        }
        for at in 8..full.len() {
            let mut bytes = full.clone();
            bytes[at] ^= 0xff;
            let _ = Package::decode_model(&bytes);
        }
    }

    #[test]
    fn the_manifest_lists_motions_and_defaults_to_the_preferred_one() {
        let package = Package::from_cubism(with_motions(), "test", "x");
        let names: Vec<_> = package.manifest.animations.iter().map(|a| (a.name.as_str(), a.duration)).collect();
        assert_eq!(names, [("Walk", 2.5), ("Idle", 4.0)]);
        assert_eq!(package.manifest.default_animation.as_deref(), Some("Idle"));

        let mut ir = with_motions();
        ir.motions.remove(1);
        let package = Package::from_cubism(ir, "test", "x");
        assert_eq!(package.manifest.default_animation.as_deref(), Some("Walk"));
    }

    #[test]
    fn a_model_without_motions_lists_none_and_names_no_default() {
        let package = Package::from_cubism(model(), "test", "x");
        assert!(package.manifest.animations.is_empty());
        assert_eq!(package.manifest.default_animation, None);
    }

    #[test]
    fn a_keyform_offset_past_the_pool_is_refused() {
        let mut ir = model();
        ir.keyforms.drawable_offsets = vec![48, 64, 4096];
        let bytes = Package::from_cubism(ir, "test", "x").encode_model().unwrap();
        let err = Package::decode_model(&bytes).expect_err("an offset past the pool should be refused");
        assert!(format!("{err}").contains("past the"), "{err}");
    }
}
