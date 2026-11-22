//! Recovering a motion from a Unity `AnimationClip`.
//!
//! Shipping bundles keep each motion only as a compressed clip (no
//! `CubismFadeMotionData`), so this is the path that yields motions for them.
//! The clip's own layout is generic Unity and lives in `a2d_unity`; what a
//! binding *means* is Cubism SDK knowledge and lives here.
//!
//! # How a binding names a parameter
//!
//! A clip stores each curve's target as hashes, never as text. For a Cubism
//! parameter the SDK animates the `Value` field of the `CubismParameter`
//! component, so a binding is a parameter curve when:
//!
//! * `type_id` is 114 (`MonoBehaviour`) and its script resolves to
//!   `CubismParameter`;
//! * `attribute == crc32("Value")`; and
//! * the PPtr, int and serialize-reference flags are all zero.
//!
//! Its `path` is `crc32("Parameters/" + id)`, where `id` is the name of the
//! `GameObject` carrying that `CubismParameter` -- which is the MOC3 parameter
//! id. The root's own name is dropped from the path. Both hashes were checked
//! against 30 real clips: every parameter curve in a full model resolves, with
//! no collisions (see `a2d_unity::animation_clip` for what established the
//! layout itself).
//!
//! Two kinds of binding legitimately resolve to nothing and are counted rather
//! than guessed at. Reduced-detail variants reuse the full model's clips, so
//! dozens of curves per clip hash to parameters their own model lacks. And a
//! clip may animate the model's opacity (`CubismRenderController`, `Opacity`),
//! which has no counterpart in the IR.
//!
//! Three more kinds are recognised by name so their notes say what was ignored
//! instead of quoting a hash: a part's opacity (class `CubismPart`, `Opacity`,
//! path `crc32("Parts/" + id)`), and the model-level eye-blink and mouth
//! controllers (`CubismEyeBlinkController` `EyeOpening`, `CubismMouthController`
//! `MouthOpening`). None of the six real models examined has a clip with any of
//! these three. All three follow the SDK's motion3-to-clip conversion as read
//! in its source, and none is confirmed against a real clip; the eye and mouth
//! cases match class and property only (an empty path). None produces a curve: the IR has
//! no part opacity and the runtime applies no model-level curves.
//!
//! # Curve values
//!
//! A streamed key carries a cubic segment `((c0*x + c1)*x + c2)*x + c3`, valid
//! until the curve's next key. It becomes a Hermite key with value `c3`, out
//! slope `c2`, and an in slope equal to the *previous* key's cubic differentiated
//! at the span between them, so the Hermite segment between two keys reproduces
//! the original cubic. A segment with `c0 = c1 = c2 = 0` whose next value differs
//! is a step. That reproduces 108,737 real segments to within 4e-5; where a
//! segment is not representable (the cubic misses the next key), it is counted.
//! A constant curve becomes one key at time zero. A dense curve becomes linear
//! keys, **on an unverified layout** (no real clip had one); that is reported.

use std::collections::{BTreeMap, HashMap};

use a2d_core::ir::cubism::{CurveKey, Motion, MotionCurve};
use a2d_core::{DecodeError, LoadReport};
use a2d_unity::{crc32_str, AnimationClip, GenericBinding, PPtr, StreamedKey};

use super::fade_motion::DEFAULT_FADE;

/// The C# class that carries a parameter's value.
const PARAMETER_CLASS: &str = "CubismParameter";
/// The C# class that carries the model-wide opacity.
const RENDER_CONTROLLER_CLASS: &str = "CubismRenderController";
/// The C# class that carries a part's opacity.
const PART_CLASS: &str = "CubismPart";
/// The C# class that drives the model-level eye-opening value.
const EYE_BLINK_CLASS: &str = "CubismEyeBlinkController";
/// The C# class that drives the model-level mouth-opening value.
const MOUTH_CLASS: &str = "CubismMouthController";
/// `MonoBehaviour`'s Unity class id, as a binding's `type_id`.
const MONO_BEHAVIOUR_TYPE: i32 = 114;
/// A cubic may miss its next key by this fraction of `max(1, |value|)` before
/// the segment is reported as not representable.
const SEGMENT_TOLERANCE: f32 = 1e-3;

/// Hash of a parameter's path back to its identifier.
#[derive(Debug, Clone, Default)]
pub(super) struct NameTable {
    by_hash: HashMap<u32, String>,
    /// Part paths, used only to name a part in a note.
    parts: HashMap<u32, String>,
}

impl NameTable {
    /// Hashes each parameter id the way a clip's binding path does. Two
    /// different ids with one hash cannot be told apart, so that is an error.
    pub(super) fn new<'a>(ids: impl IntoIterator<Item = &'a str>) -> Result<NameTable, DecodeError> {
        let mut table = NameTable::default();
        for id in ids {
            table.add(crc32_str(&format!("Parameters/{id}")), id)?;
        }
        Ok(table)
    }

    /// Adds part ids, so a part-opacity binding can be named in a note. A
    /// collision only costs the name, so the first id wins.
    pub(super) fn with_parts<'a>(mut self, ids: impl IntoIterator<Item = &'a str>) -> NameTable {
        for id in ids {
            self.parts.entry(crc32_str(&format!("Parts/{id}"))).or_insert_with(|| id.to_string());
        }
        self
    }

    fn add(&mut self, hash: u32, id: &str) -> Result<(), DecodeError> {
        match self.by_hash.get(&hash) {
            Some(existing) if existing != id => Err(DecodeError::Reconstruction {
                game: "unity_cubism".into(),
                message: format!(
                    "parameters {existing:?} and {id:?} hash to the same path ({hash:#010x}), so a clip's curves cannot be assigned to one or the other"
                ),
            }),
            Some(_) => Ok(()),
            None => {
                self.by_hash.insert(hash, id.to_string());
                Ok(())
            }
        }
    }

    fn get(&self, hash: u32) -> Option<&str> {
        self.by_hash.get(&hash).map(String::as_str)
    }
}

/// The property-name hashes a binding is compared with.
#[derive(Clone, Copy)]
struct Hashes {
    value: u32,
    opacity: u32,
    eye: u32,
    mouth: u32,
}

/// What a binding turned out to drive.
enum Target {
    Parameter(String),
    /// A parameter-shaped binding whose path matches no parameter here.
    Unresolved,
    ModelOpacity,
    /// A part's opacity; the id when the path names a part of this model.
    PartOpacity(Option<String>),
    EyeBlink,
    LipSync,
    /// Anything else, described by script class and property hash.
    Other(String),
}

fn classify(
    binding: &GenericBinding,
    names: &NameTable,
    script_class: &dyn Fn(PPtr) -> Option<String>,
    hashes: &Hashes,
) -> Target {
    let Hashes { value: value_hash, opacity: opacity_hash, eye: eye_hash, mouth: mouth_hash } = *hashes;
    let class = (binding.type_id == MONO_BEHAVIOUR_TYPE).then(|| script_class(binding.script)).flatten();
    let plain_flags =
        binding.is_pptr_curve == 0 && binding.is_int_curve == 0 && binding.is_serialize_reference_curve == 0;
    match class.as_deref() {
        Some(PARAMETER_CLASS) if binding.attribute == value_hash && plain_flags => match names.get(binding.path) {
            Some(id) => Target::Parameter(id.to_string()),
            None => Target::Unresolved,
        },
        Some(RENDER_CONTROLLER_CLASS) if binding.attribute == opacity_hash => Target::ModelOpacity,
        Some(PART_CLASS) if binding.attribute == opacity_hash => {
            Target::PartOpacity(names.parts.get(&binding.path).cloned())
        }
        Some(EYE_BLINK_CLASS) if binding.attribute == eye_hash => Target::EyeBlink,
        Some(MOUTH_CLASS) if binding.attribute == mouth_hash => Target::LipSync,
        Some(other) => Target::Other(format!("{other} property {:#010x}", binding.attribute)),
        None => Target::Other(format!("type {} property {:#010x}", binding.type_id, binding.attribute)),
    }
}

/// Everything worth saying about one clip, gathered while converting it.
#[derive(Default)]
struct Tally {
    unresolved: usize,
    opacity: usize,
    parts: Vec<Option<String>>,
    eye_blink: usize,
    lip_sync: usize,
    other: BTreeMap<String, usize>,
    dense_used: bool,
    dense_unusable: usize,
    off_segments: usize,
    non_finite: usize,
    no_keys: usize,
    duplicates: usize,
}

/// Hermite keys for one streamed curve. `off` counts segments whose cubic does
/// not reach the next key's value. `None` when anything is not finite.
fn streamed_keys(keys: &[StreamedKey], off: &mut usize) -> Option<Vec<CurveKey>> {
    let finite =
        keys.iter().all(|k| k.time.is_finite() && k.value.is_finite() && k.coeffs.iter().all(|c| c.is_finite()));
    if !finite {
        return None;
    }
    let mut out = Vec::with_capacity(keys.len());
    for (i, key) in keys.iter().enumerate() {
        let [c0, c1, c2] = key.coeffs;
        let next = keys.get(i + 1);
        let flat = c0 == 0.0 && c1 == 0.0 && c2 == 0.0;
        let stepped = flat && next.is_some_and(|n| n.value != key.value);
        if let Some(next) = next {
            if !stepped {
                let dx = next.time - key.time;
                let reached = ((c0 * dx + c1) * dx + c2) * dx + key.value;
                if (reached - next.value).abs() > SEGMENT_TOLERANCE * next.value.abs().max(1.0) {
                    *off += 1;
                }
            }
        }
        let in_slope = match i.checked_sub(1).and_then(|p| keys.get(p)) {
            Some(prev) => {
                let dx = key.time - prev.time;
                let [p0, p1, p2] = prev.coeffs;
                (3.0 * p0 * dx + 2.0 * p1) * dx + p2
            }
            None => c2,
        };
        out.push(CurveKey { time: key.time, value: key.value, in_slope, out_slope: c2, stepped });
    }
    out.iter().all(|k| k.in_slope.is_finite() && k.out_slope.is_finite()).then_some(out)
}

/// Linear keys through `(time, value)` samples, chord slopes on both sides.
fn linear_keys(samples: &[(f32, f32)]) -> Option<Vec<CurveKey>> {
    if samples.iter().any(|(t, v)| !t.is_finite() || !v.is_finite()) {
        return None;
    }
    let chord = |a: (f32, f32), b: (f32, f32)| {
        let dt = b.0 - a.0;
        if dt > 0.0 {
            (b.1 - a.1) / dt
        } else {
            0.0
        }
    };
    let mut out = Vec::with_capacity(samples.len());
    for (i, &(time, value)) in samples.iter().enumerate() {
        let next = samples.get(i + 1).map(|&n| chord((time, value), n));
        let prev = i.checked_sub(1).and_then(|p| samples.get(p)).map(|&p| chord(p, (time, value)));
        let (in_slope, out_slope) = match (prev, next) {
            (Some(p), Some(n)) => (p, n),
            (Some(p), None) => (p, p),
            (None, Some(n)) => (n, n),
            (None, None) => (0.0, 0.0),
        };
        out.push(CurveKey { time, value, in_slope, out_slope, stepped: false });
    }
    out.iter().all(|k| k.in_slope.is_finite() && k.out_slope.is_finite()).then_some(out)
}

/// Keys of curve `index`, or `None` when it cannot be used (already tallied).
fn curve_keys(clip: &AnimationClip, index: usize, offset: f32, tally: &mut Tally) -> Option<Vec<CurveKey>> {
    let dense_start = clip.dense_curve_start();
    let constant_start = clip.constant_curve_start();
    let keys = if index < dense_start {
        clip.streamed
            .get(index)
            .and_then(|keys| streamed_keys(keys, &mut tally.off_segments))
            .map(|k| shifted(k, offset))
    } else if index < constant_start {
        tally.dense_used = true;
        dense_keys(clip, index - dense_start).map(|k| shifted(k, offset))
    } else {
        let value = clip.constants.get(index - constant_start).copied().filter(|v| v.is_finite());
        value.map(|value| vec![CurveKey { time: 0.0, value, in_slope: 0.0, out_slope: 0.0, stepped: false }])
    };
    match keys {
        Some(keys) if keys.is_empty() => {
            tally.no_keys += 1;
            None
        }
        Some(keys) => Some(keys),
        None => {
            if index >= dense_start && index < constant_start {
                tally.dense_unusable += 1;
            } else {
                tally.non_finite += 1;
            }
            None
        }
    }
}

/// Moves key times so the clip's `start_time` becomes time zero, matching a
/// duration of `stop_time - start_time`.
fn shifted(mut keys: Vec<CurveKey>, offset: f32) -> Vec<CurveKey> {
    for key in &mut keys {
        key.time -= offset;
    }
    keys
}

/// UNVERIFIED reading of a dense curve: sample `frame * curve_count + curve`,
/// taken at `begin_time + frame / sample_rate`.
fn dense_keys(clip: &AnimationClip, curve: usize) -> Option<Vec<CurveKey>> {
    let d = &clip.dense;
    let frames = usize::try_from(d.frame_count).ok()?;
    let curves = d.curve_count as usize;
    if curve >= curves || !d.sample_rate.is_finite() || d.sample_rate <= 0.0 || !d.begin_time.is_finite() {
        return None;
    }
    if frames.checked_mul(curves)? > d.samples.len() {
        return None;
    }
    let samples: Vec<(f32, f32)> = (0..frames)
        .filter_map(|f| {
            let value = *d.samples.get(f * curves + curve)?;
            Some((d.begin_time + f as f32 / d.sample_rate, value))
        })
        .collect();
    linear_keys(&samples)
}

/// Converts a decoded clip to IR.
///
/// Key times are relative to the clip's `start_time`. Returns `None` (after
/// noting why) when no curve resolved, since a motion that drives nothing is
/// not worth listing.
///
/// `script_class` resolves a binding's script reference to a C# class name.
/// Never produces anything the package writer would refuse; everything dropped
/// or altered is summarised in one note on `report`.
pub(super) fn to_motion(
    clip: &AnimationClip,
    names: &NameTable,
    script_class: &dyn Fn(PPtr) -> Option<String>,
    report: &mut LoadReport,
) -> Option<Motion> {
    let hashes = Hashes {
        value: crc32_str("Value"),
        opacity: crc32_str("Opacity"),
        eye: crc32_str("EyeOpening"),
        mouth: crc32_str("MouthOpening"),
    };
    let mut tally = Tally::default();
    let mut curves: Vec<MotionCurve> = Vec::new();
    let offset = if clip.start_time.is_finite() { clip.start_time } else { 0.0 };

    for (index, binding) in clip.bindings.iter().enumerate() {
        match classify(binding, names, script_class, &hashes) {
            Target::Unresolved => tally.unresolved += 1,
            Target::ModelOpacity => tally.opacity += 1,
            Target::PartOpacity(id) => tally.parts.push(id),
            Target::EyeBlink => tally.eye_blink += 1,
            Target::LipSync => tally.lip_sync += 1,
            Target::Other(what) => *tally.other.entry(what).or_insert(0) += 1,
            Target::Parameter(id) => {
                let Some(keys) = curve_keys(clip, index, offset, &mut tally) else { continue };
                if curves.iter().any(|c| c.target == id) {
                    // Two curves on one parameter would fight; the first wins.
                    tally.duplicates += 1;
                    continue;
                }
                curves.push(MotionCurve { target: id, fade_in: None, fade_out: None, keys });
            }
        }
    }

    let mut notes: Vec<String> = Vec::new();
    let mut count = |n: usize, text: &str| {
        if n > 0 {
            notes.push(format!("{n} {text}"));
        }
    };
    count(tally.unresolved, "binding(s) match no parameter in this model and were dropped");
    count(tally.opacity, "model-opacity curve(s) ignored (no counterpart in the model)");
    count(tally.eye_blink, "eye-blink curve(s) ignored (model-level eye opening is not applied)");
    count(tally.lip_sync, "lip-sync curve(s) ignored (model-level mouth opening is not applied)");
    count(tally.dense_unusable, "dense curve(s) did not fit the assumed layout and were dropped");
    count(tally.non_finite, "curve(s) held a non-finite value and were dropped");
    count(tally.no_keys, "curve(s) had no keys and were dropped");
    count(tally.duplicates, "extra curve(s) on an already-driven parameter were dropped");
    count(tally.off_segments, "segment(s) are not exactly representable as Hermite keys (cubic misses the next key)");
    if !tally.parts.is_empty() {
        let mut ids: Vec<&str> = tally.parts.iter().flatten().map(String::as_str).collect();
        ids.sort_unstable();
        ids.dedup();
        let unnamed = tally.parts.iter().filter(|p| p.is_none()).count();
        let mut text = format!("{} part-opacity curve(s) ignored (part opacity is not applied)", tally.parts.len());
        if !ids.is_empty() {
            text.push_str(&format!(" on {}", ids.join(", ")));
        }
        if unnamed > 0 {
            let sep = if ids.is_empty() { " on " } else { ", " };
            text.push_str(&format!("{sep}{unnamed} on a part not named in this bundle"));
        }
        notes.push(text);
    }
    for (what, n) in &tally.other {
        notes.push(format!("{n} binding(s) on {what} ignored"));
    }
    if tally.dense_used {
        notes.push("dense curves were read on an unverified layout (linear between samples)".to_string());
    }

    let last_key = curves.iter().flat_map(|c| c.keys.iter()).map(|k| k.time).fold(0.0f32, f32::max).max(0.0);
    let length = clip.stop_time - clip.start_time;
    let duration = if length.is_finite() && length >= 0.0 {
        length
    } else {
        notes.push(format!("clip length {length} is not usable; the last key time {last_key} is used instead"));
        last_key
    };
    notes.push(format!(
        "fade times are unavailable; the stored default {DEFAULT_FADE} is not applied, crossfades use the viewer's mix duration"
    ));

    if curves.is_empty() {
        report.note(format!(
            "motion {:?} was not recovered from its AnimationClip: no curve drives a parameter of this model ({})",
            clip.name,
            notes.join("; ")
        ));
        return None;
    }
    report.note(format!(
        "motion {:?} recovered from its AnimationClip (no matching fade motion data): {}",
        clip.name,
        notes.join("; ")
    ));

    Some(Motion { name: clip.name.clone(), duration, fade_in: DEFAULT_FADE, fade_out: DEFAULT_FADE, curves })
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_unity::DenseClip;

    const SCRIPT_PARAM: i64 = 10;
    const SCRIPT_RENDER: i64 = 11;
    const SCRIPT_OTHER: i64 = 12;
    const SCRIPT_PART: i64 = 13;
    const SCRIPT_EYE: i64 = 14;
    const SCRIPT_MOUTH: i64 = 15;

    fn resolver(p: PPtr) -> Option<String> {
        match p.path_id {
            SCRIPT_PARAM => Some(PARAMETER_CLASS.into()),
            SCRIPT_RENDER => Some(RENDER_CONTROLLER_CLASS.into()),
            SCRIPT_OTHER => Some("SomethingElse".into()),
            SCRIPT_PART => Some(PART_CLASS.into()),
            SCRIPT_EYE => Some(EYE_BLINK_CLASS.into()),
            SCRIPT_MOUTH => Some(MOUTH_CLASS.into()),
            _ => None,
        }
    }

    fn table() -> NameTable {
        NameTable::new(["ParamA", "ParamB", "ParamC"]).unwrap().with_parts(["PartArm", "PartHead"])
    }

    fn binding(path: &str, script: i64, attribute: &str) -> GenericBinding {
        GenericBinding {
            path: crc32_str(path),
            attribute: crc32_str(attribute),
            script: PPtr { file_index: 0, path_id: script },
            type_id: 114,
            custom_type: 0,
            is_pptr_curve: 0,
            is_int_curve: 0,
            is_serialize_reference_curve: 0,
        }
    }

    fn param(id: &str) -> GenericBinding {
        binding(&format!("Parameters/{id}"), SCRIPT_PARAM, "Value")
    }

    fn sk(time: f32, coeffs: [f32; 3], value: f32) -> StreamedKey {
        StreamedKey { time, coeffs, value }
    }

    fn clip(streamed: Vec<Vec<StreamedKey>>, constants: Vec<f32>, bindings: Vec<GenericBinding>) -> AnimationClip {
        AnimationClip {
            name: "Idle".into(),
            legacy: false,
            compressed: false,
            use_high_quality_curve: true,
            sample_rate: 60.0,
            wrap_mode: 2,
            start_time: 0.0,
            stop_time: 2.0,
            loop_time: true,
            streamed,
            dense: DenseClip { frame_count: 0, curve_count: 0, sample_rate: 0.0, begin_time: 0.0, samples: Vec::new() },
            constants,
            bindings,
            events: Vec::new(),
        }
    }

    fn convert_opt(clip: &AnimationClip) -> (Option<Motion>, LoadReport) {
        let mut report = LoadReport::new();
        let motion = to_motion(clip, &table(), &resolver, &mut report);
        (motion, report)
    }

    fn convert(clip: &AnimationClip) -> (Motion, LoadReport) {
        let (motion, report) = convert_opt(clip);
        (motion.expect("this clip resolves a curve"), report)
    }

    #[test]
    fn a_clip_with_no_resolved_curve_yields_no_motion_but_a_note() {
        let c = clip(vec![vec![sk(0.0, [0.0; 3], 0.0)]], vec![], vec![param("ParamNotInThisModel")]);
        let (m, report) = convert_opt(&c);
        assert!(m.is_none());
        assert_eq!(report.len(), 1);
        assert!(report.to_string().contains("no curve drives a parameter"), "{report}");
    }

    #[test]
    fn key_times_are_relative_to_the_clip_start() {
        let mut c = clip(
            vec![vec![sk(1.0, [0.0, 0.0, 1.0], 0.0), sk(2.0, [0.0; 3], 1.0)]],
            vec![0.5],
            vec![param("ParamA"), param("ParamB")],
        );
        c.start_time = 1.0;
        c.stop_time = 3.0;
        let (m, _) = convert(&c);
        assert_eq!(m.duration, 2.0);
        assert_eq!(m.curves[0].keys.iter().map(|k| k.time).collect::<Vec<_>>(), [0.0, 1.0]);
        assert_eq!(m.curves[1].keys[0].time, 0.0, "a constant stays at zero");

        let mut d = clip(vec![], vec![], vec![param("ParamA")]);
        d.start_time = 0.5;
        d.stop_time = 1.0;
        d.dense =
            DenseClip { frame_count: 2, curve_count: 1, sample_rate: 10.0, begin_time: 0.5, samples: vec![0.0, 1.0] };
        let (m, _) = convert(&d);
        let times: Vec<f32> = m.curves[0].keys.iter().map(|k| k.time).collect();
        assert!(times[0].abs() < 1e-6 && (times[1] - 0.1).abs() < 1e-6, "{times:?}");
    }

    #[test]
    fn a_streamed_and_a_constant_curve_become_a_motion() {
        let c = clip(
            vec![vec![sk(0.0, [0.0, 0.0, 1.0], 0.0), sk(1.0, [0.0, 0.0, 0.0], 1.0)]],
            vec![0.5],
            vec![param("ParamA"), param("ParamB")],
        );
        let (m, report) = convert(&c);
        assert_eq!((m.name.as_str(), m.duration, m.fade_in, m.fade_out), ("Idle", 2.0, 1.0, 1.0));
        assert_eq!(m.curves.len(), 2);
        assert_eq!(m.curves[0].target, "ParamA");
        assert_eq!((m.curves[0].fade_in, m.curves[0].fade_out), (None, None));
        assert_eq!(
            m.curves[0].keys,
            vec![
                CurveKey { time: 0.0, value: 0.0, in_slope: 1.0, out_slope: 1.0, stepped: false },
                CurveKey { time: 1.0, value: 1.0, in_slope: 1.0, out_slope: 0.0, stepped: false },
            ]
        );
        assert_eq!(m.curves[1].target, "ParamB");
        assert_eq!(
            m.curves[1].keys,
            vec![CurveKey { time: 0.0, value: 0.5, in_slope: 0.0, out_slope: 0.0, stepped: false }]
        );
        assert_eq!(report.len(), 1, "one aggregated note per clip");
        assert!(report.to_string().contains("fade times are unavailable"), "{report}");

        // And it is a motion the package accepts.
        let ir = a2d_core::ir::cubism::CubismIr { motions: vec![m], ..Default::default() };
        assert!(a2d_pack::Package::from_cubism(ir, "m", "cubism-moc3-v3").encode_model().is_ok());
    }

    /// Evaluates a Hermite segment the way the runtime sampler does.
    fn hermite(k0: &CurveKey, k1: &CurveKey, t: f32) -> f32 {
        let span = k1.time - k0.time;
        let s = (t - k0.time) / span;
        let (s2, s3) = (s * s, s * s * s);
        (2.0 * s3 - 3.0 * s2 + 1.0) * k0.value
            + (s3 - 2.0 * s2 + s) * span * k0.out_slope
            + (-2.0 * s3 + 3.0 * s2) * k1.value
            + (s3 - s2) * span * k1.in_slope
    }

    #[test]
    fn hermite_keys_reproduce_the_source_cubics() {
        // A small deterministic generator: reproducible without a dependency.
        let mut state = 0x2545_F491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        };
        for _ in 0..200 {
            // Build keys by sampling real cubics that join continuously: each
            // key's cubic is fitted to reach the next key's value.
            let n = 2 + (next() * 6.0) as usize;
            let mut times = vec![0.0f32];
            for _ in 1..n {
                times.push(times[times.len() - 1] + 0.05 + next() * 0.5);
            }
            let values: Vec<f32> = (0..n).map(|_| next() * 4.0 - 2.0).collect();
            let mut keys = Vec::new();
            let mut slope = next() * 2.0 - 1.0;
            for i in 0..n {
                if i + 1 == n {
                    keys.push(sk(times[i], [0.0; 3], values[i]));
                    break;
                }
                let dx = times[i + 1] - times[i];
                let end_slope = next() * 2.0 - 1.0;
                // Cubic with value v0, slope `slope` at 0, v1 and `end_slope` at dx.
                let (v0, v1) = (values[i], values[i + 1]);
                let c2 = slope;
                let c0 = (end_slope * dx + c2 * dx - 2.0 * (v1 - v0)) / (dx * dx * dx);
                let c1 = ((v1 - v0) - c2 * dx - c0 * dx * dx * dx) / (dx * dx);
                keys.push(sk(times[i], [c0, c1, c2], v0));
                slope = end_slope;
            }
            let mut off = 0;
            let out = streamed_keys(&keys, &mut off).unwrap();
            assert_eq!(off, 0, "continuous cubics all reach the next key");
            for i in 0..n - 1 {
                let [c0, c1, c2] = keys[i].coeffs;
                for step in 0..=8 {
                    let x = (times[i + 1] - times[i]) * step as f32 / 8.0;
                    let want = ((c0 * x + c1) * x + c2) * x + keys[i].value;
                    let got = hermite(&out[i], &out[i + 1], times[i] + x);
                    assert!((want - got).abs() < 1e-4, "segment {i} at x={x}: {want} vs {got}");
                }
            }
        }
    }

    #[test]
    fn a_flat_segment_to_a_different_value_is_stepped() {
        let c = clip(
            vec![vec![sk(0.0, [0.0; 3], 0.0), sk(1.0, [0.0; 3], 1.0), sk(2.0, [0.0; 3], 1.0), sk(3.0, [0.0; 3], 0.0)]],
            vec![],
            vec![param("ParamA")],
        );
        let (m, _) = convert(&c);
        let stepped: Vec<bool> = m.curves[0].keys.iter().map(|k| k.stepped).collect();
        // 0->1 steps, 1->1 is merely flat, 1->0 steps, and the last key leaves nothing.
        assert_eq!(stepped, [true, false, true, false]);
    }

    #[test]
    fn a_segment_that_misses_its_next_key_is_counted() {
        let c = clip(vec![vec![sk(0.0, [0.0, 0.0, 1.0], 0.0), sk(1.0, [0.0; 3], 5.0)]], vec![], vec![param("ParamA")]);
        let (m, report) = convert(&c);
        assert_eq!(m.curves.len(), 1, "the curve is kept");
        assert!(report.to_string().contains("1 segment(s) are not exactly representable"), "{report}");
    }

    #[test]
    fn unresolved_hashes_and_model_opacity_make_notes_and_no_curves() {
        let c = clip(
            vec![vec![sk(0.0, [0.0; 3], 0.0)], vec![sk(0.0, [0.0; 3], 0.0)], vec![sk(0.0, [0.0; 3], 0.0)]],
            vec![],
            vec![param("ParamNotInThisModel"), binding("", SCRIPT_RENDER, "Opacity"), param("ParamA")],
        );
        let (m, report) = convert(&c);
        assert_eq!(m.curves.len(), 1);
        assert_eq!(m.curves[0].target, "ParamA");
        let text = report.to_string();
        assert_eq!(report.len(), 1, "{text}");
        assert!(text.contains("1 binding(s) match no parameter in this model"), "{text}");
        assert!(text.contains("1 model-opacity curve(s) ignored"), "{text}");
    }

    #[test]
    fn part_opacity_eye_blink_and_lip_sync_are_named_in_notes_and_make_no_curves() {
        let c = clip(
            (0..6).map(|_| vec![sk(0.0, [0.0; 3], 1.0)]).collect(),
            vec![],
            vec![
                binding("Parts/PartArm", SCRIPT_PART, "Opacity"),
                binding("Parts/PartHead", SCRIPT_PART, "Opacity"),
                binding("Parts/PartNotHere", SCRIPT_PART, "Opacity"),
                binding("", SCRIPT_EYE, "EyeOpening"),
                binding("", SCRIPT_MOUTH, "MouthOpening"),
                param("ParamA"),
            ],
        );
        let (m, report) = convert(&c);
        assert_eq!(m.curves.len(), 1);
        assert_eq!(m.curves[0].target, "ParamA");
        let text = report.to_string();
        assert_eq!(report.len(), 1, "{text}");
        assert!(text.contains("3 part-opacity curve(s) ignored"), "{text}");
        assert!(text.contains("on PartArm, PartHead, 1 on a part not named in this bundle"), "{text}");
        assert!(text.contains("1 eye-blink curve(s) ignored"), "{text}");
        assert!(text.contains("1 lip-sync curve(s) ignored"), "{text}");
        assert!(!text.contains("property 0x"), "none falls into the generic bucket: {text}");
    }

    #[test]
    fn a_part_binding_on_another_property_stays_generic() {
        let c = clip(
            vec![vec![sk(0.0, [0.0; 3], 1.0)], vec![sk(0.0, [0.0; 3], 1.0)]],
            vec![],
            vec![binding("Parts/PartArm", SCRIPT_PART, "Something"), param("ParamA")],
        );
        let (_, report) = convert(&c);
        let text = report.to_string();
        assert!(text.contains("CubismPart property"), "{text}");
        assert!(!text.contains("part-opacity"), "{text}");
    }

    #[test]
    fn a_binding_is_a_parameter_only_when_every_condition_holds() {
        let mut wrong_type = param("ParamA");
        wrong_type.type_id = 4;
        let mut wrong_flag = param("ParamB");
        wrong_flag.is_int_curve = 1;
        let wrong_attribute = binding("Parameters/ParamC", SCRIPT_PARAM, "Other");
        let wrong_script = binding("Parameters/ParamA", SCRIPT_OTHER, "Value");
        let unknown_script = binding("Parameters/ParamA", 999, "Value");
        let bindings = vec![wrong_type, wrong_flag, wrong_attribute, wrong_script, unknown_script];
        let n = bindings.len();
        let c = clip((0..n).map(|_| vec![sk(0.0, [0.0; 3], 0.0)]).collect(), vec![], bindings);
        let (m, report) = convert_opt(&c);
        assert!(m.is_none());
        let text = report.to_string();
        assert!(text.contains("SomethingElse property"), "{text}");
        assert!(text.contains("CubismParameter property"), "{text}");
        assert!(text.contains("type 4 property"), "{text}");
        assert!(!text.contains("match no parameter"), "{text}");
    }

    #[test]
    fn a_dense_curve_becomes_linear_keys_and_is_flagged_unverified() {
        let mut c = clip(vec![], vec![], vec![param("ParamA"), param("ParamB")]);
        c.dense = DenseClip {
            frame_count: 3,
            curve_count: 2,
            sample_rate: 10.0,
            begin_time: 0.0,
            // frame-major: (A0, B0, A1, B1, A2, B2)
            samples: vec![0.0, 9.0, 1.0, 9.0, 3.0, 9.0],
        };
        let (m, report) = convert(&c);
        let a = &m.curves[0].keys;
        assert_eq!(a.iter().map(|k| k.value).collect::<Vec<_>>(), [0.0, 1.0, 3.0]);
        assert!((a[1].time - 0.1).abs() < 1e-6);
        assert!((a[0].out_slope - 10.0).abs() < 1e-3 && (a[1].in_slope - 10.0).abs() < 1e-3);
        assert!((a[1].out_slope - 20.0).abs() < 1e-3 && (a[2].in_slope - 20.0).abs() < 1e-3);
        assert!(m.curves[1].keys.iter().all(|k| k.value == 9.0));
        assert!(report.to_string().contains("unverified layout"), "{report}");
    }

    #[test]
    fn a_dense_block_too_short_for_its_declared_size_is_dropped_and_reported() {
        let mut c = clip(vec![], vec![], vec![param("ParamA")]);
        c.dense = DenseClip { frame_count: 5, curve_count: 1, sample_rate: 10.0, begin_time: 0.0, samples: vec![0.0] };
        let (m, report) = convert_opt(&c);
        assert!(m.is_none());
        assert!(report.to_string().contains("did not fit the assumed layout"), "{report}");
    }

    #[test]
    fn non_finite_data_drops_the_curve_and_a_bad_length_falls_back() {
        let mut c = clip(
            vec![vec![sk(0.0, [0.0; 3], f32::NAN)], vec![sk(0.5, [0.0; 3], 1.0)]],
            vec![f32::INFINITY],
            vec![param("ParamA"), param("ParamB"), param("ParamC")],
        );
        c.stop_time = f32::NAN;
        let (m, report) = convert(&c);
        assert_eq!(m.curves.len(), 1);
        assert_eq!(m.curves[0].target, "ParamB");
        assert_eq!(m.duration, 0.5, "the last key time stands in for an unusable length");
        let text = report.to_string();
        assert!(text.contains("2 curve(s) held a non-finite value"), "{text}");
        assert!(text.contains("clip length"), "{text}");
        let ir = a2d_core::ir::cubism::CubismIr { motions: vec![m], ..Default::default() };
        assert!(a2d_pack::Package::from_cubism(ir, "m", "cubism-moc3-v3").encode_model().is_ok());
    }

    #[test]
    fn two_curves_on_one_parameter_keep_the_first() {
        let c = clip(
            vec![vec![sk(0.0, [0.0; 3], 1.0)], vec![sk(0.0, [0.0; 3], 2.0)]],
            vec![],
            vec![param("ParamA"), param("ParamA")],
        );
        let (m, report) = convert(&c);
        assert_eq!(m.curves.len(), 1);
        assert_eq!(m.curves[0].keys[0].value, 1.0);
        assert!(report.to_string().contains("already-driven parameter"), "{report}");
    }

    #[test]
    fn a_hash_collision_between_two_parameter_names_is_an_error() {
        // Identical names are not a collision.
        assert!(NameTable::new(["ParamA", "ParamA"]).is_ok());
        // Two CRC-32 inputs sharing a value cannot be built from valid UTF-8
        // cheaply, so the collision is injected at the hash.
        let mut t = NameTable::default();
        t.add(7, "First").unwrap();
        assert!(t.add(7, "First").is_ok());
        let err = t.add(7, "Second").unwrap_err();
        assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err}");
        assert!(err.to_string().contains("\"First\"") && err.to_string().contains("\"Second\""), "{err}");
    }
}
