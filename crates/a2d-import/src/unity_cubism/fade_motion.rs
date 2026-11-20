//! Recovering a motion from a `CubismFadeMotionData` behaviour.
//!
//! The Cubism SDK's C# class keeps what the editor's `.motion3.json` held, as
//! Unity serialized fields. With the type tree stripped, the field order is the
//! only schema, and it is taken from the SDK's public source
//! (`Framework/MotionFade/CubismFadeMotionData.cs`). Two orders exist:
//!
//! ```text
//! newer  string MotionName · f32 ModelFadeInTime · f32 ModelFadeOutTime
//!        · f32 FadeInTime · f32 FadeOutTime · string[] ParameterIds
//!        · AnimationCurve[] ParameterCurves · f32[] ParameterFadeInTimes
//!        · f32[] ParameterFadeOutTimes · f32 MotionLength
//! older  the same without the two Model* times
//! ```
//!
//! Neither has been confirmed against a real bundle, and the `AnimationCurve`
//! layout inside is itself unverified (see `a2d_unity::curve`). So decoding
//! tries both orders and accepts one only if it consumes the body exactly and
//! the four parallel arrays agree in length. Two fits, or none, is an error:
//! a wrong layout must fail loudly rather than yield a plausible motion.

use a2d_core::ir::cubism::{CurveKey, Motion, MotionCurve};
use a2d_core::{DecodeError, LoadReport};
use a2d_unity::{AnimationCurve, Reader};

/// The Cubism framework's default fade time, used when the source left it unset.
pub(super) const DEFAULT_FADE: f32 = 1.0;

/// A fade motion as serialized, before it becomes IR.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FadeMotion {
    /// The `.motion3.json` path it was built from.
    pub motion_name: String,
    pub fade_in: f32,
    pub fade_out: f32,
    pub parameter_ids: Vec<String>,
    pub curves: Vec<AnimationCurve>,
    pub parameter_fade_in: Vec<f32>,
    pub parameter_fade_out: Vec<f32>,
    pub motion_length: f32,
}

/// Reads one layout, requiring it to fit the body exactly.
fn read_layout(body: &Reader<'_>, model_fades: bool) -> Result<FadeMotion, DecodeError> {
    let mut r = body.clone();
    let motion_name = r.string()?;
    if model_fades {
        // Model-level fades have no counterpart in the IR; they are read only
        // to step over them.
        r.f32()?;
        r.f32()?;
    }
    let fade_in = r.f32()?;
    let fade_out = r.f32()?;
    let parameter_ids = r.string_array()?;
    let curves = r.animation_curve_array()?;
    let parameter_fade_in = r.f32_array()?;
    let parameter_fade_out = r.f32_array()?;
    let motion_length = r.f32()?;
    if r.remaining() != 0 {
        return Err(DecodeError::corrupt(format!("{} bytes are left after the last field", r.remaining())));
    }
    let n = parameter_ids.len();
    if curves.len() != n || parameter_fade_in.len() != n || parameter_fade_out.len() != n {
        return Err(DecodeError::corrupt(format!(
            "parallel arrays disagree: {n} ids, {} curves, {} fade-ins, {} fade-outs",
            curves.len(),
            parameter_fade_in.len(),
            parameter_fade_out.len()
        )));
    }
    Ok(FadeMotion {
        motion_name,
        fade_in,
        fade_out,
        parameter_ids,
        curves,
        parameter_fade_in,
        parameter_fade_out,
        motion_length,
    })
}

/// Decodes a body positioned after the behaviour header and `m_Name`.
pub(super) fn decode(body: &Reader<'_>) -> Result<FadeMotion, DecodeError> {
    choose_layout(read_layout(body, true), read_layout(body, false))
}

/// Picks between the 10-field (`newer`) and 8-field (`older`) readings: exactly
/// one may fit. Pure so the ambiguity arm can be tested without hand-crafting
/// bytes that parse exactly under both layouts.
fn choose_layout(
    newer: Result<FadeMotion, DecodeError>,
    older: Result<FadeMotion, DecodeError>,
) -> Result<FadeMotion, DecodeError> {
    match (newer, older) {
        (Ok(m), Err(_)) | (Err(_), Ok(m)) => Ok(m),
        (Ok(_), Ok(_)) => Err(DecodeError::Reconstruction {
            game: "unity_cubism".into(),
            message: "the fade motion data fits both the 8-field and the 10-field layout; refusing to guess".into(),
        }),
        (Err(newer), Err(older)) => Err(DecodeError::Reconstruction {
            game: "unity_cubism".into(),
            message: format!(
                "the fade motion data fits neither known layout (10-field: {newer}; 8-field: {older}); \
                 this Cubism SDK build serializes it differently"
            ),
        }),
    }
}

/// The motion's name from its source path: the file stem without
/// `.motion3.json`, falling back to the behaviour's own name.
pub(super) fn motion_stem(motion_name: &str, behaviour_name: &str) -> String {
    let file = motion_name.rsplit(['/', '\\']).next().unwrap_or(motion_name);
    let lower = file.to_ascii_lowercase();
    let stem = match lower.strip_suffix(".motion3.json") {
        Some(_) => &file[..file.len() - ".motion3.json".len()],
        None => file,
    };
    if !stem.is_empty() {
        stem.to_string()
    } else if !behaviour_name.is_empty() {
        behaviour_name.to_string()
    } else {
        "motion".to_string()
    }
}

/// Gives every name a unique form, deterministically, reporting each rename.
///
/// Input order decides who keeps the bare name, so callers sort first.
pub(super) fn dedupe_names(names: &mut [String], report: &mut LoadReport) {
    for i in 0..names.len() {
        let taken = |candidate: &str, upto: usize, names: &[String]| names[..upto].iter().any(|n| n == candidate);
        if !taken(&names[i], i, names) {
            continue;
        }
        let original = names[i].clone();
        let mut n = 2usize;
        // A suffixed name must not collide with a later, genuine one either.
        let unique = loop {
            let candidate = format!("{original}_{n}");
            if !taken(&candidate, i, names) && !names[i + 1..].contains(&candidate) {
                break candidate;
            }
            n += 1;
        };
        report.note(format!("two motions are named {original:?}; the later one is renamed {unique:?}"));
        names[i] = unique;
    }
}

/// Converts decoded fade data to IR. Never produces anything the package
/// writer would refuse; whatever has to be dropped or altered is reported.
pub(super) fn to_motion(fade: &FadeMotion, name: String, report: &mut LoadReport) -> Motion {
    let mut weighted = false;
    let mut nan_slope = false;
    let mut curves = Vec::new();

    for (i, curve) in fade.curves.iter().enumerate() {
        let id = &fade.parameter_ids[i];
        if id.is_empty() {
            report.note(format!("motion {name:?}: slot {i} has no parameter id and is skipped"));
            continue;
        }
        if curve.keys.is_empty() {
            report.note(format!("motion {name:?}: curve {id:?} has no keys and is skipped"));
            continue;
        }
        if curve.keys.iter().any(|k| !k.time.is_finite() || !k.value.is_finite()) {
            report.note(format!("motion {name:?}: curve {id:?} has a non-finite key time or value and is dropped"));
            continue;
        }
        if curve.keys.windows(2).any(|w| w[0].time > w[1].time) {
            report.note(format!("motion {name:?}: curve {id:?} has keys out of time order and is dropped"));
            continue;
        }
        weighted |= curve.keys.iter().any(|k| k.weighted_mode != 0);

        let mut keys: Vec<CurveKey> = Vec::with_capacity(curve.keys.len());
        for (k, key) in curve.keys.iter().enumerate() {
            // Unity marks a constant segment with an infinite tangent on
            // either end of it.
            let stepped =
                curve.keys.get(k + 1).is_some_and(|next| key.out_slope.is_infinite() || next.in_slope.is_infinite());
            let mut slope = |s: f32| {
                if s.is_nan() {
                    nan_slope = true;
                }
                if s.is_finite() {
                    s
                } else {
                    0.0
                }
            };
            keys.push(CurveKey {
                time: key.time,
                value: key.value,
                in_slope: slope(key.in_slope),
                out_slope: slope(key.out_slope),
                stepped,
            });
        }

        // Negative is Cubism's "unset"; a non-finite time is corrupt data.
        let mut fade_time = |v: Option<&f32>, what: &str| {
            let v = v.copied()?;
            if !v.is_finite() {
                report.note(format!("motion {name:?}: curve {id:?} has a non-finite {what} time ({v}); it is unset"));
            }
            Some(v).filter(|v| v.is_finite() && *v >= 0.0)
        };
        curves.push(MotionCurve {
            target: id.clone(),
            fade_in: fade_time(fade.parameter_fade_in.get(i), "fade-in"),
            fade_out: fade_time(fade.parameter_fade_out.get(i), "fade-out"),
            keys,
        });
    }

    if weighted {
        report.note(format!("motion {name:?}: weighted tangents are not supported; the weights are ignored"));
    }
    if nan_slope {
        report.note(format!("motion {name:?}: NaN tangents were replaced by 0"));
    }

    let duration = if fade.motion_length.is_finite() && fade.motion_length >= 0.0 {
        fade.motion_length
    } else {
        let last = curves.iter().flat_map(|c| c.keys.iter()).map(|k| k.time).fold(0.0f32, f32::max).max(0.0);
        report.note(format!(
            "motion {name:?}: length {} is not usable; the last key time {last} is used instead",
            fade.motion_length
        ));
        last
    };
    let mut motion_fade = |v: f32, what: &str| {
        if !v.is_finite() {
            report.note(format!("motion {name:?}: non-finite {what} time ({v}); the default {DEFAULT_FADE} is used"));
        }
        if v.is_finite() && v >= 0.0 {
            v
        } else {
            DEFAULT_FADE
        }
    };
    let (fade_in, fade_out) = (motion_fade(fade.fade_in, "fade-in"), motion_fade(fade.fade_out, "fade-out"));

    Motion { name, duration, fade_in, fade_out, curves }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use a2d_unity::{Endian, Keyframe};

    fn put_string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as i32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn put_f32(out: &mut Vec<u8>, v: f32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn put_i32(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn key(time: f32, value: f32, in_slope: f32, out_slope: f32) -> Keyframe {
        Keyframe { time, value, in_slope, out_slope, weighted_mode: 0, in_weight: 0.0, out_weight: 0.0 }
    }

    fn curve(keys: Vec<Keyframe>) -> AnimationCurve {
        AnimationCurve { keys, pre_infinity: 2, post_infinity: 2, rotation_order: 4 }
    }

    /// Writes `fade` in the chosen layout. The body is built from a fresh
    /// buffer, so string padding is relative to it, as in a real object once
    /// the header (a multiple of four) has been consumed.
    pub(in super::super) fn body(fade: &FadeMotion, model_fades: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, &fade.motion_name);
        if model_fades {
            put_f32(&mut out, 0.5);
            put_f32(&mut out, 0.5);
        }
        put_f32(&mut out, fade.fade_in);
        put_f32(&mut out, fade.fade_out);
        put_i32(&mut out, fade.parameter_ids.len() as i32);
        for id in &fade.parameter_ids {
            put_string(&mut out, id);
        }
        put_i32(&mut out, fade.curves.len() as i32);
        for c in &fade.curves {
            put_i32(&mut out, c.keys.len() as i32);
            for k in &c.keys {
                for v in [k.time, k.value, k.in_slope, k.out_slope] {
                    put_f32(&mut out, v);
                }
                put_i32(&mut out, k.weighted_mode);
                put_f32(&mut out, k.in_weight);
                put_f32(&mut out, k.out_weight);
            }
            for v in [c.pre_infinity, c.post_infinity, c.rotation_order] {
                put_i32(&mut out, v);
            }
        }
        for list in [&fade.parameter_fade_in, &fade.parameter_fade_out] {
            put_i32(&mut out, list.len() as i32);
            for v in list {
                put_f32(&mut out, *v);
            }
        }
        put_f32(&mut out, fade.motion_length);
        out
    }

    pub(in super::super) fn sample() -> FadeMotion {
        FadeMotion {
            motion_name: "Assets/x/motions/Hero_idle.motion3.json".into(),
            fade_in: 0.3,
            fade_out: 0.4,
            parameter_ids: vec!["ParamA".into(), "ParamAngleX".into()],
            curves: vec![
                curve(vec![key(0.0, 0.0, 0.0, 1.0), key(1.0, 1.0, 1.0, 0.0)]),
                curve(vec![key(0.0, 2.0, 0.0, 0.0)]),
            ],
            parameter_fade_in: vec![0.1, -1.0],
            parameter_fade_out: vec![-1.0, 0.2],
            motion_length: 3.0,
        }
    }

    fn decode_bytes(bytes: &[u8]) -> Result<FadeMotion, DecodeError> {
        decode(&Reader::new(bytes, Endian::Little))
    }

    #[test]
    fn the_ten_field_layout_decodes() {
        let want = sample();
        assert_eq!(decode_bytes(&body(&want, true)).unwrap(), want);
    }

    #[test]
    fn the_eight_field_layout_decodes() {
        let want = sample();
        assert_eq!(decode_bytes(&body(&want, false)).unwrap(), want);
    }

    #[test]
    fn trailing_bytes_fit_no_layout() {
        for model_fades in [true, false] {
            let mut bytes = body(&sample(), model_fades);
            bytes.extend_from_slice(&[0; 4]);
            assert!(decode_bytes(&bytes).is_err());
        }
    }

    #[test]
    fn mismatched_array_lengths_fit_no_layout() {
        let mut bad = sample();
        bad.parameter_fade_out.pop();
        assert!(decode_bytes(&body(&bad, true)).is_err());
        assert!(decode_bytes(&body(&bad, false)).is_err());
    }

    #[test]
    fn a_truncated_body_is_an_error_at_every_length() {
        let bytes = body(&sample(), true);
        for cut in 0..bytes.len() {
            assert!(decode_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn a_name_is_the_file_stem_with_fallbacks() {
        assert_eq!(motion_stem("Assets/a/Hero_idle.motion3.json", "x"), "Hero_idle");
        assert_eq!(motion_stem("a\\b\\Hero.MOTION3.JSON", "x"), "Hero");
        assert_eq!(motion_stem("", "behaviour"), "behaviour");
        assert_eq!(motion_stem(".motion3.json", "behaviour"), "behaviour");
        assert_eq!(motion_stem("", ""), "motion");
    }

    #[test]
    fn duplicate_names_are_renamed_deterministically_and_reported() {
        let mut names: Vec<String> = ["a", "a", "a_2", "a"].iter().map(|s| s.to_string()).collect();
        let mut report = LoadReport::new();
        dedupe_names(&mut names, &mut report);
        assert_eq!(names, ["a", "a_3", "a_2", "a_4"]);
        assert_eq!(report.len(), 2);
    }

    #[test]
    fn conversion_maps_fades_and_skips_unusable_slots() {
        let mut fade = sample();
        fade.parameter_ids.push(String::new());
        fade.curves.push(curve(vec![key(0.0, 1.0, 0.0, 0.0)]));
        fade.parameter_ids.push("Empty".into());
        fade.curves.push(curve(Vec::new()));
        fade.parameter_fade_in.extend([0.0, 0.0]);
        fade.parameter_fade_out.extend([0.0, 0.0]);
        fade.fade_in = -1.0;

        let mut report = LoadReport::new();
        let m = to_motion(&fade, "Hero_idle".into(), &mut report);
        assert_eq!(m.duration, 3.0);
        assert_eq!(m.fade_in, 1.0, "negative means unset, so the framework default");
        assert_eq!(m.fade_out, 0.4);
        assert_eq!(m.curves.len(), 2, "the empty id and the keyless curve are skipped");
        assert_eq!(m.curves[0].fade_in, Some(0.1));
        assert_eq!(m.curves[0].fade_out, None);
        assert_eq!(m.curves[1].fade_in, None);
        assert_eq!(m.curves[1].fade_out, Some(0.2));
        let text = report.to_string();
        assert!(text.contains("no parameter id") && text.contains("no keys"), "{text}");
    }

    #[test]
    fn an_infinite_tangent_marks_a_stepped_segment_and_is_replaced() {
        let mut fade = sample();
        fade.curves = vec![
            curve(vec![
                key(0.0, 0.0, 0.0, f32::INFINITY),
                key(1.0, 1.0, 0.0, 0.0),
                key(2.0, 0.0, f32::INFINITY, 0.0),
                key(3.0, 1.0, 0.0, f32::INFINITY),
            ]),
            curve(vec![key(0.0, 0.0, 0.0, 0.0)]),
        ];
        let m = to_motion(&fade, "m".into(), &mut LoadReport::new());
        let stepped: Vec<bool> = m.curves[0].keys.iter().map(|k| k.stepped).collect();
        // Segment 0->1 by its out tangent, 1->2 by the next key's in tangent,
        // and the last key has no segment leaving it.
        assert_eq!(stepped, [true, true, false, false]);
        assert!(m.curves[0].keys.iter().all(|k| k.in_slope.is_finite() && k.out_slope.is_finite()));
    }

    #[test]
    fn nan_tangents_and_weights_are_reported_once() {
        let mut fade = sample();
        let mut weighted = key(0.0, 0.0, f32::NAN, 0.0);
        weighted.weighted_mode = 3;
        fade.curves[0] = curve(vec![weighted, key(1.0, 1.0, 0.0, f32::NAN)]);
        let mut report = LoadReport::new();
        let m = to_motion(&fade, "m".into(), &mut report);
        assert_eq!(m.curves[0].keys[0].in_slope, 0.0);
        assert_eq!(m.curves[0].keys[1].out_slope, 0.0);
        let text = report.to_string();
        assert!(text.contains("weighted") && text.contains("NaN"), "{text}");
        assert_eq!(report.len(), 2);
    }

    #[test]
    fn curves_the_package_would_refuse_are_dropped_and_reported() {
        let mut fade = sample();
        fade.curves[0] = curve(vec![key(0.0, f32::NAN, 0.0, 0.0)]);
        fade.curves[1] = curve(vec![key(1.0, 0.0, 0.0, 0.0), key(0.5, 0.0, 0.0, 0.0)]);
        let mut report = LoadReport::new();
        let m = to_motion(&fade, "m".into(), &mut report);
        assert!(m.curves.is_empty());
        assert_eq!(report.len(), 2);
    }

    #[test]
    fn an_unusable_length_falls_back_to_the_last_key() {
        let mut fade = sample();
        fade.motion_length = f32::NAN;
        fade.fade_out = f32::INFINITY;
        let mut report = LoadReport::new();
        let m = to_motion(&fade, "m".into(), &mut report);
        assert_eq!(m.duration, 1.0);
        assert_eq!(m.fade_out, 1.0);
        assert_eq!(report.len(), 2, "the length and the non-finite fade are each reported");
        assert!(report.to_string().contains("non-finite fade-out"));
    }

    #[test]
    fn a_non_finite_curve_fade_is_reported_but_a_negative_one_is_not() {
        let mut fade = sample();
        fade.parameter_fade_in = vec![f32::NAN, -1.0];
        fade.parameter_fade_out = vec![-1.0, f32::INFINITY];
        let mut report = LoadReport::new();
        let m = to_motion(&fade, "m".into(), &mut report);
        assert_eq!((m.curves[0].fade_in, m.curves[0].fade_out), (None, None));
        assert_eq!((m.curves[1].fade_in, m.curves[1].fade_out), (None, None));
        assert_eq!(report.len(), 2, "{report}");
    }

    #[test]
    fn a_body_fitting_both_layouts_is_refused() {
        // Bytes that parse exactly under both layouts are impractical to
        // construct, so the choice is a pure function and its arms are tested.
        let ok = || Ok(sample());
        let err = || Err(DecodeError::corrupt("no"));
        assert!(matches!(choose_layout(ok(), ok()), Err(DecodeError::Reconstruction { .. })));
        assert!(choose_layout(ok(), err()).is_ok());
        assert!(choose_layout(err(), ok()).is_ok());
        assert!(matches!(choose_layout(err(), err()), Err(DecodeError::Reconstruction { .. })));
    }

    #[test]
    fn recovery_orders_renames_and_reports_and_the_result_packs() {
        use super::super::motions_from_bodies;
        let named = |path: &str| FadeMotion { motion_name: path.into(), ..sample() };
        let bytes_b = body(&named("x/Beta.motion3.json"), true);
        let bytes_a1 = body(&named("x/Alpha.motion3.json"), false);
        let bytes_a2 = body(&named("y/Alpha.motion3.json"), true);
        let junk = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        fn rd(bytes: &[u8]) -> Reader<'_> {
            Reader::new(bytes, Endian::Little)
        }
        let items = vec![
            ("beta".to_string(), Ok((rd(&bytes_b), "beta".to_string()))),
            ("junk".to_string(), Ok((rd(&junk), "junk".to_string()))),
            ("a2".to_string(), Ok((rd(&bytes_a2), "a2".to_string()))),
            ("a1".to_string(), Ok((rd(&bytes_a1), "a1".to_string()))),
        ];
        let mut report = LoadReport::new();
        let (motions, covered) = motions_from_bodies(items, &mut report);
        assert!(covered.contains("alpha") && covered.contains("beta") && !covered.contains("junk"));

        let names: Vec<&str> = motions.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Alpha_2", "Beta"]);
        let text = report.to_string();
        assert_eq!(text.matches("not recovered").count(), 1, "{text}");
        assert!(text.contains("\"junk\"") && text.contains("renamed"), "{text}");

        let ir = a2d_core::ir::cubism::CubismIr { motions, ..Default::default() };
        let package = a2d_pack::Package::from_cubism(ir, "m", "cubism-moc3-v3");
        assert!(package.encode_model().is_ok());
        assert_eq!(package.manifest.animations.len(), 3);
    }
}
