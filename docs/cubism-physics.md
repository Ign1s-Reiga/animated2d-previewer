# Cubism physics inside a Unity bundle, as reverse-engineered

A Cubism model's hair, cloth and ornaments sway because a physics rig reads
some parameters and writes others. The editor exports it as `.physics3.json`.
A Unity bundle holds no such file (and no `TextAsset` at all); the rig survives
only as serialized fields of one `MonoBehaviour`. This document records **what
that layout is** and **how each claim was established**, separately from the
code that reads it (`crates/a2d-import/src/unity_cubism/physics.rs`).

Scope of the work this records: **recover, persist and report** the rig. Nothing
simulates it yet; the IR (`a2d-core`'s `ir::cubism::Physics`) carries it and
playback ignores it.

---

## 1. Where the data is

| Fact | How it was established |
|---|---|
| Each of three real models holds exactly one `MonoBehaviour` whose script class is `CubismPhysicsController` | the object inventory's script table, on all three |
| No `TextAsset` exists in any of the three | the class histogram has no object of that class |
| The controller's `m_Name` is empty and it is attached to the model's root `GameObject` | `behaviour_owner_name` |
| `m_Enabled` is 1 on all three, so the source ran its physics | the byte after the owner `PPtr` |
| The physics rig is the controller's single serialized field, `_rig` | a `[Serializable]` class is serialized inline, and the body is consumed to the byte by that field alone (below) |

## 2. The layout

After the behaviour header and `m_Name`, strings are `i32` length, bytes,
padding to four. A `bool` is one byte padded to four.

```text
_rig
  i32  groupCount
  group[groupCount]
    i32  inputCount
    input[inputCount]
      string source
      f32 x2  translation scale        (not used by the simulation; zero in all three models)
      f32     angle scale              (not used by the simulation; zero in all three models)
      f32     weight                   (0..100)
      i32     component                (0 X, 1 Y, 2 angle)
      bool    reflect
    i32  outputCount
    output[outputCount]
      string destination
      i32     particle index
      f32 x2  translation scale        (zero in all three models)
      f32     angle scale
      f32     weight
      i32     component
      bool    reflect
    i32  particleCount
    particle[particleCount]            (24 bytes)
      f32 x2  initial position
      f32     mobility
      f32     delay
      f32     acceleration
      f32     radius
    normalization: f32 x6
      position maximum, minimum, default     <- maximum BEFORE minimum
      angle    maximum, minimum, default
  f32 x2  gravity
  f32 x2  wind
```

The field order is taken from the Cubism SDK for Unity's public source, which
was read for understanding only. Only the serialized fields appear: everything
the simulation keeps between frames (positions, velocities) is not serialized,
which the exact fit below is consistent with.

### What the IR keeps, and what it drops

* The per-input translation and angle scales are not used by the algorithm and
  are not kept.
* A particle's **initial position** is not kept. The SDK recomputes it from the
  radii at start (the root at the origin, each particle one radius further),
  and on all three models the stored values are exactly that, so it is
  redundant. The IR's particle chain is therefore defined by its radii.
* An output's single `scale` is chosen at import by its component (angle scale
  for an angle, the matching axis of the translation scale for X or Y), so the
  IR holds one number per output rather than two the component would ignore.
* The normalization ranges are stored `minimum <= maximum` whichever order the
  source listed them in.

## 3. Evidence for the layout

The layout was found by **trying every candidate and keeping the one that fits**.

### Eight candidates, one fit

The SDK's history adds two fields over time: a **name** at the start of each
group, and a **frame rate** after the wind. Whether a `bool` is padded to four
is a third unknown. That is `2 x 2 x 2 = 8` candidate layouts. Each was run over
the body of every controller, requiring the read to consume the body **to the
byte** with every count bounded by the bytes remaining.

**Exactly one candidate fits every body (three first, three more in the same
dump later, six real models in all): no name, no frame rate, bools
padded to four.** Every other candidate fails, in recognisable ways: with a name
the first string length reads as a large number made of ASCII text; with a
frame rate the read is four bytes short; with unpadded bools a count read
partway through is nonsense.

An exact fit over six bodies (the first three run 4.8 to 14.2 kilobytes), from a layout chosen
before looking at them, is strong evidence: a wrong layout does not land on the
last byte of a body.

### Corroboration

* **Every** input and output id in all three models names a parameter of the
  MOC3 (and the name of a `CubismParameter` owner). A misaligned read would not
  produce ids that resolve.
* Value ranges are coherent: mobility 0.8 to 1.0, delay 0.6 to 1.0,
  acceleration 0.8 to 1.5, radius 0 to 14 with the root always 0; weights in
  0..100; gravity `(0, -1)` and wind `(0, 0)`.
* The first particle of every group has radius 0 and each stored initial
  position is the running sum of radii, as the SDK's start-up would produce.

### The SDK version bound

The two optional fields were added to the public SDK in mid-2022 (the frame
rate) and early 2023 (the name). A build that serializes neither therefore
**predates mid-2022**. That bounds the SDK that built these prefabs from above.
The lower bound is unverified: the layout did not change for years before that.

### The importer's behaviour follows

Because the layout is established by an exact fit, the importer reads that one
layout and **requires the exact fit**. A body that does not fit (including the
newer layout with a name per group, which a test pins) is refused with a
reconstruction error that says what was tried, and the model loads without
physics, with the reason reported. It is never read on a best guess.

## 4. What the importer reports

| Situation | Result |
|---|---|
| no `CubismPhysicsController` | no physics, silently (legitimate) |
| body fits no layout | note; model loads without physics |
| more than one controller | note; the first is used |
| `m_Enabled` is 0 | note; **not recovered** (see below) |
| an output reads a particle outside `1..particleCount` | that output is dropped, note |
| a group has a non-finite value, a weight outside 0..100, a negative radius or an unknown component | that group is dropped, note |
| gravity or wind non-finite, or nothing usable left | no physics, note |
| an input or output id names no parameter | kept; reported by `validate` and at import, derived from the IR |

A **disabled** controller is not recovered. The source ran with no physics, and
a viewer that swayed hair the original never swayed would show something else.
The body is still decoded first, so a layout change is reported either way. All
three real models are enabled.

A group with no chain (no particles) is dropped and reported; a group left with no outputs is kept: it does nothing, but dropping
it would hide that the source had it.

On real data, one of the three first models has an output whose particle index is
one past the end of its two-particle group (it is that group's only output).
The SDK generation these rigs come from stops processing a group's outputs at
the first one it cannot use, so the importer does the same: the bad output and
every output after it are dropped, and the note says how many. Later SDKs skip
only the bad one. It made no difference here because the output was alone.

Import notes number groups by their place in the **source** ("source physics
group N"); `validate` and the model number them by their place in the IR, which
differs once a group has been dropped.

## 5. Package storage

`.a2dpack` format version 5 appends the rig after `warp_keyform_opacities` (see
`crates/a2d-pack/src/cubism_io.rs`). A version 4 package still loads, with no
physics, and `validate` tells the user to re-import. The same rules are checked
on write and on read (finite values, weights in `0..=100`, radius not negative,
ranges in order, every output reading a particle inside its chain), so a package
that writes is a package that reads.

## 6. What is unverified

* **Disabled controller means no physics.** That a controller with `m_Enabled`
  of 0 did not run in the source is assumed. An older SDK's update path may
  still evaluate a disabled controller when an update controller gathers
  components, so the importer's choice to skip it is not established.
* **Inverted normalization ranges.** The IR stores each range as
  `minimum <= maximum`. The SDK's movement threshold (0.001 times the serialized
  position *maximum*) uses the raw value, which this loses for a range serialized
  the wrong way round. Real data has maximum above minimum, so nothing is lost
  there; a rig with an inverted range would differ slightly.
* **No comparison with the official runtime.** The layout is verified; what the
  numbers *mean* (the evaluation of the rig) is not part of this work and is
  checked later against the SDK's description and hand-computed steps.
* The newer layouts (a name per group, a frame rate) are known from the SDK's
  history but **have never been seen in a real bundle**, so they are not read.
  A bundle built with them will report that it fits no known layout.
* The SDK's lower version bound.
* Whether the unused translation scales and input angle scale are ever non-zero
  in the wild. They are zero in all three models, which is all that is known.
* That the stored initial positions are always derivable from the radii; this
  was checked on three models only.
* Three models are all that has been seen: groups with non-angle inputs on Y,
  or X/Y outputs, do not occur in them, so those branches are exercised only by
  hand-built bodies.
