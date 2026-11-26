//! A 3.x rotate timeline whose keys straddle the 360 degree seam must play as a
//! short swing, not a near-full spin. The decoder unwraps the keys; the runtime
//! interpolates them directly and knows nothing about the source version.

use std::sync::Arc;

use a2d_core::ir::atlas::Atlas;
use a2d_core::LoadReport;
use a2d_runtime::GenericSpineModel;

fn rotation_at(skeleton_version: &str, angle_key: &str, time: f32) -> f32 {
    let json = format!(
        r#"{{
  "skeleton": {{ "spine": "{skeleton_version}" }},
  "bones": [ {{ "name": "root" }}, {{ "name": "swing", "parent": "root" }} ],
  "animations": {{ "idle": {{ "bones": {{ "swing": {{ "rotate": [
    {{ "time": 0, "{angle_key}": 10 }}, {{ "time": 1, "{angle_key}": 350 }}
  ] }} }} }} }}
}}"#
    );
    let mut report = LoadReport::new();
    let (ir, _) = a2d_spine::decode_skeleton(json.as_bytes(), Atlas::default(), &mut report).expect("should decode");
    let mut model = GenericSpineModel::load(Arc::new(ir), "rig");
    model.pose_at("idle", time).expect("known animation");
    model.pose().bones[1].local.rotation
}

#[test]
fn a_3x_swing_across_the_seam_passes_through_zero_not_180() {
    let mid = rotation_at("3.8.99", "angle", 0.5);
    assert!(mid.abs() < 1e-3, "midpoint should be about 0 degrees, got {mid}");
    let end = rotation_at("3.8.99", "angle", 1.0);
    assert!((end - -10.0).abs() < 1e-3, "{end}");
}

#[test]
fn a_4x_rotate_timeline_still_takes_the_long_way() {
    let mid = rotation_at("4.1.00", "value", 0.5);
    assert!((mid - 180.0).abs() < 1e-3, "4.x uses key values as written, got {mid}");
}

#[test]
fn a_3x_bezier_span_across_the_seam_applies_its_curve_to_the_short_arc() {
    // Normalised control points (0.25, 0, 0.75, 1): the curve is symmetric, so
    // its midpoint is 0.5 of the way along the span. The short way from 10 to
    // 350 is -20 degrees, so the midpoint must be 0; the long way would give 180.
    let json = r#"{
  "skeleton": { "spine": "3.8.99" },
  "bones": [ { "name": "root" }, { "name": "swing", "parent": "root" } ],
  "animations": { "idle": { "bones": { "swing": { "rotate": [
    { "time": 0, "angle": 10, "curve": [0.25, 0, 0.75, 1] }, { "time": 1, "angle": 350 }
  ] } } } }
}"#;
    let mut report = LoadReport::new();
    let (ir, _) = a2d_spine::decode_skeleton(json.as_bytes(), Atlas::default(), &mut report).expect("should decode");
    let mut model = GenericSpineModel::load(Arc::new(ir), "rig");
    model.pose_at("idle", 0.5).expect("known animation");
    let mid = model.pose().bones[1].local.rotation;
    assert!(mid.abs() < 1e-3, "got {mid}");
    // A quarter of the time is well under a quarter of the way along this ease.
    model.pose_at("idle", 0.25).expect("known animation");
    let early = model.pose().bones[1].local.rotation;
    assert!(early < 10.0 && early > 0.0, "got {early}");
}
