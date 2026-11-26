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
//! model root: its `GameObject` carries the `CubismModel` (the SDK finds the
//! model on that `GameObject` only, so a controller without one applies
//! nothing). A controller anywhere else leaves the rig out with a note. A
//! controller whose `m_Enabled` is 0 ran with no look in the source and is left
//! out too. The controller's own position, rotation and scale are all excluded
//! from the goal (it is measured in that space, not through it).
//!
//! # What the source would apply, and so what is recovered
//!
//! The rig is recovered only when the source would apply it. The SDK's
//! per-frame update (`CubismLookController`, its public source) returns without
//! touching a parameter when the controller is disabled or inactive, when the
//! target is null (no look-target component to read), or when that component
//! reports itself inactive (the standard one is `isActiveAndEnabled`: enabled,
//! on an active `GameObject` whose ancestors are all active). In each of those
//! cases the source applies **nothing**, so applying a rest offset here would
//! be wrong; the rig is left out and the import says which condition held.
//!
//! Otherwise the rig is applied at the target's position, but only a standard
//! target is known to return that position. The SDK reads the target as
//! `self as ICubismLookTarget`, or, when the pointer names a `GameObject`, its
//! first component implementing the interface; so a pointer at a `Transform`
//! (or any other component) yields no target and the source applies nothing.
//! The target must be a `GameObject` carrying exactly one look-target script, or
//! that behaviour itself, recognised by its full name
//! `Live2D.Cubism.Framework.LookAt.CubismLookTargetBehaviour` (a class of the
//! same name elsewhere is game code), enabled, on an active `GameObject` with an
//! active chain to the root. Every other component on that `GameObject` must be
//! a named SDK component known not to implement the interface (a closed list);
//! anything else (the SDK's sample targets included) may be answered first, so
//! the rig cannot be verified. The controller itself must be enabled, on the
//! `GameObject` that carries the `CubismModel` (the SDK looks for the model on
//! its own `GameObject` only; without one it applies nothing), and its
//! `GameObject` and ancestors active. Where a check cannot be made (an unreadable object, a
//! pointer out of the file) the rig is left out with that reason; nothing is
//! recovered silently. `m_IsActive` is the `GameObject`'s byte after its name
//! and tag; `m_Enabled` is the behaviour's byte after its owner.
//!
//! "The scene as the prefab defines it" is the standard: nothing outside the
//! bundle is assumed to move the target or the model root.
//!
//! # Center
//!
//! Two shapes of a "no center" are in real data, and both give `(0, 0, 0)`:
//!
//! * a null `Center`, which the SDK's current source treats as
//!   `Vector3.zero`;
//! * a `Center` that points at the controller's own transform (the generation
//!   these prefabs were built with substitutes it for null), whose
//!   `m_LocalPosition` is subtracted. For a prefab root at its parent's origin
//!   that is also zero. Pointing at a root that was moved would subtract that
//!   position instead; the two shapes agree only for an unmoved root.
//!
//! A non-null `Center` follows the generation these prefabs use: the SDK
//! subtracts the center object's own `m_LocalPosition`, not its position
//! composed through its parents. For a center that is a direct child of the
//! controller's transform the two agree. Of the six real prefabs read, two
//! have a null `Center` and four point it at the root's own transform.
//!
//! The generations disagree otherwise: the older one substitutes the
//! controller's own transform for a null `Center` and subtracts its
//! `m_LocalPosition`, and subtracts a `Transform` center's `m_LocalPosition`;
//! the newer one subtracts zero for a null `Center` and treats a `Transform` as
//! no center (zero). Both agree only when the subtracted position is zero, so
//! only then is the center verified; otherwise the rig is recovered with the
//! generation dependence in [`Recovery::caveats`], which keeps the import line a
//! failing note.

use a2d_core::ir::cubism::{LookAxis, LookBlend, LookParameter, LookRig};
use a2d_unity::serialized::Object;
use a2d_unity::{behaviour_body, behaviour_owner_name, ClassId, Endian, Inventory, PPtr, Reader, SerializedFile};

pub(super) const LOOK_CONTROLLER: &str = "CubismLookController";
pub(super) const LOOK_PARAMETER: &str = "CubismLookParameter";
/// The SDK's standard look target (full name), which returns its own
/// transform's position.
pub(super) const LOOK_TARGET: &str = "Live2D.Cubism.Framework.LookAt.CubismLookTargetBehaviour";

/// SDK components known not to implement the look-target interface: class names
/// in the `Live2D.Cubism.Core`, `.Framework` and `.Rendering` namespaces. A
/// closed list; anything not on it (samples included) is unknown.
const NON_TARGET_SDK: &[&str] = &[
    "CubismModel",
    "CubismParameter",
    "CubismPart",
    "CubismDrawable",
    "CubismLookController",
    "CubismLookParameter",
    "CubismUpdateController",
    "CubismParameterStore",
    "CubismRenderController",
    "CubismRenderer",
    "CubismMaskController",
    "CubismRaycastable",
    "CubismHitDrawable",
    "CubismFadeController",
    "CubismFadeStateObserver",
    "CubismEyeBlinkController",
    "CubismEyeBlinkParameter",
    "CubismAutoEyeBlinkInput",
    "CubismMouthController",
    "CubismMouthParameter",
    "CubismAutoMouthInput",
    "CubismHarmonicMotionController",
    "CubismHarmonicMotionParameter",
    "CubismBreathController",
    "CubismBreathParameter",
    "CubismPhysicsController",
    "CubismPoseController",
    "CubismPosePart",
    "CubismExpressionController",
];

fn is_non_target_sdk(s: &a2d_unity::ScriptInfo) -> bool {
    ["Live2D.Cubism.Core", "Live2D.Cubism.Framework", "Live2D.Cubism.Rendering"]
        .iter()
        .any(|ns| s.namespace == *ns || s.namespace.starts_with(&format!("{ns}.")))
        && NON_TARGET_SDK.contains(&s.class_name.as_str())
}

/// What recovery found: the rig (not yet checked against the model) or the
/// reason there is none. Both `None` means the bundle holds no look component.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Recovery {
    pub rig: Option<LookRig>,
    pub failure: Option<String>,
    /// Reasons the recovered rig's numbers may differ from the source's (the
    /// center depends on the SDK generation). Empty when none; a non-empty list
    /// keeps the import line a failing note.
    pub caveats: Vec<String>,
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
    match build(file, inventory, &controllers, &parameters, &model_roots) {
        Ok((rig, caveats)) => Recovery { rig: Some(rig), failure: None, caveats },
        Err(reason) => Recovery { rig: None, failure: Some(reason), caveats: Vec::new() },
    }
}

fn build(
    file: &SerializedFile,
    inventory: &Inventory,
    controllers: &[i64],
    parameters: &[i64],
    model_roots: &[i64],
) -> Result<(LookRig, Vec<String>), String> {
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

    if !behaviour_enabled(file, object).map_err(|e| format!("the controller's enabled flag is unreadable: {e}"))? {
        return Err("the controller is disabled, so the source applies no look-at".into());
    }
    let controller_go =
        game_object_of(file, object).map_err(|e| format!("the controller's owner is unreadable: {e}"))?;
    let controller_tf = transform_of_game_object(file, controller_go)
        .map_err(|e| format!("the controller's own transform was not found: {e}"))?;
    if !model_roots.contains(&controller_go) {
        return Err("the controller's GameObject has no CubismModel (the SDK looks for the model on that GameObject only, so the source finds none and applies nothing)".into());
    }
    if !active_in_hierarchy(file, controller_go).map_err(|e| format!("the controller's active state: {e}"))? {
        return Err(
            "the controller's GameObject or one of its ancestors is inactive, so the source applies no look-at".into(),
        );
    }
    if target.is_null() {
        return Err("the controller has no target, so the source applies nothing".into());
    }
    check_target(file, inventory, target)?;
    let rest_target = position_in(file, target, controller_tf).map_err(|e| format!("target: {e}"))?;
    let mut caveats = Vec::new();
    let generations = "the SDK generations disagree on the center (older: a null Center is the controller's own transform and a Transform center is subtracted by its m_LocalPosition; newer: a null Center and a Transform center are zero)";
    let center = if center.is_null() {
        let own = read_node(file, controller_tf)?.position;
        if own != [0.0; 3] {
            caveats.push(format!(
                "{generations}; the controller's m_LocalPosition is ({}, {}, {}), not zero",
                own[0], own[1], own[2]
            ));
        }
        // The newer SDK's Start also does `Center = GetComponent<ICubismLookCenter>()`
        // on the controller's GameObject when Center is null. The SDK's only
        // implementers are `CubismLookCenterTransform` and `CubismLookCenterArtMesh`
        // (the latter gives a non-zero offset); game scripts may implement it too.
        // The closed `NON_TARGET_SDK` list qualifies here as well (none of its
        // entries is a CubismLookCenter* class); any other component, an
        // unresolved script included, may be the center.
        let (components, _) =
            game_object_state(file, controller_go).map_err(|e| format!("the controller's components: {e}"))?;
        let mut maybe_center = Vec::new();
        for &id in &components {
            if !file.find_object(id).is_some_and(|o| o.class_id == ClassId::MONO_BEHAVIOUR) {
                continue;
            }
            match inventory.objects.iter().find(|i| i.path_id == id).and_then(|i| i.script.as_ref()) {
                // The standard look target implements only the target interface.
                Some(s) if is_non_target_sdk(s) || s.full_name() == LOOK_TARGET => {}
                Some(s) => maybe_center.push(s.full_name()),
                None => maybe_center.push("a script that did not resolve".to_string()),
            }
        }
        if !maybe_center.is_empty() {
            caveats.push(format!(
                "{generations}; the controller's GameObject also carries {}, which may implement ICubismLookCenter and supply the center when Center is null",
                maybe_center.join(", ")
            ));
        }
        (0.0, 0.0, 0.0)
    } else {
        let c = local_position(file, center).map_err(|e| format!("center: {e}"))?;
        let is_transform = file.find_object(center.path_id).is_some_and(|o| o.class_id == ClassId::TRANSFORM);
        if c != (0.0, 0.0, 0.0) || !is_transform {
            caveats.push(format!(
                "{generations}; the center is {} at ({}, {}, {}), not a Transform at the origin",
                if is_transform { "a Transform" } else { "not a Transform" },
                c.0,
                c.1,
                c.2
            ));
        }
        c
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
    Ok((LookRig { blend, damping, rest_target, center, parameters: driven }, caveats))
}

/// `m_Enabled` of a behaviour: the byte right after its owner's `PPtr`.
fn behaviour_enabled(file: &SerializedFile, object: &Object) -> Result<bool, String> {
    let data = file.object_data(object).map_err(|e| e.to_string())?;
    data.get(12).map(|b| *b != 0).ok_or_else(|| "the behaviour is shorter than its header".to_string())
}

/// A `GameObject`'s component list and `m_IsActive` (the byte after the name
/// and the `u16` tag).
fn game_object_state(file: &SerializedFile, go: i64) -> Result<(Vec<i64>, bool), String> {
    let object = file.find_object(go).filter(|o| o.class_id == ClassId::GAME_OBJECT).ok_or("not a GameObject")?;
    let data = file.object_data(object).map_err(|e| e.to_string())?;
    let mut r = Reader::new(data, Endian::Little);
    let err = |e: a2d_core::DecodeError| format!("a GameObject is unreadable: {e}");
    let n = r.i32().map_err(err)?;
    if !(0..=100_000).contains(&n) {
        return Err(format!("a GameObject declares {n} components"));
    }
    let mut components = Vec::new();
    for _ in 0..n {
        let c = pptr(&mut r).map_err(err)?;
        if c.is_local() {
            components.push(c.path_id);
        }
    }
    r.i32().map_err(err)?; // m_Layer
    r.string().map_err(err)?; // m_Name
    r.u16().map_err(err)?; // m_Tag
    let active = r.u8().map_err(err)? != 0;
    Ok((components, active))
}

/// Whether a `GameObject` and every ancestor up to the root is active. `Err`
/// when the chain cannot be read to the root.
fn active_in_hierarchy(file: &SerializedFile, go: i64) -> Result<bool, String> {
    let mut go = go;
    for _ in 0..256 {
        let (_, active) = game_object_state(file, go)?;
        if !active {
            return Ok(false);
        }
        let node = read_node(file, transform_of_game_object(file, go)?)?;
        if node.father.file_index == 0 && node.father.path_id == 0 {
            return Ok(true);
        }
        if !node.father.is_local() {
            return Err("a parent is outside this file".into());
        }
        let parent = read_node(file, node.father.path_id)?;
        if !parent.owner.is_local() {
            return Err("a transform has no local owner".into());
        }
        go = parent.owner.path_id;
    }
    Err("transform parents loop".into())
}

/// Whether the source would read this target: the standard look-target script,
/// enabled, on an active `GameObject` (ancestors included). Anything else is an
/// `Err` that says whether the source applies nothing or the target cannot be
/// told.
fn check_target(file: &SerializedFile, inventory: &Inventory, target: PPtr) -> Result<(), String> {
    if !target.is_local() {
        return Err(format!(
            "target: points outside this file (file {}, path {}), so its look-target script cannot be checked",
            target.file_index, target.path_id
        ));
    }
    let object = file.find_object(target.path_id).ok_or("target: points at an object this file does not hold")?;
    let script_of = |id: i64| inventory.objects.iter().find(|i| i.path_id == id).and_then(|i| i.script.as_ref());
    let unresolved = || "a script that did not resolve".to_string();
    let (go, standard): (i64, i64) = if object.class_id == ClassId::MONO_BEHAVIOUR {
        // `self as ICubismLookTarget`: the behaviour itself must be the standard one.
        let go = game_object_of(file, object).map_err(|e| format!("target: {e}"))?;
        match script_of(object.path_id) {
            Some(s) if s.full_name() == LOOK_TARGET => (go, object.path_id),
            Some(s) => return Err(non_standard(&[s.full_name()])),
            None => return Err(non_standard(&[unresolved()])),
        }
    } else if object.class_id == ClassId::GAME_OBJECT {
        let (components, _) = game_object_state(file, object.path_id).map_err(|e| format!("target: {e}"))?;
        let mut standard = Vec::new();
        let mut other = Vec::new();
        for &id in &components {
            if !file.find_object(id).is_some_and(|o| o.class_id == ClassId::MONO_BEHAVIOUR) {
                continue;
            }
            match script_of(id) {
                Some(s) if s.full_name() == LOOK_TARGET => standard.push(id),
                Some(s) if is_non_target_sdk(s) => {}
                // Anything else may implement the interface and be answered first.
                Some(s) => other.push(s.full_name()),
                None => other.push(unresolved()),
            }
        }
        if !other.is_empty() {
            return Err(non_standard(&other));
        }
        match standard.as_slice() {
            [one] => (object.path_id, *one),
            [] => return Err("the target carries no look-target component, so the source applies nothing".into()),
            _ => {
                return Err(
                    "the target carries several standard look-target components; which one is read is not known".into(),
                )
            }
        }
    } else {
        return Err(format!(
            "the target pointer names a {}, not a GameObject or a look-target behaviour, so the source finds no look target and applies nothing",
            object.class_id.name()
        ));
    };
    let standard_object = file.find_object(standard).ok_or("target: the look-target component vanished")?;
    if !behaviour_enabled(file, standard_object).map_err(|e| format!("the target's enabled flag is unreadable: {e}"))? {
        return Err("the target's look-target behaviour is disabled, so the source applies nothing".into());
    }
    if !active_in_hierarchy(file, go).map_err(|e| format!("the target's active state: {e}"))? {
        return Err("the target's GameObject or one of its ancestors is inactive, so the source applies nothing".into());
    }
    Ok(())
}

fn non_standard(names: &[String]) -> String {
    format!(
        "the target's look-target script is not the SDK's {LOOK_TARGET} ({}); it may be game or sample code the bundle cannot show, so the rig cannot be verified",
        names.join(", ")
    )
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
    owner: PPtr,
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
    let owner = pptr(&mut r).map_err(err)?;
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
    Ok(Node { owner, rotation, position, scale, father })
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
        go_active(components, name, true)
    }

    fn go_active(components: &[i64], name: &str, active: bool) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, components.len() as i32);
        for c in components {
            put_pptr(&mut o, 0, *c);
        }
        put_i32(&mut o, 0);
        put_string(&mut o, name);
        o.extend_from_slice(&0u16.to_le_bytes()); // m_Tag
        o.push(active as u8);
        o.extend_from_slice(&[0; 3]);
        o
    }

    /// A behaviour with `m_Enabled` set as given.
    fn behaviour_enabled_as(owner: i64, script: i64, body: &[u8], enabled: bool) -> Vec<u8> {
        let mut b = behaviour(owner, script, body);
        b[12] = enabled as u8;
        b
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

    /// A `MonoScript` with a namespace of the caller's choosing.
    fn script_in(class: &str, namespace: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, class);
        put_i32(&mut o, 0);
        o.extend_from_slice(&[0; 16]);
        put_string(&mut o, class);
        put_string(&mut o, namespace);
        put_string(&mut o, "Live2D");
        o
    }

    /// The SDK's standard look target.
    fn target_script() -> Vec<u8> {
        script_in("CubismLookTargetBehaviour", "Live2D.Cubism.Framework.LookAt")
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
        scene_edited(root, middle, target_position, controller, parameters, |_| {})
    }

    /// Replaces the object with path id `id`.
    fn set(objects: &mut [(i64, usize, Vec<u8>)], id: i64, bytes: Vec<u8>) {
        objects.iter_mut().find(|o| o.0 == id).expect("an object to replace").2 = bytes;
    }

    /// The full scene: model root (go 1, transform 2, components: transform,
    /// controller 20, and a stock target 21 so the root can be a target),
    /// node (go 3, transform 4), target GameObject 5 (transform 6, the
    /// standard look-target behaviour 30), then the parameters. `edit` may
    /// change anything before the file is built.
    fn scene_edited(
        root: Node3,
        middle: Node3,
        target_position: [f32; 3],
        controller: Vec<u8>,
        parameters: &[(&str, Vec<u8>)],
        edit: impl FnOnce(&mut Vec<(i64, usize, Vec<u8>)>),
    ) -> Recovery {
        let mut objects = vec![
            (1, 0, go(&[2, 20, 21, 22], "Model")),
            (2, 3, transform(1, root.0, root.1, root.2, 0)),
            (3, 0, go(&[4], "Node")),
            (4, 3, transform(3, middle.0, middle.1, middle.2, 2)),
            (5, 0, go(&[6, 30], "LookAtTarget")),
            (6, 3, transform(5, ID, target_position, ONE, 4)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (11, 1, mono_script(LOOK_PARAMETER)),
            (12, 1, target_script()),
            (14, 1, mono_script("CubismModel")),
            (20, 2, behaviour(1, 10, &controller)),
            (21, 2, behaviour(1, 12, &[])),
            (22, 2, behaviour(1, 14, &[])),
            (30, 2, behaviour(5, 12, &[])),
        ];
        edit(&mut objects);
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
            scene([0.0; 3], (ID, [0.5, 0.25, 0.0], ONE), [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 3), 0.0), &[])
                .rig
                .expect("a rig");
        assert_eq!(rig.center, (0.5, 0.25, 0.0));
        let (x, y, _) = rig.rest_target;
        assert!((x - 0.6).abs() < 1e-6 && (y - 0.25).abs() < 1e-6, "{:?}", rig.rest_target);
    }

    #[test]
    fn a_target_that_is_the_controllers_own_transform_is_the_origin() {
        let rig =
            scene([3.0, 3.0, 3.0], FLAT, [0.0; 3], controller_body(1, (0, 1), (0, 0), 0.0), &[]).rig.expect("a rig");
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
        assert_eq!(why, "the controller has no target, so the source applies nothing");
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (1, 5), (0, 0), 0.0), &p));
        assert!(why.contains("outside this file"), "{why}");
        let why = failure_of(scene([0.0; 3], FLAT, [0.0; 3], controller_body(1, (0, 999), (0, 0), 0.0), &p));
        assert!(why.contains("does not hold"), "{why}");
        // A target whose parent chain never meets the controller's transform.
        let file = build(&[
            (1, 0, go(&[2], "Model")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (3, 0, go(&[4, 21], "Elsewhere")),
            (4, 3, transform(3, ID, [1.0, 0.0, 0.0], ONE, 0)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (12, 1, target_script()),
            (14, 1, mono_script("CubismModel")),
            (21, 2, behaviour(3, 12, &[])),
            (22, 2, behaviour(1, 14, &[])),
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
            scene([0.0; 3], (ID, [0.5, 0.25, 0.0], ONE), [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 6), 0.0), &[])
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
        assert_eq!(why, "the controller is disabled, so the source applies no look-at");
    }

    /// A controller under a parent, with and without the `CubismModel` beside it.
    fn parented(with_model: bool) -> Recovery {
        let mut objects = vec![
            (1, 0, go(&[2], "Outer")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (3, 0, go(&[4, 20, 21, 22], "Model")),
            (4, 3, transform(3, ID, [0.0; 3], ONE, 2)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (12, 1, mono_script("CubismModel")),
            (13, 1, target_script()),
            (22, 2, behaviour(3, 13, &[])),
            (20, 2, behaviour(3, 10, &controller_body(1, (0, 3), (0, 0), 0.0))),
        ];
        if with_model {
            objects.push((21, 2, behaviour(3, 12, &[])));
        }
        let file = build(&objects);
        recover(&file, &Inventory::build(&file))
    }

    #[test]
    fn a_controller_needs_a_cubism_model_on_its_own_game_object() {
        assert!(failure_of(parented(false)).contains("no CubismModel"));
        assert!(parented(true).rig.is_some());
    }

    /// A script outside the SDK's namespace, as game code would be.
    fn foreign_script(class: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, class);
        put_i32(&mut o, 0);
        o.extend_from_slice(&[0; 16]);
        put_string(&mut o, class);
        put_string(&mut o, "Game.Code");
        put_string(&mut o, "Assembly-CSharp");
        o
    }

    type Objects = Vec<(i64, usize, Vec<u8>)>;

    fn edited(controller: Vec<u8>, edit: impl FnOnce(&mut Objects)) -> Recovery {
        scene_edited(FLAT, FLAT, [0.1, 0.2, 0.3], controller, &[("ParamAngleX", parameter_body(0, 1.0))], edit)
    }

    fn at_target() -> Vec<u8> {
        controller_body(1, (0, 5), (0, 0), 0.0)
    }

    #[test]
    fn the_standard_enabled_active_target_is_verified() {
        // Through the GameObject and through the behaviour itself.
        for target in [5, 30] {
            let r = edited(controller_body(1, (0, target), (0, 0), 0.0), |_| {});
            assert_eq!(r.failure, None, "{target}");
            assert_eq!(r.rig.expect("a rig").rest_target, (0.1, 0.2, 0.3));
        }
    }

    #[test]
    fn an_inactive_controller_or_an_inactive_target_applies_nothing_in_the_source() {
        let why = failure_of(edited(at_target(), |o| set(o, 1, go_active(&[2, 20, 21, 22], "Model", false))));
        assert_eq!(
            why,
            "the controller's GameObject or one of its ancestors is inactive, so the source applies no look-at"
        );
        // The target's own GameObject.
        let why = failure_of(edited(at_target(), |o| set(o, 5, go_active(&[6, 30], "LookAtTarget", false))));
        assert_eq!(why, "the target's GameObject or one of its ancestors is inactive, so the source applies nothing");
        // An ancestor of the target (the node, under the root).
        let why = failure_of(edited(at_target(), |o| set(o, 3, go_active(&[4], "Node", false))));
        assert_eq!(why, "the target's GameObject or one of its ancestors is inactive, so the source applies nothing");
    }

    #[test]
    fn a_disabled_target_behaviour_applies_nothing_in_the_source() {
        let why = failure_of(edited(at_target(), |o| set(o, 30, behaviour_enabled_as(5, 12, &[], false))));
        assert_eq!(why, "the target's look-target behaviour is disabled, so the source applies nothing");
    }

    #[test]
    fn a_target_with_no_look_target_component_applies_nothing_in_the_source() {
        let why = failure_of(edited(at_target(), |o| set(o, 5, go(&[6], "LookAtTarget"))));
        assert_eq!(why, "the target carries no look-target component, so the source applies nothing");
    }

    #[test]
    fn a_non_standard_or_unresolved_target_script_cannot_be_verified() {
        // Game code instead of the standard script.
        let why = failure_of(edited(at_target(), |o| {
            o.push((40, 1, foreign_script("MyTarget")));
            set(o, 30, behaviour(5, 40, &[]));
        }));
        assert!(
            why.contains("not the SDK's Live2D.Cubism.Framework.LookAt.CubismLookTargetBehaviour (Game.Code.MyTarget)"),
            "{why}"
        );
        assert!(why.contains("cannot be verified"), "{why}");
        // Game code beside the standard script: which one answers is unknown.
        let why = failure_of(edited(at_target(), |o| {
            o.push((40, 1, foreign_script("MyTarget")));
            o.push((31, 2, behaviour(5, 40, &[])));
            set(o, 5, go(&[6, 30, 31], "LookAtTarget"));
        }));
        assert!(why.contains("Game.Code.MyTarget"), "{why}");
        // A script that does not resolve.
        let why = failure_of(edited(at_target(), |o| set(o, 30, behaviour(5, 999, &[]))));
        assert!(why.contains("a script that did not resolve"), "{why}");
        // The pointer names a non-standard behaviour directly.
        let why = failure_of(edited(controller_body(1, (0, 31), (0, 0), 0.0), |o| {
            o.push((40, 1, foreign_script("MyTarget")));
            o.push((31, 2, behaviour(5, 40, &[])));
            set(o, 5, go(&[6, 30, 31], "LookAtTarget"));
        }));
        assert!(why.contains("Game.Code.MyTarget"), "{why}");
    }

    #[test]
    fn a_check_that_cannot_be_made_is_a_failure_not_a_silent_recovery() {
        // The target's GameObject is cut short.
        let why = failure_of(edited(at_target(), |o| set(o, 5, vec![1, 0, 0, 0])));
        assert!(why.contains("unreadable"), "{why}");
        // The chain to the root leaves the file: the node's parent is in another file.
        let mut leaves = transform(3, ID, [0.0; 3], ONE, 0);
        let n = leaves.len();
        leaves[n - 12..n - 8].copy_from_slice(&1i32.to_le_bytes());
        leaves[n - 8..].copy_from_slice(&77i64.to_le_bytes());
        let why = failure_of(edited(at_target(), |o| set(o, 4, leaves)));
        assert!(why.contains("outside this file"), "{why}");
    }

    #[test]
    fn a_null_center_and_a_center_on_the_roots_own_transform_both_resolve_to_the_origin_and_agree() {
        // Null, and the root's own transform (2), the root at the origin: both
        // SDK generations give zero, so there is nothing to caveat.
        for center in [0, 2] {
            let r = scene([0.0; 3], FLAT, [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, center), 0.0), &[]);
            assert!(r.caveats.is_empty(), "{center}: {:?}", r.caveats);
            assert_eq!(r.rig.expect("a rig").center, (0.0, 0.0, 0.0), "{center}");
        }
    }

    #[test]
    fn a_null_center_is_a_caveat_when_the_controllers_game_object_may_supply_one() {
        // Only allowlisted SDK components beside the controller: no caveat.
        let r = scene([0.0; 3], FLAT, [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 0), 0.0), &[]);
        assert!(r.caveats.is_empty(), "{:?}", r.caveats);
        // An SDK center component, a game script, an unresolved script.
        for (name, script) in [
            (
                "Live2D.Cubism.Framework.LookAt.CubismLookCenterTransform",
                Some(script_in("CubismLookCenterTransform", "Live2D.Cubism.Framework.LookAt")),
            ),
            ("Game.Code.MyCenter", Some(foreign_script("MyCenter"))),
            ("a script that did not resolve", None),
        ] {
            let r = scene_edited(FLAT, FLAT, [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 0), 0.0), &[], |o| {
                let sid = if script.is_some() { 41 } else { 999 };
                if let Some(bytes) = script {
                    o.push((41, 1, bytes));
                }
                o.push((42, 2, behaviour(1, sid, &[])));
                set(o, 1, go(&[2, 20, 21, 22, 42], "Model"));
            });
            assert_eq!(r.failure, None, "{name}");
            assert_eq!(r.caveats.len(), 1, "{name}: {:?}", r.caveats);
            assert!(r.caveats[0].contains(name) && r.caveats[0].contains("ICubismLookCenter"), "{}", r.caveats[0]);
        }
    }

    #[test]
    fn a_center_the_sdk_generations_disagree_on_is_recovered_with_a_caveat() {
        // A moved root: null is zero in the newer generation and the root's
        // position in the older; a Transform at the root is that position in the
        // older and zero in the newer.
        for center in [0, 2] {
            let r = scene([2.0, 0.0, 0.0], FLAT, [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, center), 0.0), &[]);
            assert_eq!(r.failure, None);
            assert_eq!(r.caveats.len(), 1, "{center}: {:?}", r.caveats);
            assert!(r.caveats[0].contains("generations disagree"), "{}", r.caveats[0]);
            let want = if center == 0 { (0.0, 0.0, 0.0) } else { (2.0, 0.0, 0.0) };
            assert_eq!(r.rig.expect("a rig").center, want, "{center}");
        }
        // A center that is not a Transform (the root's GameObject).
        let r = scene([0.0; 3], FLAT, [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 1), 0.0), &[]);
        assert!(r.caveats[0].contains("not a Transform"), "{:?}", r.caveats);
        // A non-zero Transform center elsewhere.
        let r =
            scene([0.0; 3], (ID, [0.5, 0.0, 0.0], ONE), [0.1, 0.0, 0.0], controller_body(1, (0, 5), (0, 4), 0.0), &[]);
        assert_eq!(r.caveats.len(), 1, "{:?}", r.caveats);
    }

    #[test]
    fn a_pointer_at_a_transform_or_other_component_yields_no_target_in_the_source() {
        for target in [6, 2] {
            let why = failure_of(edited(controller_body(1, (0, target), (0, 0), 0.0), |_| {}));
            assert!(why.contains("names a Transform") && why.contains("applies nothing"), "{target}: {why}");
        }
    }

    #[test]
    fn only_the_full_name_of_the_standard_target_is_standard() {
        // A class of the same name elsewhere is game code.
        let why = failure_of(edited(at_target(), |o| {
            o.push((40, 1, script_in("CubismLookTargetBehaviour", "Game")));
            set(o, 30, behaviour(5, 40, &[]));
        }));
        assert!(why.contains("Game.CubismLookTargetBehaviour") && why.contains("cannot be verified"), "{why}");
        // The SDK's sample target is answered first when it precedes the standard one.
        let sample = || script_in("CubismLookTarget", "Live2D.Cubism.Samples.OriginalWorkflow.Demo");
        let why = failure_of(edited(at_target(), |o| {
            o.push((40, 1, sample()));
            o.push((31, 2, behaviour(5, 40, &[])));
            set(o, 5, go(&[6, 31, 30], "LookAtTarget"));
        }));
        assert!(why.contains("Samples.OriginalWorkflow.Demo.CubismLookTarget"), "{why}");
        // A named SDK component that does not implement the interface is fine.
        let r = edited(at_target(), |o| {
            o.push((31, 2, behaviour(5, 14, &[])));
            set(o, 5, go(&[6, 31, 30], "LookAtTarget"));
        });
        assert_eq!(r.failure, None);
        // Several standard components: which answers is not known.
        let why = failure_of(edited(at_target(), |o| {
            o.push((31, 2, behaviour(5, 12, &[])));
            set(o, 5, go(&[6, 30, 31], "LookAtTarget"));
        }));
        assert!(why.contains("several standard look-target components"), "{why}");
    }

    #[test]
    fn a_controller_without_a_cubism_model_on_its_own_game_object_applies_nothing_in_the_source() {
        // Parentless, but no CubismModel beside it.
        let file = build(&[
            (1, 0, go(&[2], "Model")),
            (2, 3, transform(1, ID, [0.0; 3], ONE, 0)),
            (3, 0, go(&[4, 30], "LookAtTarget")),
            (4, 3, transform(3, ID, [0.0; 3], ONE, 2)),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (12, 1, target_script()),
            (20, 2, behaviour(1, 10, &controller_body(1, (0, 3), (0, 0), 0.0))),
            (30, 2, behaviour(3, 12, &[])),
        ]);
        let why = failure_of(recover(&file, &Inventory::build(&file)));
        assert!(why.contains("has no CubismModel") && why.contains("applies nothing"), "{why}");
    }
}
