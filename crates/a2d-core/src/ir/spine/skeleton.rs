//! Skeleton structure: metadata, bones, slots, skins.

use crate::ir::ids::{AttachmentId, BoneId, SkinId, SlotId};
use crate::math::{Rgb, Rgba, Vec2};
use crate::render::BlendMode;

/// Skeleton-level information carried through from the source file.
///
/// `source_version` is **informational only**. Nothing downstream of
/// `formats/` may branch on it; if you find yourself wanting to, the decoder is
/// the layer that should have handled the difference (spec §7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkeletonMetadata {
    /// Skeleton name, when the source recorded one.
    pub name: Option<String>,
    /// Exporter version string as written by the source, e.g. `3.8.99`.
    pub source_version: String,
    /// Export hash, used to tell two exports of the same rig apart.
    pub hash: Option<String>,
    /// Setup-pose bounding box origin.
    pub origin: Vec2,
    /// Setup-pose bounding box size.
    pub size: Vec2,
    /// Authoring frame rate, when recorded. Playback is delta-time based and
    /// does not use this; it is here for display and for AnimationClip import.
    pub fps: Option<f32>,
    /// Path the exporter recorded for the images folder.
    pub images_path: Option<String>,
    pub audio_path: Option<String>,
}

/// How a bone inherits its parent's world transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformInherit {
    /// Inherit rotation, scale and reflection.
    #[default]
    Normal,
    /// Inherit only the parent's translation.
    OnlyTranslation,
    /// Inherit translation and scale, but not rotation or reflection.
    NoRotationOrReflection,
    /// Inherit translation and rotation, but not scale.
    NoScale,
    /// Inherit translation and rotation, but neither scale nor reflection.
    NoScaleOrReflection,
}

impl TransformInherit {
    pub fn as_str(self) -> &'static str {
        match self {
            TransformInherit::Normal => "normal",
            TransformInherit::OnlyTranslation => "onlyTranslation",
            TransformInherit::NoRotationOrReflection => "noRotationOrReflection",
            TransformInherit::NoScale => "noScale",
            TransformInherit::NoScaleOrReflection => "noScaleOrReflection",
        }
    }

    /// Parses the spelling used by Spine JSON. Both the 3.x `transform` and the
    /// 4.2 `inherit` keys use these names.
    pub fn parse(s: &str) -> Option<TransformInherit> {
        Some(match s {
            "normal" => TransformInherit::Normal,
            "onlyTranslation" => TransformInherit::OnlyTranslation,
            "noRotationOrReflection" => TransformInherit::NoRotationOrReflection,
            "noScale" => TransformInherit::NoScale,
            "noScaleOrReflection" => TransformInherit::NoScaleOrReflection,
            _ => return None,
        })
    }

    /// Binary encodings store the enum by ordinal, in this order.
    pub fn from_ordinal(n: u32) -> Option<TransformInherit> {
        Some(match n {
            0 => TransformInherit::Normal,
            1 => TransformInherit::OnlyTranslation,
            2 => TransformInherit::NoRotationOrReflection,
            3 => TransformInherit::NoScale,
            4 => TransformInherit::NoScaleOrReflection,
            _ => return None,
        })
    }
}

/// A bone's local transform in the setup pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneLocal {
    pub position: Vec2,
    /// Degrees, counter-clockwise.
    pub rotation: f32,
    pub scale: Vec2,
    /// Shear in degrees, per axis.
    pub shear: Vec2,
}

impl Default for BoneLocal {
    fn default() -> Self {
        BoneLocal { position: Vec2::ZERO, rotation: 0.0, scale: Vec2::ONE, shear: Vec2::ZERO }
    }
}

/// One bone in the hierarchy.
///
/// Bones are stored parent-before-child, so a single forward pass computes
/// world transforms. Decoders are responsible for establishing that order.
#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    pub name: String,
    /// `None` only for the root.
    pub parent: Option<BoneId>,
    /// Bone length, used by IK and by path constraints.
    pub length: f32,
    pub setup: BoneLocal,
    pub inherit: TransformInherit,
    /// When true the bone is only updated if the active skin requires it.
    pub skin_required: bool,
}

impl Bone {
    pub fn new(name: impl Into<String>, parent: Option<BoneId>) -> Self {
        Bone {
            name: name.into(),
            parent,
            length: 0.0,
            setup: BoneLocal::default(),
            inherit: TransformInherit::Normal,
            skin_required: false,
        }
    }
}

/// A slot: the attachment site that a bone drives.
///
/// Slot order in [`SpineIr::slots`](super::SpineIr::slots) *is* the setup-pose
/// draw order; index 0 draws first.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub name: String,
    pub bone: BoneId,
    pub color: Rgba,
    /// Second tint colour for Spine's two-colour tinting. `None` means the slot
    /// uses ordinary single-colour tinting.
    pub dark_color: Option<Rgb>,
    /// Attachment placeholder name active in the setup pose.
    pub setup_attachment: Option<String>,
    pub blend_mode: BlendMode,
}

impl Slot {
    pub fn new(name: impl Into<String>, bone: BoneId) -> Self {
        Slot {
            name: name.into(),
            bone,
            color: Rgba::WHITE,
            dark_color: None,
            setup_attachment: None,
            blend_mode: BlendMode::Normal,
        }
    }
}

/// One `(slot, placeholder name) -> attachment` binding inside a skin.
#[derive(Debug, Clone, PartialEq)]
pub struct SkinEntry {
    pub slot: SlotId,
    /// The placeholder name, which is what timelines and the setup pose refer
    /// to. It is often, but not always, equal to the attachment's own name.
    pub name: String,
    pub attachment: AttachmentId,
}

/// A named set of attachment bindings.
///
/// Every skeleton has a default skin at [`SkinId`] 0, even when the source did
/// not name one, so lookup never has a special case.
#[derive(Debug, Clone, PartialEq)]
pub struct Skin {
    pub name: String,
    /// Sorted by `(slot, name)`, so lookup is a binary search and serialization
    /// is deterministic.
    pub entries: Vec<SkinEntry>,
    /// Bones this skin needs updated, for `skin_required` bones.
    pub bones: Vec<BoneId>,
}

impl Skin {
    pub fn new(name: impl Into<String>) -> Self {
        Skin { name: name.into(), entries: Vec::new(), bones: Vec::new() }
    }

    /// Restores the sorted-by-`(slot, name)` invariant. Call once after parsing.
    pub fn sort_entries(&mut self) {
        self.entries.sort_by(|a, b| a.slot.cmp(&b.slot).then_with(|| a.name.cmp(&b.name)));
    }

    pub fn find(&self, slot: SlotId, name: &str) -> Option<AttachmentId> {
        self.entries
            .binary_search_by(|e| e.slot.cmp(&slot).then_with(|| e.name.as_str().cmp(name)))
            .ok()
            .map(|i| self.entries[i].attachment)
    }

    /// Every placeholder bound for `slot`, in sorted order.
    pub fn entries_for_slot(&self, slot: SlotId) -> impl Iterator<Item = &SkinEntry> {
        self.entries.iter().filter(move |e| e.slot == slot)
    }
}

/// The default skin's handle. Always present.
pub const DEFAULT_SKIN: SkinId = SkinId(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_inherit_json_names_round_trip() {
        for m in [
            TransformInherit::Normal,
            TransformInherit::OnlyTranslation,
            TransformInherit::NoRotationOrReflection,
            TransformInherit::NoScale,
            TransformInherit::NoScaleOrReflection,
        ] {
            assert_eq!(TransformInherit::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn unknown_transform_inherit_is_rejected_not_defaulted() {
        assert_eq!(TransformInherit::parse("noSuchMode"), None);
        assert_eq!(TransformInherit::from_ordinal(5), None);
    }

    #[test]
    fn transform_inherit_ordinals_match_the_json_names() {
        for (n, m) in [
            (0, TransformInherit::Normal),
            (1, TransformInherit::OnlyTranslation),
            (2, TransformInherit::NoRotationOrReflection),
            (3, TransformInherit::NoScale),
            (4, TransformInherit::NoScaleOrReflection),
        ] {
            assert_eq!(TransformInherit::from_ordinal(n), Some(m));
        }
    }

    #[test]
    fn bone_setup_defaults_to_identity_with_unit_scale() {
        let b = Bone::new("root", None);
        assert_eq!(b.setup.position, Vec2::ZERO);
        assert_eq!(b.setup.scale, Vec2::ONE);
        assert_eq!(b.setup.rotation, 0.0);
    }

    fn skin_with(entries: &[(u16, &str, u32)]) -> Skin {
        let mut s = Skin::new("default");
        for (slot, name, att) in entries {
            s.entries.push(SkinEntry { slot: SlotId(*slot), name: (*name).into(), attachment: AttachmentId(*att) });
        }
        s.sort_entries();
        s
    }

    #[test]
    fn skin_lookup_finds_the_right_binding() {
        let s = skin_with(&[(1, "head", 10), (0, "body", 20), (1, "hat", 30)]);
        assert_eq!(s.find(SlotId(1), "hat"), Some(AttachmentId(30)));
        assert_eq!(s.find(SlotId(0), "body"), Some(AttachmentId(20)));
    }

    #[test]
    fn skin_lookup_is_slot_scoped() {
        // The same placeholder name on two slots must not collide.
        let s = skin_with(&[(0, "x", 1), (1, "x", 2)]);
        assert_eq!(s.find(SlotId(0), "x"), Some(AttachmentId(1)));
        assert_eq!(s.find(SlotId(1), "x"), Some(AttachmentId(2)));
        assert_eq!(s.find(SlotId(2), "x"), None);
    }

    #[test]
    fn entries_for_slot_lists_only_that_slot() {
        let s = skin_with(&[(0, "a", 1), (1, "b", 2), (1, "c", 3)]);
        let names: Vec<_> = s.entries_for_slot(SlotId(1)).map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b", "c"]);
    }

    #[test]
    fn sort_entries_orders_by_slot_then_name() {
        let s = skin_with(&[(1, "z", 1), (0, "b", 2), (0, "a", 3)]);
        let seen: Vec<_> = s.entries.iter().map(|e| (e.slot.0, e.name.as_str())).collect();
        assert_eq!(seen, vec![(0, "a"), (0, "b"), (1, "z")]);
    }
}
