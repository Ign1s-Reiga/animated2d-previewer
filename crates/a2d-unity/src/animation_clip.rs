//! Unity's `AnimationClip` (class 74), as a muscle-clip (compressed) clip
//! stores it.
//!
//! This is generic Unity: it decodes the container's layout and says nothing
//! about what the curves drive. Interpreting a binding's hashes is the
//! importer's job.
//!
//! # Layout
//!
//! Unity 2022.3, little-endian, type tree stripped, so the field order is the
//! only schema. In order (strings and bool runs are padded to four bytes):
//!
//! ```text
//! string m_Name
//! u8 m_Legacy · u8 m_Compressed · u8 m_UseHighQualityCurve        + align
//! 7 x i32 counts: rotation, compressed rotation, euler, position, scale,
//!                 float, PPtr curves                               (all zero)
//! f32 m_SampleRate · i32 m_WrapMode · 6 x f32 m_Bounds
//! u32 m_MuscleClipSize                  (NOT the serialized length; unused)
//! HumanPose m_DeltaPose                 (read and discarded, see below)
//! 4 x xform (m_StartX, m_StopX, m_LeftFootStartX, m_RightFootStartX)
//! float3 m_AverageSpeed
//! StreamedClip  u32[] data · u16 curveCount · u16 discreteCurveCount
//! DenseClip     i32 frameCount · u32 curveCount · f32 sampleRate
//!               · f32 beginTime · f32[] samples
//! ConstantClip  f32[] data
//! f32 m_StartTime · f32 m_StopTime · f32 m_OrientationOffsetY · f32 m_Level
//!     · f32 m_CycleOffset · f32 m_AverageAngularSpeed
//! u32[] m_IndexArray · {f32,f32}[] m_ValueArrayDelta · f32[] m_ValueArrayReferencePose
//! 11 x u8 flags (m_Mirror, m_LoopTime, ...)                        + align
//! GenericBinding[] genericBindings      (28 bytes each)
//! 12-byte[] pptrCurveMapping
//! u8 m_HasGenericRootTransform · u8 m_HasMotionFloatCurves         + align
//! Event[] m_Events    f32 time · string function · string data · PPtr
//!                     · f32 float · i32 int · i32 messageOptions
//! ```
//!
//! `xform` is `float3 t, float4 q, float3 s` (40 bytes). `HumanPose` is
//! `xform` · `float3` · `float4` · goals (`i32` count, 64 bytes each) · two
//! hand poses (`xform` · `f32[]` · 4 x `f32`) · `f32[]` · `float3[]`. Every array
//! in it is read by its own count and stepped over; nothing in it is kept,
//! because a clip with no humanoid rig carries nothing useful there.
//!
//! No length field delimits the muscle clip, so the check that the layout is
//! right is the exact end of the object: leftover bytes are an error.
//!
//! # Curves
//!
//! Curve indices are one space: streamed `[0, ns)`, then dense `[ns, ns+nd)`,
//! then constant `[ns+nd, total)`. `total` must equal the binding count, and
//! binding `i` describes curve `i`. The streamed data is a byte stream of
//! frames, `f32 time · i32 n · n x { i32 curve · f32 c0 c1 c2 c3 }`. The first
//! frame carries the time `-f32::MAX` (it holds every curve at its first value
//! with zero coefficients) and the last carries `+inf` with `n = 0`; both are
//! consumed here and neither appears in the output. Between a curve's key at
//! frame time `t0` and its next key, the value is the cubic
//! `((c0*x + c1)*x + c2)*x + c3` with `x = t - t0`.
//!
//! # What established these claims (CLAUDE.md §16)
//!
//! * The walk consumed all 30 `AnimationClip` objects of six real Unity
//!   2022.3.20 bundles to the exact final byte, with the streamed frames also
//!   consuming their stream exactly and the curve total equalling the binding
//!   count in every clip.
//! * Decoded fields matched those from UnityPy 1.25.4's type tree on the same
//!   objects (used as an oracle only; no code is shared).
//! * The segment polynomial was checked on 108,737 real segments: each key's
//!   cubic reached the next key's value to within 4e-5.
//! * The `u16 curveCount` / `u16 discreteCurveCount` split rests on the type
//!   tree alone, and `discreteCurveCount` was zero in every clip.
//!
//! # UNVERIFIED
//!
//! * **DenseClip semantics.** No real clip had a dense curve (the count was
//!   zero everywhere, though `frameCount` was set). The fields are returned
//!   raw; the reading "sample `frame * curveCount + curve` at
//!   `beginTime + frame / sampleRate`" is the importer's assumption.
//! * Non-empty legacy curve vectors, transform bindings (`typeID` 4), and
//!   PPtr or int curves were never present. A non-empty legacy vector is
//!   refused rather than walked.

use a2d_core::DecodeError;

use crate::objects::PPtr;
use crate::reader::{Endian, Reader};

/// One key of a streamed curve: a cubic segment starting at `time`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamedKey {
    pub time: f32,
    /// `c0`, `c1`, `c2` of `((c0*x + c1)*x + c2)*x + c3`. `c2` is the outgoing
    /// slope; all three are zero on a curve's last key and on a stepped one.
    pub coeffs: [f32; 3],
    /// `c3`: the value at `time`.
    pub value: f32,
}

/// A `DenseClip`, raw. Its semantics are unverified; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseClip {
    pub frame_count: i32,
    pub curve_count: u32,
    pub sample_rate: f32,
    pub begin_time: f32,
    pub samples: Vec<f32>,
}

/// What one curve targets, as hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericBinding {
    /// CRC-32 of the target's path relative to the animated root.
    pub path: u32,
    /// CRC-32 of the property name.
    pub attribute: u32,
    /// For a script-driven property, the `MonoScript` it belongs to.
    pub script: PPtr,
    /// Unity class id of the target component (114 for a `MonoBehaviour`).
    pub type_id: i32,
    pub custom_type: u8,
    pub is_pptr_curve: u8,
    pub is_int_curve: u8,
    pub is_serialize_reference_curve: u8,
}

/// An animation event, recorded for completeness.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipEvent {
    pub time: f32,
    pub function_name: String,
    pub data: String,
    pub object_reference: PPtr,
    pub float_parameter: f32,
    pub int_parameter: i32,
    pub message_options: i32,
}

/// A decoded clip.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClip {
    pub name: String,
    pub legacy: bool,
    pub compressed: bool,
    pub use_high_quality_curve: bool,
    pub sample_rate: f32,
    pub wrap_mode: i32,
    pub start_time: f32,
    pub stop_time: f32,
    /// `m_LoopTime`.
    pub loop_time: bool,
    /// Streamed curves, indexed by curve index `[0, streamed.len())`. A curve
    /// that never keys is an empty list. The sentinel first frame and the
    /// terminator are not included.
    pub streamed: Vec<Vec<StreamedKey>>,
    pub dense: DenseClip,
    /// Constant curves: one value each, held for the whole clip.
    pub constants: Vec<f32>,
    /// One per curve, in curve-index order.
    pub bindings: Vec<GenericBinding>,
    pub events: Vec<ClipEvent>,
}

/// `-f32::MAX`, the time of a streamed clip's first frame.
const SENTINEL_TIME: f32 = -f32::MAX;
/// Bytes of one streamed key: an `i32` curve index and four `f32`s.
const STREAM_KEY_BYTES: usize = 20;
/// Bytes of a streamed frame's header: `f32` time and `i32` count.
const STREAM_FRAME_HEADER: usize = 8;
const HUMAN_GOAL_BYTES: usize = 64;
const XFORM_BYTES: usize = 40;
const BINDING_BYTES: usize = 28;
const EVENT_MIN_BYTES: usize = 4 + 4 + 4 + 12 + 4 + 4 + 4;
const LEGACY_VECTORS: [&str; 7] = [
    "m_RotationCurves",
    "m_CompressedRotationCurves",
    "m_EulerCurves",
    "m_PositionCurves",
    "m_ScaleCurves",
    "m_FloatCurves",
    "m_PPtrCurves",
];

impl AnimationClip {
    /// Number of streamed curves, `ns`.
    pub fn streamed_curve_count(&self) -> usize {
        self.streamed.len()
    }

    /// Index of the first dense curve (`ns`).
    pub fn dense_curve_start(&self) -> usize {
        self.streamed.len()
    }

    /// Index of the first constant curve (`ns + nd`).
    pub fn constant_curve_start(&self) -> usize {
        self.streamed.len().saturating_add(self.dense.curve_count as usize)
    }

    /// Decodes the bytes of one `AnimationClip` object.
    ///
    /// Strict: anything that does not fit the layout in the module docs is an
    /// error, because a wrong layout would otherwise yield plausible curves.
    pub fn parse(data: &[u8]) -> Result<AnimationClip, DecodeError> {
        let mut r = Reader::new(data, Endian::Little);
        let name = r.string()?;
        let legacy = r.bool()?;
        let compressed = r.bool()?;
        let use_high_quality_curve = r.bool()?;
        r.align(4)?;

        for what in LEGACY_VECTORS {
            let at = r.position() as u64;
            let n = r.i32()?;
            if n != 0 {
                return Err(DecodeError::UnsupportedFormat(format!(
                    "AnimationClip {name:?} has {n} entries in {what} (offset {at}); \
                     only clips whose legacy curve vectors are empty are read"
                )));
            }
        }

        let sample_rate = r.f32()?;
        let wrap_mode = r.i32()?;
        r.skip(6 * 4)?; // m_Bounds
        let _muscle_clip_size = r.u32()?;

        skip_human_pose(&mut r)?;
        r.skip(4 * XFORM_BYTES + 12)?; // four xforms and m_AverageSpeed

        // StreamedClip
        let words = r.count(4, "the streamed clip")?;
        let stream = r.bytes(words * 4)?;
        let streamed_curve_count = usize::from(r.u16()?);
        let discrete = r.u16()?;
        if discrete != 0 {
            return Err(DecodeError::UnsupportedFormat(format!(
                "the streamed clip declares {discrete} discrete curves; none are expected and their layout is unknown"
            )));
        }

        // DenseClip
        let dense = DenseClip {
            frame_count: r.i32()?,
            curve_count: r.u32()?,
            sample_rate: r.f32()?,
            begin_time: r.f32()?,
            samples: r.f32_array()?,
        };
        // ConstantClip
        let constants = r.f32_array()?;

        let start_time = r.f32()?;
        let stop_time = r.f32()?;
        r.skip(4 * 4)?; // orientation offset, level, cycle offset, angular speed

        let index_array = r.count(4, "m_IndexArray")?;
        r.skip(index_array * 4)?;
        let deltas = r.count(8, "m_ValueArrayDelta")?;
        r.skip(deltas * 8)?;
        let reference_pose = r.count(4, "m_ValueArrayReferencePose")?;
        r.skip(reference_pose * 4)?;
        let flags = r.bytes(11)?;
        let loop_time = flags.get(1).is_some_and(|f| *f != 0);
        r.align(4)?;

        let binding_count = r.count(BINDING_BYTES, "the generic bindings")?;
        let mut bindings = Vec::with_capacity(binding_count);
        for _ in 0..binding_count {
            let path = r.u32()?;
            let attribute = r.u32()?;
            let script = PPtr { file_index: r.i32()?, path_id: r.i64()? };
            let type_id = r.i32()?;
            let custom_type = r.u8()?;
            let is_pptr_curve = r.u8()?;
            let is_int_curve = r.u8()?;
            let is_serialize_reference_curve = r.u8()?;
            bindings.push(GenericBinding {
                path,
                attribute,
                script,
                type_id,
                custom_type,
                is_pptr_curve,
                is_int_curve,
                is_serialize_reference_curve,
            });
        }
        let mapping = r.count(12, "pptrCurveMapping")?;
        r.skip(mapping * 12)?;
        r.bytes(2)?; // m_HasGenericRootTransform, m_HasMotionFloatCurves
        r.align(4)?;

        let event_count = r.count(EVENT_MIN_BYTES, "m_Events")?;
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            events.push(ClipEvent {
                time: r.f32()?,
                function_name: r.string()?,
                data: r.string()?,
                object_reference: PPtr { file_index: r.i32()?, path_id: r.i64()? },
                float_parameter: r.f32()?,
                int_parameter: r.i32()?,
                message_options: r.i32()?,
            });
        }

        if r.remaining() != 0 {
            return Err(DecodeError::corrupt_at(
                format!(
                    "{} bytes are left after the last field of AnimationClip {name:?}; the layout read here is not this file's",
                    r.remaining()
                ),
                r.position() as u64,
            ));
        }

        // The three curve groups must tile the binding list exactly.
        let total = streamed_curve_count
            .checked_add(dense.curve_count as usize)
            .and_then(|n| n.checked_add(constants.len()))
            .ok_or_else(|| DecodeError::corrupt("the curve counts overflow".to_string()))?;
        if total != bindings.len() {
            return Err(DecodeError::corrupt(format!(
                "AnimationClip {name:?} has {streamed_curve_count} streamed + {} dense + {} constant curves \
                 but {} bindings",
                dense.curve_count,
                constants.len(),
                bindings.len()
            )));
        }

        let streamed = decode_streamed(stream, streamed_curve_count)?;
        Ok(AnimationClip {
            name,
            legacy,
            compressed,
            use_high_quality_curve,
            sample_rate,
            wrap_mode,
            start_time,
            stop_time,
            loop_time,
            streamed,
            dense,
            constants,
            bindings,
            events,
        })
    }
}

/// Steps over `m_DeltaPose`, reading each array by its own count.
fn skip_human_pose(r: &mut Reader<'_>) -> Result<(), DecodeError> {
    r.skip(XFORM_BYTES + 12 + 16)?; // m_RootX, m_LookAtPosition, m_LookAtWeight
    let goals = r.count(HUMAN_GOAL_BYTES, "m_GoalArray")?;
    r.skip(goals * HUMAN_GOAL_BYTES)?;
    for _ in 0..2 {
        r.skip(XFORM_BYTES)?; // m_GrabX
        let dof = r.count(4, "a hand pose m_DoFArray")?;
        r.skip(dof * 4)?;
        r.skip(4 * 4)?; // m_Override, m_CloseOpen, m_InOut, m_Grab
    }
    let dof = r.count(4, "m_DoFArray")?;
    r.skip(dof * 4)?;
    let tdof = r.count(12, "m_TDoFArray")?;
    r.skip(tdof * 12)
}

/// Decodes the streamed frames into per-curve key lists.
fn decode_streamed(raw: &[u8], curve_count: usize) -> Result<Vec<Vec<StreamedKey>>, DecodeError> {
    let mut curves: Vec<Vec<StreamedKey>> = vec![Vec::new(); curve_count];
    if raw.is_empty() {
        if curve_count == 0 {
            return Ok(curves);
        }
        return Err(DecodeError::corrupt(format!(
            "the streamed clip has {curve_count} curves but no data, not even the sentinel frame"
        )));
    }

    let mut r = Reader::new(raw, Endian::Little);
    let mut first = true;
    let mut previous = SENTINEL_TIME;
    let mut terminated = false;
    while r.remaining() > 0 {
        let at = r.position() as u64;
        if terminated {
            return Err(DecodeError::corrupt_at(
                format!("{} bytes follow the streamed clip's terminator frame", r.remaining()),
                at,
            ));
        }
        if r.remaining() < STREAM_FRAME_HEADER {
            return Err(DecodeError::corrupt_at(
                format!("the streamed clip does not end on a frame boundary: {} stray bytes", r.remaining()),
                at,
            ));
        }
        let time = r.f32()?;
        let n = r.i32()?;

        if first {
            if time.to_bits() != SENTINEL_TIME.to_bits() {
                return Err(DecodeError::corrupt_at(
                    format!("the streamed clip's first frame is at time {time}, not the -FLT_MAX sentinel"),
                    at,
                ));
            }
        } else if time == f32::INFINITY {
            if n != 0 {
                return Err(DecodeError::corrupt_at(format!("the terminator frame holds {n} keys, not 0"), at));
            }
            terminated = true;
            continue;
        } else if !time.is_finite() || time <= previous {
            return Err(DecodeError::corrupt_at(
                format!("streamed frame time {time} does not follow the previous frame at {previous}"),
                at,
            ));
        }

        let n = usize::try_from(n)
            .ok()
            .filter(|n| *n <= curve_count && n.checked_mul(STREAM_KEY_BYTES).is_some_and(|b| b <= r.remaining()))
            .ok_or_else(|| {
                DecodeError::corrupt_at(
                    format!(
                        "a streamed frame declares {n} keys with {} bytes left and {curve_count} curves; \
                         the stream does not end on a frame boundary",
                        r.remaining()
                    ),
                    at,
                )
            })?;
        let mut previous_index: Option<i32> = None;
        for _ in 0..n {
            let index = r.i32()?;
            if previous_index.is_some_and(|p| index <= p) {
                return Err(DecodeError::corrupt_at(
                    format!("streamed key indices must strictly ascend within a frame, but {index} follows {previous_index:?}"),
                    at,
                ));
            }
            previous_index = Some(index);
            let coeffs = [r.f32()?, r.f32()?, r.f32()?];
            let value = r.f32()?;
            let slot = usize::try_from(index).ok().and_then(|i| curves.get_mut(i)).ok_or_else(|| {
                DecodeError::corrupt_at(format!("a streamed key names curve {index} of {curve_count}"), at)
            })?;
            if !first {
                slot.push(StreamedKey { time, coeffs, value });
            }
        }
        previous = time;
        first = false;
    }
    if !terminated {
        return Err(DecodeError::corrupt("the streamed clip ends without its +inf terminator frame".to_string()));
    }
    Ok(curves)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_f32(out: &mut Vec<u8>, v: f32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn put_i32(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn pad(out: &mut Vec<u8>) {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    fn put_string(out: &mut Vec<u8>, s: &str) {
        put_i32(out, s.len() as i32);
        out.extend_from_slice(s.as_bytes());
        pad(out);
    }
    fn zeros(out: &mut Vec<u8>, n: usize) {
        out.extend(std::iter::repeat_n(0u8, n));
    }

    type Frame = (f32, Vec<(i32, [f32; 4])>);

    /// Serializes frames the way the streamed block holds them.
    fn frames_to_bytes(frames: &[Frame]) -> Vec<u8> {
        let mut out = Vec::new();
        for (time, keys) in frames {
            put_f32(&mut out, *time);
            put_i32(&mut out, keys.len() as i32);
            for (index, c) in keys {
                put_i32(&mut out, *index);
                for v in c {
                    put_f32(&mut out, *v);
                }
            }
        }
        out
    }

    /// Everything a test may want to vary.
    #[derive(Clone)]
    struct Spec {
        legacy_counts: [i32; 7],
        stream: Vec<u8>,
        streamed_curves: u16,
        discrete: u16,
        dense_curves: u32,
        constants: Vec<f32>,
        bindings: usize,
        goals: usize,
        trailing: Vec<u8>,
    }

    fn good_frames() -> Vec<Frame> {
        vec![
            (-f32::MAX, vec![(0, [0.0, 0.0, 0.0, 0.0]), (1, [0.0, 0.0, 0.0, 5.0])]),
            (0.0, vec![(0, [1.0, 2.0, 3.0, 0.0]), (1, [0.0, 0.0, 0.0, 5.0])]),
            (0.5, vec![(0, [0.0, 0.0, 0.0, 1.5])]),
            (f32::INFINITY, vec![]),
        ]
    }

    fn good() -> Spec {
        Spec {
            legacy_counts: [0; 7],
            stream: frames_to_bytes(&good_frames()),
            streamed_curves: 2,
            discrete: 0,
            dense_curves: 0,
            constants: vec![0.25],
            bindings: 3,
            goals: 1,
            trailing: Vec::new(),
        }
    }

    fn write(spec: &Spec) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, "clip_a"); // 6 chars: exercises padding
        o.extend_from_slice(&[0, 0, 1]);
        pad(&mut o);
        for n in spec.legacy_counts {
            put_i32(&mut o, n);
        }
        put_f32(&mut o, 60.0);
        put_i32(&mut o, 2);
        zeros(&mut o, 24); // bounds
        put_u32(&mut o, 7); // muscle clip size, ignored
                            // HumanPose
        zeros(&mut o, 40 + 12 + 16);
        put_i32(&mut o, spec.goals as i32);
        zeros(&mut o, 64 * spec.goals);
        for dofs in [3usize, 2] {
            zeros(&mut o, 40);
            put_i32(&mut o, dofs as i32);
            zeros(&mut o, 4 * dofs);
            zeros(&mut o, 16);
        }
        put_i32(&mut o, 2);
        zeros(&mut o, 8);
        put_i32(&mut o, 1);
        zeros(&mut o, 12);
        zeros(&mut o, 4 * 40 + 12);
        // StreamedClip
        put_i32(&mut o, (spec.stream.len() / 4) as i32);
        o.extend_from_slice(&spec.stream);
        o.extend_from_slice(&spec.streamed_curves.to_le_bytes());
        o.extend_from_slice(&spec.discrete.to_le_bytes());
        // DenseClip
        put_i32(&mut o, 4);
        put_u32(&mut o, spec.dense_curves);
        put_f32(&mut o, 30.0);
        put_f32(&mut o, 0.0);
        put_i32(&mut o, 0);
        // ConstantClip
        put_i32(&mut o, spec.constants.len() as i32);
        for c in &spec.constants {
            put_f32(&mut o, *c);
        }
        for v in [0.0f32, 2.0, 0.0, 0.0, 0.0, 0.0] {
            put_f32(&mut o, v);
        }
        put_i32(&mut o, 3); // m_IndexArray
        zeros(&mut o, 12);
        put_i32(&mut o, 1); // m_ValueArrayDelta
        zeros(&mut o, 8);
        put_i32(&mut o, 0); // reference pose
        o.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        pad(&mut o);
        // bindings
        put_i32(&mut o, spec.bindings as i32);
        for i in 0..spec.bindings {
            put_u32(&mut o, 100 + i as u32);
            put_u32(&mut o, 200 + i as u32);
            put_i32(&mut o, 0);
            o.extend_from_slice(&(40i64 + i as i64).to_le_bytes());
            put_i32(&mut o, 114);
            o.extend_from_slice(&[0, 0, 0, 0]);
        }
        put_i32(&mut o, 0); // pptrCurveMapping
        o.extend_from_slice(&[0, 0]);
        pad(&mut o);
        // one event
        put_i32(&mut o, 1);
        put_f32(&mut o, 0.0);
        put_string(&mut o, "InstanceId");
        put_string(&mut o, "");
        put_i32(&mut o, 0);
        o.extend_from_slice(&0i64.to_le_bytes());
        put_f32(&mut o, 0.0);
        put_i32(&mut o, 9);
        put_i32(&mut o, 0);
        o.extend_from_slice(&spec.trailing);
        o
    }

    #[test]
    fn a_clip_round_trips() {
        let clip = AnimationClip::parse(&write(&good())).unwrap();
        assert_eq!(clip.name, "clip_a");
        assert!(!clip.legacy && !clip.compressed && clip.use_high_quality_curve);
        assert_eq!((clip.sample_rate, clip.wrap_mode), (60.0, 2));
        assert_eq!((clip.start_time, clip.stop_time), (0.0, 2.0));
        assert!(clip.loop_time);
        assert_eq!(clip.streamed_curve_count(), 2);
        assert_eq!(clip.constant_curve_start(), 2);
        assert_eq!(clip.constants, vec![0.25]);
        assert_eq!(clip.dense.frame_count, 4);
        assert!(clip.dense.samples.is_empty());

        // The sentinel and the terminator are gone; only real keys remain.
        assert_eq!(
            clip.streamed[0],
            vec![
                StreamedKey { time: 0.0, coeffs: [1.0, 2.0, 3.0], value: 0.0 },
                StreamedKey { time: 0.5, coeffs: [0.0, 0.0, 0.0], value: 1.5 },
            ]
        );
        assert_eq!(clip.streamed[1], vec![StreamedKey { time: 0.0, coeffs: [0.0; 3], value: 5.0 }]);

        assert_eq!(clip.bindings.len(), 3);
        assert_eq!((clip.bindings[2].path, clip.bindings[2].attribute), (102, 202));
        assert_eq!(clip.bindings[1].script, PPtr { file_index: 0, path_id: 41 });
        assert_eq!(clip.bindings[0].type_id, 114);

        assert_eq!(clip.events.len(), 1);
        assert_eq!(clip.events[0].function_name, "InstanceId");
        assert_eq!(clip.events[0].int_parameter, 9);
    }

    #[test]
    fn a_clip_without_streamed_curves_or_data_is_fine() {
        let spec = Spec { stream: Vec::new(), streamed_curves: 0, bindings: 1, ..good() };
        let clip = AnimationClip::parse(&write(&spec)).unwrap();
        assert!(clip.streamed.is_empty());
        assert_eq!(clip.constants.len(), 1);
    }

    fn fails(spec: Spec, needle: &str) {
        let err = AnimationClip::parse(&write(&spec)).expect_err("this clip must be refused").to_string();
        assert!(err.contains(needle), "error {err:?} should mention {needle:?}");
    }

    #[test]
    fn non_empty_legacy_curve_vectors_are_refused() {
        for i in 0..7 {
            let mut legacy_counts = [0; 7];
            legacy_counts[i] = 1;
            fails(Spec { legacy_counts, ..good() }, LEGACY_VECTORS[i]);
        }
    }

    #[test]
    fn counts_beyond_the_remaining_bytes_are_refused() {
        let mut bytes = write(&good());
        // The name is 4 + 8 bytes, then 4 flag bytes: the first legacy count
        // starts at 12 + 4 = 16.
        bytes[16..20].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(AnimationClip::parse(&bytes).is_err());
        // A goal count that cannot fit.
        let spec = Spec { goals: 1, ..good() };
        let mut bytes = write(&spec);
        let goal_count_at = 12 + 4 + 28 + 32 + 4 + 68;
        bytes[goal_count_at..goal_count_at + 4].copy_from_slice(&1_000_000i32.to_le_bytes());
        let err = AnimationClip::parse(&bytes).unwrap_err().to_string();
        assert!(err.contains("m_GoalArray"), "{err}");
    }

    #[test]
    fn a_curve_index_beyond_the_curve_count_is_refused() {
        let mut frames = good_frames();
        frames[2].1[0].0 = 2;
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "names curve 2 of 2");
        let mut frames = good_frames();
        frames[1].1[0].0 = -1;
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "names curve -1");
    }

    #[test]
    fn a_stream_not_ending_on_a_frame_boundary_is_refused() {
        // Four stray bytes are a whole word but not a frame header.
        let mut stream = frames_to_bytes(&good_frames()[..1]);
        stream.extend_from_slice(&[0; 4]);
        fails(Spec { stream, ..good() }, "stray bytes");
        // And anything after the terminator is refused too.
        let mut stream = frames_to_bytes(&good_frames());
        stream.extend_from_slice(&[0; 4]);
        fails(Spec { stream, ..good() }, "follow the streamed clip's terminator");
        // A frame that declares more keys than the stream holds.
        let mut stream = frames_to_bytes(&good_frames()[..3]);
        let at = stream.len() - 20 - 8 + 4;
        stream[at..at + 4].copy_from_slice(&2i32.to_le_bytes());
        fails(Spec { stream, ..good() }, "frame boundary");
    }

    #[test]
    fn a_missing_sentinel_is_refused() {
        let frames = good_frames()[1..].to_vec();
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "sentinel");
        fails(Spec { stream: Vec::new(), ..good() }, "no data");
    }

    #[test]
    fn a_missing_terminator_is_refused() {
        let frames = good_frames()[..3].to_vec();
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "terminator");
    }

    #[test]
    fn frame_times_must_increase_and_the_terminator_must_be_last() {
        let mut frames = good_frames();
        frames[2].0 = 0.0;
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "does not follow");
        let mut frames = good_frames();
        frames[3].1.push((0, [0.0; 4]));
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "terminator frame holds");
        let mut frames = good_frames();
        frames.push((9.0, vec![]));
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "follow");
    }

    #[test]
    fn discrete_curves_are_unsupported() {
        let err = AnimationClip::parse(&write(&Spec { discrete: 1, ..good() })).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedFormat(_)), "{err}");
        assert!(err.to_string().contains("discrete"));
    }

    #[test]
    fn key_indices_must_strictly_ascend_within_a_frame() {
        let mut frames = good_frames();
        frames[1].1.swap(0, 1);
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "strictly ascend");
        let mut frames = good_frames();
        frames[1].1[1].0 = 0; // a repeated index
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "strictly ascend");
    }

    #[test]
    fn the_curve_total_must_equal_the_binding_count() {
        fails(Spec { bindings: 2, ..good() }, "2 bindings");
        fails(Spec { bindings: 4, ..good() }, "4 bindings");
        fails(Spec { dense_curves: 1, ..good() }, "1 dense");
    }

    #[test]
    fn leftover_bytes_are_refused() {
        fails(Spec { trailing: vec![0; 4], ..good() }, "bytes are left");
    }

    #[test]
    fn truncation_at_every_length_is_an_error_and_never_a_panic() {
        let bytes = write(&good());
        for cut in 0..bytes.len() {
            assert!(AnimationClip::parse(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        assert!(AnimationClip::parse(&bytes).is_ok());
    }

    #[test]
    fn a_non_finite_sentinel_lookalike_is_not_accepted() {
        let mut frames = good_frames();
        frames[0].0 = -1.0e30;
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "sentinel");
        frames[0].0 = f32::NAN;
        fails(Spec { stream: frames_to_bytes(&frames), ..good() }, "sentinel");
    }
}
