//! Binary skeleton decoders, dispatched by detected version family.

pub mod v3;
pub mod v4;

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::spine::SpineIr;
use a2d_core::{DecodeError, LoadReport, ModelKind};

use crate::detect::{SpineDetection, SpineFamily};

/// Decodes a binary skeleton using the decoder for its version family.
pub fn decode(
    bytes: &[u8],
    detection: &SpineDetection,
    atlas: Atlas,
    report: &mut LoadReport,
) -> Result<SpineIr, DecodeError> {
    match detection.family() {
        Some(SpineFamily::V3) => v3::decode(bytes, detection, atlas, report),
        // Spine 2.x binary predates the string table and uses a different
        // attachment enum. No target asset has needed it, and rule §4.10 says
        // not to write it speculatively.
        Some(SpineFamily::V2) => Err(DecodeError::unsupported_version(
            ModelKind::Spine,
            detection.raw_version.clone(),
            "the 2.x binary layout has no decoder yet; export as JSON or open an issue with the asset",
        )),
        // 4.0 and 4.1 only: see `v4::check_minor` for why 4.2 is refused, and
        // the module docs for what "not yet validated" means here.
        Some(SpineFamily::V4) => {
            v4::check_minor(detection)?;
            v4::decode(bytes, detection, atlas, report)
        }
        None => Err(DecodeError::unsupported_version(
            ModelKind::Spine,
            detection.raw_version.clone(),
            "unrecognised Spine major version",
        )),
    }
}

#[cfg(test)]
pub(crate) mod writer {
    //! A minimal Spine binary writer, used to build test fixtures.
    //!
    //! Round-tripping through a writer is how the decoders are tested without
    //! committing a real game asset (spec §11 asset policy). The primitives are
    //! shared by the 3.x and 4.x fixtures; each version's own test module lays
    //! them out to match the layout documented in [`super::v3`] or
    //! [`super::v4`], and a disagreement shows up as a decode failure.
    //!
    //! What a round-trip does **not** prove is that either side matches a file
    //! the Spine editor wrote. For 3.x that has been checked; for 4.x it has
    //! not. See the note at the top of [`super::v4`].

    #[derive(Default)]
    pub struct SkelWriter {
        pub out: Vec<u8>,
    }

    impl SkelWriter {
        pub fn new() -> Self {
            SkelWriter::default()
        }

        pub fn u8(&mut self, v: u8) -> &mut Self {
            self.out.push(v);
            self
        }

        pub fn bool(&mut self, v: bool) -> &mut Self {
            self.u8(u8::from(v))
        }

        pub fn u32(&mut self, v: u32) -> &mut Self {
            self.out.extend_from_slice(&v.to_be_bytes());
            self
        }

        pub fn u64(&mut self, v: u64) -> &mut Self {
            self.out.extend_from_slice(&v.to_be_bytes());
            self
        }

        pub fn u16(&mut self, v: u16) -> &mut Self {
            self.out.extend_from_slice(&v.to_be_bytes());
            self
        }

        pub fn f32(&mut self, v: f32) -> &mut Self {
            self.out.extend_from_slice(&v.to_be_bytes());
            self
        }

        pub fn varint(&mut self, mut v: u32) -> &mut Self {
            loop {
                if v >> 7 == 0 {
                    self.out.push(v as u8);
                    return self;
                }
                self.out.push(((v & 0x7f) | 0x80) as u8);
                v >>= 7;
            }
        }

        pub fn varint_signed(&mut self, v: i32) -> &mut Self {
            self.varint(((v << 1) ^ (v >> 31)) as u32)
        }

        /// Length-prefixed string; `None` writes the null marker.
        pub fn string(&mut self, v: Option<&str>) -> &mut Self {
            match v {
                None => self.varint(0),
                Some(s) => {
                    self.varint(s.len() as u32 + 1);
                    self.out.extend_from_slice(s.as_bytes());
                    self
                }
            }
        }

        /// One-based string-table reference; 0 means null.
        pub fn string_ref(&mut self, index: Option<usize>) -> &mut Self {
            self.varint(index.map_or(0, |i| i as u32 + 1))
        }

        pub fn floats(&mut self, values: &[f32]) -> &mut Self {
            for v in values {
                self.f32(*v);
            }
            self
        }

        pub fn finish(&self) -> Vec<u8> {
            self.out.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::writer::SkelWriter;
    use super::*;
    use a2d_core::ir::ids::{AttachmentId, BoneId, SkinId, SlotId};
    use a2d_core::ir::spine::{AttachmentKind, Axes, Timeline, TransformInherit, VertexData};
    use a2d_core::{BlendMode, Interpolation, Rgba, Vec2};

    /// Names in the string table, in the order the fixture writes them.
    const STRINGS: &[&str] = &["shirt", "body", "clip", "footstep", "body_blue", "blue"];

    fn s(name: &str) -> Option<usize> {
        STRINGS.iter().position(|n| *n == name)
    }

    /// Builds a Spine 3.8 skeleton covering every construct the decoder handles.
    fn fixture(nonessential: bool) -> Vec<u8> {
        let mut w = SkelWriter::new();

        // Header.
        w.string(Some("HASH1234567"));
        w.string(Some("3.8.99"));
        w.f32(-10.0).f32(-20.0).f32(100.0).f32(200.0);
        w.bool(nonessential);
        if nonessential {
            w.f32(30.0);
            w.string(Some("./images/"));
            w.string(None);
        }

        // String table.
        w.varint(STRINGS.len() as u32);
        for name in STRINGS {
            w.string(Some(name));
        }

        // Bones: root, torso, arm, target.
        w.varint(4);
        // root
        w.string(Some("root"));
        w.f32(0.0).f32(0.0).f32(0.0).f32(1.0).f32(1.0).f32(0.0).f32(0.0).f32(0.0);
        w.varint(0); // inherit: normal
        w.bool(false);
        if nonessential {
            w.u32(0xff00_00ff);
        }
        // torso
        w.string(Some("torso"));
        w.varint(0); // parent index
        w.f32(30.0).f32(1.0).f32(2.0).f32(2.0).f32(3.0).f32(4.0).f32(5.0).f32(50.0);
        w.varint(3); // inherit: noScale
        w.bool(false);
        if nonessential {
            w.u32(0);
        }
        // arm
        w.string(Some("arm"));
        w.varint(1);
        w.f32(0.0).f32(10.0).f32(0.0).f32(1.0).f32(1.0).f32(0.0).f32(0.0).f32(20.0);
        w.varint(0);
        w.bool(false);
        if nonessential {
            w.u32(0);
        }
        // target
        w.string(Some("target"));
        w.varint(0);
        w.f32(0.0).f32(40.0).f32(40.0).f32(1.0).f32(1.0).f32(0.0).f32(0.0).f32(0.0);
        w.varint(0);
        w.bool(false);
        if nonessential {
            w.u32(0);
        }

        // Slots: body, clip.
        w.varint(2);
        w.string(Some("body"));
        w.varint(1); // bone: torso
        w.u32(0xff00_00ff); // colour
        w.u32(0x0011_2233); // dark colour
        w.string_ref(s("shirt"));
        w.varint(1); // blend: additive
        w.string(Some("clip"));
        w.varint(0);
        w.u32(0xffff_ffff);
        w.u32(0xffff_ffff); // no dark colour
        w.string_ref(None);
        w.varint(0);

        // IK constraint.
        w.varint(1);
        w.string(Some("aim"));
        w.varint(5); // order
        w.bool(false);
        w.varint(1).varint(2); // one bone: arm
        w.varint(3); // target: target
        w.f32(0.75); // mix
        w.f32(2.0); // softness
        w.u8(0xff); // bend direction -1
        w.bool(true).bool(true).bool(false);

        // Transform constraint.
        w.varint(1);
        w.string(Some("tc"));
        w.varint(1);
        w.bool(false);
        w.varint(1).varint(2);
        w.varint(1); // target: torso
        w.bool(false).bool(true); // local, relative
        w.f32(45.0).f32(1.0).f32(2.0).f32(0.5).f32(0.25).f32(0.1);
        w.f32(0.9).f32(0.8).f32(0.7).f32(0.6); // rotate/translate/scale/shear mixes

        // Path constraints: none.
        w.varint(0);

        // Default skin: 2 slots.
        w.varint(2);
        // slot 0 (body): a mesh and a linked mesh placeholder.
        w.varint(0);
        w.varint(1);
        w.string_ref(s("shirt"));
        w.string_ref(s("body")); // attachment name
        w.u8(2); // mesh
        w.string_ref(s("body")); // path
        w.u32(0xffff_ffff);
        w.varint(4); // vertex count
        w.floats(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0]); // uvs
        w.varint(6).u16(0).u16(1).u16(2).u16(2).u16(3).u16(0); // triangles
        w.bool(true); // weighted
        for i in 0..4u32 {
            w.varint(1); // one influence per vertex
            w.varint(1); // bone: torso
            w.f32(i as f32).f32(i as f32 * 2.0).f32(1.0);
        }
        w.varint(4); // hull length
        if nonessential {
            w.varint(0); // edges
            w.f32(10.0).f32(20.0); // size
        }
        // slot 1 (clip): a clipping attachment.
        w.varint(1);
        w.varint(1);
        w.string_ref(s("clip"));
        w.string_ref(s("clip"));
        w.u8(6); // clipping
        w.varint(0); // end slot index
        w.varint(3); // vertex count
        w.bool(false);
        w.floats(&[0.0, 0.0, 10.0, 0.0, 5.0, 10.0]);
        if nonessential {
            w.u32(0xff00_00ff);
        }

        // One extra skin holding a linked mesh.
        w.varint(1);
        w.string_ref(s("blue"));
        w.varint(0); // skin bones
        w.varint(0).varint(0).varint(0); // skin ik/transform/path
        w.varint(1); // slot count
        w.varint(0); // slot 0
        w.varint(1); // one entry
        w.string_ref(s("shirt"));
        w.string_ref(s("body_blue"));
        w.u8(3); // linked mesh
        w.string_ref(s("body_blue")); // path
        w.u32(0xffff_ffff);
        w.string_ref(None); // skin: default
        w.string_ref(s("shirt")); // parent placeholder
        w.bool(true); // inherit timelines
        if nonessential {
            w.f32(10.0).f32(20.0);
        }

        // Events.
        w.varint(1);
        w.string_ref(s("footstep"));
        w.varint_signed(7);
        w.f32(1.5);
        w.string(Some("left"));
        w.string(None); // no audio

        // Animations.
        w.varint(1);
        w.string(Some("idle"));
        write_idle_animation(&mut w);
        w.finish()
    }

    fn write_idle_animation(w: &mut SkelWriter) {
        // Slot timelines: one attachment channel and one colour channel.
        w.varint(1);
        w.varint(0); // slot 0
        w.varint(2); // two channels
        w.u8(0); // attachment
        w.varint(2);
        w.f32(0.0);
        w.string_ref(s("shirt"));
        w.f32(0.5);
        w.string_ref(None); // hide
        w.u8(1); // colour
        w.varint(2);
        w.f32(0.0).u32(0xffff_ffff).u8(2).f32(0.25).f32(0.0).f32(0.75).f32(1.0); // bezier
        w.f32(1.0).u32(0x0000_00ff);

        // Bone timelines: rotate and translate on torso.
        w.varint(1);
        w.varint(1); // bone 1
        w.varint(2);
        w.u8(0); // rotate
        w.varint(2);
        w.f32(0.0).f32(0.0).u8(0); // linear
        w.f32(1.0).f32(90.0);
        w.u8(1); // translate
        w.varint(2);
        w.f32(0.0).f32(0.0).f32(0.0).u8(1); // stepped
        w.f32(1.0).f32(5.0).f32(-5.0);

        // IK timeline.
        w.varint(1);
        w.varint(0);
        w.varint(1);
        w.f32(0.0).f32(1.0).f32(0.0).u8(1).bool(false).bool(false);

        // Transform timeline.
        w.varint(1);
        w.varint(0);
        w.varint(1);
        w.f32(0.0).f32(1.0).f32(0.5).f32(0.25).f32(0.125);

        // Path timelines: none.
        w.varint(0);

        // Deform timeline on the default skin's mesh.
        w.varint(1);
        w.varint(0); // skin 0
        w.varint(1);
        w.varint(0); // slot 0
        w.varint(1);
        w.string_ref(s("shirt"));
        w.varint(2); // frames
        w.f32(0.0).varint(0).u8(0); // empty key, linear
        w.f32(1.0).varint(2).varint(2).f32(3.0).f32(4.0); // two values at offset 2

        // Draw order.
        w.varint(1);
        w.f32(0.0);
        w.varint(1);
        w.varint(1).varint_signed(-1); // slot 1 moves to the front

        // Events.
        w.varint(1);
        w.f32(0.25).varint(0).varint_signed(9).f32(2.5).bool(true).string(Some("right"));
    }

    fn decode_fixture(nonessential: bool) -> (SpineIr, LoadReport) {
        let bytes = fixture(nonessential);
        let detection = crate::detect::detect(&bytes).expect("fixture should be detected");
        assert_eq!(detection.family(), Some(SpineFamily::V3));
        let mut report = LoadReport::new();
        let ir = decode(&bytes, &detection, Atlas::default(), &mut report).expect("fixture should decode");
        (ir, report)
    }

    #[test]
    fn the_header_round_trips() {
        let (ir, _) = decode_fixture(true);
        assert_eq!(ir.metadata.source_version, "3.8.99");
        assert_eq!(ir.metadata.hash.as_deref(), Some("HASH1234567"));
        assert_eq!(ir.metadata.origin, Vec2::new(-10.0, -20.0));
        assert_eq!(ir.metadata.size, Vec2::new(100.0, 200.0));
        assert_eq!(ir.metadata.fps, Some(30.0));
        assert_eq!(ir.metadata.images_path.as_deref(), Some("./images/"));
    }

    #[test]
    fn the_essential_only_variant_decodes_identically_apart_from_editor_data() {
        let (full, _) = decode_fixture(true);
        let (lean, _) = decode_fixture(false);
        assert_eq!(full.bones, lean.bones);
        assert_eq!(full.slots, lean.slots);
        assert_eq!(full.animations, lean.animations);
        assert_eq!(lean.metadata.fps, None);
        assert_eq!(lean.metadata.images_path, None);
    }

    #[test]
    fn bones_decode_with_hierarchy_and_setup_pose() {
        let (ir, _) = decode_fixture(true);
        assert_eq!(ir.bones.len(), 4);
        assert_eq!(ir.bones[0].parent, None);
        let torso = &ir.bones[1];
        assert_eq!(torso.name, "torso");
        assert_eq!(torso.parent, Some(BoneId(0)));
        assert_eq!(torso.setup.rotation, 30.0);
        assert_eq!(torso.setup.position, Vec2::new(1.0, 2.0));
        assert_eq!(torso.setup.scale, Vec2::new(2.0, 3.0));
        assert_eq!(torso.setup.shear, Vec2::new(4.0, 5.0));
        assert_eq!(torso.length, 50.0);
        assert_eq!(torso.inherit, TransformInherit::NoScale);
    }

    #[test]
    fn slots_decode_with_colours_and_blend_modes() {
        let (ir, _) = decode_fixture(true);
        let body = &ir.slots[0];
        assert_eq!(body.bone, BoneId(1));
        assert_eq!(body.color, Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert!(body.dark_color.is_some());
        assert_eq!(body.setup_attachment.as_deref(), Some("shirt"));
        assert_eq!(body.blend_mode, BlendMode::Additive);
        // The sentinel dark colour means "none", not white.
        assert_eq!(ir.slots[1].dark_color, None);
    }

    #[test]
    fn the_ik_constraint_decodes_including_its_signed_bend_direction() {
        let (ir, _) = decode_fixture(true);
        let c = &ir.ik_constraints[0];
        assert_eq!(c.name, "aim");
        assert_eq!(c.order, 5);
        assert_eq!(c.bones, vec![BoneId(2)]);
        assert_eq!(c.target, BoneId(3));
        assert_eq!(c.mix, 0.75);
        assert_eq!(c.softness, 2.0);
        assert!(!c.bend_positive, "0xff is -1, meaning a negative bend");
        assert!(c.compress);
        assert!(c.stretch);
        assert!(!c.uniform);
    }

    #[test]
    fn the_transform_constraint_expands_grouped_mixes_per_axis() {
        let (ir, _) = decode_fixture(true);
        let c = &ir.transform_constraints[0];
        assert_eq!(c.offset_rotation, 45.0);
        assert!(c.relative && !c.local);
        assert_eq!(c.mix_rotate, 0.9);
        assert_eq!((c.mix_x, c.mix_y), (0.8, 0.8));
        assert_eq!((c.mix_scale_x, c.mix_scale_y), (0.7, 0.7));
        assert_eq!(c.mix_shear_y, 0.6);
    }

    #[test]
    fn constraint_update_order_follows_the_authored_order() {
        let (ir, _) = decode_fixture(true);
        assert_eq!(ir.constraint_order.len(), 2);
        // The transform constraint has order 1, the IK constraint order 5.
        assert_eq!(ir.constraint_order[0].kind, a2d_core::ir::spine::ConstraintKind::Transform);
    }

    #[test]
    fn a_weighted_mesh_decodes_with_its_influences() {
        let (ir, _) = decode_fixture(true);
        let id = ir.resolve_attachment(SkinId(0), SlotId(0), "shirt").expect("mesh should be bound");
        match &ir.attachment(id).unwrap().kind {
            AttachmentKind::Mesh(m) => {
                assert_eq!(m.path, "body");
                assert_eq!(m.uvs.len(), 4);
                assert_eq!(m.triangles, vec![0, 1, 2, 2, 3, 0]);
                assert_eq!(m.hull_length, 4);
                let VertexData::Weighted(wv) = &m.vertices else { panic!("expected weighted vertices") };
                assert!(wv.is_well_formed());
                assert_eq!(wv.vertex_count(), 4);
                assert_eq!(wv.influences_for(2)[0].position, Vec2::new(2.0, 4.0));
                assert_eq!(m.vertices.deform_len(), 8);
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
    }

    #[test]
    fn a_clipping_attachment_resolves_its_end_slot() {
        let (ir, _) = decode_fixture(true);
        let id = ir.resolve_attachment(SkinId(0), SlotId(1), "clip").expect("clip should be bound");
        match &ir.attachment(id).unwrap().kind {
            AttachmentKind::Clipping(c) => {
                assert_eq!(c.end_slot, Some(SlotId(0)));
                assert_eq!(c.vertices.vertex_count(), 3);
            }
            other => panic!("expected clipping, got {other:?}"),
        }
    }

    #[test]
    fn a_linked_mesh_inherits_geometry_from_the_default_skin() {
        let (ir, report) = decode_fixture(true);
        let blue = ir.skin_by_name("blue").expect("the extra skin should exist");
        let id = ir.resolve_attachment(blue, SlotId(0), "shirt").expect("linked mesh should be bound");
        match &ir.attachment(id).unwrap().kind {
            AttachmentKind::Mesh(m) => {
                assert_eq!(m.path, "body_blue", "the linked mesh keeps its own path");
                assert_eq!(m.triangles, vec![0, 1, 2, 2, 3, 0], "geometry comes from the parent");
                assert_eq!(m.vertices.vertex_count(), 4);
                assert!(m.linked_to.as_ref().unwrap().resolved.is_some());
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn events_decode_with_their_declared_defaults() {
        let (ir, _) = decode_fixture(true);
        assert_eq!(ir.events.len(), 1);
        let e = &ir.events[0];
        assert_eq!(e.name, "footstep");
        assert_eq!(e.int_value, 7);
        assert_eq!(e.float_value, 1.5);
        assert_eq!(e.string_value, "left");
        assert_eq!(e.audio_path, None);
    }

    fn timeline(ir: &SpineIr, pick: impl Fn(&Timeline) -> bool) -> &Timeline {
        ir.animations[0].timelines.iter().find(|t| pick(t)).expect("timeline should be present")
    }

    #[test]
    fn the_animation_decodes_with_the_expected_duration() {
        let (ir, _) = decode_fixture(true);
        assert_eq!(ir.animations.len(), 1);
        assert_eq!(ir.animations[0].name, "idle");
        assert_eq!(ir.animations[0].duration, 1.0);
    }

    #[test]
    fn attachment_keyframes_decode_including_the_hide_key() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::SlotAttachment { .. })) {
            Timeline::SlotAttachment { slot, keys } => {
                assert_eq!(*slot, SlotId(0));
                assert_eq!(keys[0].name.as_deref(), Some("shirt"));
                assert_eq!(keys[1].name, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_binary_bezier_curve_decodes_as_normalised_control_points() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::SlotColor { .. })) {
            Timeline::SlotColor { keys, .. } => {
                match keys[0].interp[0] {
                    Interpolation::Bezier(b) => {
                        assert_eq!((b.cx1, b.cy1, b.cx2, b.cy2), (0.25, 0.0, 0.75, 1.0));
                    }
                    other => panic!("expected a bezier, got {other:?}"),
                }
                // The final keyframe carries no curve.
                assert_eq!(keys[1].interp[0], Interpolation::Linear);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn bone_timelines_decode_with_their_curve_types() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::BoneRotate { .. })) {
            Timeline::BoneRotate { bone, keys } => {
                assert_eq!(*bone, BoneId(1));
                assert_eq!(keys[1].value, 90.0);
                assert_eq!(keys[0].interp, Interpolation::Linear);
            }
            other => panic!("unexpected {other:?}"),
        }
        match timeline(&ir, |t| matches!(t, Timeline::BoneTranslate { .. })) {
            Timeline::BoneTranslate { axes, keys, .. } => {
                assert_eq!(*axes, Axes::Both, "3.x always keys both axes together");
                assert_eq!(keys[0].interp_x, Interpolation::Stepped);
                assert_eq!(keys[0].interp_y, Interpolation::Stepped);
                assert_eq!(keys[1].value, Vec2::new(5.0, -5.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn deform_keyframes_keep_their_sparse_window() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::Deform { .. })) {
            Timeline::Deform { keys, attachment, .. } => {
                assert_eq!(*attachment, AttachmentId(0));
                assert!(keys[0].values.is_empty(), "an empty run means no deformation");
                assert_eq!(keys[1].offset, 2);
                assert_eq!(keys[1].values, vec![3.0, 4.0]);
                assert_eq!(keys[1].value_at(1), 0.0);
                assert_eq!(keys[1].value_at(2), 3.0);
                assert_eq!(keys[1].value_at(3), 4.0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn draw_order_offsets_expand_to_an_explicit_order() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::DrawOrder { .. })) {
            Timeline::DrawOrder { keys } => {
                assert_eq!(keys[0].order.as_ref().unwrap(), &vec![SlotId(1), SlotId(0)]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn event_keyframes_override_their_declared_defaults() {
        let (ir, _) = decode_fixture(true);
        match timeline(&ir, |t| matches!(t, Timeline::Event { .. })) {
            Timeline::Event { keys } => {
                assert_eq!(keys[0].time, 0.25);
                assert_eq!(keys[0].int_value, 9);
                assert_eq!(keys[0].float_value, 2.5);
                assert_eq!(keys[0].string_value.as_deref(), Some("right"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_clean_fixture_produces_no_warnings() {
        let (_, report) = decode_fixture(true);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn truncation_at_any_point_is_an_error_and_never_a_panic() {
        let full = fixture(true);
        for cut in (10..full.len()).step_by(7) {
            let bytes = &full[..cut];
            let Ok(detection) = crate::detect::detect(bytes) else { continue };
            let mut report = LoadReport::new();
            // The only requirement is that it terminates without panicking.
            let _ = decode(bytes, &detection, Atlas::default(), &mut report);
        }
    }

    #[test]
    fn corrupting_single_bytes_never_panics() {
        let full = fixture(true);
        for at in (0..full.len()).step_by(13) {
            let mut bytes = full.clone();
            bytes[at] ^= 0xff;
            let Ok(detection) = crate::detect::detect(&bytes) else { continue };
            let mut report = LoadReport::new();
            let _ = decode(&bytes, &detection, Atlas::default(), &mut report);
        }
    }

    #[test]
    fn the_broken_3_8_75_export_is_refused_by_name() {
        let mut w = SkelWriter::new();
        w.string(Some("h")).string(Some("3.8.75"));
        w.f32(0.0).f32(0.0).f32(0.0).f32(0.0).bool(false);
        let bytes = w.finish();
        let detection = crate::detect::detect(&bytes).unwrap();
        let mut report = LoadReport::new();
        let err = decode(&bytes, &detection, Atlas::default(), &mut report).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedVersion { .. }), "{err}");
        assert!(err.to_string().contains("3.8.76"), "{err}");
    }

    #[test]
    fn the_42_binary_layout_is_refused_rather_than_guessed_at() {
        // 4.2 packed the IK flags and added physics constraints, so reading it
        // with the 4.0/4.1 layout would silently misplace everything after.
        let detection = SpineDetection {
            encoding: crate::detect::SpineEncoding::Binary,
            version: crate::detect::SpineVersion::new(4, 2, 7),
            raw_version: "4.2.07".into(),
            hash: None,
        };
        let mut report = LoadReport::new();
        let err = decode(&[0u8; 32], &detection, Atlas::default(), &mut report).unwrap_err();
        match err {
            DecodeError::UnsupportedVersion { version, detail, .. } => {
                assert_eq!(version, "4.2.07");
                assert!(detail.contains("physics constraints"), "{detail}");
                assert!(detail.contains("4.0 and 4.1"), "{detail}");
            }
            other => panic!("expected an unsupported-version error, got {other}"),
        }
    }

    #[test]
    fn the_2x_binary_layout_is_refused_rather_than_guessed_at() {
        let detection = SpineDetection {
            encoding: crate::detect::SpineEncoding::Binary,
            version: crate::detect::SpineVersion::new(2, 1, 27),
            raw_version: "2.1.27".into(),
            hash: None,
        };
        let mut report = LoadReport::new();
        assert!(matches!(
            decode(&[0u8; 32], &detection, Atlas::default(), &mut report),
            Err(DecodeError::UnsupportedVersion { .. })
        ));
    }
}
