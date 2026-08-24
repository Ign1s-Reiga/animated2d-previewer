//! The Generic Spine IR (spec §6).
//!
//! Source-version independent by construction. Version decoders in `a2d-spine`
//! translate *up* into this shape; nothing downstream may ask which Spine
//! version produced it.
//!
//! # Naming
//!
//! The spec calls both the normalised data and the playable object
//! `GenericSpineModel`. They are split here, the way Spine itself splits
//! `SkeletonData` from `Skeleton`: [`SpineIr`] is the immutable decoded data,
//! and `a2d_runtime::GenericSpineModel` is the `AnimatedModel` implementation
//! that holds a shared reference to one and owns the mutable pose. One `SpineIr`
//! can back several on-screen instances.

pub mod animation;
pub mod attachment;
pub mod constraint;
pub mod skeleton;

pub use animation::{
    search_keys, Animation, AttachmentKey, Axes, ColorChannels, ColorKey, DeformKey, DrawOrderKey, EventData, EventKey,
    IkKey, PathMixKey, ScalarKey, Timeline, TransformKey, TwoColorKey, Vec2Key,
};
pub use attachment::{
    Attachment, AttachmentKind, BoundingBoxAttachment, ClippingAttachment, LinkedMesh, MeshAttachment, PathAttachment,
    PointAttachment, RegionAttachment, Sequence, VertexData, VertexInfluence, WeightedVertices,
};
pub use constraint::{
    build_update_order, ConstraintKind, ConstraintSlot, IkConstraint, PathConstraint, PathPositionMode, PathRotateMode,
    PathSpacingMode, TransformConstraint,
};
pub use skeleton::{Bone, BoneLocal, SkeletonMetadata, Skin, SkinEntry, Slot, TransformInherit, DEFAULT_SKIN};

use crate::ir::atlas::Atlas;
use crate::ir::ids::{AttachmentId, BoneId, SkinId, SlotId};

/// A fully normalised Spine skeleton.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpineIr {
    pub metadata: SkeletonMetadata,
    /// Parent-before-child, so world transforms need a single forward pass.
    pub bones: Vec<Bone>,
    /// In setup-pose draw order; index 0 draws first.
    pub slots: Vec<Slot>,
    /// Index 0 is always the default skin, even when the source did not name one.
    pub skins: Vec<Skin>,
    /// Flat arena. Skins reference entries by [`AttachmentId`], which lets two
    /// skins share one attachment and gives deform timelines a stable target.
    pub attachments: Vec<Attachment>,
    pub ik_constraints: Vec<IkConstraint>,
    pub transform_constraints: Vec<TransformConstraint>,
    pub path_constraints: Vec<PathConstraint>,
    /// Sorted by name, so lookup is a binary search.
    pub animations: Vec<Animation>,
    pub events: Vec<EventData>,
    /// Merged constraint evaluation order, built by [`SpineIr::rebuild_derived`].
    pub constraint_order: Vec<ConstraintSlot>,
    pub atlas: Atlas,
}

impl SpineIr {
    pub fn bone(&self, id: BoneId) -> Option<&Bone> {
        self.bones.get(id.index())
    }

    pub fn slot(&self, id: SlotId) -> Option<&Slot> {
        self.slots.get(id.index())
    }

    pub fn skin(&self, id: SkinId) -> Option<&Skin> {
        self.skins.get(id.index())
    }

    pub fn attachment(&self, id: AttachmentId) -> Option<&Attachment> {
        self.attachments.get(id.index())
    }

    pub fn bone_by_name(&self, name: &str) -> Option<BoneId> {
        self.bones.iter().position(|b| b.name == name).and_then(BoneId::from_index)
    }

    pub fn slot_by_name(&self, name: &str) -> Option<SlotId> {
        self.slots.iter().position(|s| s.name == name).and_then(SlotId::from_index)
    }

    pub fn skin_by_name(&self, name: &str) -> Option<SkinId> {
        self.skins.iter().position(|s| s.name == name).and_then(SkinId::from_index)
    }

    pub fn animation_by_name(&self, name: &str) -> Option<&Animation> {
        self.animations.binary_search_by(|a| a.name.as_str().cmp(name)).ok().map(|i| &self.animations[i])
    }

    /// Resolves a placeholder name for a slot, checking the active skin first
    /// and falling back to the default skin, which is what Spine does.
    pub fn resolve_attachment(&self, skin: SkinId, slot: SlotId, name: &str) -> Option<AttachmentId> {
        if let Some(s) = self.skin(skin) {
            if let Some(id) = s.find(slot, name) {
                return Some(id);
            }
        }
        if skin != DEFAULT_SKIN {
            if let Some(s) = self.skin(DEFAULT_SKIN) {
                return s.find(slot, name);
            }
        }
        None
    }

    /// Restores every sorted-order invariant and rebuilds the constraint order.
    ///
    /// Decoders call this once, at the end of normalisation. Everything that
    /// binary-searches depends on it.
    pub fn rebuild_derived(&mut self) {
        for skin in &mut self.skins {
            skin.sort_entries();
        }
        self.animations.sort_by(|a, b| a.name.cmp(&b.name));
        self.atlas.sort_regions();
        self.constraint_order =
            build_update_order(&self.ik_constraints, &self.transform_constraints, &self.path_constraints);
    }

    /// Total attachment count across all skins, counting shared attachments once.
    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ids::AttachmentId;
    use crate::math::Vec2;

    fn ir() -> SpineIr {
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None), Bone::new("torso", Some(BoneId(0)))],
            slots: vec![Slot::new("body", BoneId(1)), Slot::new("head", BoneId(1))],
            skins: vec![Skin::new("default"), Skin::new("blue")],
            attachments: vec![
                Attachment {
                    name: "body".into(),
                    kind: AttachmentKind::Point(PointAttachment {
                        position: Vec2::ZERO,
                        rotation: 0.0,
                        color: crate::math::Rgba::WHITE,
                    }),
                },
                Attachment {
                    name: "body-blue".into(),
                    kind: AttachmentKind::Point(PointAttachment {
                        position: Vec2::ZERO,
                        rotation: 0.0,
                        color: crate::math::Rgba::WHITE,
                    }),
                },
            ],
            animations: vec![Animation::new("walk"), Animation::new("idle")],
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(0) });
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(1), name: "head".into(), attachment: AttachmentId(0) });
        ir.skins[1].entries.push(SkinEntry { slot: SlotId(0), name: "body".into(), attachment: AttachmentId(1) });
        ir.rebuild_derived();
        ir
    }

    #[test]
    fn name_lookups_resolve_to_handles() {
        let ir = ir();
        assert_eq!(ir.bone_by_name("torso"), Some(BoneId(1)));
        assert_eq!(ir.slot_by_name("head"), Some(SlotId(1)));
        assert_eq!(ir.skin_by_name("blue"), Some(SkinId(1)));
        assert_eq!(ir.bone_by_name("nope"), None);
    }

    #[test]
    fn animation_lookup_works_after_the_sort() {
        let ir = ir();
        assert_eq!(ir.animations[0].name, "idle");
        assert!(ir.animation_by_name("walk").is_some());
        assert!(ir.animation_by_name("idle").is_some());
        assert!(ir.animation_by_name("run").is_none());
    }

    #[test]
    fn active_skin_shadows_the_default_skin() {
        let ir = ir();
        assert_eq!(ir.resolve_attachment(SkinId(1), SlotId(0), "body"), Some(AttachmentId(1)));
        assert_eq!(ir.resolve_attachment(DEFAULT_SKIN, SlotId(0), "body"), Some(AttachmentId(0)));
    }

    #[test]
    fn unmatched_placeholder_falls_back_to_the_default_skin() {
        let ir = ir();
        // "head" exists only in the default skin.
        assert_eq!(ir.resolve_attachment(SkinId(1), SlotId(1), "head"), Some(AttachmentId(0)));
    }

    #[test]
    fn a_placeholder_in_neither_skin_resolves_to_nothing() {
        let ir = ir();
        assert_eq!(ir.resolve_attachment(SkinId(1), SlotId(1), "hat"), None);
    }

    #[test]
    fn resolving_against_a_nonexistent_skin_still_checks_the_default() {
        let ir = ir();
        assert_eq!(ir.resolve_attachment(SkinId(99), SlotId(0), "body"), Some(AttachmentId(0)));
    }

    #[test]
    fn rebuild_derived_is_idempotent() {
        let mut a = ir();
        let before = a.clone();
        a.rebuild_derived();
        assert_eq!(a, before);
    }
}
