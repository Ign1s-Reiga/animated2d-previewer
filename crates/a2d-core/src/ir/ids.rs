//! Index newtypes used throughout the IR.
//!
//! Rule §4.6 asks for explicit typed data models. Bare `usize` indices into six
//! different arrays are exactly the loosely-typed shape that rule exists to
//! prevent, so every arena gets its own handle type.

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $repr:ty) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $repr);

        impl $name {
            #[inline]
            pub const fn index(self) -> usize {
                self.0 as usize
            }

            /// Builds a handle from a decoded index, rejecting anything the
            /// handle's representation cannot hold.
            #[inline]
            pub fn from_index(i: usize) -> Option<Self> {
                if i > <$repr>::MAX as usize {
                    None
                } else {
                    Some($name(i as $repr))
                }
            }
        }
    };
}

define_id!(
    /// Index into [`SpineIr::bones`](crate::ir::spine::SpineIr::bones).
    BoneId, u16
);
define_id!(
    /// Index into [`SpineIr::slots`](crate::ir::spine::SpineIr::slots), which is
    /// also the setup-pose draw order.
    SlotId, u16
);
define_id!(
    /// Index into [`SpineIr::skins`](crate::ir::spine::SpineIr::skins).
    SkinId, u16
);
define_id!(
    /// Index into the flat attachment arena.
    AttachmentId, u32
);
define_id!(
    /// Index into [`SpineIr::events`](crate::ir::spine::SpineIr::events).
    EventId, u16
);
define_id!(
    /// Index into [`SpineIr::ik_constraints`](crate::ir::spine::SpineIr::ik_constraints).
    IkConstraintId, u16
);
define_id!(
    /// Index into [`SpineIr::transform_constraints`](crate::ir::spine::SpineIr::transform_constraints).
    TransformConstraintId, u16
);
define_id!(
    /// Index into [`SpineIr::path_constraints`](crate::ir::spine::SpineIr::path_constraints).
    PathConstraintId, u16
);
define_id!(
    /// Index into [`Atlas::pages`](crate::ir::atlas::Atlas::pages).
    AtlasPageId, u16
);
define_id!(
    /// Index into [`Atlas::regions`](crate::ir::atlas::Atlas::regions).
    AtlasRegionId, u32
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trips() {
        assert_eq!(BoneId::from_index(7).unwrap().index(), 7);
        assert_eq!(AttachmentId::from_index(70_000).unwrap().index(), 70_000);
    }

    #[test]
    fn out_of_range_index_is_rejected_rather_than_truncated() {
        assert_eq!(BoneId::from_index(65_536), None);
        assert!(BoneId::from_index(65_535).is_some());
        assert_eq!(AttachmentId::from_index(u32::MAX as usize + 1), None);
    }

    #[test]
    fn handles_of_different_arenas_are_distinct_types() {
        // Compile-time property; the assertion is incidental.
        let b = BoneId(3);
        let s = SlotId(3);
        assert_eq!(b.index(), s.index());
    }
}
