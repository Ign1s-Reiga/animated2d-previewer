//! Path-constrained bones of a real Spine rig end on their path.
//!
//! A path constraint places each bone on a curve; a later constraint must not
//! undo that for bones it has no business touching. The runtime used to
//! recompute every bone with a higher index after each constraint, which dragged
//! earlier-placed chains off their curve (a path-driven rod folded back on
//! itself). This holds the finished pose of a real rig against the invariant
//! that cannot be satisfied by accident: with a full translation mix, a bone of
//! a path constraint lies on the path the constraint follows, or on the
//! straight continuation of its first or last tangent where the constraint ran
//! off an end.
//!
//! The invariant is weaker than "the exact position the constraint computed"
//! (that is private to the runtime), and it is also not guaranteed when a *later*
//! constraint legitimately moves an ancestor of the chain, the bone carrying the
//! path slot, or the bones a weighted path is skinned to, because the reference
//! runtime recomputes the chain or the path from the moved bones then. A failure here is
//! therefore reported with the constraint and bone so it can be told apart from
//! that case by reading the rig's constraint order.
//!
//! Gated on `A2D_FIXTURE_SPINE_BUNDLE`: extracted assets are never committed.

use std::sync::Arc;

use a2d_core::ir::spine::{AttachmentKind, VertexData};
use a2d_core::{LoadReport, Vec2};
use a2d_runtime::GenericSpineModel;

/// `None` only when the variable is unset; with it set, any failure to load is
/// a test failure rather than a silent pass.
fn load() -> Option<GenericSpineModel> {
    let path = std::env::var("A2D_FIXTURE_SPINE_BUNDLE").ok()?;
    let bytes = std::fs::read(&path).expect("the fixture bundle should be readable");
    let mut report = LoadReport::new();
    let rig = a2d_import::discover_single_rig(&bytes, &mut report).expect("the bundle should hold one rig");
    let ir = a2d_import::decode_rig(&rig, &mut report).expect("the rig should decode");
    Some(GenericSpineModel::load(Arc::new(ir), "rig"))
}

/// Cubic segments of an authored path, in the layout the runtime reads:
/// anchors at vertices 1, 4, 7, ... and, when closed, a final segment that
/// wraps round to the start.
fn curves(world: &[Vec2], closed: bool) -> Vec<[Vec2; 4]> {
    let mut out = Vec::new();
    let n = world.len();
    if n < 3 {
        return out;
    }
    let mut i = 1;
    while i + 3 < n {
        out.push([world[i], world[i + 1], world[i + 2], world[i + 3]]);
        i += 3;
    }
    if closed {
        out.push([world[n - 2], world[n - 1], world[0], world[1]]);
    }
    out
}

fn bezier(c: &[Vec2; 4], t: f32) -> Vec2 {
    let u = 1.0 - t;
    c[0] * (u * u * u) + c[1] * (3.0 * u * u * t) + c[2] * (3.0 * u * t * t) + c[3] * (t * t * t)
}

fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.x * ab.x + ab.y * ab.y;
    let t = if len2 > 0.0 { (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let q = a + ab * t;
    ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt()
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE_BUNDLE to a Unity bundle"]
fn path_constrained_bones_end_on_their_path() {
    let Some(mut model) = load() else { return };
    let ir = Arc::clone(model.ir());
    if ir.path_constraints.is_empty() {
        eprintln!("this rig has no path constraint; nothing to check");
        return;
    }

    let mut checked = 0usize;
    let mut skipped_mix = 0usize;
    let mut failures = Vec::new();
    for animation in &ir.animations {
        for step in 0..8 {
            let time = animation.duration * step as f32 / 8.0;
            model.pose_at(&animation.name, time).expect("known animation");
            let pose = model.pose();
            for (ci, constraint) in ir.path_constraints.iter().enumerate() {
                let Some(state) = pose.path.get(ci) else { continue };
                if state.mix_x != 1.0 || state.mix_y != 1.0 {
                    skipped_mix += 1;
                    continue;
                }
                let slot = constraint.target_slot.index();
                let Some(attachment) = pose.slots[slot].attachment.and_then(|id| ir.attachment(id)) else { continue };
                let AttachmentKind::Path(path) = &attachment.kind else { continue };
                if matches!(path.vertices, VertexData::Rigid(ref v) if v.len() < 3) {
                    continue;
                }
                let slot_bone = pose.bones[ir.slots[slot].bone.index()].world;
                let world = pose.world_vertices(&path.vertices, &pose.slots[slot].deform, slot_bone);
                let segments = curves(&world, path.closed);
                if segments.is_empty() {
                    continue;
                }

                // A dense polyline of the path, plus long straight extensions
                // at both ends for the "ran off the end" case.
                let mut line: Vec<Vec2> = Vec::new();
                for c in &segments {
                    for k in 0..=200 {
                        line.push(bezier(c, k as f32 / 200.0));
                    }
                }
                let total: f32 = line.windows(2).map(|w| (w[1] - w[0]).length()).sum();
                let mut lines = vec![line.clone()];
                if !path.closed && line.len() >= 2 {
                    let (s0, s1) = (line[0], line[1]);
                    let (e0, e1) = (line[line.len() - 2], line[line.len() - 1]);
                    let d0 = (s0 - s1).normalize_or_zero();
                    let d1 = (e1 - e0).normalize_or_zero();
                    lines.push(vec![s0, s0 + d0 * 1.0e5]);
                    lines.push(vec![e1, e1 + d1 * 1.0e5]);
                }
                let tolerance = (total * 0.004).max(0.5);

                for bone in &constraint.bones {
                    let p = pose.bones[bone.index()].world.translation();
                    let nearest = lines
                        .iter()
                        .flat_map(|l| l.windows(2).map(|w| distance_to_segment(p, w[0], w[1])))
                        .fold(f32::INFINITY, f32::min);
                    checked += 1;
                    if nearest > tolerance {
                        failures.push(format!(
                            "{:?} t={time:.3} constraint {:?} bone {:?}: {nearest:.2} off a {total:.1}-long path (tolerance {tolerance:.2})",
                            animation.name, constraint.name, ir.bones[bone.index()].name
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "{checked} bone placements checked on {} path constraint(s); {skipped_mix} skipped for a partial mix",
        ir.path_constraints.len()
    );
    assert!(failures.is_empty(), "{} placement(s) off their path:\n{}", failures.len(), failures.join("\n"));
}
