//! The look-at rig of a Cubism Unity prefab, as a [`LookRig`].
//!
//! Three kinds of component make it up, recognised by the public SDK's class
//! names:
//!
//! * `CubismLookController`, one per model. Its fields, in serialized order:
//!   `i32 BlendMode`, `PPtr _target`, `PPtr Center`, `f32 Damping`. That is 32
//!   bytes after the behaviour header and name, and the whole body is read
//!   (anything left over, or missing, is a layout this does not know).
//! * `CubismLookParameter`, one per driven parameter: `i32 Axis`, `f32 Factor`
//!   (8 bytes). The parameter's id is the name of the `GameObject` it sits on.
//! * the target, a `GameObject` (or a behaviour or transform on one) whose
//!   position is where the rig looks when nothing else drives it.
//!
//! # Enumerations
//!
//! `BlendMode` is `CubismParameterBlendMode`, declared `Override, Additive,
//! Multiply`, so 0, 1, 2 (checked against the SDK source; real data holds 1,
//! additive). `Axis` is `CubismLookAxis`: 0 X, 1 Y, 2 Z. Any other value means
//! the layout is not the one read here and the whole rig is left out with a
//! note, rather than guessing which blend a new number stands for.
//!
//! # Space and unit
//!
//! The SDK computes the goal as `controller.transform.InverseTransformPoint(
//! target.position) - center`: the target's position in the local space of
//! the controller's own transform. That is what is reconstructed, by composing
//! each transform from the target up to (not including) the controller's, with
//! position, rotation and scale, so a target parented under a moved, turned or
//! scaled node is placed correctly. A target that is not below the controller's
//! transform has no such position in the bundle and the rig is left out.
//!
//! Unity units are copied as model-space units without conversion. What
//! established it: Core's vertex positions feed the Unity mesh directly, so the
//! mesh is in the model's own units; and on every real prefab read, the rest
//! target lies well inside the 1-unit-wide canvas (a few tenths of a unit),
//! which a wrong unit scale would not give. That the SDK imports at
//! `pixels_per_unit` engine units per pixel is the usual convention and is
//! **inferred**, not read from the bundle. Z is copied unchanged: both Unity
//! and the IR are left-handed with +Z into the screen.
//!
//! # Which controller
//!
//! The goal is measured in the controller's own transform, and the IR's space
//! is the model root's. They are the same only when the controller sits on the
//! model root: its `GameObject` carries the `CubismModel`, or its transform has
//! no parent. A controller anywhere else leaves the rig out with a note. A
//! controller whose `m_Enabled` is 0 ran with no look in the source and is left
//! out too. The controller's own position, rotation and scale are all excluded
//! from the goal (it is measured in that space, not through it).
//!
//! # Why the recovered rig is reported as a failing note
//!
//! Whether the rest target matches what the source computes depends on the
//! target's script (only the stock target returns its transform's position) and
//! on active state (the target's `m_Enabled`, its GameObject's `m_IsActive` and
//! its ancestors'), and none of that is verified here. The import therefore
//! reports the rig as a note, never as information, so it fails `validate`.
//!
//! # Center
//!
//! A null `Center` is zero in the SDK's current source (`centerPosition =
//! Vector3.zero`). The generation these prefabs were built with instead
//! substitutes the controller's own transform, whose position is subtracted;
//! for a prefab root (no parent, not moved) that position is also zero, so both
//! give `(0, 0, 0)`. Which generation shipped is not recorded in the bundle.
//!
//! A non-null `Center` follows the generation these prefabs use (RESEARCH.md
//! 2.1): the SDK subtracts the center object's own `m_LocalPosition`, not its
//! position composed through its parents. For a center that is a direct child
//! of the controller's transform the two agree. **Unverified on real data**:
//! all six real prefabs have a null `Center`.

use a2d_core::ir::cubism::{LookAxis, LookBlend, LookParameter, LookRig};
use a2d_unity::serialized::Object;
use a2d_unity::{behaviour_body, behaviour_owner_name, ClassId, Endian, Inventory, PPtr, Reader, SerializedFile};

pub(super) const LOOK_CONTROLLER: &str = "CubismLookController";
pub(super) const LOOK_PARAMETER: &str = "CubismLookParameter";

/// What recovery found: the rig (not yet checked against the model) or the
/// reason there is none. Both `None` means the bundle holds no look component.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Recovery {
    pub rig: Option<LookRig>,
    pub failure: Option<String>,
}

pub(super) fn recover(file: &SerializedFile, inventory: &Inventory) -> Recovery {
    let controllers: Vec<i64> = inventory.by_script(LOOK_CONTROLLER).map(|o| o.path_id).collect();
    let parameters: Vec<i64> = inventory.by_script(LOOK_PARAMETER).map(|o| o.path_id).collect();
    if controllers.is_empty() && parameters.is_empty() {
        return Recovery::default();
    }
    let model_roots: Vec<i64> = inventory
        .by_script("CubismModel")
        .filter_map(|o| file.find_object(o.path_id))
        .filter_map(|o| game_object_of(file, o).ok())
        .collect();
    match build(file, &controllers, &parameters, &model_roots) {
        Ok(rig) => Recovery { rig: Some(rig), failure: None },
        Err(reason) => Recovery { rig: None, failure: Some(reason) },
    }
}

fn build(
    file: &SerializedFile,
    controllers: &[i64],
    parameters: &[i64],
    model_roots: &[i64],
) -> Result<LookRig, String> {
    let controller = match controllers {
        [one] => *one,
        [] => return Err("the controller is missing".into()),
        many => {
            return Err(format!(
                "the bundle holds {} look controllers, so which one drives the model is ambiguous",
                many.len()
            ))
        }
    };
    let object = file.find_object(controller).ok_or("the controller object vanished")?;
    let (mut r, _) = behaviour_body(file, object).map_err(|e| format!("the controller is unreadable: {e}"))?;
    let err = |e: a2d_core::DecodeError| format!("the controller body is unreadable: {e}");
    let blend = r.i32().map_err(err)?;
    let target = pptr(&mut r).map_err(err)?;
    let center = pptr(&mut r).map_err(err)?;
    let damping = r.f32().map_err(err)?;
    if r.remaining() != 0 {
        return Err(format!(
            "the controller body has {} bytes left after its four fields; its layout is not the one read",
            r.remaining()
        ));
    }
    let blend = match blend {
        0 => LookBlend::Override,
        1 => LookBlend::Additive,
        2 => LookBlend::Multiply,
        other => return Err(format!("unknown BlendMode {other}")),
    };

    // `m_Enabled` is the byte right after the owner's `PPtr` (12 bytes).
    let enabled = file.object_data(object).ok().and_then(|data| data.get(12).copied());
    if enabled == Some(0) {
        return Err("the controller is disabled in the source".into());
    }
    let controller_go =
        game_object_of(file, object).map_err(|e| format!("the controller's owner is unreadable: {e}"))?;
    let controller_tf = transform_of_game_object(file, controller_go)
        .map_err(|e| format!("the controller's own transform was not found: {e}"))?;
    if !model_roots.contains(&controller_go) && read_node(file, controller_tf)?.father.path_id != 0 {
        return Err("the controller is not on the model root (its GameObject has no CubismModel and its transform has a parent), so its space is not the model's".into());
    }
    if target.is_null() {
        return Err("the controller has no target".into());
    }
    let rest_target = position_in(file, target, controller_tf).map_err(|e| format!("target: {e}"))?;
    let center = if center.is_null() {
        (0.0, 0.0, 0.0)
    } else {
        local_position(file, center).map_err(|e| format!("center: {e}"))?
    };

    let mut driven = Vec::new();
    for &id in parameters {
        let object = file.find_object(id).ok_or("a look parameter vanished")?;
        let owner = behaviour_owner_name(file, object)
            .ok()
            .flatten()
            .filter(|n| !n.is_empty())
            .ok_or("a look parameter has no readable owner to name its parameter")?;
        let (mut r, _) =
            behaviour_body(file, object).map_err(|e| format!("look parameter {owner:?} is unreadable: {e}"))?;
        let err = |e: a2d_core::DecodeError| format!("look parameter {owner:?} body is unreadable: {e}");
        let axis = r.i32().map_err(err)?;
        let factor = r.f32().map_err(err)?;
        if r.remaining() != 0 {
            return Err(format!("look parameter {owner:?} has {} bytes left after its two fields", r.remaining()));
        }
        let axis = match axis {
            0 => LookAxis::X,
            1 => LookAxis::Y,
            2 => LookAxis::Z,
            other => return Err(format!("look parameter {owner:?} has unknown Axis {other}")),
        };
        driven.push(LookParameter { parameter: owner, axis, factor });
    }
    Ok(LookRig { blend, damping, rest_target, center, parameters: driven })
}

fn pptr(r: &mut Reader<'_>) -> Result<PPtr, a2d_core::DecodeError> {
    Ok(PPtr { file_index: r.i32()?, path_id: r.i64()? })
}

/// The `GameObject` a component or transform belongs to, as a path id.
fn game_object_of(file: &SerializedFile, object: &Object) -> Result<i64, String> {
    if object.class_id == ClassId::GAME_OBJECT {
        return Ok(object.path_id);
    }
    let data = file.object_data(object).map_err(|e| e.to_string())?;
    let mut r = Reader::new(data, Endian::Little);
    let owner = pptr(&mut r).map_err(|e| e.to_string())?;
    if !owner.is_local() {
        return Err("a component has no local owner".into());
    }
    Ok(owner.path_id)
}

/// Path id of the `Transform` on a `GameObject`.
fn transform_of_game_object(file: &SerializedFile, go: i64) -> Result<i64, String> {
    let object = file.find_object(go).filter(|o| o.class_id == ClassId::GAME_OBJECT).ok_or("not a GameObject")?;
    let data = file.object_data(object).map_err(|e| e.to_string())?;
    let mut r = Reader::new(data, Endian::Little);
    let n = r.i32().map_err(|e| e.to_string())?;
    if !(0..=100_000).contains(&n) {
        return Err(format!("a GameObject declares {n} components"));
    }
    for _ in 0..n {
        let c = pptr(&mut r).map_err(|e| e.to_string())?;
        if c.is_local() && file.find_object(c.path_id).is_some_and(|o| o.class_id == ClassId::TRANSFORM) {
            return Ok(c.path_id);
        }
    }
    Err("a GameObject has no Transform".into())
}

/// One transform: local rotation (x, y, z, w), position, scale, and its parent.
struct Node {
    rotation: [f32; 4],
    position: [f32; 3],
    scale: [f32; 3],
    father: PPtr,
}

fn read_node(file: &SerializedFile, path_id: i64) -> Result<Node, String> {
    let object = file.find_object(path_id).filter(|o| o.class_id == ClassId::TRANSFORM).ok_or("not a Transform")?;
    let data = file.object_data(object).map_err(|e| e.to_string())?;
    let mut r = Reader::new(data, Endian::Little);
    let err = |e: a2d_core::DecodeError| format!("a transform is unreadable: {e}");
    pptr(&mut r).map_err(err)?;
    let mut f = || r.f32().map_err(err);
    let rotation = [f()?, f()?, f()?, f()?];
    let position = [f()?, f()?, f()?];
    let scale = [f()?, f()?, f()?];
    let children = r.i32().map_err(err)?;
    if !(0..=1_000_000).contains(&children) {
        return Err(format!("a transform declares {children} children"));
    }
    r.skip(children as usize * 12).map_err(err)?;
    let father = pptr(&mut r).map_err(err)?;
    if rotation.iter().chain(&position).chain(&scale).any(|v| !v.is_finite()) {
        return Err("a transform holds a non-finite value".into());
    }
    Ok(Node { rotation, position, scale, father })
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> Result<[f32; 3], String> {
    let norm = q.iter().map(|c| c * c).sum::<f32>().sqrt();
    if norm < 1e-6 {
        return Err("a transform has a zero rotation".into());
    }
    let [x, y, z, w] = q.map(|c| c / norm);
    // v' = v + 2w (q x v) + 2 q x (q x v)
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    Ok([
        v[0] + w * t[0] + (y * t[2] - z * t[1]),
        v[1] + w * t[1] + (z * t[0] - x * t[2]),
        v[2] + w * t[2] + (x * t[1] - y * t[0]),
    ])
}

/// The transform an object pointer names: itself, or the one on its `GameObject`.
fn transform_id(file: &SerializedFile, to: PPtr) -> Result<i64, String> {
    if !to.is_local() {
        return Err(format!("points outside this file (file {}, path {})", to.file_index, to.path_id));
    }
    let object = file.find_object(to.path_id).ok_or("points at an object this file does not hold")?;
    if object.class_id == ClassId::TRANSFORM {
        return Ok(object.path_id);
    }
    let go = game_object_of(file, object)?;
    transform_of_game_object(file, go)
}

/// The object's own `m_LocalPosition`, with no parent composed in.
fn local_position(file: &SerializedFile, to: PPtr) -> Result<(f32, f32, f32), String> {
    let p = read_node(file, transform_id(file, to)?)?.position;
    Ok((p[0], p[1], p[2]))
}

/// The position of the object `to` points at, in the local space of
/// transform `base`. `to` may be a `GameObject`, a component on one, or a
/// `Transform`.
fn position_in(file: &SerializedFile, to: PPtr, base: i64) -> Result<(f32, f32, f32), String> {
    if !to.is_local() {
        return Err(format!("points outside this file (file {}, path {})", to.file_index, to.path_id));
    }
    let object = file.find_object(to.path_id).ok_or("points at an object this file does not hold")?;
    let start = if object.class_id == ClassId::TRANSFORM {
        object.path_id
    } else {
        let go = game_object_of(file, object)?;
        transform_of_game_object(file, go)?
    };
    if start == base {
        // The base's own local position is not part of its own space.
        return Ok((0.0, 0.0, 0.0));
    }
    let mut node = read_node(file, start)?;
    let mut p = node.position;
    for _ in 0..256 {
        if !node.father.is_local() {
            return Err("is not below the controller's transform".into());
        }
        if node.father.path_id == base {
            return Ok((p[0], p[1], p[2]));
        }
        let father = read_node(file, node.father.path_id)?;
        let scaled = [p[0] * father.scale[0], p[1] * father.scale[1], p[2] * father.scale[2]];
        let r = rotate(father.rotation, scaled)?;
        p = [father.position[0] + r[0], father.position[1] + r[1], father.position[2] + r[2]];
        node = father;
    }
    Err("transform parents loop".into())
}

#[cfg(test)]
mod tests {
    use super::super::drivers::tests::{behaviour, build, game_object, mono_script, put_i32, put_pptr, put_string};
    use super::*;

    fn f32s(o: &mut Vec<u8>, v: &[f32]) {
        for x in v {
            o.extend_from_slice(&x.to_le_bytes());
        }
    }

    fn go(components: &[i64], name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, components.len() as i32);
        for c in components {
            put_pptr(&mut o, 0, *c);
        }
        put_i32(&mut o, 0);
        put_string(&mut o, name);
        o
    }

    fn transform(go: i64, rotation: [f32; 4], position: [f32; 3], scale: [f32; 3], father: i64) -> Vec<u8> {
        let mut o = Vec::new();
        put_pptr(&mut o, 0, go);
        f32s(&mut o, &rotation);
        f32s(&mut o, &position);
        f32s(&mut o, &scale);
        put_i32(&mut o, 0);
        put_pptr(&mut o, 0, father);
        o
    }

    const ID: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const ONE: [f32; 3] = [1.0; 3];
    type Node3 = ([f32; 4], [f32; 3], [f32; 3]);
    const FLAT: Node3 = (ID, [0.0; 3], ONE);

    fn controller_body(blend: i32, target: (i32, i64), center: (i32, i64), damping: f32) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, blend);
        put_pptr(&mut o, target.0, target.1);
        put_pptr(&mut o, center.0, center.1);
        f32s(&mut o, &[damping]);
        o
    }

    fn parameter_body(axis: i32, factor: f32) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, axis);
        f32s(&mut o, &[factor]);
        o
    }

    /// Model root (go 1, transform 2, at `root_position`, the controller on
    /// it), a node under it (go 3, transform 4) and the target under that node
    /// (go 5, transform 6); then one parameter object per `(owner, body)`.
    fn scene(
        root_position: [f32; 3],
        middle: Node3,
        target_position: [f32; 3],
        controller: Vec<u8>,
        parameters: &[(&str, Vec<u8>)],
    ) -> Recovery {
        scene_with_root((ID, root_position, ONE), middle, target_position, controller, parameters)
    }

    fn scene_with_root(
        root: Node3,
        middle: Node3,
        target_position: [f32; 3],
        controller: Vec<u8>,
        parameters: &[(&str, Vec<u8>)],
    ) -> Recovery {
        let mut objects = vec![
            (1, 0, go(&[2, 20], "Model")),
            (2, 3, transform(1, root.0, root.1, root.2, 0)),
            (3, 0, go(&[4], "Node")),
            (4, 3, transform(3, middle.0, middle.1, middle.2, 2)),
            (5, 0, go(&[6], "LookAtTarget")),
            (6, 3, transform(5, ID, target_position, ONE, 4)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (11, 1, mono_script(LOOK_PARAMETER)),
            (20, 2, behaviour(1, 10, &controller)),
        ];
        for (i, (name, body)) in parameters.iter().enumerate() {
            let go_id = 100 + 2 * i as i64;
            objects.push((go_id, 0, game_object(name)));
            objects.push((go_id + 1, 2, behaviour(go_id, 11, body)));
        }
        let file = build(&objects);
        let inventory = Inventory::build(&file);
        recover(&file, &inventory)
    }

    fn failure_of(r: Recovery) -> String {
        assert!(r.rig.is_none(), "{:?}", r.rig);
        r.failure.expect("a failure was expected")
    }

    #[test]
    fn an_exact_fit_rig_decodes_with_its_mapped_enums() {
        let r = scene(
            [9.0, 9.0, 9.0],
            FLAT,
            [0.18, 0.04, 0.05],
            controller_body(1, (0, 5), (0, 0), 0.15),
            &[
                ("ParamAngleX", parameter_body(0, 3.0)),
                ("ParamAngleY", parameter_body(1, 2.0)),
                ("ParamAngleZ", parameter_body(2, 0.5)),
            ],
        );
        assert_eq!(r.failure, None);
        let rig = r.rig.expect("a rig");
        assert_eq!(rig.blend, LookBlend::Additive);
        assert_eq!(rig.damping, 0.15);
        // The controller's own (9, 9, 9) position is not part of its own
        // space. A null center is zero.
        assert_eq!(rig.rest_target, (0.18, 0.04, 0.05));
        assert_eq!(rig.center, (0.0, 0.0, 0.0));
        let got: Vec<(&str, LookAxis, f32)> =
            rig.parameters.iter().map(|p| (p.parameter.as_str(), p.axis, p.factor)).collect();
        assert_eq!(
            got,
            [("ParamAngleX", LookAxis::X, 3.0), ("ParamAngleY", LookAxis::Y, 2.0), ("ParamAngleZ", LookAxis::Z, 0.5)]
        );
    }

    #[test]
    fn every_blend_mode_maps_by_the_sdk_declaration_order() {
        for (n, want) in [(0, LookBlend::Override), (1, LookBlend::Additive), (2, LookBlend::Multiply)] {
            let rig = scene([0.0; 3], FLAT, [0.0; 3], controller_body(n, (0, 5), (0, 0), 0.0), &[]).rig.expect("a rig");
            assert_eq!(rig.blend, want, "{n}");
        }
    }

    #[test]
    fn a_target_under_a_turned_scaled_node_is_composed() {
        // Node at (1, 0, 0), turned 90 degrees about Z, scaled by 2; the target
        // is (0, 1, 0) inside it: scaled to (0, 2, 0), turned to (-2, 0, 0),
        // moved to (-1, 0, 0). The root's own position (4, 5, 6) is excluded.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let rig = scene(
            [4.0, 5.0, 6.0],
            ([0.0, 0.0, s, s], [1.0, 0.0, 0.0], [2.0, 2.0, 2.0]),
            [0.0, 1.0, 0.0],
            controller_body(1, (0, 5), (0, 0), 0.0),
            &[],
        )
        .rig
        .expect("a rig");
        let (x, y, z) = rig.rest_target;
        assert!((x + 1.0).abs() < 1e-5 && y.abs() < 1e-5 && z.abs() < 1e-5, "{:?}", rig.rest_target);
    }

    #[test]
    fn a_non_null_center_is_placed_like_the_target_and_a_transform_pointer_resolves() {
        // Center names the node's GameObject (3); the target is a transform (6).
        let rig =
            scene([0.0; 3], (ID, [0.5, 0.25, 0.0], ONE), [0.1, 0.0, 0.0], controller_body(1, (0, 6), (0, 3), 0.0), &[])
                .rig
                .expect("a rig");
        assert_eq!(rig.center, (0.5, 0.25, 0.0));
        let (x, y, _) = rig.rest_target;
        assert!((x - 0.6).abs() < 1e-6 && (y - 0.25).abs() < 1e-6, "{:?}", rig.rest_target);
    }

    #[test]
    fn a_target_that_is_the_controllers_own_transform_is_the_origin() {
        let rig =
            scene([3.0, 3.0, 3.0], FLAT, [0.0; 3], controller_body(1, (0, 2), (0, 0), 0.0), &[]).rig.expect("a rig");
        assert_eq!(rig.rest_target, (0.0, 0.0, 0.0));
    }

    #[test]
    fn unknown_enums_and_wrong_sizes_leave_the_rig_out_with_a_reason() {
        let p = [("ParamAngleX", parameter_body(0, 1.0))];
        let ok = controller_body(1, (0, 5), (0, 0), 0.0);
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(7, (0, 5), (0, 0), 0.0), &p));
        assert_eq!(why, "unknown BlendMode 7");
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], ok.clone(), &[("ParamAngleX", parameter_body(3, 1.0))]));
        assert!(why.contains("unknown Axis 3"), "{why}");
        let mut long = ok.clone();
        long.extend_from_slice(&[0; 4]);
        assert!(failure_of(scene([0.0; 3], FLAT, [0.0; 3], long, &p)).contains("4 bytes left"));
        let mut short = ok.clone();
        short.truncate(28);
        assert!(failure_of(scene([0.0; 3], FLAT, [0.0; 3], short, &p)).contains("unreadable"));
        let mut long_param = parameter_body(0, 1.0);
        long_param.extend_from_slice(&[0; 4]);
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], ok, &[("ParamAngleX", long_param)]));
        assert!(why.contains("bytes left"), "{why}");
    }

    #[test]
    fn a_missing_foreign_or_unrooted_target_leaves_the_rig_out() {
        let p = [("ParamAngleX", parameter_body(0, 1.0))];
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (0, 0), (0, 0), 0.0), &p));
        assert_eq!(why, "the controller has no target");
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (1, 5), (0, 0), 0.0), &p));
        assert!(why.contains("outside this file"), "{why}");
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (0, 999), (0, 0), 0.0), &p));
        assert!(why.contains("does not hold"), "{why}");
        // A target whose parent chain never meets the controller's transform.
        let file = build(&[
            (1, 0, go(&[2], "Model")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (3, 0, go(&[4], "Elsewhere")),
            (4, 3, transform(3, ID, [1.0, 0.0, 0.0], ONE, 0)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (20, 2, behaviour(1, 10, &controller_body(1, (0, 3), (0, 0), 0.0))),
        ]);
        let r = recover(&file, &Inventory::build(&file));
        assert!(failure_of(r).contains("not below the controller"));
    }

    #[test]
    fn parameters_without_a_controller_are_reported_and_an_empty_bundle_is_silent() {
        let file = build(&[(10, 1, mono_script(LOOK_PARAMETER)), (11, 2, behaviour(0, 10, &parameter_body(0, 1.0)))]);
        assert_eq!(failure_of(recover(&file, &Inventory::build(&file))), "the controller is missing");
        let empty = build(&[]);
        assert_eq!(recover(&empty, &Inventory::build(&empty)), Recovery::default());
    }

    #[test]
    fn the_ir_check_runs_on_the_recovered_rig() {
        let rig = scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (0, 5), (0, 0), -1.0), &[]).rig.expect("a rig");
        assert!(rig.problem(&[]).expect("a negative damping is refused").contains("damping"));
    }

    #[test]
    fn a_center_two_levels_down_uses_its_own_local_position() {
        // Node at (0.5, 0.25, 0); the target (0.1, 0, 0) under it. Composed the
        // center would be (0.6, 0.25, 0); the rule followed subtracts only its
        // own m_LocalPosition, (0.1, 0, 0).
        let rig =
            scene([0.0; 3], (ID, [0.5, 0.25, 0.0], ONE), [0.1, 0.0, 0.0], controller_body(1, (0, 6), (0, 6), 0.0), &[])
                .rig
                .expect("a rig");
        assert_eq!(rig.center, (0.1, 0.0, 0.0));
        let (x, y, _) = rig.rest_target;
        assert!((x - 0.6).abs() < 1e-6 && (y - 0.25).abs() < 1e-6, "{:?}", rig.rest_target);
    }

    #[test]
    fn the_controllers_whole_transform_is_excluded_from_the_goal() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let plain = scene([0.0; 3], FLAT, [0.2, 0.1, 0.3], controller_body(1, (0, 5), (0, 0), 0.0), &[]);
        let moved = scene_with_root(
            ([0.0, 0.0, s, s], [7.0, -3.0, 2.0], [3.0, 0.5, 4.0]),
            FLAT,
            [0.2, 0.1, 0.3],
            controller_body(1, (0, 5), (0, 0), 0.0),
            &[],
        );
        assert_eq!(plain.rig.expect("a rig").rest_target, moved.rig.expect("a rig").rest_target);
    }

    #[test]
    fn a_disabled_controller_has_no_rig() {
        let mut b = behaviour(1, 10, &controller_body(1, (0, 2), (0, 0), 0.0));
        b[12] = 0;
        let file = build(&[
            (1, 0, go(&[2, 20], "Model")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (20, 2, b),
        ]);
        let why = failure_of(recover(&file, &Inventory::build(&file)));
        assert_eq!(why, "the controller is disabled in the source");
    }

    /// A controller under a parent, with and without the `CubismModel` beside it.
    fn parented(with_model: bool) -> Recovery {
        let mut objects = vec![
            (1, 0, go(&[2], "Outer")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (3, 0, go(&[4, 20, 21], "Model")),
            (4, 3, transform(3, ID, [0.0; 3], ONE, 2)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (12, 1, mono_script("CubismModel")),
            (20, 2, behaviour(3, 10, &controller_body(1, (0, 4), (0, 0), 0.0))),
        ];
        if with_model {
            objects.push((21, 2, behaviour(3, 12, &[])));
        }
        let file = build(&objects);
        recover(&file, &Inventory::build(&file))
    }

    #[test]
    fn a_parented_controller_must_be_the_model_root() {
        assert!(failure_of(parented(false)).contains("not on the model root"));
        assert!(parented(true).rig.is_some());
    }
}
