//! Attachment decoding from JSON.

use a2d_core::ir::ids::{BoneId, SlotId};
use a2d_core::ir::spine::{
    Attachment, AttachmentKind, BoundingBoxAttachment, ClippingAttachment, LinkedMesh, MeshAttachment, PathAttachment,
    PointAttachment, RegionAttachment, Sequence, VertexData, VertexInfluence, WeightedVertices,
};
use a2d_core::{DecodeError, LoadReport, Rgba, Vec2};

use crate::json::fields::Fields;

/// Decodes one attachment.
///
/// `placeholder` is the name it is bound under in the skin; it is also the
/// default region path and the default attachment name, which is what Spine
/// does when `name` and `path` are omitted.
pub fn read_attachment(
    value: &serde_json::Value,
    placeholder: &str,
    slot: SlotId,
    context: &str,
    report: &mut LoadReport,
) -> Result<Attachment, DecodeError> {
    let mut f = Fields::new(value, context);
    let type_name = f.str("type").unwrap_or("region").to_string();
    let name = f.string("name").unwrap_or_else(|| placeholder.to_string());

    let kind = match type_name.as_str() {
        "region" => AttachmentKind::Region(read_region(&mut f, &name, report)?),
        "mesh" => AttachmentKind::Mesh(read_mesh(&mut f, &name, context, report)?),
        "linkedmesh" => AttachmentKind::Mesh(read_linked_mesh(&mut f, &name, slot, report)?),
        "boundingbox" => AttachmentKind::BoundingBox(BoundingBoxAttachment {
            vertices: read_declared_vertices(&mut f, context)?,
            color: read_color(&mut f, "color", Rgba::new(0.24, 0.64, 0.13, 1.0), context, report),
        }),
        "clipping" => AttachmentKind::Clipping(ClippingAttachment {
            // Resolved to a handle by the caller, which knows the slot table.
            end_slot: None,
            vertices: read_declared_vertices(&mut f, context)?,
            color: read_color(&mut f, "color", Rgba::new(0.87, 0.13, 0.24, 1.0), context, report),
        }),
        "point" => AttachmentKind::Point(PointAttachment {
            position: Vec2::new(f.f32("x", 0.0), f.f32("y", 0.0)),
            rotation: f.f32("rotation", 0.0),
            color: read_color(&mut f, "color", Rgba::new(0.9, 0.9, 0.15, 1.0), context, report),
        }),
        "path" => AttachmentKind::Path(PathAttachment {
            closed: f.bool("closed", false),
            constant_speed: f.bool("constantSpeed", true),
            lengths: f.f32_array("lengths")?,
            vertices: read_declared_vertices(&mut f, context)?,
            color: read_color(&mut f, "color", Rgba::new(1.0, 0.5, 0.0, 1.0), context, report),
        }),
        other => return Err(DecodeError::UnsupportedFormat(format!("{context}: unknown attachment type {other:?}"))),
    };

    // `end` is read by the caller once slot handles are known.
    if type_name == "clipping" {
        f.mark("end");
    }
    f.finish(report);
    Ok(Attachment { name, kind })
}

/// Reads the `end` slot name of a clipping attachment, before slot resolution.
pub fn clipping_end_name(value: &serde_json::Value) -> Option<&str> {
    value.get("end").and_then(|v| v.as_str())
}

fn read_region(f: &mut Fields<'_>, name: &str, report: &mut LoadReport) -> Result<RegionAttachment, DecodeError> {
    let context = f.context().to_string();
    Ok(RegionAttachment {
        path: f.string("path").unwrap_or_else(|| name.to_string()),
        region: None,
        position: Vec2::new(f.f32("x", 0.0), f.f32("y", 0.0)),
        rotation: f.f32("rotation", 0.0),
        scale: Vec2::new(f.f32("scaleX", 1.0), f.f32("scaleY", 1.0)),
        size: Vec2::new(f.f32("width", 0.0), f.f32("height", 0.0)),
        color: read_color(f, "color", Rgba::WHITE, &context, report),
        sequence: read_sequence(f),
    })
}

fn read_mesh(
    f: &mut Fields<'_>,
    name: &str,
    context: &str,
    report: &mut LoadReport,
) -> Result<MeshAttachment, DecodeError> {
    let uvs_flat = f.f32_array("uvs")?;
    if uvs_flat.len() % 2 != 0 {
        return Err(DecodeError::corrupt(format!("{context}: `uvs` has an odd number of values")));
    }
    let uvs: Vec<Vec2> = uvs_flat.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect();
    let triangles = f.u16_array("triangles")?;
    if triangles.len() % 3 != 0 {
        return Err(DecodeError::corrupt(format!("{context}: `triangles` is not a whole number of triangles")));
    }
    let vertices = read_vertices(f, uvs.len(), context)?;

    if let Some(&max) = triangles.iter().max() {
        if max as usize >= uvs.len() {
            return Err(DecodeError::corrupt(format!(
                "{context}: triangle index {max} exceeds the {} vertices declared by `uvs`",
                uvs.len()
            )));
        }
    }

    Ok(MeshAttachment {
        path: f.string("path").unwrap_or_else(|| name.to_string()),
        region: None,
        uvs,
        triangles,
        vertices,
        hull_length: f.u32("hull", 0),
        edges: f.u16_array("edges")?,
        size: Vec2::new(f.f32("width", 0.0), f.f32("height", 0.0)),
        color: read_color(f, "color", Rgba::WHITE, context, report),
        linked_to: None,
        sequence: read_sequence(f),
    })
}

fn read_linked_mesh(
    f: &mut Fields<'_>,
    name: &str,
    slot: SlotId,
    report: &mut LoadReport,
) -> Result<MeshAttachment, DecodeError> {
    let context = f.context().to_string();
    let parent =
        f.string("parent").ok_or_else(|| DecodeError::corrupt(format!("{context}: linked mesh has no `parent`")))?;
    // Spine 4.2 renamed `deform` to `timelines`; both mean "follow the parent's
    // deform timelines".
    let inherit_timelines = match (f.get("deform"), f.get("timelines")) {
        (Some(v), _) => v.as_bool().unwrap_or(true),
        (None, Some(v)) => v.as_bool().unwrap_or(true),
        (None, None) => true,
    };
    Ok(MeshAttachment {
        path: f.string("path").unwrap_or_else(|| name.to_string()),
        region: None,
        // Geometry is copied from the parent during normalisation.
        uvs: Vec::new(),
        triangles: Vec::new(),
        vertices: VertexData::Rigid(Vec::new()),
        hull_length: 0,
        edges: Vec::new(),
        size: Vec2::new(f.f32("width", 0.0), f.f32("height", 0.0)),
        color: read_color(f, "color", Rgba::WHITE, &context, report),
        linked_to: Some(LinkedMesh { skin: f.string("skin"), slot, parent, inherit_timelines, resolved: None }),
        sequence: read_sequence(f),
    })
}

/// Reads vertices for a type that declares its own `vertexCount`.
fn read_declared_vertices(f: &mut Fields<'_>, context: &str) -> Result<VertexData, DecodeError> {
    let count = f.u32("vertexCount", 0) as usize;
    read_vertices(f, count, context)
}

/// Reads the `vertices` array in either the rigid or the weighted layout.
///
/// Spine distinguishes them by length alone: exactly `2 * vertex_count` floats
/// means rigid positions, anything else is the weighted encoding.
pub fn read_vertices(f: &mut Fields<'_>, vertex_count: usize, context: &str) -> Result<VertexData, DecodeError> {
    let raw = f.f32_array("vertices")?;
    parse_vertices(&raw, vertex_count, context)
}

/// Shared by the JSON and binary decoders.
pub fn parse_vertices(raw: &[f32], vertex_count: usize, context: &str) -> Result<VertexData, DecodeError> {
    if raw.len() == vertex_count * 2 {
        return Ok(VertexData::Rigid(raw.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect()));
    }

    let mut offsets = Vec::with_capacity(vertex_count + 1);
    let mut influences = Vec::new();
    offsets.push(0u32);
    let mut i = 0usize;
    for vertex in 0..vertex_count {
        let bone_count = *raw
            .get(i)
            .ok_or_else(|| DecodeError::corrupt(format!("{context}: weighted vertices end before vertex {vertex}")))?
            as i64;
        if !(0..=1024).contains(&bone_count) {
            return Err(DecodeError::corrupt(format!(
                "{context}: vertex {vertex} declares an implausible bone count {bone_count}"
            )));
        }
        i += 1;
        for _ in 0..bone_count {
            let chunk = raw.get(i..i + 4).ok_or_else(|| {
                DecodeError::corrupt(format!("{context}: weighted vertices truncated at vertex {vertex}"))
            })?;
            let bone_index = chunk[0] as i64;
            let bone = u16::try_from(bone_index).ok().map(BoneId).ok_or_else(|| {
                DecodeError::corrupt(format!("{context}: vertex {vertex} references bone index {bone_index}"))
            })?;
            influences.push(VertexInfluence { bone, position: Vec2::new(chunk[1], chunk[2]), weight: chunk[3] });
            i += 4;
        }
        offsets.push(influences.len() as u32);
    }

    if i != raw.len() {
        return Err(DecodeError::corrupt(format!(
            "{context}: weighted vertices have {} trailing values",
            raw.len() - i
        )));
    }

    Ok(VertexData::Weighted(WeightedVertices { offsets, influences }))
}

pub fn read_color(
    f: &mut Fields<'_>,
    key: &'static str,
    default: Rgba,
    context: &str,
    report: &mut LoadReport,
) -> Rgba {
    match f.str(key) {
        None => default,
        Some(hex) => match Rgba::from_hex(hex) {
            Some(c) => c,
            None => {
                report.note(format!("{context}: malformed `{key}` value {hex:?}, using the default"));
                default
            }
        },
    }
}

fn read_sequence(f: &mut Fields<'_>) -> Option<Sequence> {
    let value = f.get("sequence")?;
    let mut s = Fields::new(value, "sequence");
    Some(Sequence {
        count: s.u32("count", 0),
        start: s.u32("start", 1),
        digits: s.u32("digits", 0),
        setup_index: s.u32("setup", 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(value: &serde_json::Value, placeholder: &str) -> (Attachment, LoadReport) {
        let mut report = LoadReport::new();
        let a = read_attachment(value, placeholder, SlotId(0), "test", &mut report).unwrap();
        (a, report)
    }

    #[test]
    fn a_bare_object_defaults_to_a_region_named_after_its_placeholder() {
        let (a, report) = read(&json!({}), "head");
        assert_eq!(a.name, "head");
        match &a.kind {
            AttachmentKind::Region(r) => {
                assert_eq!(r.path, "head");
                assert_eq!(r.scale, Vec2::ONE);
                assert_eq!(r.color, Rgba::WHITE);
            }
            other => panic!("expected a region, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn an_explicit_path_overrides_the_placeholder_name() {
        let (a, _) = read(&json!({"path": "atlas/head_01"}), "head");
        assert_eq!(a.kind.region_path(), Some("atlas/head_01"));
    }

    #[test]
    fn region_transform_fields_are_read() {
        let (a, _) = read(
            &json!({"x": 3.0, "y": -4.0, "rotation": 90.0, "scaleX": 2.0, "scaleY": 0.5,
                                  "width": 64, "height": 128}),
            "r",
        );
        match a.kind {
            AttachmentKind::Region(r) => {
                assert_eq!(r.position, Vec2::new(3.0, -4.0));
                assert_eq!(r.rotation, 90.0);
                assert_eq!(r.scale, Vec2::new(2.0, 0.5));
                assert_eq!(r.size, Vec2::new(64.0, 128.0));
            }
            other => panic!("expected a region, got {other:?}"),
        }
    }

    #[test]
    fn a_rigid_mesh_reads_its_geometry() {
        let value = json!({
            "type": "mesh",
            "uvs": [0, 0, 1, 0, 1, 1, 0, 1],
            "triangles": [0, 1, 2, 2, 3, 0],
            "vertices": [0, 0, 10, 0, 10, 20, 0, 20],
            "hull": 4,
            "width": 10, "height": 20
        });
        let (a, report) = read(&value, "body");
        match a.kind {
            AttachmentKind::Mesh(m) => {
                assert_eq!(m.uvs.len(), 4);
                assert_eq!(m.triangles, vec![0, 1, 2, 2, 3, 0]);
                assert_eq!(m.hull_length, 4);
                assert_eq!(
                    m.vertices,
                    VertexData::Rigid(vec![
                        Vec2::new(0.0, 0.0),
                        Vec2::new(10.0, 0.0),
                        Vec2::new(10.0, 20.0),
                        Vec2::new(0.0, 20.0)
                    ])
                );
                assert_eq!(m.vertices.deform_len(), 8);
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_weighted_mesh_reads_its_influences() {
        // Two vertices: the first bound to two bones, the second to one.
        let value = json!({
            "type": "mesh",
            "uvs": [0, 0, 1, 1],
            "triangles": [0, 1, 0],
            "vertices": [
                2, 0, 1.0, 2.0, 0.75, 1, 3.0, 4.0, 0.25,
                1, 1, 5.0, 6.0, 1.0
            ]
        });
        let (a, _) = read(&value, "body");
        match a.kind {
            AttachmentKind::Mesh(m) => {
                let VertexData::Weighted(w) = &m.vertices else { panic!("expected weighted vertices") };
                assert!(w.is_well_formed());
                assert_eq!(w.vertex_count(), 2);
                assert_eq!(w.influences_for(0).len(), 2);
                assert_eq!(w.influences_for(0)[0].bone, BoneId(0));
                assert_eq!(w.influences_for(0)[0].position, Vec2::new(1.0, 2.0));
                assert_eq!(w.influences_for(0)[0].weight, 0.75);
                assert_eq!(w.influences_for(1).len(), 1);
                assert_eq!(w.influences_for(1)[0].bone, BoneId(1));
                // Deform addresses influences, not vertices.
                assert_eq!(m.vertices.deform_len(), 6);
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
    }

    #[test]
    fn a_mesh_with_out_of_range_triangles_is_corrupt() {
        let value = json!({"type": "mesh", "uvs": [0, 0, 1, 1], "triangles": [0, 1, 7], "vertices": [0, 0, 1, 1]});
        let mut report = LoadReport::new();
        let err = read_attachment(&value, "m", SlotId(0), "test", &mut report).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[test]
    fn a_mesh_with_a_partial_triangle_is_corrupt() {
        let value = json!({"type": "mesh", "uvs": [0, 0, 1, 1], "triangles": [0, 1], "vertices": [0, 0, 1, 1]});
        let mut report = LoadReport::new();
        assert!(read_attachment(&value, "m", SlotId(0), "test", &mut report).is_err());
    }

    #[test]
    fn odd_uv_arrays_are_corrupt() {
        let value = json!({"type": "mesh", "uvs": [0, 0, 1], "triangles": [], "vertices": []});
        let mut report = LoadReport::new();
        assert!(read_attachment(&value, "m", SlotId(0), "test", &mut report).is_err());
    }

    #[test]
    fn truncated_weighted_vertices_are_corrupt_not_a_panic() {
        // Declares two bones for the vertex but supplies data for one.
        let err = parse_vertices(&[2.0, 0.0, 1.0, 2.0, 1.0], 1, "test").unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    #[test]
    fn trailing_weighted_data_is_corrupt() {
        let err = parse_vertices(&[1.0, 0.0, 1.0, 2.0, 1.0, 99.0], 1, "test").unwrap_err();
        assert!(err.to_string().contains("trailing"), "{err}");
    }

    #[test]
    fn a_negative_bone_index_is_corrupt() {
        let err = parse_vertices(&[1.0, -3.0, 1.0, 2.0, 1.0], 1, "test").unwrap_err();
        assert!(err.to_string().contains("bone index"), "{err}");
    }

    #[test]
    fn an_absurd_bone_count_is_corrupt() {
        // Three floats for one vertex cannot be rigid, so this takes the
        // weighted path and the leading value is read as a bone count.
        let err = parse_vertices(&[1e9, 0.0, 0.0], 1, "test").unwrap_err();
        assert!(err.to_string().contains("implausible"), "{err}");
    }

    #[test]
    fn a_matching_length_is_read_as_rigid_positions() {
        let v = parse_vertices(&[1.0, 2.0, 3.0, 4.0], 2, "test").unwrap();
        assert_eq!(v, VertexData::Rigid(vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]));
    }

    #[test]
    fn a_linked_mesh_records_its_parent_and_defaults_to_inheriting_timelines() {
        let value = json!({"type": "linkedmesh", "parent": "body", "skin": "blue"});
        let (a, _) = read(&value, "body-blue");
        match a.kind {
            AttachmentKind::Mesh(m) => {
                let link = m.linked_to.expect("linked mesh should record its link");
                assert_eq!(link.parent, "body");
                assert_eq!(link.skin.as_deref(), Some("blue"));
                assert!(link.inherit_timelines);
                assert!(link.resolved.is_none());
            }
            other => panic!("expected a mesh, got {other:?}"),
        }
    }

    #[test]
    fn a_linked_mesh_honours_both_spellings_of_the_deform_flag() {
        for key in ["deform", "timelines"] {
            let value = json!({"type": "linkedmesh", "parent": "body", key: false});
            let (a, report) = read(&value, "x");
            match a.kind {
                AttachmentKind::Mesh(m) => assert!(!m.linked_to.unwrap().inherit_timelines, "key={key}"),
                other => panic!("expected a mesh, got {other:?}"),
            }
            assert!(report.is_empty(), "{report}");
        }
    }

    #[test]
    fn a_linked_mesh_without_a_parent_is_corrupt() {
        let value = json!({"type": "linkedmesh"});
        let mut report = LoadReport::new();
        assert!(read_attachment(&value, "x", SlotId(0), "test", &mut report).is_err());
    }

    #[test]
    fn clipping_reads_its_polygon_and_defers_the_end_slot() {
        let value = json!({"type": "clipping", "end": "torso", "vertexCount": 3,
                           "vertices": [0, 0, 10, 0, 5, 10]});
        assert_eq!(clipping_end_name(&value), Some("torso"));
        let (a, report) = read(&value, "clip");
        match a.kind {
            AttachmentKind::Clipping(c) => {
                assert_eq!(c.vertices.vertex_count(), 3);
                assert!(c.end_slot.is_none());
            }
            other => panic!("expected clipping, got {other:?}"),
        }
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn bounding_boxes_read_their_polygon() {
        let value = json!({"type": "boundingbox", "vertexCount": 4,
                           "vertices": [0, 0, 1, 0, 1, 1, 0, 1]});
        let (a, _) = read(&value, "hitbox");
        match a.kind {
            AttachmentKind::BoundingBox(b) => assert_eq!(b.vertices.vertex_count(), 4),
            other => panic!("expected a bounding box, got {other:?}"),
        }
    }

    #[test]
    fn points_read_position_and_rotation() {
        let (a, _) = read(&json!({"type": "point", "x": 5, "y": 6, "rotation": 45}), "muzzle");
        match a.kind {
            AttachmentKind::Point(p) => {
                assert_eq!(p.position, Vec2::new(5.0, 6.0));
                assert_eq!(p.rotation, 45.0);
            }
            other => panic!("expected a point, got {other:?}"),
        }
    }

    #[test]
    fn paths_read_their_flags_and_lengths() {
        let value = json!({"type": "path", "closed": true, "constantSpeed": false,
                           "lengths": [10.0, 20.0], "vertexCount": 4,
                           "vertices": [0, 0, 1, 1, 2, 2, 3, 3]});
        let (a, _) = read(&value, "route");
        match a.kind {
            AttachmentKind::Path(p) => {
                assert!(p.closed);
                assert!(!p.constant_speed);
                assert_eq!(p.lengths, vec![10.0, 20.0]);
            }
            other => panic!("expected a path, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_attachment_type_is_an_unsupported_format_error() {
        let mut report = LoadReport::new();
        let err = read_attachment(&json!({"type": "hologram"}), "x", SlotId(0), "test", &mut report).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedFormat(_)), "{err}");
    }

    #[test]
    fn colors_parse_from_hex_and_report_when_malformed() {
        let (a, report) = read(&json!({"color": "ff0000ff"}), "r");
        match &a.kind {
            AttachmentKind::Region(r) => assert_eq!(r.color, Rgba::new(1.0, 0.0, 0.0, 1.0)),
            other => panic!("expected a region, got {other:?}"),
        }
        assert!(report.is_empty());

        let (a, report) = read(&json!({"color": "zzz"}), "r");
        match &a.kind {
            AttachmentKind::Region(r) => assert_eq!(r.color, Rgba::WHITE),
            other => panic!("expected a region, got {other:?}"),
        }
        assert!(report.to_string().contains("malformed"), "{report}");
    }

    #[test]
    fn sequences_are_read_when_present() {
        let value = json!({"sequence": {"count": 8, "start": 1, "digits": 2, "setup": 0}});
        let (a, _) = read(&value, "r");
        match a.kind {
            AttachmentKind::Region(r) => {
                let s = r.sequence.expect("sequence should be read");
                assert_eq!(s.count, 8);
                assert_eq!(s.digits, 2);
            }
            other => panic!("expected a region, got {other:?}"),
        }
    }

    #[test]
    fn unknown_attachment_keys_are_reported() {
        let (_, report) = read(&json!({"mysteryKey": 1}), "r");
        assert!(report.to_string().contains("mysteryKey"), "{report}");
    }
}
