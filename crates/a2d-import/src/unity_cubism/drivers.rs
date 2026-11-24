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
//! look-at rig is recovered in full (`look.rs`) and checked against the
//! model's parameters: it is carried into the package, and its note says it
//! was recovered, or why it was not. It is still not applied during playback.
//! Hit areas are reported too: hit testing is not implemented, so a drawable
//! the source marked as hittable hits nothing here.
//! Components that only do bookkeeping (the parameter store, the update
//! controller, the fade observer, render and mask controllers, a
//! pose controller or expression controller with nothing to apply) stay
//! silent. Nothing here can fail an import: what cannot be read is a note.

use std::collections::BTreeMap;

use a2d_core::ir::cubism::{LookBlend, LookRig, Parameter};
use a2d_core::LoadReport;
use a2d_unity::{behaviour_body, behaviour_owner_name, Inventory, SerializedFile};

use super::look::{self, LOOK_CONTROLLER, LOOK_PARAMETER};
const EXPRESSION_CONTROLLER: &str = "CubismExpressionController";

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

/// Notes the components found and not applied, one per kind.
pub(super) fn note_scan(scan: &Scan, report: &mut LoadReport) {
    for note in notes(scan) {
        report.note(note);
    }
}

pub(super) fn scan(file: &SerializedFile, inventory: &Inventory) -> Scan {
    let mut out = Scan::default();
    let look = look::recover(file, inventory);
    out.look = look.rig;
    out.look_failure = look.failure;
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

/// The notes for a scan; empty when nothing unapplied is present.
pub(super) fn notes(scan: &Scan) -> Vec<String> {
    let mut out = Vec::new();
    let count = |name: &str| scan.classes.get(name).copied().unwrap_or(0);
    if count(LOOK_CONTROLLER) > 0 || !scan.look_ids.is_empty() {
        let mut ids: Vec<String> = scan.look_ids.iter().flatten().cloned().collect();
        ids.sort();
        ids.dedup();
        let unnamed = scan.look_ids.iter().filter(|i| i.is_none()).count();
        let mut line = match (&scan.look, &scan.look_failure) {
            (Some(rig), _) => format!(
                "look-at rig ({LOOK_CONTROLLER}) recovered: {} look parameter(s) drive [{}], {} blend, damping {} s; not yet applied, not driven by default",
                rig.parameters.len(),
                ids.join(", "),
                match rig.blend {
                    LookBlend::Additive => "additive",
                    LookBlend::Multiply => "multiply",
                    LookBlend::Override => "override",
                },
                rig.damping
            ),
            (None, _) => format!(
                "look-at rig ({LOOK_CONTROLLER}) is present and not applied: {} look parameter(s) drive [{}]",
                scan.look_ids.len(),
                ids.join(", ")
            ),
        };
        if unnamed > 0 {
            line.push_str(&format!(", {unnamed} with no readable owner"));
        }
        if let (None, Some(failure)) = (&scan.look, &scan.look_failure) {
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
    fn a_recovered_look_rig_is_one_note_naming_its_parameters_and_that_it_is_not_applied() {
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
        let n = notes(&s);
        assert_eq!(
            n,
            ["look-at rig (CubismLookController) recovered: 2 look parameter(s) drive [AngleX, EyeBallX], additive blend, damping 0.15 s; not yet applied, not driven by default"]
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
    /// Type 0 is GameObject, 1 MonoScript, 2 MonoBehaviour, 3 Transform.
    pub(in crate::unity_cubism) fn build(objects: &[(i64, usize, Vec<u8>)]) -> SerializedFile {
        let mut meta = Vec::new();
        meta.extend_from_slice(b"2022.3.0f1 ");
        put_i32(&mut meta, 5);
        meta.push(0);
        put_i32(&mut meta, 4);
        for class in [1i32, 115, 114, 4] {
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
