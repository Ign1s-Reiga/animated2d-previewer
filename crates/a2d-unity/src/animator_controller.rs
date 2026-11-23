//! Unity's `AnimatorController` (class 91), as a shipping bundle stores it.
//!
//! This is generic Unity: it decodes the container's layout and says nothing
//! about what a state is for. Deciding that a state named "x" is an emotion is
//! the importer's job. The layout and what established each field are recorded
//! in `docs/unity-animator-controller.md`; the summary is below.
//!
//! # Layout
//!
//! Unity 2022.3, little-endian, type tree stripped, so the field order is the
//! only schema. Strings and bool runs are padded to four bytes.
//!
//! ```text
//! string m_Name · u32 m_ControllerSize
//! ControllerConstant
//!   LayerConstant[]  u32 stateMachineIndex · u32 motionSetIndex · 3 x u32 bodyMask
//!                    · {u32,f32}[] skeletonMask · u32 binding · i32 blendingMode
//!                    · f32 defaultWeight · u8 ikPass · u8 syncedAffectsTiming + align
//!   StateMachineConstant[]
//!     StateConstant[]  TransitionConstant[] · i32[] blendTreeIndex
//!                      · BlendTreeConstant[] (each: BlendTreeNodeConstant[])
//!                      · u32 nameID, pathID, fullPathID, tagID
//!                      · u32 speedParam, mirrorParam, cycleOffsetParam, timeParam
//!                      · f32 speed · f32 cycleOffset
//!                      · u8 ik, writeDefaults, loop, mirror + align
//!     TransitionConstant[]  any-state transitions
//!     SelectorStateConstant[]  {u32 destination, Condition[]}[] · u32 fullPathID
//!                              · u8 isEntry + align
//!     u32 defaultState · u32 motionSetCount
//!   ValueConstant[]  u32 id · u32 type · u32 index
//!   ValueArray       float3[] · float4[] · float3[] · f32[] · i32[] · u8[] + align
//! map<u32,string> m_TOS                                        (count, then pairs)
//! PPtr[] m_AnimationClips
//! {u32 stateID, i32 layer, u32 start, u32 count}[]  behaviour ranges
//! u32[] behaviour indices · PPtr[] m_StateMachineBehaviours
//! u8 m_Multithreaded + align
//! ```
//!
//! `TransitionConstant` is `Condition[] {u32 mode, u32 eventID, f32 threshold,
//! f32 exitTime}` · `u32 destination, fullPathID, ID, userID` · `f32 duration,
//! offset, exitTime` · `u8 hasExit, hasFixedDuration` + align · `i32
//! interruptionSource` · `u8 orderedInterruption, canTransitionToSelf` + align.
//!
//! `BlendTreeNodeConstant` is `u32 blendType, eventID, eventYID` · `u32[]
//! children` · `f32[] thresholds` · `float2[] positions` · `f32[] magnitudes` ·
//! `float2[] pairVectors` · `f32[] pairAvgMagInv` · `u32[][] neighbours` ·
//! `u32[] directIDs` · `u8 normalized` + align · `u32 clipID` · `f32 duration,
//! cycleOffset` · `u8 mirror` + align.
//!
//! No length field delimits the object, so the check that the layout is right
//! is its exact end: leftover bytes are an error, and so is a bool byte that is
//! neither 0 nor 1.
//!
//! # What established these claims (CLAUDE.md §16)
//!
//! * Six real Unity 2022.3 controllers decode to the exact final byte, and every
//!   name id (states, transition paths) resolves through `m_TOS`.
//!
//! # UNVERIFIED
//!
//! The real controllers had one layer, one state machine, no conditions, no
//! any-state transitions, no controller parameters (so an empty `ValueArray`),
//! no selector transitions with conditions, and only single-clip states (one
//! leaf node per blend tree, no children). The layouts of those parts follow
//! Unity's field order and nothing here has been checked against bytes. They
//! decode strictly (the final-byte check still applies) but must not be taken
//! to be established. See the document for the list.

use std::collections::BTreeMap;

use a2d_core::DecodeError;

use crate::objects::PPtr;
use crate::reader::{Endian, Reader};

/// One `{mode, eventID, threshold, exitTime}` entry of a transition.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub mode: u32,
    /// The id (a `m_TOS` key) of the controller parameter tested.
    pub event_id: u32,
    pub threshold: f32,
    pub exit_time: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub conditions: Vec<Condition>,
    /// Index of the destination state in the same state machine.
    pub destination: u32,
    pub full_path_id: u32,
    pub id: u32,
    pub user_id: u32,
    pub duration: f32,
    pub offset: f32,
    pub exit_time: f32,
    pub has_exit_time: bool,
    /// Whether `duration` is in seconds (true) or a fraction of the source state.
    pub has_fixed_duration: bool,
    pub interruption_source: i32,
    pub ordered_interruption: bool,
    pub can_transition_to_self: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlendTreeNode {
    pub blend_type: u32,
    pub blend_event_id: u32,
    pub blend_event_y_id: u32,
    pub children: Vec<u32>,
    pub child_thresholds: Vec<f32>,
    pub child_positions: Vec<[f32; 2]>,
    pub child_magnitudes: Vec<f32>,
    pub child_pair_vectors: Vec<[f32; 2]>,
    pub child_pair_avg_mag_inv: Vec<f32>,
    pub child_neighbours: Vec<Vec<u32>>,
    pub blend_direct_ids: Vec<u32>,
    pub normalized_blend_values: bool,
    /// Index into [`AnimatorController::clips`]; meaningful on a leaf (a node
    /// with no children).
    pub clip_index: u32,
    pub duration: f32,
    pub cycle_offset: f32,
    pub mirror: bool,
}

impl BlendTreeNode {
    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlendTree {
    pub nodes: Vec<BlendTreeNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub transitions: Vec<Transition>,
    /// One entry per motion set: an index into `blend_trees`, or negative for none.
    pub blend_tree_indices: Vec<i32>,
    pub blend_trees: Vec<BlendTree>,
    /// A key of [`AnimatorController::names`].
    pub name_id: u32,
    pub path_id: u32,
    pub full_path_id: u32,
    pub tag_id: u32,
    pub speed_param_id: u32,
    pub mirror_param_id: u32,
    pub cycle_offset_param_id: u32,
    pub time_param_id: u32,
    pub speed: f32,
    pub cycle_offset: f32,
    pub ik: bool,
    pub write_defaults: bool,
    pub looping: bool,
    pub mirror: bool,
}

impl State {
    /// The clip indices of every leaf node of every blend tree of this state,
    /// in order. A single-clip state yields one.
    pub fn leaf_clip_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.blend_trees.iter().flat_map(|t| t.nodes.iter()).filter(|n| n.is_leaf()).map(|n| n.clip_index)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectorTransition {
    pub destination: u32,
    pub conditions: Vec<Condition>,
}

/// An entry/exit selector: the choice made on entering a state machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub transitions: Vec<SelectorTransition>,
    pub full_path_id: u32,
    pub is_entry: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateMachine {
    pub states: Vec<State>,
    pub any_state_transitions: Vec<Transition>,
    pub selectors: Vec<Selector>,
    pub default_state: u32,
    pub motion_set_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub state_machine_index: u32,
    pub motion_set_index: u32,
    pub body_mask: [u32; 3],
    /// `(path hash, weight)` pairs.
    pub skeleton_mask: Vec<(u32, f32)>,
    pub binding: u32,
    pub blending_mode: i32,
    pub default_weight: f32,
    pub ik_pass: bool,
    pub synced_layer_affects_timing: bool,
}

/// One controller parameter: its id (a `m_TOS` key), type code and the index of
/// its default in the matching [`ValueArray`] vector.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueConstant {
    pub id: u32,
    pub value_type: u32,
    pub index: u32,
}

/// Parameter default values, by kind.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ValueArray {
    pub position_values: Vec<[f32; 3]>,
    pub quaternion_values: Vec<[f32; 4]>,
    pub scale_values: Vec<[f32; 3]>,
    pub float_values: Vec<f32>,
    pub int_values: Vec<i32>,
    pub bool_values: Vec<bool>,
}

/// Which `StateMachineBehaviour`s a state carries: `count` entries of
/// [`AnimatorController::behaviour_indices`] from `start`, each indexing
/// [`AnimatorController::state_machine_behaviours`].
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviourRange {
    pub state_id: u32,
    pub layer: i32,
    pub start: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimatorController {
    pub name: String,
    pub controller_size: u32,
    pub layers: Vec<Layer>,
    pub state_machines: Vec<StateMachine>,
    pub parameters: Vec<ValueConstant>,
    pub default_values: ValueArray,
    /// `m_TOS`: name id to string. Ids are CRC-32-like hashes; a state's
    /// `name_id` is looked up here.
    pub names: BTreeMap<u32, String>,
    /// `m_AnimationClips`, the target of every blend-tree leaf's clip index.
    pub clips: Vec<PPtr>,
    pub behaviour_ranges: Vec<BehaviourRange>,
    pub behaviour_indices: Vec<u32>,
    pub state_machine_behaviours: Vec<PPtr>,
    pub multithreaded: bool,
}

// Lower bounds on serialized sizes, used only to refuse counts that cannot fit.
const CONDITION_BYTES: usize = 16;
const TRANSITION_BYTES: usize = 4 + 16 + 12 + 4 + 4 + 4;
const NODE_BYTES: usize = 12 + 8 * 4 + 4 + 16;
const STATE_BYTES: usize = 4 * 3 + 32 + 8 + 4;

/// The first value a selector transition's destination takes when it is not a
/// state index. Observed: 30000, on the one transition of the second selector
/// of every real controller. Two readings are inferred and neither is checked:
/// "no state of this machine / leave the machine", or the encoding Unity is
/// reported to use for a selector target, `30000 + selector index` (so 30000 is
/// selector 0, the entry selector, and leaving re-enters at the default state).
/// The decoder keeps the raw value; the meaning is the importer's to decide.
pub const SELECTOR_DESTINATION_BASE: u32 = 30000;

fn flag(r: &mut Reader<'_>, what: &str) -> Result<bool, DecodeError> {
    let at = r.position() as u64;
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DecodeError::corrupt_at(format!("{what} is {other}, not a 0/1 flag"), at)),
    }
}

fn u32_array(r: &mut Reader<'_>, what: &str) -> Result<Vec<u32>, DecodeError> {
    let n = r.count(4, what)?;
    (0..n).map(|_| r.u32()).collect()
}

fn f32_vec<const N: usize>(r: &mut Reader<'_>, what: &str) -> Result<Vec<[f32; N]>, DecodeError> {
    let n = r.count(4 * N, what)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut v = [0.0; N];
        for x in &mut v {
            *x = r.f32()?;
        }
        out.push(v);
    }
    Ok(out)
}

fn conditions(r: &mut Reader<'_>) -> Result<Vec<Condition>, DecodeError> {
    let n = r.count(CONDITION_BYTES, "a condition list")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(Condition { mode: r.u32()?, event_id: r.u32()?, threshold: r.f32()?, exit_time: r.f32()? });
    }
    Ok(out)
}

fn transition(r: &mut Reader<'_>) -> Result<Transition, DecodeError> {
    let conditions = conditions(r)?;
    let destination = r.u32()?;
    let full_path_id = r.u32()?;
    let id = r.u32()?;
    let user_id = r.u32()?;
    let duration = r.f32()?;
    let offset = r.f32()?;
    let exit_time = r.f32()?;
    let has_exit_time = flag(r, "a transition's hasExitTime")?;
    let has_fixed_duration = flag(r, "a transition's hasFixedDuration")?;
    r.align(4)?;
    let interruption_source = r.i32()?;
    let ordered_interruption = flag(r, "a transition's orderedInterruption")?;
    let can_transition_to_self = flag(r, "a transition's canTransitionToSelf")?;
    r.align(4)?;
    Ok(Transition {
        conditions,
        destination,
        full_path_id,
        id,
        user_id,
        duration,
        offset,
        exit_time,
        has_exit_time,
        has_fixed_duration,
        interruption_source,
        ordered_interruption,
        can_transition_to_self,
    })
}

fn transitions(r: &mut Reader<'_>, what: &str) -> Result<Vec<Transition>, DecodeError> {
    let n = r.count(TRANSITION_BYTES, what)?;
    (0..n).map(|_| transition(r)).collect()
}

fn blend_node(r: &mut Reader<'_>) -> Result<BlendTreeNode, DecodeError> {
    let blend_type = r.u32()?;
    let blend_event_id = r.u32()?;
    let blend_event_y_id = r.u32()?;
    let children = u32_array(r, "a blend node's children")?;
    let child_thresholds = r.f32_array()?;
    let child_positions = f32_vec::<2>(r, "a blend node's positions")?;
    let child_magnitudes = r.f32_array()?;
    let child_pair_vectors = f32_vec::<2>(r, "a blend node's pair vectors")?;
    let child_pair_avg_mag_inv = r.f32_array()?;
    let lists = r.count(4, "a blend node's neighbour lists")?;
    let mut child_neighbours = Vec::with_capacity(lists);
    for _ in 0..lists {
        child_neighbours.push(u32_array(r, "a neighbour list")?);
    }
    let blend_direct_ids = u32_array(r, "a blend node's direct ids")?;
    let normalized_blend_values = flag(r, "a blend node's normalized flag")?;
    r.align(4)?;
    let clip_index = r.u32()?;
    let duration = r.f32()?;
    let cycle_offset = r.f32()?;
    let mirror = flag(r, "a blend node's mirror flag")?;
    r.align(4)?;
    Ok(BlendTreeNode {
        blend_type,
        blend_event_id,
        blend_event_y_id,
        children,
        child_thresholds,
        child_positions,
        child_magnitudes,
        child_pair_vectors,
        child_pair_avg_mag_inv,
        child_neighbours,
        blend_direct_ids,
        normalized_blend_values,
        clip_index,
        duration,
        cycle_offset,
        mirror,
    })
}

fn state(r: &mut Reader<'_>) -> Result<State, DecodeError> {
    let transitions = transitions(r, "a state's transitions")?;
    let n = r.count(4, "a state's blend tree indices")?;
    let blend_tree_indices = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>, _>>()?;
    let n = r.count(4, "a state's blend trees")?;
    let mut blend_trees = Vec::with_capacity(n);
    for _ in 0..n {
        let nodes = r.count(NODE_BYTES, "a blend tree's nodes")?;
        blend_trees.push(BlendTree { nodes: (0..nodes).map(|_| blend_node(r)).collect::<Result<_, _>>()? });
    }
    let name_id = r.u32()?;
    let path_id = r.u32()?;
    let full_path_id = r.u32()?;
    let tag_id = r.u32()?;
    let speed_param_id = r.u32()?;
    let mirror_param_id = r.u32()?;
    let cycle_offset_param_id = r.u32()?;
    let time_param_id = r.u32()?;
    let speed = r.f32()?;
    let cycle_offset = r.f32()?;
    let ik = flag(r, "a state's ik flag")?;
    let write_defaults = flag(r, "a state's writeDefaults flag")?;
    let looping = flag(r, "a state's loop flag")?;
    let mirror = flag(r, "a state's mirror flag")?;
    r.align(4)?;
    Ok(State {
        transitions,
        blend_tree_indices,
        blend_trees,
        name_id,
        path_id,
        full_path_id,
        tag_id,
        speed_param_id,
        mirror_param_id,
        cycle_offset_param_id,
        time_param_id,
        speed,
        cycle_offset,
        ik,
        write_defaults,
        looping,
        mirror,
    })
}

fn selector(r: &mut Reader<'_>) -> Result<Selector, DecodeError> {
    let n = r.count(8, "a selector's transitions")?;
    let mut transitions = Vec::with_capacity(n);
    for _ in 0..n {
        transitions.push(SelectorTransition { destination: r.u32()?, conditions: conditions(r)? });
    }
    let full_path_id = r.u32()?;
    let is_entry = flag(r, "a selector's isEntry flag")?;
    r.align(4)?;
    Ok(Selector { transitions, full_path_id, is_entry })
}

fn state_machine(r: &mut Reader<'_>) -> Result<StateMachine, DecodeError> {
    let n = r.count(STATE_BYTES, "a state machine's states")?;
    let states = (0..n).map(|_| state(r)).collect::<Result<Vec<_>, _>>()?;
    let any_state_transitions = transitions(r, "a state machine's any-state transitions")?;
    let n = r.count(12, "a state machine's selectors")?;
    let selectors = (0..n).map(|_| selector(r)).collect::<Result<Vec<_>, _>>()?;
    let default_state = r.u32()?;
    let motion_set_count = r.u32()?;
    Ok(StateMachine { states, any_state_transitions, selectors, default_state, motion_set_count })
}

fn layer(r: &mut Reader<'_>) -> Result<Layer, DecodeError> {
    let state_machine_index = r.u32()?;
    let motion_set_index = r.u32()?;
    let body_mask = [r.u32()?, r.u32()?, r.u32()?];
    let n = r.count(8, "a layer's skeleton mask")?;
    let mut skeleton_mask = Vec::with_capacity(n);
    for _ in 0..n {
        skeleton_mask.push((r.u32()?, r.f32()?));
    }
    let binding = r.u32()?;
    let blending_mode = r.i32()?;
    let default_weight = r.f32()?;
    let ik_pass = flag(r, "a layer's ikPass flag")?;
    let synced_layer_affects_timing = flag(r, "a layer's syncedLayerAffectsTiming flag")?;
    r.align(4)?;
    Ok(Layer {
        state_machine_index,
        motion_set_index,
        body_mask,
        skeleton_mask,
        binding,
        blending_mode,
        default_weight,
        ik_pass,
        synced_layer_affects_timing,
    })
}

fn pptrs(r: &mut Reader<'_>, what: &str) -> Result<Vec<PPtr>, DecodeError> {
    let n = r.count(12, what)?;
    (0..n).map(|_| PPtr::read(r)).collect()
}

impl AnimatorController {
    /// Decodes the body of an `AnimatorController` object.
    ///
    /// Strict: anything that does not fit the layout in the module docs is an
    /// error, because a wrong layout would otherwise yield a plausible graph.
    pub fn parse(data: &[u8]) -> Result<AnimatorController, DecodeError> {
        let mut r = Reader::new(data, Endian::Little);
        let name = r.string()?;
        let controller_size = r.u32()?;

        let n = r.count(40, "the layers")?;
        let layers = (0..n).map(|_| layer(&mut r)).collect::<Result<Vec<_>, _>>()?;
        let n = r.count(12, "the state machines")?;
        let state_machines = (0..n).map(|_| state_machine(&mut r)).collect::<Result<Vec<_>, _>>()?;

        let n = r.count(12, "the controller parameters")?;
        let mut parameters = Vec::with_capacity(n);
        for _ in 0..n {
            parameters.push(ValueConstant { id: r.u32()?, value_type: r.u32()?, index: r.u32()? });
        }
        let position_values = f32_vec::<3>(&mut r, "the position defaults")?;
        let quaternion_values = f32_vec::<4>(&mut r, "the quaternion defaults")?;
        let scale_values = f32_vec::<3>(&mut r, "the scale defaults")?;
        let float_values = r.f32_array()?;
        let n = r.count(4, "the int defaults")?;
        let int_values = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>, _>>()?;
        let n = r.count(1, "the bool defaults")?;
        let mut bool_values = Vec::with_capacity(n);
        for _ in 0..n {
            bool_values.push(flag(&mut r, "a bool default")?);
        }
        r.align(4)?;
        let default_values =
            ValueArray { position_values, quaternion_values, scale_values, float_values, int_values, bool_values };

        let n = r.count(8, "m_TOS")?;
        let mut names = BTreeMap::new();
        for _ in 0..n {
            let at = r.position() as u64;
            let key = r.u32()?;
            let value = r.string()?;
            if names.insert(key, value).is_some() {
                return Err(DecodeError::corrupt_at(format!("m_TOS holds name id {key:#010x} twice"), at));
            }
        }

        let clips = pptrs(&mut r, "m_AnimationClips")?;
        let n = r.count(16, "the behaviour ranges")?;
        let mut behaviour_ranges = Vec::with_capacity(n);
        for _ in 0..n {
            behaviour_ranges.push(BehaviourRange {
                state_id: r.u32()?,
                layer: r.i32()?,
                start: r.u32()?,
                count: r.u32()?,
            });
        }
        let behaviour_indices = u32_array(&mut r, "the behaviour indices")?;
        let state_machine_behaviours = pptrs(&mut r, "m_StateMachineBehaviours")?;
        let multithreaded = flag(&mut r, "m_Multithreaded")?;
        r.align(4)?;

        if r.remaining() != 0 {
            return Err(DecodeError::corrupt_at(
                format!("{} bytes follow the AnimatorController body", r.remaining()),
                r.position() as u64,
            ));
        }
        Ok(AnimatorController {
            name,
            controller_size,
            layers,
            state_machines,
            parameters,
            default_values,
            names,
            clips,
            behaviour_ranges,
            behaviour_indices,
            state_machine_behaviours,
            multithreaded,
        })
    }

    /// The string a name id stands for, when `m_TOS` holds it.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// The name of state `index` of state machine `machine`.
    pub fn state_name(&self, machine: usize, index: usize) -> Option<&str> {
        self.name(self.state_machines.get(machine)?.states.get(index)?.name_id)
    }

    /// Every reference in the graph that does not resolve, as a sentence each:
    /// a layer's state machine and motion set, a default state, transition
    /// destinations, blend-tree indices, blend-node children, a leaf's clip
    /// index, a null clip pointer, behaviour ranges and indices. Empty means the
    /// graph is closed. Name ids missing from `m_TOS` are not listed: an
    /// unnamed state is still a state.
    ///
    /// A selector destination of `SELECTOR_DESTINATION_BASE + k` with `k` below
    /// the machine's selector count is a reference to selector `k` and counts as
    /// resolved; any other value outside the states is unresolved.
    pub fn unresolved_references(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            match self.state_machines.get(l.state_machine_index as usize) {
                None => out.push(format!("layer {i} names state machine {}", l.state_machine_index)),
                Some(sm) if l.motion_set_index >= sm.motion_set_count => {
                    out.push(format!("layer {i} names motion set {} of {}", l.motion_set_index, sm.motion_set_count))
                }
                Some(_) => {}
            }
        }
        for (m, sm) in self.state_machines.iter().enumerate() {
            let states = sm.states.len();
            // A state, or selector `k` of this machine.
            let resolves = |d: u32| {
                (d as usize) < states
                    || (d >= SELECTOR_DESTINATION_BASE
                        && ((d - SELECTOR_DESTINATION_BASE) as usize) < sm.selectors.len())
            };
            if !sm.states.is_empty() && sm.default_state as usize >= states {
                out.push(format!("state machine {m} defaults to state {}", sm.default_state));
            }
            for (s, st) in sm.states.iter().enumerate() {
                for (t, tr) in st.transitions.iter().enumerate() {
                    if !resolves(tr.destination) {
                        out.push(format!("machine {m} state {s} transition {t} goes to state {}", tr.destination));
                    }
                }
                for &bt in &st.blend_tree_indices {
                    if bt >= 0 && bt as usize >= st.blend_trees.len() {
                        out.push(format!("machine {m} state {s} names blend tree {bt} of {}", st.blend_trees.len()));
                    }
                }
                for (b, tree) in st.blend_trees.iter().enumerate() {
                    for (n, node) in tree.nodes.iter().enumerate() {
                        for &child in &node.children {
                            if child as usize >= tree.nodes.len() {
                                out.push(format!(
                                    "machine {m} state {s} tree {b} node {n} has child {child} of {}",
                                    tree.nodes.len()
                                ));
                            }
                        }
                        if node.is_leaf() && node.clip_index as usize >= self.clips.len() {
                            out.push(format!(
                                "machine {m} state {s} plays clip {} of {}",
                                node.clip_index,
                                self.clips.len()
                            ));
                        }
                    }
                }
            }
            for (t, tr) in sm.any_state_transitions.iter().enumerate() {
                if !resolves(tr.destination) {
                    out.push(format!("machine {m} any-state transition {t} goes to state {}", tr.destination));
                }
            }
            for (s, sel) in sm.selectors.iter().enumerate() {
                for tr in &sel.transitions {
                    let d = tr.destination;
                    let state = (d as usize) < states;
                    let selector = d >= SELECTOR_DESTINATION_BASE
                        && ((d - SELECTOR_DESTINATION_BASE) as usize) < sm.selectors.len();
                    if !state && !selector {
                        out.push(format!("machine {m} selector {s} goes to {d}, neither a state nor a selector"));
                    }
                }
            }
        }
        for (i, c) in self.clips.iter().enumerate() {
            if c.is_null() {
                out.push(format!("m_AnimationClips[{i}] is null"));
            }
        }
        for (i, r) in self.behaviour_ranges.iter().enumerate() {
            match r.start.checked_add(r.count) {
                Some(end) if end as usize <= self.behaviour_indices.len() => {}
                _ => out.push(format!(
                    "behaviour range {i} ({} + {}) exceeds the {} behaviour indices",
                    r.start,
                    r.count,
                    self.behaviour_indices.len()
                )),
            }
        }
        for (i, &b) in self.behaviour_indices.iter().enumerate() {
            if b as usize >= self.state_machine_behaviours.len() {
                out.push(format!("behaviour index {i} names behaviour {b} of {}", self.state_machine_behaviours.len()));
            }
        }
        out
    }
}

/// The `m_Controller` pointer of an `Animator` component (class 95).
///
/// Only the leading fields are read: `PPtr m_GameObject` · `u8 m_Enabled` +
/// align · `PPtr m_Avatar` · `PPtr m_Controller`. The rest of the body (culling
/// and update modes, root-motion flags) is not read and not length-checked.
pub fn animator_controller_pptr(data: &[u8]) -> Result<PPtr, DecodeError> {
    let mut r = Reader::new(data, Endian::Little);
    PPtr::read(&mut r)?;
    flag(&mut r, "m_Enabled")?;
    r.align(4)?;
    PPtr::read(&mut r)?;
    PPtr::read(&mut r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a body in the layout of the module docs.
    #[derive(Default)]
    struct W(Vec<u8>);

    impl W {
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn i32(&mut self, v: i32) -> &mut Self {
            self.u32(v as u32)
        }
        fn f32(&mut self, v: f32) -> &mut Self {
            self.u32(v.to_bits())
        }
        fn flags(&mut self, v: &[u8]) -> &mut Self {
            self.0.extend_from_slice(v);
            while self.0.len() % 4 != 0 {
                self.0.push(0);
            }
            self
        }
        fn string(&mut self, s: &str) -> &mut Self {
            self.u32(s.len() as u32);
            self.0.extend_from_slice(s.as_bytes());
            while self.0.len() % 4 != 0 {
                self.0.push(0);
            }
            self
        }
        fn pptr(&mut self, file: i32, id: i64) -> &mut Self {
            self.i32(file);
            self.0.extend_from_slice(&id.to_le_bytes());
            self
        }
        fn transition(&mut self, conds: &[(u32, u32, f32, f32)], dest: u32, duration: f32, exit: f32) -> &mut Self {
            self.u32(conds.len() as u32);
            for c in conds {
                self.u32(c.0).u32(c.1).f32(c.2).f32(c.3);
            }
            self.u32(dest).u32(11).u32(12).u32(13).f32(duration).f32(0.0).f32(exit);
            self.flags(&[1, 0]).i32(2).flags(&[1, 0])
        }
        fn node(&mut self, children: &[u32], clip: u32) -> &mut Self {
            self.u32(1).u32(2).u32(3);
            self.u32(children.len() as u32);
            for c in children {
                self.u32(*c);
            }
            self.u32(1).f32(0.5); // thresholds
            self.u32(1).f32(1.0).f32(2.0); // positions
            self.u32(1).f32(3.0); // magnitudes
            self.u32(1).f32(4.0).f32(5.0); // pair vectors
            self.u32(1).f32(6.0); // pair avg
            self.u32(1).u32(1).u32(9); // one neighbour list holding one entry
            self.u32(1).u32(8); // direct ids
            self.flags(&[1]);
            self.u32(clip).f32(2.5).f32(0.125).flags(&[1])
        }
        fn state(&mut self, name_id: u32, dest: Option<u32>, leaves: &[u32]) -> &mut Self {
            match dest {
                Some(d) => {
                    self.u32(1);
                    self.transition(&[], d, 0.25, 0.9);
                }
                None => {
                    self.u32(0);
                }
            }
            self.u32(2).i32(-1).i32(0); // blend tree indices
            self.u32(1).u32(leaves.len() as u32); // one tree
            for l in leaves {
                self.node(&[], *l);
            }
            self.u32(name_id).u32(1).u32(2).u32(3).u32(4).u32(5).u32(6).u32(7);
            self.f32(1.5).f32(0.25).flags(&[1, 0, 0, 1])
        }
        fn body(&mut self) -> &mut Self {
            self.string("ctrl").u32(1234);
            // one layer
            self.u32(1).u32(0).u32(2).u32(1).u32(2).u32(3);
            self.u32(1).u32(77).f32(0.5); // skeleton mask
            self.u32(5).i32(1).f32(0.75).flags(&[1, 0]);
            // one machine, two states, one any-state transition, one selector
            self.u32(1);
            self.u32(2);
            self.state(100, Some(1), &[0]);
            self.state(101, None, &[1]);
            self.u32(1);
            self.transition(&[(3, 200, 1.5, 0.25)], 0, 0.1, 0.0);
            self.u32(1).u32(1).u32(SELECTOR_DESTINATION_BASE);
            self.u32(1).u32(4).u32(201).f32(2.5).f32(0.5); // one condition
            self.u32(42).flags(&[1]);
            self.u32(0).u32(3);
            // parameters
            self.u32(1).u32(200).u32(1).u32(0);
            self.u32(1).f32(1.0).f32(2.0).f32(3.0); // position
            self.u32(1).f32(4.0).f32(5.0).f32(6.0).f32(7.0); // quaternion
            self.u32(1).f32(8.0).f32(9.0).f32(10.0); // scale
            self.u32(1).f32(0.75); // floats
            self.u32(1).i32(-4); // ints
            self.u32(1).flags(&[1]); // bools
                                     // m_TOS
            self.u32(4).u32(100).string("a").u32(101).string("bb").u32(200).string("ccc").u32(42).string("dd");
            // clips
            self.u32(2).pptr(0, 5).pptr(0, 6);
            // behaviours
            self.u32(1).u32(100).i32(0).u32(0).u32(1);
            self.u32(1).u32(0);
            self.u32(1).pptr(0, 9);
            self.flags(&[1])
        }
    }

    fn good() -> Vec<u8> {
        let mut w = W::default();
        w.body();
        w.0
    }

    #[test]
    fn a_hand_built_controller_decodes_field_by_field() {
        let c = AnimatorController::parse(&good()).unwrap();
        assert_eq!((c.name.as_str(), c.controller_size), ("ctrl", 1234));
        assert_eq!(c.layers.len(), 1);
        let l = &c.layers[0];
        assert_eq!((l.state_machine_index, l.motion_set_index, l.body_mask, l.binding), (0, 2, [1, 2, 3], 5));
        assert_eq!(l.skeleton_mask, vec![(77, 0.5)]);
        assert_eq!(
            (l.blending_mode, l.default_weight, l.ik_pass, l.synced_layer_affects_timing),
            (1, 0.75, true, false)
        );

        let sm = &c.state_machines[0];
        assert_eq!(sm.states.len(), 2);
        let s0 = &sm.states[0];
        assert_eq!((s0.name_id, s0.speed, s0.cycle_offset), (100, 1.5, 0.25));
        assert_eq!((s0.ik, s0.write_defaults, s0.looping, s0.mirror), (true, false, false, true));
        assert_eq!((s0.path_id, s0.full_path_id, s0.tag_id, s0.speed_param_id), (1, 2, 3, 4));
        assert_eq!((s0.mirror_param_id, s0.cycle_offset_param_id, s0.time_param_id), (5, 6, 7));
        assert_eq!(s0.blend_tree_indices, vec![-1, 0]);
        let t = &s0.transitions[0];
        assert_eq!((t.destination, t.duration, t.offset, t.exit_time), (1, 0.25, 0.0, 0.9));
        assert!(t.has_exit_time && !t.has_fixed_duration && t.ordered_interruption && !t.can_transition_to_self);
        assert_eq!((t.full_path_id, t.id, t.user_id, t.interruption_source), (11, 12, 13, 2));
        assert_eq!(s0.leaf_clip_indices().collect::<Vec<_>>(), vec![0]);
        assert_eq!(sm.states[1].leaf_clip_indices().collect::<Vec<_>>(), vec![1]);
        let node = &s0.blend_trees[0].nodes[0];
        assert_eq!((node.blend_type, node.blend_event_id, node.blend_event_y_id), (1, 2, 3));
        assert_eq!(node.child_thresholds, vec![0.5]);
        assert_eq!(node.child_positions, vec![[1.0, 2.0]]);
        assert_eq!(node.child_magnitudes, vec![3.0]);
        assert_eq!(node.child_pair_vectors, vec![[4.0, 5.0]]);
        assert_eq!(node.child_pair_avg_mag_inv, vec![6.0]);
        assert_eq!(node.child_neighbours, vec![vec![9]]);
        assert_eq!(node.blend_direct_ids, vec![8]);
        assert_eq!(
            (node.duration, node.cycle_offset, node.normalized_blend_values, node.mirror),
            (2.5, 0.125, true, true)
        );

        assert_eq!(sm.any_state_transitions.len(), 1);
        assert_eq!(
            sm.any_state_transitions[0].conditions,
            vec![Condition { mode: 3, event_id: 200, threshold: 1.5, exit_time: 0.25 }]
        );
        let sel = &sm.selectors[0];
        assert_eq!((sel.is_entry, sel.full_path_id), (true, 42));
        assert_eq!(sel.transitions.len(), 1);
        assert_eq!(sel.transitions[0].destination, SELECTOR_DESTINATION_BASE);
        assert_eq!(
            sel.transitions[0].conditions,
            vec![Condition { mode: 4, event_id: 201, threshold: 2.5, exit_time: 0.5 }]
        );
        assert_eq!((sm.default_state, sm.motion_set_count), (0, 3));

        assert_eq!(c.parameters, vec![ValueConstant { id: 200, value_type: 1, index: 0 }]);
        let v = &c.default_values;
        assert_eq!(v.position_values, vec![[1.0, 2.0, 3.0]]);
        assert_eq!(v.quaternion_values, vec![[4.0, 5.0, 6.0, 7.0]]);
        assert_eq!(v.scale_values, vec![[8.0, 9.0, 10.0]]);
        assert_eq!(v.float_values, vec![0.75]);
        assert_eq!(v.int_values, vec![-4]);
        assert_eq!(v.bool_values, vec![true]);
        assert_eq!(c.state_name(0, 1), Some("bb"));
        assert_eq!(c.name(200), Some("ccc"));
        assert_eq!(c.name(42), Some("dd"));
        assert_eq!(c.name(1), None);
        assert_eq!(c.clips, vec![PPtr { file_index: 0, path_id: 5 }, PPtr { file_index: 0, path_id: 6 }]);
        assert_eq!(c.behaviour_ranges, vec![BehaviourRange { state_id: 100, layer: 0, start: 0, count: 1 }]);
        assert_eq!(c.behaviour_indices, vec![0]);
        assert_eq!(c.state_machine_behaviours, vec![PPtr { file_index: 0, path_id: 9 }]);
        assert!(c.multithreaded);
        assert!(c.unresolved_references().is_empty(), "{:?}", c.unresolved_references());
    }

    #[test]
    fn a_trailing_byte_is_refused() {
        let mut bytes = good();
        bytes.push(0);
        let err = AnimatorController::parse(&bytes).unwrap_err().to_string();
        assert!(err.contains("follow the AnimatorController body"), "{err}");
    }

    #[test]
    fn every_truncation_is_an_error_never_a_panic() {
        let bytes = good();
        for len in 0..bytes.len() {
            assert!(AnimatorController::parse(&bytes[..len]).is_err(), "length {len} decoded");
        }
    }

    #[test]
    fn absurd_counts_are_refused_before_allocating() {
        // The layer count sits after the name (4 + 4) and the size (4).
        let mut bytes = good();
        bytes[12..16].copy_from_slice(&i32::MAX.to_le_bytes());
        let err = AnimatorController::parse(&bytes).unwrap_err().to_string();
        assert!(err.contains("the layers"), "{err}");
        bytes[12..16].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(AnimatorController::parse(&bytes).is_err());
    }

    #[test]
    fn a_flag_byte_that_is_not_zero_or_one_is_refused() {
        // The layer's ikPass byte: header 12, count 4, 5 u32 (20), mask count + pair (12), 3 u32 (12).
        let mut bytes = good();
        let at = 12 + 4 + 20 + 12 + 12;
        bytes[at] = 2;
        let err = AnimatorController::parse(&bytes).unwrap_err().to_string();
        assert!(err.contains("ikPass"), "{err}");
    }

    #[test]
    fn a_repeated_name_id_is_refused() {
        let mut w = W::default();
        w.body();
        let mut bytes = w.0;
        // Rewrite name id 101 as 100: find the pair by its string.
        let at = bytes.windows(2).position(|p| p == b"bb").unwrap() - 8;
        bytes[at..at + 4].copy_from_slice(&100u32.to_le_bytes());
        let err = AnimatorController::parse(&bytes).unwrap_err().to_string();
        assert!(err.contains("twice"), "{err}");
    }

    /// Breaks the good controller with `f` and expects exactly one complaint
    /// that mentions `needle`.
    fn one_problem(f: impl FnOnce(&mut AnimatorController), needle: &str) {
        let mut c = AnimatorController::parse(&good()).unwrap();
        f(&mut c);
        let refs = c.unresolved_references();
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert!(refs[0].contains(needle), "{refs:?} should mention {needle:?}");
    }

    #[test]
    fn unresolved_references_are_listed() {
        one_problem(|c| c.state_machines[0].states[0].transitions[0].destination = 9, "transition 0 goes to state 9");
        one_problem(|c| c.state_machines[0].states[1].blend_trees[0].nodes[0].clip_index = 4, "plays clip 4");
        one_problem(|c| c.layers[0].state_machine_index = 3, "state machine 3");
        one_problem(|c| c.clips[1] = PPtr { file_index: 0, path_id: 0 }, "is null");
        one_problem(|c| c.state_machines[0].default_state = 5, "defaults to state 5");
        one_problem(|c| c.state_machines[0].any_state_transitions[0].destination = 7, "any-state transition 0");
    }

    #[test]
    fn blend_tree_indices_children_and_motion_sets_are_checked() {
        one_problem(|c| c.state_machines[0].states[0].blend_tree_indices = vec![1], "blend tree 1 of 1");
        // A negative index means "none" and is fine.
        let mut c = AnimatorController::parse(&good()).unwrap();
        c.state_machines[0].states[0].blend_tree_indices = vec![-1, -7];
        assert!(c.unresolved_references().is_empty());
        one_problem(|c| c.state_machines[0].states[0].blend_trees[0].nodes[0].children = vec![3], "has child 3 of 1");
        one_problem(|c| c.layers[0].motion_set_index = 3, "motion set 3 of 3");
    }

    #[test]
    fn selector_destinations_resolve_to_states_or_selectors_only() {
        let set =
            |d: u32| move |c: &mut AnimatorController| c.state_machines[0].selectors[0].transitions[0].destination = d;
        // A state, and selector 0 (the only selector), resolve.
        for d in [0, 1, SELECTOR_DESTINATION_BASE] {
            let mut c = AnimatorController::parse(&good()).unwrap();
            set(d)(&mut c);
            assert!(c.unresolved_references().is_empty(), "{d}");
        }
        one_problem(set(2), "neither a state nor a selector");
        one_problem(set(SELECTOR_DESTINATION_BASE + 1), "neither a state nor a selector");
        one_problem(set(u32::MAX), "neither a state nor a selector");
    }

    #[test]
    fn state_and_any_state_transitions_may_name_a_selector_too() {
        let mut c = AnimatorController::parse(&good()).unwrap();
        c.state_machines[0].states[0].transitions[0].destination = SELECTOR_DESTINATION_BASE;
        c.state_machines[0].any_state_transitions[0].destination = SELECTOR_DESTINATION_BASE;
        assert!(c.unresolved_references().is_empty(), "{:?}", c.unresolved_references());
        // The machine has one selector, so selector 1 and a bare 2 do not exist.
        one_problem(
            |c| c.state_machines[0].states[0].transitions[0].destination = SELECTOR_DESTINATION_BASE + 1,
            "transition 0 goes to state",
        );
        one_problem(|c| c.state_machines[0].any_state_transitions[0].destination = 2, "any-state transition 0");
    }

    #[test]
    fn behaviour_ranges_and_indices_are_checked() {
        one_problem(|c| c.behaviour_ranges[0].count = 2, "exceeds the 1 behaviour indices");
        one_problem(|c| c.behaviour_ranges[0].start = u32::MAX, "exceeds");
        one_problem(|c| c.behaviour_indices[0] = 1, "names behaviour 1 of 1");
    }

    #[test]
    fn the_animator_component_names_its_controller() {
        let mut w = W::default();
        w.pptr(0, 3).flags(&[1]).pptr(0, 0).pptr(0, 77).u32(0).u32(0);
        assert_eq!(animator_controller_pptr(&w.0).unwrap(), PPtr { file_index: 0, path_id: 77 });
        assert!(animator_controller_pptr(&w.0[..30]).is_err());
    }
}
