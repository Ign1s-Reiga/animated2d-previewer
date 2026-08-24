//! Keyframe curve reading — the single largest JSON dialect difference.
//!
//! * **Spine 3.x** writes one curve per keyframe, shared by every component,
//!   with control points already normalised into `0..1` on both axes:
//!   `"curve": 0.25, "c2": 0, "c3": 0.75, "c4": 1`.
//! * **Spine 4.x** writes one quad of control points *per component*, in
//!   absolute time/value coordinates: `"curve": [t1, v1, t2, v2, ...]`.
//!
//! The IR stores normalised control points, so 4.x values are divided through
//! by the span between the two keyframes they sit between. That conversion
//! needs the *next* keyframe, which is why curves are resolved in a second pass
//! rather than while each key is read.

use a2d_core::math::Bezier;
use a2d_core::{Interpolation, LoadReport};
use serde_json::Value;

use crate::detect::SpineFamily;
use crate::json::fields::{as_f32, Fields};

/// A curve as written, before it is resolved against neighbouring keyframes.
#[derive(Debug, Clone, PartialEq)]
pub enum RawCurve {
    Linear,
    Stepped,
    /// Spine 3.x: normalised control points shared by all components.
    Normalized(Bezier),
    /// Spine 4.x: absolute control points, one `[cx1, cy1, cx2, cy2]` per component.
    Absolute(Vec<[f32; 4]>),
}

/// Reads the `curve` key (and, for 3.x, its `c2`/`c3`/`c4` siblings).
///
/// `components` is how many independently-curved values the timeline carries:
/// 1 for rotate, 2 for translate, 4 for rgba, 7 for two-colour.
pub fn read_raw_curve(
    fields: &mut Fields<'_>,
    family: SpineFamily,
    components: usize,
    report: &mut LoadReport,
) -> RawCurve {
    // Read the 3.x siblings unconditionally so they never show up as unknown
    // keys; on 4.x assets they are simply absent.
    let c2 = fields.opt_f32("c2");
    let c3 = fields.opt_f32("c3");
    let c4 = fields.opt_f32("c4");

    let Some(curve) = fields.get("curve") else { return RawCurve::Linear };

    if let Some(s) = curve.as_str() {
        if s == "stepped" {
            return RawCurve::Stepped;
        }
        report.note(format!("{}: unknown curve type {s:?}, using linear", fields.context()));
        return RawCurve::Linear;
    }

    if let Some(arr) = curve.as_array() {
        return match family {
            SpineFamily::V4 => read_absolute(arr, components, fields.context(), report),
            // Spine 2.x wrote the four normalised control points as an array.
            SpineFamily::V2 | SpineFamily::V3 => match read_quad(arr, 0) {
                Some(q) => RawCurve::Normalized(Bezier::new(q[0], q[1], q[2], q[3])),
                None => {
                    report.note(format!("{}: malformed curve array, using linear", fields.context()));
                    RawCurve::Linear
                }
            },
        };
    }

    // Spine 3.x scalar form: `curve` is cx1 and the siblings supply the rest.
    match as_f32(curve) {
        Some(cx1) => RawCurve::Normalized(Bezier::new(cx1, c2.unwrap_or(0.0), c3.unwrap_or(1.0), c4.unwrap_or(1.0))),
        None => {
            report.note(format!("{}: uninterpretable curve value, using linear", fields.context()));
            RawCurve::Linear
        }
    }
}

fn read_absolute(arr: &[Value], components: usize, context: &str, report: &mut LoadReport) -> RawCurve {
    let expected = components * 4;
    if arr.len() < expected {
        report.note(format!("{context}: curve array has {} values, expected {expected}; using linear", arr.len()));
        return RawCurve::Linear;
    }
    let mut quads = Vec::with_capacity(components);
    for c in 0..components {
        match read_quad(arr, c * 4) {
            Some(q) => quads.push(q),
            None => {
                report.note(format!("{context}: curve array contains a non-numeric value, using linear"));
                return RawCurve::Linear;
            }
        }
    }
    RawCurve::Absolute(quads)
}

fn read_quad(arr: &[Value], at: usize) -> Option<[f32; 4]> {
    Some([as_f32(arr.get(at)?)?, as_f32(arr.get(at + 1)?)?, as_f32(arr.get(at + 2)?)?, as_f32(arr.get(at + 3)?)?])
}

/// Converts a raw curve into the IR's normalised form for one component.
///
/// `(t0, v0)` is this keyframe and `(t1, v1)` the next. When the two keyframes
/// share a time or a value there is nothing to ease between, so the result is
/// linear — dividing by that span would produce infinities.
pub fn resolve(raw: &RawCurve, component: usize, t0: f32, v0: f32, t1: f32, v1: f32) -> Interpolation {
    match raw {
        RawCurve::Linear => Interpolation::Linear,
        RawCurve::Stepped => Interpolation::Stepped,
        RawCurve::Normalized(b) => Interpolation::Bezier(*b),
        RawCurve::Absolute(quads) => {
            let Some(q) = quads.get(component) else { return Interpolation::Linear };
            let dt = t1 - t0;
            let dv = v1 - v0;
            if dt.abs() <= f32::EPSILON {
                return Interpolation::Linear;
            }
            if dv.abs() <= f32::EPSILON {
                // The value does not change across the span; easing is a no-op,
                // and normalising by `dv` would divide by zero.
                return Interpolation::Linear;
            }
            let bezier = Bezier::new((q[0] - t0) / dt, (q[1] - v0) / dv, (q[2] - t0) / dt, (q[3] - v0) / dv);
            if bezier_is_finite(&bezier) {
                Interpolation::Bezier(bezier)
            } else {
                Interpolation::Linear
            }
        }
    }
}

fn bezier_is_finite(b: &Bezier) -> bool {
    b.cx1.is_finite() && b.cy1.is_finite() && b.cx2.is_finite() && b.cy2.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(value: &Value, family: SpineFamily, components: usize) -> (RawCurve, LoadReport) {
        let mut report = LoadReport::new();
        let mut fields = Fields::new(value, "test");
        let raw = read_raw_curve(&mut fields, family, components, &mut report);
        (raw, report)
    }

    #[test]
    fn an_absent_curve_is_linear() {
        let (raw, report) = read(&json!({"time": 0.0}), SpineFamily::V3, 1);
        assert_eq!(raw, RawCurve::Linear);
        assert!(report.is_empty());
    }

    #[test]
    fn stepped_is_recognised_in_both_families() {
        for family in [SpineFamily::V3, SpineFamily::V4] {
            let (raw, _) = read(&json!({"curve": "stepped"}), family, 1);
            assert_eq!(raw, RawCurve::Stepped);
        }
    }

    #[test]
    fn v3_scalar_curve_uses_the_sibling_control_points() {
        let (raw, report) = read(&json!({"curve": 0.25, "c2": 0.1, "c3": 0.75, "c4": 0.9}), SpineFamily::V3, 1);
        assert_eq!(raw, RawCurve::Normalized(Bezier::new(0.25, 0.1, 0.75, 0.9)));
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn v3_scalar_curve_defaults_missing_siblings_the_way_spine_does() {
        let (raw, _) = read(&json!({"curve": 0.25}), SpineFamily::V3, 1);
        assert_eq!(raw, RawCurve::Normalized(Bezier::new(0.25, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn v2_array_curve_is_read_as_normalised() {
        let (raw, _) = read(&json!({"curve": [0.2, 0.3, 0.4, 0.5]}), SpineFamily::V2, 1);
        assert_eq!(raw, RawCurve::Normalized(Bezier::new(0.2, 0.3, 0.4, 0.5)));
    }

    #[test]
    fn v4_array_curve_is_read_per_component() {
        let value = json!({"curve": [0.1, 1.0, 0.2, 2.0, 0.3, 3.0, 0.4, 4.0]});
        let (raw, report) = read(&value, SpineFamily::V4, 2);
        assert_eq!(raw, RawCurve::Absolute(vec![[0.1, 1.0, 0.2, 2.0], [0.3, 3.0, 0.4, 4.0]]));
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_short_v4_curve_array_reports_and_falls_back() {
        let (raw, report) = read(&json!({"curve": [0.1, 1.0, 0.2, 2.0]}), SpineFamily::V4, 2);
        assert_eq!(raw, RawCurve::Linear);
        assert!(report.to_string().contains("expected 8"), "{report}");
    }

    #[test]
    fn an_unknown_curve_string_reports_and_falls_back() {
        let (raw, report) = read(&json!({"curve": "bouncy"}), SpineFamily::V4, 1);
        assert_eq!(raw, RawCurve::Linear);
        assert!(report.to_string().contains("bouncy"), "{report}");
    }

    #[test]
    fn v3_sibling_keys_are_never_reported_as_unknown() {
        let value = json!({"time": 0.0, "angle": 10.0, "curve": 0.25, "c2": 0.0, "c3": 1.0, "c4": 1.0});
        let mut report = LoadReport::new();
        let mut fields = Fields::new(&value, "test");
        fields.f32("time", 0.0);
        fields.f32("angle", 0.0);
        read_raw_curve(&mut fields, SpineFamily::V3, 1, &mut report);
        fields.finish(&mut report);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn resolving_linear_and_stepped_ignores_the_neighbours() {
        assert_eq!(resolve(&RawCurve::Linear, 0, 0.0, 0.0, 1.0, 1.0), Interpolation::Linear);
        assert_eq!(resolve(&RawCurve::Stepped, 0, 0.0, 0.0, 1.0, 1.0), Interpolation::Stepped);
    }

    #[test]
    fn v3_curves_pass_through_unchanged() {
        let b = Bezier::new(0.25, 0.1, 0.75, 0.9);
        assert_eq!(resolve(&RawCurve::Normalized(b), 0, 3.0, 50.0, 9.0, 90.0), Interpolation::Bezier(b));
    }

    #[test]
    fn v4_absolute_curves_are_normalised_against_the_keyframe_span() {
        // Key at (t=1, v=10), next at (t=3, v=30). Control points at
        // (1.5, 15) and (2.5, 25) are a quarter and three quarters along both axes.
        let raw = RawCurve::Absolute(vec![[1.5, 15.0, 2.5, 25.0]]);
        match resolve(&raw, 0, 1.0, 10.0, 3.0, 30.0) {
            Interpolation::Bezier(b) => {
                assert!((b.cx1 - 0.25).abs() < 1e-5, "{b:?}");
                assert!((b.cy1 - 0.25).abs() < 1e-5, "{b:?}");
                assert!((b.cx2 - 0.75).abs() < 1e-5, "{b:?}");
                assert!((b.cy2 - 0.75).abs() < 1e-5, "{b:?}");
            }
            other => panic!("expected a bezier, got {other:?}"),
        }
    }

    #[test]
    fn v4_curves_pick_the_quad_for_their_component() {
        let raw = RawCurve::Absolute(vec![[0.0, 0.0, 1.0, 1.0], [0.5, 5.0, 0.5, 5.0]]);
        let a = resolve(&raw, 0, 0.0, 0.0, 1.0, 10.0);
        let b = resolve(&raw, 1, 0.0, 0.0, 1.0, 10.0);
        assert_ne!(a, b);
    }

    #[test]
    fn a_component_beyond_the_curve_array_falls_back_to_linear() {
        let raw = RawCurve::Absolute(vec![[0.0, 0.0, 1.0, 1.0]]);
        assert_eq!(resolve(&raw, 5, 0.0, 0.0, 1.0, 10.0), Interpolation::Linear);
    }

    #[test]
    fn a_zero_length_time_span_resolves_to_linear_rather_than_infinity() {
        let raw = RawCurve::Absolute(vec![[0.0, 0.0, 1.0, 1.0]]);
        assert_eq!(resolve(&raw, 0, 2.0, 0.0, 2.0, 10.0), Interpolation::Linear);
    }

    #[test]
    fn an_unchanging_value_resolves_to_linear_rather_than_infinity() {
        let raw = RawCurve::Absolute(vec![[0.25, 7.0, 0.75, 7.0]]);
        assert_eq!(resolve(&raw, 0, 0.0, 7.0, 1.0, 7.0), Interpolation::Linear);
    }
}
