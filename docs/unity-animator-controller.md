# Unity `AnimatorController` layout (2022.3, stripped type tree)

Decoded by `crates/a2d-unity/src/animator_controller.rs`. This records what is
known about the layout and what established each part (CLAUDE.md §16). The
decoder is decode-only: nothing in the importer or the IR consumes it yet.

Shipping bundles carry no type tree, so the field order is the only schema.
Everything is little-endian; strings and bool runs are padded to four bytes.

## Layout

```text
string m_Name · u32 m_ControllerSize
ControllerConstant
  LayerConstant[]        u32 stateMachineIndex · u32 motionSetIndex · 3 x u32 bodyMask
                         · {u32,f32}[] skeletonMask · u32 binding · i32 blendingMode
                         · f32 defaultWeight · u8 ikPass · u8 syncedLayerAffectsTiming + align
  StateMachineConstant[]
    StateConstant[]      TransitionConstant[] · i32[] blendTreeIndex
                         · BlendTreeConstant[] { BlendTreeNodeConstant[] }
                         · u32 nameID, pathID, fullPathID, tagID
                         · u32 speedParam, mirrorParam, cycleOffsetParam, timeParam
                         · f32 speed · f32 cycleOffset
                         · u8 ik, writeDefaults, loop, mirror + align
    TransitionConstant[] any-state transitions
    SelectorStateConstant[]  { {u32 destination, Condition[]}[] · u32 fullPathID · u8 isEntry + align }
    u32 defaultState · u32 motionSetCount
  ValueConstant[]        u32 id · u32 type · u32 index
  ValueArray             float3[] · float4[] · float3[] · f32[] · i32[] · u8[] + align
map<u32,string> m_TOS    i32 count, then (u32 key, string) pairs
PPtr[] m_AnimationClips
{u32 stateID, i32 layer, u32 start, u32 count}[]   behaviour ranges
u32[] behaviour indices · PPtr[] m_StateMachineBehaviours
u8 m_Multithreaded + align
```

`TransitionConstant`: `Condition[]` (`u32 mode, u32 eventID, f32 threshold, f32
exitTime`), `u32 destination, fullPathID, ID, userID`, `f32 duration, offset,
exitTime`, `u8 hasExit, hasFixedDuration` + align, `i32 interruptionSource`,
`u8 orderedInterruption, canTransitionToSelf` + align.

`BlendTreeNodeConstant`: `u32 blendType, eventID, eventYID`, `u32[] children`,
`f32[] thresholds`, `float2[] positions`, `f32[] magnitudes`, `float2[]
pairVectors`, `f32[] pairAvgMagInv`, `u32[][] neighbours`, `u32[] directIDs`,
`u8 normalized` + align, `u32 clipID`, `f32 duration, cycleOffset`, `u8 mirror`
+ align.

The object has no length field, so the decoder demands the exact end: trailing
bytes are an error, counts are checked against the bytes remaining before
anything is allocated, and a bool byte other than 0 or 1 is an error.

The `Animator` component (class 95) holds the controller as its `m_Controller`
pointer: `PPtr m_GameObject` (12) · `u8 m_Enabled` + align · `PPtr m_Avatar`
(12) · `PPtr m_Controller` at byte 28. `animator_controller_pptr` reads only
those leading fields and does not length-check the rest of the body.

## What established each part

Confirmed by exact decoding of six real controllers, each from a different
real bundle:

* Every layout piece that holds data in them. Each object decodes to the
  exact last byte; a misplaced field anywhere shifts everything after it, and
  the m_TOS table sits near the end, so a wrong earlier layout would not land
  on the end of the object.
* Every state name id and transition path id resolves through `m_TOS` to a
  readable string, so the u32 name fields are at the offsets claimed.
* Transition destinations index the states of the same state machine, and
  every clip index of a blend-tree leaf indexes `m_AnimationClips`; every clip
  pointer is local and lands on an `AnimationClip` object. The `Animator`
  component's `m_Controller` pointer lands on the bundle's controller.
* Transition durations and exit times are plausible and reproduce known
  relations (a fixed 0.25 s duration with exit time `1 - 0.25 / clip length`
  to 1e-6 in 23 of 24 cases), so the `f32` fields are in the right order.

Inferred from Unity's field order (the bytes were consistent, but the
real data does not discriminate them):

* The meaning of `m_ControllerSize` (read and kept, unused), the layer's
  `bodyMask`, `binding`, `blendingMode`, and the order of the four parameter
  id fields of a state (all zero or constant in the real data).
* The names of the two bools of a layer, the four of a state, and the two
  trailing bools of a transition (their values were identical across the
  real controllers, so swapping two would still have decoded).
* The order of the layer's `stateMachineIndex` and `motionSetIndex` (not examined for distinctness; both resolve
  in the real data), of a selector transition's `destination` before its
  `Condition[]` (the real ones have no conditions), and the meaning of
  `blendTreeIndex` as one entry per motion set indexing the state's blend trees
  (every entry resolves in the real data).
* `isEntry` and `fullPathID` of a selector. Observed in all six: the first
  selector has `isEntry` 1, one transition to state 0, no conditions; the
  second has `isEntry` 0, one transition with destination 30000, no
  conditions; both `fullPathID`s resolve through `m_TOS`. That the first is the
  entry selector pointing at the default state is a reading, not a check.

Untested shapes. These were absent from all six controllers, so their layouts
are Unity's field order and have never met real bytes. They decode strictly
(the exact-end check still applies, so a wrong layout most likely fails loudly
rather than decoding) but must not be relied on as established:

* Transitions or selector transitions **with conditions** (the `Condition`
  record and its `mode` codes).
* **Any-state transitions.**
* **Blend trees with more than one node, or a node with children**: every
  `BlendTreeNodeConstant` array other than a leaf's (all empty in the data),
  including the 2D neighbour lists and direct ids.
* **More than one layer**, a layer with a skeleton mask, or more than one
  state machine; a `motionSetCount` other than 1 (the layer and state checks in `unresolved_references` assume `motionSetIndex < motionSetCount`, `blendTreeIndex` entries are negative or below the blend-tree count, and node children index the same tree: all inferred).
* **Controller parameters**: `ValueConstant` and every `ValueArray` vector (all
  empty in the data), including the type codes.
* Non-empty `m_StateMachineBehaviours` data beyond the pointer list itself
  (the real ones were pointers to behaviours with empty bodies).
* The reading of `m_TOS` ids as hashes: the decoder takes them as opaque keys.

The decoder does not interpret any of this. In particular it does not say that
a state is an emotion, or what happens when a script plays a state; that is the
importer's concern and is not part of the stored graph.

## Selectors and the 30000 destination

Observed in all six controllers: two selectors (see above). The second one's
transition has destination `30000`, far beyond any state count, so it is not a
state index. The decoder keeps the raw value and exports
`SELECTOR_DESTINATION_BASE = 30000`. Two readings are inferred, neither
checked against bytes or a runtime:

* "no state of this machine" / leave the machine; or
* Unity's reported encoding of a selector target, `30000 + selector index`,
  which would make 30000 selector 0, the entry selector, so that leaving the
  machine re-enters it at the default state.

Which is right is the importer's to decide. `unresolved_references` treats a
destination that is a state index, or `30000 + k` with `k` below the machine's
selector count, as resolved, and reports anything else.
