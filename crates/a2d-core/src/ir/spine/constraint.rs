//! Constraints.
//!
//! Every constraint carries the authored `order`. Spec §6.5 lists IK, transform
//! and path in *implementation priority* order; evaluation order is whatever the
//! rig author chose, and mixing the two up produces subtly wrong poses.

use crate::ir::ids::BoneId;

/// Constraint kinds, for reporting and for the sorted update list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstraintKind {
    Ik,
    Transform,
    Path,
}

impl ConstraintKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConstraintKind::Ik => "ik",
            ConstraintKind::Transform => "transform",
            ConstraintKind::Path => "path",
        }
    }
}

/// Inverse kinematics over one or two bones.
#[derive(Debug, Clone, PartialEq)]
pub struct IkConstraint {
    pub name: String,
    /// Authored evaluation order, shared across all three constraint kinds.
    pub order: u32,
    pub skin_required: bool,
    /// One bone (single-bone IK) or two (two-bone IK). Longer chains are not
    /// something Spine authors, and are reported if encountered.
    pub bones: Vec<BoneId>,
    pub target: BoneId,
    pub mix: f32,
    /// Radians of slack before the chain straightens, for two-bone IK.
    pub softness: f32,
    /// `true` bends the chain one way, `false` the other.
    pub bend_positive: bool,
    /// Shorten the bones when the target is closer than the chain length.
    pub compress: bool,
    /// Lengthen the bones when the target is further than the chain length.
    pub stretch: bool,
    /// Apply stretch to both axes rather than along the bone only.
    pub uniform: bool,
}

/// Copies a target bone's transform onto other bones, with per-channel mixing.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformConstraint {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bones: Vec<BoneId>,
    pub target: BoneId,
    /// Offsets added to the copied transform, in the constraint's space.
    pub offset_rotation: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_scale_x: f32,
    pub offset_scale_y: f32,
    pub offset_shear_y: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub mix_scale_x: f32,
    pub mix_scale_y: f32,
    pub mix_shear_y: f32,
    /// Offsets are relative to the constrained bone's own setup transform.
    pub relative: bool,
    /// Operate on local transforms rather than world transforms.
    pub local: bool,
}

/// Where along a path a constrained bone is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathPositionMode {
    #[default]
    Fixed,
    Percent,
}

/// How spacing between constrained bones is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathSpacingMode {
    #[default]
    Length,
    Fixed,
    Percent,
    Proportional,
}

/// How a constrained bone is rotated to follow the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathRotateMode {
    #[default]
    Tangent,
    Chain,
    ChainScale,
}

/// Binds bones along a path attachment.
#[derive(Debug, Clone, PartialEq)]
pub struct PathConstraint {
    pub name: String,
    pub order: u32,
    pub skin_required: bool,
    pub bones: Vec<BoneId>,
    /// Slot holding the path attachment this constraint follows.
    pub target_slot: crate::ir::ids::SlotId,
    pub position_mode: PathPositionMode,
    pub spacing_mode: PathSpacingMode,
    pub rotate_mode: PathRotateMode,
    pub offset_rotation: f32,
    pub position: f32,
    pub spacing: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
}

/// One entry in the merged, order-sorted constraint update list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstraintSlot {
    pub kind: ConstraintKind,
    /// Index into the arena for `kind`.
    pub index: u16,
    pub order: u32,
}

/// Builds the evaluation order for all constraints.
///
/// Sorting is by authored `order`, then by kind, then by index — the last two
/// only to make the result deterministic when a source file reuses an order
/// value, which real exports do.
pub fn build_update_order(
    ik: &[IkConstraint],
    transform: &[TransformConstraint],
    path: &[PathConstraint],
) -> Vec<ConstraintSlot> {
    let mut out = Vec::with_capacity(ik.len() + transform.len() + path.len());
    for (i, c) in ik.iter().enumerate() {
        out.push(ConstraintSlot { kind: ConstraintKind::Ik, index: i as u16, order: c.order });
    }
    for (i, c) in transform.iter().enumerate() {
        out.push(ConstraintSlot { kind: ConstraintKind::Transform, index: i as u16, order: c.order });
    }
    for (i, c) in path.iter().enumerate() {
        out.push(ConstraintSlot { kind: ConstraintKind::Path, index: i as u16, order: c.order });
    }
    out.sort_by(|a, b| a.order.cmp(&b.order).then(a.kind.cmp(&b.kind)).then(a.index.cmp(&b.index)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ids::SlotId;

    fn ik(name: &str, order: u32) -> IkConstraint {
        IkConstraint {
            name: name.into(),
            order,
            skin_required: false,
            bones: vec![BoneId(0)],
            target: BoneId(1),
            mix: 1.0,
            softness: 0.0,
            bend_positive: true,
            compress: false,
            stretch: false,
            uniform: false,
        }
    }

    fn transform(name: &str, order: u32) -> TransformConstraint {
        TransformConstraint {
            name: name.into(),
            order,
            skin_required: false,
            bones: vec![BoneId(0)],
            target: BoneId(1),
            offset_rotation: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_scale_x: 0.0,
            offset_scale_y: 0.0,
            offset_shear_y: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
            mix_scale_x: 1.0,
            mix_scale_y: 1.0,
            mix_shear_y: 1.0,
            relative: false,
            local: false,
        }
    }

    fn path(name: &str, order: u32) -> PathConstraint {
        PathConstraint {
            name: name.into(),
            order,
            skin_required: false,
            bones: vec![BoneId(0)],
            target_slot: SlotId(0),
            position_mode: PathPositionMode::default(),
            spacing_mode: PathSpacingMode::default(),
            rotate_mode: PathRotateMode::default(),
            offset_rotation: 0.0,
            position: 0.0,
            spacing: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
        }
    }

    #[test]
    fn update_order_follows_the_authored_order_not_the_kind() {
        // A transform constraint authored before an IK constraint must run first.
        let order = build_update_order(&[ik("a", 5)], &[transform("b", 1)], &[]);
        assert_eq!(order[0].kind, ConstraintKind::Transform);
        assert_eq!(order[1].kind, ConstraintKind::Ik);
    }

    #[test]
    fn ties_break_deterministically_by_kind_then_index() {
        let order = build_update_order(&[ik("i0", 0), ik("i1", 0)], &[transform("t0", 0)], &[path("p0", 0)]);
        let seen: Vec<_> = order.iter().map(|s| (s.kind, s.index)).collect();
        assert_eq!(
            seen,
            vec![
                (ConstraintKind::Ik, 0),
                (ConstraintKind::Ik, 1),
                (ConstraintKind::Transform, 0),
                (ConstraintKind::Path, 0),
            ]
        );
    }

    #[test]
    fn every_constraint_appears_exactly_once() {
        let order = build_update_order(&[ik("a", 3), ik("b", 1)], &[transform("c", 2)], &[path("d", 0)]);
        assert_eq!(order.len(), 4);
        let orders: Vec<_> = order.iter().map(|s| s.order).collect();
        assert_eq!(orders, vec![0, 1, 2, 3]);
    }

    #[test]
    fn no_constraints_yields_an_empty_order() {
        assert!(build_update_order(&[], &[], &[]).is_empty());
    }
}
