//! Runtime-driver components a bundle carries and the viewer does not (yet) apply.
//!
//! A Cubism Unity prefab can hold components that move parameters while it
//! runs: a look-at rig, eye blink, mouth, harmonic motion, breath, an
//! expression list, pose parts. None of them is applied here; what a motion
//! keys is played, and nothing else drives a parameter. Ignoring them without
//! a word would let a model look different from its source with no trace, so
//! each one found is named in one note.
//!
//! Recognition is by the script's class name (the names are the public Cubism
//! Unity SDK's) and by presence only, with two exceptions. The pointer of an
//! expression controller is read, to tell an empty list from a set one. The
//! look-at rig is recovered in full (`look.rs`), only when the source would
//! apply it, and checked against the model's parameters: it is carried into the
//! package and reported as information (nothing is dropped), or, when it is not
//! recovered, as a note that says why. Playback applies it at its rest target
//! and follows a pointer when the host drives it.
//! Hit areas are reported too: hit testing is not implemented, so a drawable
//! the source marked as hittable hits nothing here.
//! Components that only do bookkeeping (the parameter store, the update
//! controller, the fade observer, render and mask controllers, a
//! pose controller or expression controller with nothing to apply) stay
//! silent. Nothing here can fail an import: what cannot be read is a note.

use std::collections::BTreeMap;

use a2d_core::ir::cubism::{LookAxis, LookBlend, LookRig, Parameter};
use a2d_core::LoadReport;
use a2d_unity::{behaviour_body, behaviour_owner_name, ClassId, Inventory, SerializedFile};

use super::look::{self, LOOK_CONTROLLER, LOOK_PARAMETER};
const EXPRESSION_CONTROLLER: &str = "CubismExpressionController";
const FADE_CONTROLLER: &str = "CubismFadeController";

/// A driver kind reported only when present, with the classes that mark it.
const KINDS: &[(&str, &[&str])] = &[
    ("eye blink", &["CubismEyeBlinkController", "CubismEyeBlinkParameter", "CubismAutoEyeBlinkInput"]),
    ("mouth", &["CubismMouthController", "CubismMouthParameter", "CubismAudioMouthInput", "CubismAutoMouthInput"]),
    ("harmonic motion", &["CubismHarmonicMotionController", "CubismHarmonicMotionParameter"]),
    ("breath", &["CubismBreathController"]),
    ("pose part", &["CubismPosePart"]),
    ("hit area", &["CubismHitDrawable"]),
];

/// What a bundle's components say, apart from the bundle.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Scan {
    /// Instances per script class name.
    pub classes: BTreeMap<String, usize>,
    /// The ids the look parameters drive, in bundle order; `None` where the
    /// owner could not be named.
    pub look_ids: Vec<Option<String>>,
    /// Whether any expression controller points at a list.
    pub expression_set: bool,
    /// Whether any expression controller's pointer could not be read.
    pub expression_unreadable: bool,
    /// The look rig recovered from the components, while it is still usable.
    pub look: Option<LookRig>,
    /// Why there is no rig although look components are present.
    pub look_failure: Option<String>,
    /// Reasons the recovered rig may differ from the source (see `look.rs`).
    pub look_caveats: Vec<String>,
}

impl Scan {
    /// Checks the recovered rig against the model's parameters; a rig the
    /// model cannot use is dropped and the reason kept for the note.
    pub fn validate_look(&mut self, parameters: &[Parameter]) {
        if let Some(problem) = self.look.as_ref().and_then(|rig| rig.problem(parameters)) {
            self.look = None;
            self.look_failure = Some(format!("the rig does not fit the model: {problem}"));
        }
    }
}

/// Reports the components found, one line per kind. Every line is a note except
/// a recovered look-at rig, which is information only when there are no
/// caveats and the offset at rest is exactly zero (and the blend is additive):
/// `look::recover` hands back a rig only after verifying that the source would
/// apply it (controller and target enabled and active, the target the SDK's
/// standard one), and [`look_unverified`] lists the remaining doubts: the SDK
/// generations' disagreement on the center, and whether the Cubism Core clamps
/// a parameter the way playback does. With none, the rig adds nothing at rest,
/// so playback and source agree.
pub(super) fn note_scan(scan: &Scan, report: &mut LoadReport) {
    if let Some(info) = look_info(scan) {
        report.info(info);
    }
    for note in notes(scan) {
        report.note(note);
    }
}

/// Whether the source may fade its motions with authored times that this
/// bundle's clips do not carry: a `CubismFadeController` points at a fade-motion
/// list (the data lives in other objects, possibly of another bundle), or one
/// cannot be read to tell. `false` only when there is no such controller and
/// every one there is has a null list.
///
/// Every doubt fails toward `true` (authored fades possible):
///
/// * the list is taken to be the controller's first serialized field, after the
///   behaviour header, and the body must hold that one `PPtr` and nothing else.
///   Nothing in this repository records the controller's layout, so a body with
///   anything left over is not a layout this knows and counts as unreadable;
/// * a `MonoBehaviour` whose script did not resolve (an external or unreadable
///   `MonoScript`, so invisible to a search by class name) may be a fade
///   controller, so one existing at all counts.
pub(super) fn fades_may_be_authored(file: &SerializedFile, inventory: &Inventory) -> bool {
    if inventory.by_class(ClassId::MONO_BEHAVIOUR).any(|info| info.script.is_none()) {
        return true;
    }
    inventory.by_script(FADE_CONTROLLER).any(|info| {
        let list = file.find_object(info.path_id).ok_or(()).and_then(|object| {
            behaviour_body(file, object)
                .and_then(|(mut r, _)| {
                    let file_index = r.i32()?;
                    let path = r.i64()?;
                    Ok((file_index, path, r.remaining()))
                })
                .map_err(|_| ())
        });
        // Only a null PPtr (file 0, path 0) with nothing after it is "no list".
        !matches!(list, Ok((0, 0, 0)))
    })
}

pub(super) fn scan(file: &SerializedFile, inventory: &Inventory) -> Scan {
    let mut out = Scan::default();
    let look = look::recover(file, inventory);
    out.look = look.rig;
    out.look_failure = look.failure;
    out.look_caveats = look.caveats;
    for info in &inventory.objects {
        let Some(script) = &info.script else { continue };
        *out.classes.entry(script.class_name.clone()).or_insert(0) += 1;
        let Some(object) = file.find_object(info.path_id) else { continue };
        match script.class_name.as_str() {
            LOOK_PARAMETER => {
                let owner = behaviour_owner_name(file, object).ok().flatten().filter(|n| !n.is_empty());
                out.look_ids.push(owner);
            }
            EXPRESSION_CONTROLLER => {
                // `PPtr ExpressionsList`: an `i32` file and an `i64` path id; 0 is null.
                let path = behaviour_body(file, object).and_then(|(mut r, _)| {
                    r.i32()?;
                    r.i64()
                });
                match path {
                    Ok(path) => out.expression_set |= path != 0,
                    Err(_) => out.expression_unreadable = true,
                }
            }
            _ => {}
        }
    }
    out
}

/// What keeps a recovered rig from being verified: the recovery's caveats plus
/// the clamp. The source adds the offset to the parameter and hands the raw sum
/// to the Cubism Core; whether the Core clamps it is not established, while
/// playback clamps (or wraps) after adding. That cannot matter when the offset
/// is zero, so the rig is verified only when it is additive and every look
/// parameter's offset at rest, `factor * (rest target - center)[axis]`, is
/// exactly zero.
pub(super) fn look_unverified(scan: &Scan, rig: &LookRig) -> Vec<String> {
    let mut out = scan.look_caveats.clone();
    if rig.blend != LookBlend::Additive {
        out.push(
            "the blend is not additive, and how the Cubism Core treats the source's raw value is not established"
                .into(),
        );
    }
    let goal = [rig.rest_target.0 - rig.center.0, rig.rest_target.1 - rig.center.1, rig.rest_target.2 - rig.center.2];
    let axis = |a: LookAxis| match a {
        LookAxis::X => 0,
        LookAxis::Y => 1,
        LookAxis::Z => 2,
    };
    if rig.parameters.iter().any(|p| p.factor * goal[axis(p.axis)] != 0.0) {
        out.push("the rig adds a non-zero offset at rest; the source passes the raw sum to the Cubism Core, whether the Core clamps it is not established, and playback clamps or wraps it".into());
    }
    out
}

fn look_description(scan: &Scan, rig: &LookRig) -> String {
    let mut ids: Vec<String> = scan.look_ids.iter().flatten().cloned().collect();
    ids.sort();
    ids.dedup();
    format!(
        "{} look parameter(s) drive [{}], {} blend, damping {} s",
        rig.parameters.len(),
        ids.join(", "),
        match rig.blend {
            LookBlend::Additive => "additive",
            LookBlend::Multiply => "multiply",
            LookBlend::Override => "override",
        },
        rig.damping
    )
}

/// The information line for a verified, recovered look-at rig; `None` when
/// there is no rig (its absence is a note, see [`notes`]) or when it is not
/// verified (see [`look_unverified`]).
pub(super) fn look_info(scan: &Scan) -> Option<String> {
    let rig = scan.look.as_ref()?;
    if !look_unverified(scan, rig).is_empty() {
        return None;
    }
    Some(format!(
        "look-at rig ({LOOK_CONTROLLER}) recovered: {}; the controller and its standard look target are enabled and active as the source requires, and the prefab-defined target gives a zero offset at rest, so nothing is added to any parameter; following a pointer is the host's option",
        look_description(scan, rig)
    ))
}

/// The notes for a scan; empty when nothing unapplied is present. A recovered,
/// verified look-at rig is not among them (see [`look_info`]).
pub(super) fn notes(scan: &Scan) -> Vec<String> {
    let mut out = Vec::new();
    let count = |name: &str| scan.classes.get(name).copied().unwrap_or(0);
    if let Some(rig) = &scan.look {
        let unverified = look_unverified(scan, rig);
        if !unverified.is_empty() {
            out.push(format!(
                "look-at rig ({LOOK_CONTROLLER}) recovered: {}; applied at its rest target, follows a pointer when the host drives it; not verified against the source: {}",
                look_description(scan, rig),
                unverified.join("; ")
            ));
        }
    } else if count(LOOK_CONTROLLER) > 0 || !scan.look_ids.is_empty() {
        let mut ids: Vec<String> = scan.look_ids.iter().flatten().cloned().collect();
        ids.sort();
        ids.dedup();
        let unnamed = scan.look_ids.iter().filter(|i| i.is_none()).count();
        let mut line = format!(
            "look-at rig ({LOOK_CONTROLLER}) is present and not applied: {} look parameter(s) drive [{}]",
            scan.look_ids.len(),
            ids.join(", ")
        );
        if unnamed > 0 {
            line.push_str(&format!(", {unnamed} with no readable owner"));
        }
        if let Some(failure) = &scan.look_failure {
            line.push_str(&format!("; not recovered: {failure}"));
        }
        out.push(line);
    }
    for (kind, classes) in KINDS {
        let present: Vec<String> =
            classes.iter().filter(|c| count(c) > 0).map(|c| format!("{c} x{}", count(c))).collect();
        if !present.is_empty() {
            out.push(format!("{kind} components are present and not applied: {}", present.join(", ")));
        }
    }
    let assets = count("CubismExpressionData");
    if scan.expression_set || assets > 0 {
        let mut line = String::from("an expression list is present and not applied (expressions are not supported)");
        if assets > 0 {
            line.push_str(&format!(": {assets} expression asset(s) in this bundle"));
        }
        out.push(line);
    }
    if scan.expression_unreadable {
        out.push(
            "an expression controller is present but its list could not be read; expressions are not applied".into(),
        );
    }
    out
}

#[cfg(test)]
pub(in crate::unity_cubism) mod tests {
    use super::*;

    fn scan_of(classes: &[(&str, usize)], look: &[Option<&str>], expr: (bool, bool)) -> Scan {
        Scan {
            classes: classes.iter().map(|(c, n)| (c.to_string(), *n)).collect(),
            look_ids: look.iter().map(|i| i.map(String::from)).collect(),
            expression_set: expr.0,
            expression_unreadable: expr.1,
            ..Scan::default()
        }
    }

    #[test]
    fn bookkeeping_components_and_an_empty_expression_list_are_silent() {
        let s = scan_of(
            &[
                ("CubismParameterStore", 1),
                ("CubismUpdateController", 1),
                ("CubismPoseController", 1),
                ("CubismRenderController", 1),
                ("CubismMaskController", 1),
                ("CubismRaycastable", 1),
                ("CubismFadeStateObserver", 1),
                (EXPRESSION_CONTROLLER, 1),
            ],
            &[],
            (false, false),
        );
        assert!(notes(&s).is_empty());
        assert!(notes(&Scan::default()).is_empty());
    }

    fn rig() -> LookRig {
        LookRig {
            blend: LookBlend::Additive,
            damping: 0.15,
            rest_target: (0.0, 0.0, 0.0),
            center: (0.0, 0.0, 0.0),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn a_recovered_look_rig_is_one_information_line_naming_its_parameters_and_the_verification() {
        let mut s =
            scan_of(&[(LOOK_CONTROLLER, 1), (LOOK_PARAMETER, 2)], &[Some("AngleX"), Some("EyeBallX")], (false, false));
        let mut r = rig();
        for id in ["AngleX", "EyeBallX"] {
            r.parameters.push(a2d_core::ir::cubism::LookParameter {
                parameter: id.into(),
                axis: a2d_core::ir::cubism::LookAxis::X,
                factor: 1.0,
            });
        }
        s.look = Some(r);
        assert!(notes(&s).is_empty(), "a verified rig is not a note");
        assert_eq!(
            look_info(&s).as_deref(),
            Some("look-at rig (CubismLookController) recovered: 2 look parameter(s) drive [AngleX, EyeBallX], additive blend, damping 0.15 s; the controller and its standard look target are enabled and active as the source requires, and the prefab-defined target gives a zero offset at rest, so nothing is added to any parameter; following a pointer is the host's option")
        );
    }

    #[test]
    fn a_look_rig_that_was_not_recovered_says_why() {
        let mut s = scan_of(&[(LOOK_PARAMETER, 2)], &[Some("AngleX"), None], (false, false));
        s.look_failure = Some("the controller is missing".into());
        let n = notes(&s);
        assert_eq!(
            n,
            ["look-at rig (CubismLookController) is present and not applied: 2 look parameter(s) drive [AngleX], 1 with no readable owner; not recovered: the controller is missing"]
        );
    }

    #[test]
    fn a_rig_the_model_cannot_use_is_dropped_with_the_reason() {
        let mut s = scan_of(&[(LOOK_CONTROLLER, 1), (LOOK_PARAMETER, 1)], &[Some("AngleX")], (false, false));
        let mut r = rig();
        r.parameters.push(a2d_core::ir::cubism::LookParameter {
            parameter: "AngleX".into(),
            axis: a2d_core::ir::cubism::LookAxis::X,
            factor: 1.0,
        });
        s.look = Some(r);
        s.validate_look(&[]);
        assert!(s.look.is_none());
        let n = notes(&s);
        assert!(
            n[0].contains("not recovered: the rig does not fit the model: parameter \"AngleX\" names no parameter"),
            "{}",
            n[0]
        );
    }

    #[test]
    fn each_optional_kind_is_noted_only_when_present() {
        for (kind, class) in [
            ("eye blink", "CubismEyeBlinkController"),
            ("eye blink", "CubismAutoEyeBlinkInput"),
            ("mouth", "CubismMouthController"),
            ("mouth", "CubismAutoMouthInput"),
            ("harmonic motion", "CubismHarmonicMotionController"),
            ("breath", "CubismBreathController"),
            ("pose part", "CubismPosePart"),
            ("hit area", "CubismHitDrawable"),
        ] {
            let n = notes(&scan_of(&[(class, 3)], &[], (false, false)));
            assert_eq!(n.len(), 1, "{class}");
            assert!(n[0].starts_with(kind) && n[0].contains(&format!("{class} x3")), "{}", n[0]);
        }
    }

    #[test]
    fn an_expression_list_that_is_set_or_unreadable_is_noted() {
        let n = notes(&scan_of(&[(EXPRESSION_CONTROLLER, 1)], &[], (true, false)));
        assert_eq!(n.len(), 1);
        assert!(n[0].contains("expression list is present") && !n[0].contains("asset"), "{}", n[0]);
        let n = notes(&scan_of(&[("CubismExpressionList", 1), ("CubismExpressionData", 4)], &[], (false, false)));
        assert!(n[0].contains("4 expression asset(s)"), "{}", n[0]);
        // One unreadable controller is not hidden by another with a set list.
        let n = notes(&scan_of(&[(EXPRESSION_CONTROLLER, 2)], &[], (true, true)));
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(n[1].contains("could not be read"), "{}", n[1]);
    }

    // --- A minimal serialized file, enough to go through `scan`. The builder
    // is a small copy of the one in `a2d-unity`'s tests, so that crate's public
    // API does not grow a test-support surface. ---

    pub(in crate::unity_cubism) fn put_i32(o: &mut Vec<u8>, v: i32) {
        o.extend_from_slice(&v.to_le_bytes());
    }
    pub(in crate::unity_cubism) fn put_string(o: &mut Vec<u8>, s: &str) {
        put_i32(o, s.len() as i32);
        o.extend_from_slice(s.as_bytes());
        while o.len() % 4 != 0 {
            o.push(0);
        }
    }
    pub(in crate::unity_cubism) fn put_pptr(o: &mut Vec<u8>, file: i32, path: i64) {
        put_i32(o, file);
        o.extend_from_slice(&path.to_le_bytes());
    }

    /// `(path_id, type_index, bytes)` into a version-17 file with no type tree.
    /// Type 0 is GameObject, 1 MonoScript, 2 MonoBehaviour, 3 Transform, 4 Animator.
    pub(in crate::unity_cubism) fn build(objects: &[(i64, usize, Vec<u8>)]) -> SerializedFile {
        let mut meta = Vec::new();
        meta.extend_from_slice(b"2022.3.0f1\0");
        put_i32(&mut meta, 5);
        meta.push(0);
        put_i32(&mut meta, 5);
        for class in [1i32, 115, 114, 4, 95] {
            put_i32(&mut meta, class);
            meta.push(0);
            meta.extend_from_slice(&(-1i16).to_le_bytes());
            if class == 114 {
                meta.extend_from_slice(&[0; 16]);
            }
            meta.extend_from_slice(&[0; 16]);
        }
        put_i32(&mut meta, objects.len() as i32);
        let mut data = Vec::new();
        for (path_id, type_index, bytes) in objects {
            while (20 + meta.len()) % 4 != 0 {
                meta.push(0);
            }
            meta.extend_from_slice(&path_id.to_le_bytes());
            meta.extend_from_slice(&(data.len() as u32).to_le_bytes());
            meta.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            put_i32(&mut meta, *type_index as i32);
            data.extend_from_slice(bytes);
        }
        put_i32(&mut meta, 0);
        put_i32(&mut meta, 0);
        let mut file = Vec::new();
        file.extend_from_slice(&0u32.to_be_bytes());
        file.extend_from_slice(&0u32.to_be_bytes());
        file.extend_from_slice(&17u32.to_be_bytes());
        file.extend_from_slice(&((20 + meta.len()) as u32).to_be_bytes());
        file.extend_from_slice(&[0, 0, 0, 0]);
        file.extend_from_slice(&meta);
        file.extend_from_slice(&data);
        SerializedFile::parse(&file).expect("the synthetic file should parse")
    }

    pub(in crate::unity_cubism) fn game_object(name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, 0);
        put_i32(&mut o, 0);
        put_string(&mut o, name);
        o
    }

    pub(in crate::unity_cubism) fn mono_script(class: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, class);
        put_i32(&mut o, 0);
        o.extend_from_slice(&[0; 16]);
        put_string(&mut o, class);
        put_string(&mut o, "Live2D.Cubism.Framework");
        put_string(&mut o, "Live2D");
        o
    }

    pub(in crate::unity_cubism) fn behaviour(owner: i64, script: i64, body: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        put_pptr(&mut o, 0, owner);
        o.extend_from_slice(&[1, 0, 0, 0]);
        put_pptr(&mut o, 0, script);
        put_string(&mut o, "");
        o.extend_from_slice(body);
        o
    }

    #[test]
    fn authored_fades_are_possible_only_with_a_fade_controller_that_names_a_list_or_cannot_be_read() {
        let state = |body: Option<Vec<u8>>| {
            let mut objects = vec![(10, 1, mono_script(FADE_CONTROLLER))];
            if let Some(body) = body {
                objects.push((20, 2, behaviour(0, 10, &body)));
            }
            let file = build(&objects);
            fades_may_be_authored(&file, &Inventory::build(&file))
        };
        let pptr = |path: i64| {
            let mut o = Vec::new();
            put_pptr(&mut o, 0, path);
            o
        };
        assert!(!state(None), "no controller: nothing authored");
        assert!(!state(Some(pptr(0))), "a null list: nothing authored");
        assert!(state(Some(pptr(7))), "a list is named: its times may be authored");
        assert!(state(Some(Vec::new())), "unreadable: cannot be shown to hold no list");
        // A null list followed by more serialized data is not the layout known.
        let mut longer = pptr(0);
        longer.extend_from_slice(&[0; 4]);
        assert!(state(Some(longer)), "bytes left after the one field: cannot be trusted");

        // A behaviour whose script did not resolve may be a fade controller.
        let file = build(&[(20, 2, behaviour(0, 99, &pptr(0)))]);
        assert!(fades_may_be_authored(&file, &Inventory::build(&file)));
        // ... even beside a fade controller that names no list.
        let file = build(&[
            (10, 1, mono_script(FADE_CONTROLLER)),
            (20, 2, behaviour(0, 10, &pptr(0))),
            (21, 2, behaviour(0, 98, &[])),
        ]);
        assert!(fades_may_be_authored(&file, &Inventory::build(&file)));
        // No fade controller class at all.
        let file = build(&[(10, 1, mono_script(EXPRESSION_CONTROLLER))]);
        assert!(!fades_may_be_authored(&file, &Inventory::build(&file)));
    }

    #[test]
    fn a_verified_look_rig_is_information_and_never_fails_a_report() {
        use a2d_core::Degradation;
        let mut scan = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("ParamAngleX")], (false, false));
        scan.look = Some(rig());
        let mut report = LoadReport::new();
        note_scan(&scan, &mut report);
        assert_eq!(report.len(), 1, "{report}");
        assert!(report.to_string().contains("look-at rig (CubismLookController) recovered"), "{report}");
        assert!(matches!(report.warnings()[0], Degradation::Info(_)), "{report}");
    }

    fn driven(axis: a2d_core::ir::cubism::LookAxis, factor: f32) -> a2d_core::ir::cubism::LookParameter {
        a2d_core::ir::cubism::LookParameter { parameter: "AngleX".into(), axis, factor }
    }

    fn report_of(scan: &Scan) -> LoadReport {
        let mut report = LoadReport::new();
        note_scan(scan, &mut report);
        report
    }

    #[test]
    fn a_recovered_rig_is_information_only_when_its_offset_at_rest_is_exactly_zero() {
        use a2d_core::ir::cubism::LookAxis;
        use a2d_core::Degradation;
        // Zero because the target equals the center on the driven axis, or the factor is zero.
        for (rest, center, axis, factor) in [
            ((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), LookAxis::X, 3.0),
            ((0.5, 0.0, 0.0), (0.5, 0.0, 0.0), LookAxis::X, 3.0),
            ((0.5, 0.0, 0.0), (0.0, 0.0, 0.0), LookAxis::Y, 3.0),
            ((0.5, 0.2, 0.0), (0.0, 0.0, 0.0), LookAxis::Z, 1.0),
            ((0.5, 0.0, 0.0), (0.0, 0.0, 0.0), LookAxis::X, 0.0),
        ] {
            let mut s = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("AngleX")], (false, false));
            let mut r = rig();
            r.rest_target = rest;
            r.center = center;
            r.parameters.push(driven(axis, factor));
            s.look = Some(r);
            let report = report_of(&s);
            assert!(matches!(report.warnings()[0], Degradation::Info(_)), "{rest:?} {center:?}: {report}");
        }
        // Non-zero on a driven axis: a failing note, rig still recovered, clamp named.
        for (rest, axis) in [((0.5, 0.0, 0.0), LookAxis::X), ((0.0, 0.0, 1e-7), LookAxis::Z)] {
            let mut s = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("AngleX")], (false, false));
            let mut r = rig();
            r.rest_target = rest;
            r.parameters.push(driven(axis, 1.0));
            s.look = Some(r);
            let report = report_of(&s);
            assert_eq!(report.len(), 1, "{report}");
            assert!(matches!(report.warnings()[0], Degradation::Note(_)), "{report}");
            let text = report.to_string();
            assert!(text.contains("non-zero offset at rest") && text.contains("clamp"), "{text}");
            assert!(text.contains("applied at its rest target"), "{text}");
        }
    }

    #[test]
    fn a_non_additive_or_generation_dependent_rig_is_a_failing_note_even_when_zero_at_rest() {
        use a2d_core::Degradation;
        let mut s = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("AngleX")], (false, false));
        let mut r = rig();
        r.blend = LookBlend::Override;
        s.look = Some(r);
        let report = report_of(&s);
        assert!(matches!(report.warnings()[0], Degradation::Note(_)), "{report}");
        assert!(report.to_string().contains("not additive"), "{report}");

        let mut s = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("AngleX")], (false, false));
        s.look = Some(rig());
        s.look_caveats = vec!["the SDK generations disagree on the center".into()];
        let report = report_of(&s);
        assert_eq!(report.len(), 1, "{report}");
        assert!(matches!(report.warnings()[0], Degradation::Note(_)), "{report}");
        assert!(report.to_string().contains("generations disagree"), "{report}");
    }

    #[test]
    fn a_look_rig_that_is_not_recovered_stays_a_failing_note() {
        use a2d_core::Degradation;
        for why in [
            "the controller has no target, so the source applies nothing",
            "the target's look-target script is not the SDK's CubismLookTargetBehaviour (Game.Foo)",
            "the controller's active state: a GameObject is unreadable",
        ] {
            let mut scan = scan_of(&[(LOOK_CONTROLLER, 1)], &[Some("ParamAngleX")], (false, false));
            scan.look_failure = Some(why.into());
            let mut report = LoadReport::new();
            note_scan(&scan, &mut report);
            assert_eq!(report.len(), 1, "{report}");
            assert!(matches!(report.warnings()[0], Degradation::Note(_)), "{report}");
            assert!(report.to_string().contains(&format!("not recovered: {why}")), "{report}");
        }
    }

    #[test]
    fn a_synthetic_bundle_goes_through_scan_to_the_exact_notes() {
        let mut null_pptr = Vec::new();
        put_pptr(&mut null_pptr, 0, 0);
        let mut axis = Vec::new();
        put_i32(&mut axis, 0);
        axis.extend_from_slice(&1.0f32.to_le_bytes());
        let file = build(&[
            (1, 0, game_object("ParamAngleX")),
            (10, 1, mono_script(LOOK_CONTROLLER)),
            (11, 1, mono_script(LOOK_PARAMETER)),
            (12, 1, mono_script(EXPRESSION_CONTROLLER)),
            (13, 1, mono_script("CubismHitDrawable")),
            (14, 1, mono_script("CubismParameterStore")),
            (20, 2, behaviour(0, 10, &[0; 32])),
            (21, 2, behaviour(1, 11, &axis)),
            (22, 2, behaviour(0, 11, &axis)),
            (23, 2, behaviour(0, 12, &null_pptr)),
            (24, 2, behaviour(0, 13, &[0; 4])),
            (25, 2, behaviour(0, 14, &[])),
        ]);
        let inventory = Inventory::build(&file);
        let mut report = LoadReport::default();
        note_scan(&scan(&file, &inventory), &mut report);
        let got: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(
            got,
            [
                "look-at rig (CubismLookController) is present and not applied: 2 look parameter(s) drive [ParamAngleX], 1 with no readable owner; not recovered: the controller's owner is unreadable: a component has no local owner",
                "hit area components are present and not applied: CubismHitDrawable x1",
            ]
        );
    }
}
