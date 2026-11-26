//! Turning a decoded animator controller into the default motion and the
//! handovers between motions.
//!
//! Everything Unity-specific ends here: what leaves is a motion name and a
//! `{ at, mix }` in seconds. The rules are deliberately narrow. A controller
//! is only read when it has the shape the real exports have -- one layer, a
//! default state playing a single clip, and states that each fall back to
//! another single-clip state at a fixed point of their clip. Anything else is
//! named in one note and produces no handover; nothing is guessed or clamped.

use a2d_core::ir::cubism::MotionHandover;
use a2d_core::{Handover, LoadReport};
use a2d_unity::animator_controller::{AnimatorController, State, SELECTOR_DESTINATION_BASE};

/// The motion a controller clip became, resolved by the caller.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClipMotion {
    pub name: String,
    /// Seconds in one pass of the recovered motion.
    pub duration: f32,
}

/// Why a controller clip has no motion to hand over to or from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipGap {
    /// The pointer leaves the bundle's serialized file.
    OtherFile,
    /// The clip's motion came from fade data, which is not tied to the clip
    /// object.
    FromFadeData,
    /// The clip failed to decode, or yielded no motion.
    NotConverted,
}

impl std::fmt::Display for ClipGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ClipGap::OtherFile => "plays a clip stored in another file",
            ClipGap::FromFadeData => "plays a clip whose motion came from fade data and cannot be tied to the clip",
            ClipGap::NotConverted => "plays a clip that was not converted to a motion",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Recovered {
    pub default_motion: Option<String>,
    pub handovers: Vec<MotionHandover>,
}

/// Why a state has no single motion.
enum NoMotion {
    NoClip,
    Several(usize),
    Unrecovered(ClipGap),
}

impl std::fmt::Display for NoMotion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoMotion::NoClip => f.write_str("plays no clip"),
            NoMotion::Several(n) => write!(f, "plays {n} clips"),
            NoMotion::Unrecovered(gap) => gap.fmt(f),
        }
    }
}

/// What one single-clip state came to.
struct Outcome {
    motion: String,
    state: String,
    is_default: bool,
    result: Result<Option<Handover>, String>,
}

/// Reads `controller`; `clips[i]` is the motion of `controller.clips[i]`, or
/// why that clip has none.
pub(super) fn recover(
    controller: &AnimatorController,
    clips: &[Result<ClipMotion, ClipGap>],
    report: &mut LoadReport,
) -> Recovered {
    let label = controller.name.as_str();
    if controller.layers.is_empty() {
        // A note, not information: with no default motion the viewer does not
        // play nothing, it falls back to a motion chosen by name.
        report.note(format!("controller {label:?} has no layer; no default motion or handovers are recovered"));
        return Recovered::default();
    }
    let problems = controller.unresolved_references();
    if !problems.is_empty() {
        report.note(format!(
            "controller {label:?} has unresolved references ({}); no default motion or handovers are recovered",
            problems.join("; ")
        ));
        return Recovered::default();
    }
    if controller.layers.len() > 1 {
        report.note(format!("controller {label:?} has {} layers; only the first is read", controller.layers.len()));
    }
    let machine_index = controller.layers[0].state_machine_index as usize;
    let Some(machine) = controller.state_machines.get(machine_index) else {
        return Recovered::default();
    };
    let states = &machine.states;
    let state_label = |i: usize| match controller.state_name(machine_index, i) {
        Some(name) => format!("{name:?}"),
        None => format!("state#{i}"),
    };

    let motion_of = |i: usize| -> Result<&ClipMotion, NoMotion> {
        let leaves: Vec<u32> = states[i].leaf_clip_indices().collect();
        match leaves.as_slice() {
            [] => Err(NoMotion::NoClip),
            [one] => match clips.get(*one as usize) {
                Some(Ok(m)) => Ok(m),
                Some(Err(gap)) => Err(NoMotion::Unrecovered(*gap)),
                None => Err(NoMotion::Unrecovered(ClipGap::NotConverted)),
            },
            many => Err(NoMotion::Several(many.len())),
        }
    };

    let mut out = Recovered::default();
    let default_state = machine.default_state as usize;
    if default_state < states.len() {
        match motion_of(default_state) {
            Ok(m) => {
                out.default_motion = Some(m.name.clone());
                // The name stands, but a state that plays its clip faster, slower
                // or from an offset is not the clip as recovered.
                let problems = timing_problems(&states[default_state]);
                if !problems.is_empty() {
                    report.note(format!(
                        "the default state {} has {}; the default motion is kept as the clip's own",
                        state_label(default_state),
                        problems.join(", ")
                    ));
                }
            }
            Err(why) => {
                let line = format!("no default motion: the default state {} {why}", state_label(default_state));
                // Always a note: with no default motion the viewer falls back to a
                // motion chosen by name, so it does not play what the source plays.
                report.note(line)
            }
        }
    } else {
        // A note for the same reason: the viewer still picks something to play.
        report.note("no default motion: the controller has no default state");
    }

    let mut issues: Vec<String> = Vec::new();
    let mut motionless: Vec<String> = Vec::new();
    let mut outcomes: Vec<Outcome> = Vec::new();
    for (i, state) in states.iter().enumerate() {
        let name = state_label(i);
        let source = match motion_of(i) {
            Ok(m) => m,
            Err(NoMotion::NoClip) if state.transitions.is_empty() => {
                motionless.push(name);
                continue;
            }
            Err(NoMotion::NoClip) => {
                issues.push(format!("{name} plays no clip but has {} transition(s)", state.transitions.len()));
                continue;
            }
            Err(why) => {
                issues.push(format!("{name} {why}"));
                continue;
            }
        };
        outcomes.push(Outcome {
            motion: source.name.clone(),
            is_default: i == default_state,
            result: handover_of(state, source, states, &motion_of, &state_label),
            state: name,
        });
    }

    // Judge each motion once, in the order the states first play it. A motion
    // several states play gets a handover only if every one of them gives the
    // same one: choosing between disagreeing states would be a guess.
    let mut judged: Vec<&str> = Vec::new();
    for first in &outcomes {
        if judged.contains(&first.motion.as_str()) {
            continue;
        }
        judged.push(&first.motion);
        let group: Vec<&Outcome> = outcomes.iter().filter(|o| o.motion == first.motion).collect();
        if let [one] = group.as_slice() {
            match &one.result {
                Ok(Some(h)) => out.handovers.push(MotionHandover { from: one.motion.clone(), handover: h.clone() }),
                Ok(None) if !one.is_default => {
                    issues.push(format!("{} has no transition, so nothing follows it", one.state))
                }
                Ok(None) => {}
                Err(why) => issues.push(format!("{} {why}", one.state)),
            }
            continue;
        }
        let agreed = match &group[0].result {
            Ok(Some(h)) if group.iter().all(|o| matches!(&o.result, Ok(Some(other)) if other == h)) => Some(h.clone()),
            _ => None,
        };
        if let Some(handover) = agreed {
            out.handovers.push(MotionHandover { from: first.motion.clone(), handover });
        } else if group.iter().all(|o| matches!(o.result, Ok(None))) {
            if !group.iter().any(|o| o.is_default) {
                let names: Vec<&str> = group.iter().map(|o| o.state.as_str()).collect();
                issues.push(format!(
                    "{} states play motion {:?} and none has a transition, so nothing follows it: {}",
                    group.len(),
                    first.motion,
                    names.join(", ")
                ));
            }
        } else {
            let parts: Vec<String> = group
                .iter()
                .map(|o| match &o.result {
                    Ok(Some(h)) => format!("{} (hands over at {}s)", o.state, h.at),
                    Ok(None) => format!("{} (no transition)", o.state),
                    Err(why) => format!("{} ({why})", o.state),
                })
                .collect();
            issues.push(format!(
                "{} states play motion {:?} and do not agree on one handover, so it has none: {}",
                group.len(),
                first.motion,
                parts.join(", ")
            ));
        }
    }

    if !machine.any_state_transitions.is_empty() {
        issues.push(format!("{} any-state transition(s) are not read", machine.any_state_transitions.len()));
    }
    if !issues.is_empty() {
        report.note(format!("controller {label:?}: {}", issues.join("; ")));
    }
    if !motionless.is_empty() {
        // A note: such a state may still be reached (by another state's transition
        // or an any-state transition), where the source would then play nothing,
        // and nothing here shows it is unreachable.
        report.note(format!(
            "controller {label:?} has {} state(s) with no clip and no transition: {}",
            motionless.len(),
            motionless.join(", ")
        ));
    }
    out
}

/// The handover a single-clip state gives, `Ok(None)` when it has no
/// transition, or why its transition cannot be read.
fn handover_of<'a>(
    state: &State,
    source: &ClipMotion,
    states: &[State],
    motion_of: &dyn Fn(usize) -> Result<&'a ClipMotion, NoMotion>,
    state_label: &dyn Fn(usize) -> String,
) -> Result<Option<Handover>, String> {
    let t = match state.transitions.as_slice() {
        [] => return Ok(None),
        [one] => one,
        many => return Err(format!("has {} transitions", many.len())),
    };
    let mut reasons: Vec<String> = Vec::new();
    for problem in timing_problems(state) {
        reasons.push(format!("has {problem}"));
    }
    if !t.has_exit_time {
        reasons.push("has no exit time".into());
    }
    if !t.conditions.is_empty() {
        reasons.push(format!("is conditional ({} condition(s))", t.conditions.len()));
    }
    if t.offset != 0.0 {
        reasons.push(format!("starts its target at offset {}", t.offset));
    }
    if !(t.exit_time.is_finite() && t.exit_time > 0.0 && t.exit_time <= 1.0) {
        reasons.push(format!("has exit time {}, outside (0, 1]", t.exit_time));
    }
    if !(t.duration.is_finite() && t.duration >= 0.0) {
        reasons.push(format!("has duration {}", t.duration));
    }
    let destination = t.destination as usize;
    let target = if t.destination >= SELECTOR_DESTINATION_BASE {
        reasons.push("goes to a selector, not a state".into());
        None
    } else if destination >= states.len() {
        reasons.push(format!("goes to state {destination}, which does not exist"));
        None
    } else {
        match motion_of(destination) {
            Ok(m) => {
                for problem in timing_problems(&states[destination]) {
                    reasons.push(format!("goes to {} which has {problem}", state_label(destination)));
                }
                Some(m)
            }
            Err(why) => {
                reasons.push(format!("goes to {} which {why}", state_label(destination)));
                None
            }
        }
    };
    if !(source.duration.is_finite() && source.duration > 0.0) {
        reasons.push(format!("has a motion of length {}", source.duration));
    }
    let (Some(target), true) = (target, reasons.is_empty()) else {
        return Err(reasons.join(", "));
    };

    let length = source.duration;
    // exit <= 1 was checked above, so this cannot pass the end of the motion.
    let at = t.exit_time * length;
    let mix = if t.has_fixed_duration { t.duration } else { t.duration * length };
    if !(mix.is_finite() && mix >= 0.0) {
        return Err(format!("has a mix of {mix}s"));
    }
    Ok(Some(Handover { animation: target.name.clone(), at, mix }))
}

/// Anything on a state that changes how fast or from where its clip plays. The
/// handover's `at` assumes the clip runs at its own speed from its start, so a
/// state that does otherwise is not read. A parameter id of 0 is "none" (what
/// every real controller holds).
fn timing_problems(state: &State) -> Vec<String> {
    let mut out = Vec::new();
    if state.speed != 1.0 {
        out.push(format!("speed {}", state.speed));
    }
    if state.cycle_offset != 0.0 {
        out.push(format!("cycle offset {}", state.cycle_offset));
    }
    for (id, what) in [
        (state.speed_param_id, "a speed parameter"),
        (state.cycle_offset_param_id, "a cycle-offset parameter"),
        (state.time_param_id, "a time parameter"),
    ] {
        if id != 0 {
            out.push(what.to_string());
        }
    }
    for node in state.blend_trees.iter().flat_map(|t| t.nodes.iter()).filter(|n| n.is_leaf()) {
        if node.cycle_offset != 0.0 {
            out.push(format!("a leaf cycle offset {}", node.cycle_offset));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::Degradation;
    use a2d_unity::animator_controller::{
        BlendTree, BlendTreeNode, Condition, Layer, Selector, StateMachine, Transition,
    };
    use a2d_unity::PPtr;

    type StateChange = fn(&mut State);
    use std::collections::BTreeMap;

    fn node(clip: u32) -> BlendTreeNode {
        BlendTreeNode {
            blend_type: 0,
            blend_event_id: 0,
            blend_event_y_id: 0,
            children: vec![],
            child_thresholds: vec![],
            child_positions: vec![],
            child_magnitudes: vec![],
            child_pair_vectors: vec![],
            child_pair_avg_mag_inv: vec![],
            child_neighbours: vec![],
            blend_direct_ids: vec![],
            normalized_blend_values: false,
            clip_index: clip,
            duration: 1.0,
            cycle_offset: 0.0,
            mirror: false,
        }
    }

    fn transition(dest: u32, exit: f32, duration: f32) -> Transition {
        Transition {
            conditions: vec![],
            destination: dest,
            full_path_id: 0,
            id: 0,
            user_id: 0,
            duration,
            offset: 0.0,
            exit_time: exit,
            has_exit_time: true,
            has_fixed_duration: true,
            interruption_source: 0,
            ordered_interruption: true,
            can_transition_to_self: false,
        }
    }

    fn state(name_id: u32, leaves: &[u32], transitions: Vec<Transition>) -> State {
        State {
            transitions,
            blend_tree_indices: vec![if leaves.is_empty() { -1 } else { 0 }],
            blend_trees: if leaves.is_empty() {
                vec![]
            } else {
                vec![BlendTree { nodes: leaves.iter().map(|&c| node(c)).collect() }]
            },
            name_id,
            path_id: 0,
            full_path_id: 0,
            tag_id: 0,
            speed_param_id: 0,
            mirror_param_id: 0,
            cycle_offset_param_id: 0,
            time_param_id: 0,
            speed: 1.0,
            cycle_offset: 0.0,
            ik: false,
            write_defaults: true,
            looping: false,
            mirror: false,
        }
    }

    /// State `i` is named `s{i}`; clips are `clip_count` pointers.
    fn controller(states: Vec<State>, clip_count: usize) -> AnimatorController {
        let names: BTreeMap<u32, String> = (0..states.len() as u32).map(|i| (100 + i, format!("s{i}"))).collect();
        AnimatorController {
            name: "ctl".into(),
            controller_size: 0,
            layers: vec![Layer {
                state_machine_index: 0,
                motion_set_index: 0,
                body_mask: [0; 3],
                skeleton_mask: vec![],
                binding: 0,
                blending_mode: 0,
                default_weight: 1.0,
                ik_pass: false,
                synced_layer_affects_timing: false,
            }],
            state_machines: vec![StateMachine {
                states,
                any_state_transitions: vec![],
                selectors: vec![],
                default_state: 0,
                motion_set_count: 1,
            }],
            parameters: vec![],
            default_values: Default::default(),
            names,
            clips: (0..clip_count).map(|i| PPtr { file_index: 0, path_id: 10 + i as i64 }).collect(),
            behaviour_ranges: vec![],
            behaviour_indices: vec![],
            state_machine_behaviours: vec![],
            multithreaded: false,
        }
    }

    fn motions(spec: &[(&str, f32)]) -> Vec<Result<ClipMotion, ClipGap>> {
        spec.iter().map(|(n, d)| Ok(ClipMotion { name: (*n).into(), duration: *d })).collect()
    }

    /// Default plays clip 0; state 1 plays clip 1 and returns.
    fn pair(t: Transition) -> AnimatorController {
        controller(vec![state(100, &[0], vec![]), state(101, &[1], vec![t])], 2)
    }

    fn run(c: &AnimatorController, clips: &[Result<ClipMotion, ClipGap>]) -> (Recovered, String) {
        let mut report = LoadReport::new();
        let r = recover(c, clips, &mut report);
        (r, report.to_string())
    }

    #[test]
    fn a_fixed_duration_return_becomes_a_handover_in_seconds() {
        let c = pair(transition(0, 0.9, 0.25));
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.5)]));
        assert_eq!(r.default_motion.as_deref(), Some("Idle"));
        assert_eq!(r.handovers.len(), 1);
        let h = &r.handovers[0];
        assert_eq!(h.from, "Wave");
        assert_eq!(h.handover.animation, "Idle");
        assert!((h.handover.at - 2.25).abs() < 1e-5 && (h.handover.mix - 0.25).abs() < 1e-6, "{h:?}");
        assert!(!text.contains("no transition"), "{text}");
    }

    #[test]
    fn a_normalized_duration_is_a_share_of_the_source_length() {
        let mut t = transition(0, 0.9, 0.1);
        t.has_fixed_duration = false;
        let (r, _) = run(&pair(t), &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!((r.handovers[0].handover.mix - 0.2).abs() < 1e-6, "{:?}", r.handovers);
    }

    #[test]
    fn a_normalized_mix_that_overflows_is_refused_for_that_state_only() {
        let mut t = transition(0, 0.9, 3.0e38);
        t.has_fixed_duration = false;
        let (r, text) = run(&pair(t), &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert_eq!(r.default_motion.as_deref(), Some("Idle"));
        assert!(text.contains("\"s1\" has a mix of inf"), "{text}");
    }

    #[test]
    fn an_exit_time_of_one_ends_at_the_motion_and_a_hair_over_is_refused_not_clamped() {
        let (r, _) = run(&pair(transition(0, 1.0, 0.25)), &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert_eq!(r.handovers[0].handover.at, 2.0);
        let (r, text) = run(&pair(transition(0, 1.000_001, 0.25)), &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("outside (0, 1]"), "{text}");
    }

    fn rejected(t: Transition, why: &str) {
        let (r, text) = run(&pair(t), &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty(), "{:?}", r.handovers);
        assert_eq!(r.default_motion.as_deref(), Some("Idle"), "the default survives a rejected handover");
        assert!(text.contains("\"s1\"") && text.contains(why), "want {why:?} in {text}");
    }

    #[test]
    fn each_unreadable_transition_is_named_and_gives_no_handover() {
        let mut t = transition(0, 0.9, 0.25);
        t.has_exit_time = false;
        rejected(t, "has no exit time");
        let mut t = transition(0, 0.9, 0.25);
        t.conditions.push(Condition { mode: 1, event_id: 5, threshold: 0.0, exit_time: 0.0 });
        rejected(t, "is conditional");
        let mut t = transition(0, 0.9, 0.25);
        t.offset = 0.3;
        rejected(t, "offset 0.3");
        rejected(transition(0, 0.0, 0.25), "outside (0, 1]");
        rejected(transition(0, 1.5, 0.25), "outside (0, 1]");
        rejected(transition(0, f32::NAN, 0.25), "outside (0, 1]");
        rejected(transition(0, 0.9, -1.0), "has duration -1");
    }

    /// Three states: a default and two sources returning to it; `f` edits them.
    fn three(f: impl FnOnce(&mut Vec<State>)) -> (Recovered, String) {
        let mut states = vec![
            state(100, &[0], vec![]),
            state(101, &[1], vec![transition(0, 0.9, 0.25)]),
            state(102, &[2], vec![transition(0, 0.8, 0.25)]),
        ];
        f(&mut states);
        run(&controller(states, 3), &motions(&[("Idle", 4.0), ("Wave", 2.0), ("Nod", 3.0)]))
    }

    #[test]
    fn a_selector_destination_is_refused_for_that_state_only() {
        let mut c = controller(
            vec![
                state(100, &[0], vec![]),
                state(101, &[1], vec![transition(SELECTOR_DESTINATION_BASE, 0.9, 0.25)]),
                state(102, &[2], vec![transition(0, 0.8, 0.25)]),
            ],
            3,
        );
        c.state_machines[0].selectors.push(Selector { transitions: vec![], full_path_id: 0, is_entry: true });
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0), ("Nod", 3.0)]));
        assert_eq!(r.default_motion.as_deref(), Some("Idle"));
        assert_eq!(r.handovers.len(), 1, "{:?}", r.handovers);
        assert_eq!(r.handovers[0].from, "Nod");
        assert!(text.contains("\"s1\" goes to a selector, not a state"), "{text}");
    }

    #[test]
    fn each_timing_field_of_the_source_refuses_that_state() {
        let cases: [(&str, StateChange); 7] = [
            ("speed 2", |s| s.speed = 2.0),
            ("cycle offset 0.5", |s| s.cycle_offset = 0.5),
            ("a speed parameter", |s| s.speed_param_id = 9),
            ("a cycle-offset parameter", |s| s.cycle_offset_param_id = 9),
            ("a time parameter", |s| s.time_param_id = 9),
            ("a leaf cycle offset 0.25", |s| s.blend_trees[0].nodes[0].cycle_offset = 0.25),
            ("speed NaN", |s| s.speed = f32::NAN),
        ];
        for (why, change) in cases {
            let (r, text) = three(|states| change(&mut states[1]));
            assert_eq!(r.default_motion.as_deref(), Some("Idle"), "{why}");
            assert_eq!(r.handovers.len(), 1, "{why}: {:?}", r.handovers);
            assert_eq!(r.handovers[0].from, "Nod", "{why}");
            assert!(text.contains(&format!("\"s1\" has {why}")), "want {why:?} in {text}");
        }
    }

    #[test]
    fn each_timing_field_of_the_destination_refuses_its_sources() {
        let cases: [(&str, StateChange); 6] = [
            ("speed 2", |s| s.speed = 2.0),
            ("cycle offset 0.5", |s| s.cycle_offset = 0.5),
            ("a speed parameter", |s| s.speed_param_id = 9),
            ("a cycle-offset parameter", |s| s.cycle_offset_param_id = 9),
            ("a time parameter", |s| s.time_param_id = 9),
            ("a leaf cycle offset 0.25", |s| s.blend_trees[0].nodes[0].cycle_offset = 0.25),
        ];
        for (why, change) in cases {
            let (r, text) = three(|states| change(&mut states[0]));
            // The default still names its motion; both returns into it are refused.
            assert_eq!(r.default_motion.as_deref(), Some("Idle"), "{why}");
            assert!(r.handovers.is_empty(), "{why}: {:?}", r.handovers);
            assert!(text.contains(&format!("goes to \"s0\" which has {why}")), "want {why:?} in {text}");
        }
    }

    #[test]
    fn several_transitions_or_any_state_transitions_are_not_guessed_between() {
        let mut c = pair(transition(0, 0.9, 0.25));
        c.state_machines[0].states[1].transitions.push(transition(1, 0.5, 0.25));
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("\"s1\" has 2 transitions"), "{text}");

        let mut c = pair(transition(0, 0.9, 0.25));
        c.state_machines[0].any_state_transitions.push(transition(0, 0.0, 0.1));
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert_eq!(r.handovers.len(), 1, "the state's own transition is still read");
        assert!(text.contains("1 any-state transition(s) are not read"), "{text}");
    }

    #[test]
    fn a_destination_without_exactly_one_clip_is_refused() {
        let c = controller(
            vec![state(100, &[0], vec![]), state(101, &[1], vec![transition(2, 0.9, 0.25)]), state(102, &[], vec![])],
            2,
        );
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("goes to \"s2\" which plays no clip"), "{text}");

        let c = controller(
            vec![
                state(100, &[0], vec![]),
                state(101, &[1], vec![transition(2, 0.9, 0.25)]),
                state(102, &[0, 1], vec![]),
            ],
            2,
        );
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("which plays 2 clips"), "{text}");
    }

    #[test]
    fn a_state_with_several_clips_gets_no_handover_and_the_default_may_be_one() {
        let c = controller(vec![state(100, &[0, 1], vec![]), state(101, &[1, 0], vec![transition(0, 0.9, 0.25)])], 2);
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert_eq!(r, Recovered::default());
        assert!(text.contains("no default motion: the default state \"s0\" plays 2 clips"), "{text}");
        assert!(text.contains("\"s1\" plays 2 clips"), "{text}");
    }

    #[test]
    fn a_default_state_with_no_clip_has_no_default_motion() {
        let c = controller(vec![state(100, &[], vec![]), state(101, &[0], vec![transition(0, 0.9, 0.25)])], 1);
        let (r, text) = run(&c, &motions(&[("Wave", 2.0)]));
        assert_eq!(r.default_motion, None);
        assert!(text.contains("plays no clip"), "{text}");
        // Its destination plays no clip either, so there is no handover.
        assert!(r.handovers.is_empty());
    }

    #[test]
    fn motionless_states_are_summarised_in_one_note_and_those_with_transitions_are_named() {
        let c = controller(
            vec![
                state(100, &[0], vec![]),
                state(101, &[], vec![]),
                state(102, &[], vec![transition(0, 0.75, 0.25)]),
                state(103, &[], vec![]),
            ],
            1,
        );
        let (r, text) = run(&c, &motions(&[("Idle", 4.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("2 state(s) with no clip and no transition: \"s1\", \"s3\""), "{text}");
        assert!(text.contains("\"s2\" plays no clip but has 1 transition(s)"), "{text}");
    }

    #[test]
    fn a_single_clip_state_with_no_transition_is_named_but_the_default_need_not_have_one() {
        let c = controller(vec![state(100, &[0], vec![]), state(101, &[1], vec![])], 2);
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("\"s1\" has no transition"), "{text}");
        assert!(!text.contains("\"s0\""), "{text}");
    }

    #[test]
    fn each_kind_of_clip_without_a_motion_has_its_own_reason() {
        for (gap, words) in [
            (ClipGap::OtherFile, "stored in another file"),
            (ClipGap::FromFadeData, "came from fade data"),
            (ClipGap::NotConverted, "was not converted"),
        ] {
            let mut clips = motions(&[("Idle", 4.0), ("Wave", 2.0)]);
            clips[1] = Err(gap);
            let (r, text) = run(&pair(transition(0, 0.9, 0.25)), &clips);
            assert!(r.handovers.is_empty());
            assert!(text.contains("\"s1\" plays a clip") && text.contains(words), "{text}");

            let mut clips = motions(&[("Idle", 4.0), ("Wave", 2.0)]);
            clips[0] = Err(gap);
            let (r, text) = run(&pair(transition(0, 0.9, 0.25)), &clips);
            assert_eq!(r.default_motion, None);
            assert!(text.contains("no default motion") && text.contains(words), "{text}");
        }
    }

    /// The entries a recovery reports, with whether each is information.
    fn entries(c: &AnimatorController, clips: &[Result<ClipMotion, ClipGap>]) -> Vec<(bool, String)> {
        let mut report = LoadReport::new();
        recover(c, clips, &mut report);
        report.warnings().iter().map(|d| (matches!(d, Degradation::Info(_)), d.to_string())).collect()
    }

    /// The one entry whose text contains `needle`, as `(is_info, text)`.
    fn entry(entries: &[(bool, String)], needle: &str) -> (bool, String) {
        let found: Vec<_> = entries.iter().filter(|(_, t)| t.contains(needle)).collect();
        assert_eq!(found.len(), 1, "{needle:?} in {entries:?}");
        found[0].clone()
    }

    #[test]
    fn no_controller_entry_is_information_because_the_viewer_falls_back_to_a_motion_by_name() {
        let two = motions(&[("Idle", 4.0), ("Wave", 2.0)]);
        let one = motions(&[("Wave", 2.0)]);
        let mut all: Vec<(bool, String)> = Vec::new();

        // A default state with no clip, with and without a transition; several
        // clips; a clip that could not be recovered.
        let c = controller(vec![state(100, &[], vec![]), state(101, &[0], vec![transition(0, 0.9, 0.25)])], 1);
        all.push(entry(&entries(&c, &one), "no default motion"));
        let c = controller(vec![state(100, &[], vec![transition(1, 0.9, 0.25)]), state(101, &[0], vec![])], 1);
        all.push(entry(&entries(&c, &one), "no default motion"));
        let c = controller(vec![state(100, &[0, 1], vec![]), state(101, &[1], vec![])], 2);
        all.push(entry(&entries(&c, &two), "no default motion"));
        let mut gap = two.clone();
        gap[0] = Err(ClipGap::NotConverted);
        all.push(entry(&entries(&pair(transition(0, 0.9, 0.25)), &gap), "no default motion"));

        // No layer, and no default state (a machine with no state at all; a
        // default pointing past its states is an unresolved reference instead).
        let mut c = pair(transition(0, 0.9, 0.25));
        c.layers.clear();
        all.push(entry(&entries(&c, &[]), "has no layer"));
        let c = controller(vec![], 0);
        all.push(entry(&entries(&c, &[]), "no default state"));

        // States with no clip and no transition, which another state may reach.
        let c = controller(
            vec![state(100, &[0], vec![]), state(101, &[], vec![]), state(102, &[], vec![transition(0, 0.75, 0.25)])],
            1,
        );
        let got = entries(&c, &motions(&[("Idle", 4.0)]));
        all.push(entry(&got, "no clip and no transition"));
        all.push(entry(&got, "plays no clip but has 1 transition"));

        let mut c = pair(transition(0, 0.9, 0.25));
        let layer = c.layers[0].clone();
        c.layers.push(layer);
        all.push(entry(&entries(&c, &two), "only the first is read"));
        let mut c = pair(transition(0, 0.9, 0.25));
        c.state_machines[0].states[1].transitions[0].destination = 9;
        all.push(entry(&entries(&c, &two), "unresolved references"));

        for (is_info, text) in &all {
            assert!(!is_info, "must be a failing Note: {text}");
        }
    }

    #[test]
    fn states_sharing_a_motion_keep_a_handover_only_when_they_agree_exactly() {
        let build = |second_exit: f32| {
            controller(
                vec![
                    state(100, &[0], vec![]),
                    state(101, &[1], vec![transition(0, 0.9, 0.25)]),
                    state(102, &[1], vec![transition(0, second_exit, 0.25)]),
                ],
                2,
            )
        };
        let clips = motions(&[("Idle", 4.0), ("Wave", 2.0)]);
        let (r, text) = run(&build(0.9), &clips);
        assert_eq!(r.handovers.len(), 1, "{text}");
        assert!((r.handovers[0].handover.at - 1.8).abs() < 1e-5);

        let (r, text) = run(&build(0.8), &clips);
        assert!(r.handovers.is_empty());
        assert!(text.contains("2 states play motion \"Wave\""), "{text}");
        assert!(text.contains("\"s1\" (hands over at") && text.contains("\"s2\" (hands over at"), "{text}");

        // One of them having no transition is a disagreement too.
        let c = controller(
            vec![state(100, &[0], vec![]), state(101, &[1], vec![transition(0, 0.9, 0.25)]), state(102, &[1], vec![])],
            2,
        );
        let (r, text) = run(&c, &clips);
        assert!(r.handovers.is_empty());
        assert!(text.contains("\"s2\" (no transition)"), "{text}");
    }

    #[test]
    fn extra_layers_and_unresolved_references_are_notes() {
        let mut c = pair(transition(0, 0.9, 0.25));
        let layer = c.layers[0].clone();
        c.layers.push(layer);
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert_eq!(r.handovers.len(), 1);
        assert!(text.contains("2 layers; only the first is read"), "{text}");

        let mut c = pair(transition(0, 0.9, 0.25));
        c.state_machines[0].states[1].transitions[0].destination = 9;
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert_eq!(r, Recovered::default());
        assert!(text.contains("unresolved references"), "{text}");

        let mut c = pair(transition(0, 0.9, 0.25));
        c.layers.clear();
        let (r, text) = run(&c, &[]);
        assert_eq!(r, Recovered::default());
        assert!(text.contains("no layer"), "{text}");
    }

    #[test]
    fn states_sharing_a_motion_with_no_transition_are_named_unless_one_is_the_default() {
        let c = controller(vec![state(100, &[0], vec![]), state(101, &[1], vec![]), state(102, &[1], vec![])], 2);
        let (r, text) = run(&c, &motions(&[("Idle", 4.0), ("Wave", 2.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("2 states play motion \"Wave\" and none has a transition"), "{text}");
        assert!(text.contains("\"s1\", \"s2\""), "{text}");

        // The default state plays the motion too: no follow-up is expected.
        let c = controller(vec![state(100, &[0], vec![]), state(101, &[0], vec![])], 1);
        let (_, text) = run(&c, &motions(&[("Idle", 4.0)]));
        assert!(!text.contains("nothing follows"), "{text}");
    }

    #[test]
    fn a_default_state_with_its_own_timing_keeps_its_motion_but_says_so() {
        let mut s = state(100, &[0], vec![]);
        s.speed = 2.0;
        s.cycle_offset = 0.5;
        let (r, text) = run(&controller(vec![s], 1), &motions(&[("Idle", 4.0)]));
        assert_eq!(r.default_motion.as_deref(), Some("Idle"));
        assert!(text.contains("the default state \"s0\" has speed 2, cycle offset 0.5"), "{text}");
        // A plain default says nothing.
        let (_, text) = run(&controller(vec![state(100, &[0], vec![])], 1), &motions(&[("Idle", 4.0)]));
        assert!(text.is_empty() || !text.contains("default state"), "{text}");
    }

    #[test]
    fn a_motion_of_no_length_cannot_hand_over() {
        let (r, text) = run(&pair(transition(0, 0.9, 0.25)), &motions(&[("Idle", 4.0), ("Wave", 0.0)]));
        assert!(r.handovers.is_empty());
        assert!(text.contains("motion of length 0"), "{text}");
    }

    /// Two clips named alike are renamed by the merge; the controller reaches
    /// each by its object, so each handover names the right motion.
    #[test]
    fn a_renamed_duplicate_motion_is_reached_through_its_clip_object() {
        use super::super::{merge_motions, resolve_clips, MotionSet};
        use a2d_core::ir::cubism::Motion;
        let stub = |name: &str, duration: f32| Motion { name: name.into(), duration, ..Motion::default() };
        let mut motions = Vec::new();
        let mut report = LoadReport::new();
        // Clip objects 10 and 11 are both called "Pose"; 12 is "Idle".
        let by_clip = merge_motions(
            &mut motions,
            vec![(12, stub("Idle", 4.0)), (10, stub("Pose", 2.0)), (11, stub("Pose", 3.0))],
            &mut report,
        );
        assert!(report.to_string().contains("renamed"), "{report}");
        let set = MotionSet { motions, by_clip, fade_covered: Default::default() };
        let mut c = controller(
            vec![
                state(100, &[2], vec![]),
                state(101, &[0], vec![transition(0, 0.5, 0.25)]),
                state(102, &[1], vec![transition(0, 0.5, 0.25)]),
            ],
            3,
        );
        c.clips = vec![
            PPtr { file_index: 0, path_id: 10 },
            PPtr { file_index: 0, path_id: 11 },
            PPtr { file_index: 0, path_id: 12 },
        ];
        let clips = resolve_clips(&c.clips, &set);
        let (r, text) = run(&c, &clips);
        assert_eq!(r.default_motion.as_deref(), Some("Idle"), "{text}");
        let from: Vec<(&str, f32)> = r.handovers.iter().map(|h| (h.from.as_str(), h.handover.at)).collect();
        assert_eq!(from, [("Pose", 1.0), ("Pose_2", 1.5)], "{text}");
    }
}
