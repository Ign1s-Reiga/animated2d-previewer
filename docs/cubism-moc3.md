# The MOC3 container, as reverse-engineered

MOC3 is Live2D Cubism's compiled model format. It is undocumented, and this
project decodes it with an independent parser rather than the proprietary
Cubism Core (`CLAUDE.md` §13.1).

This document records **what the format is**, separately from the code that
reads it, and — just as importantly — **how each claim was established**. A
reverse-engineered layout is only as trustworthy as its evidence, so every
section below says what would have failed had the reading been wrong.

The implementation is `crates/a2d-cubism/`: `moc3.rs` reads the container,
`eval.rs` poses a model, `emit.rs` turns a pose into renderer-neutral meshes.
The model those work on is `a2d-core`'s `ir::cubism::CubismIr`, which lives
there rather than beside the reader because a package stores it and `a2d-pack`
may not depend on a format crate. `a2d-pack`'s `cubism_io.rs` writes it into
`model.bin`, and `a2d-import`'s `unity_cubism` reconstructs both from a
bundle.

---

## 1. Container

```text
0x00  "MOC3"
0x04  u8   format version
0x05  u8   non-zero if the file is big-endian
0x06  ..   reserved through 0x40
0x40  u32[] section offsets
```

The offset table has no stored length: **the first offset marks where the table
ends**, so the table holds `offsets[0] / 4` entries minus the header words.

Every section is a flat array addressed by its offset. Sections are padded, so
the distance to the next offset is an *upper bound* on the bytes a section uses,
never its exact size. Nothing may be inferred from that distance.

Identifiers are fixed 64-byte slots, NUL-padded.

Format versions seen so far append sections rather than reorder them, so a
version above the highest checked is read on the same layout rather than
refused. Reading is bounds-checked throughout, and every declared count is
validated against the bytes actually present before anything is allocated — an
unvalidated count once turned a single corrupted byte into a 51 GB allocation.

## 2. Section indices

The table is positional, which is what makes the format addressable. Indices
used by the parser:

| # | Contents | | # | Contents |
|---|---|---|---|---|
| 0 | counts | | 50 | parameter ids |
| 1 | canvas | | 51 | parameter maximums |
| 3 | part ids | | 52 | parameter minimums |
| 11 | deformer ids | | 53 | parameter defaults |
| | | | 54 | parameter repeat flag (`u32`, 0 or 1) |
| | | | 55 | parameter decimal places (`u32`, not read) |
| 16 | deformer parent | | 56 | parameter binding begin |
| 17 | deformer type (0 warp, 1 rotation) | | 57 | parameter binding count |
| 18 | deformer index within its type | | 59 | warp keyform opacity |
| | | | 60 | warp keyform position offsets |
| 19 | warp keyform binding | | 61 | rotation keyform opacity |
| 20 | warp keyform begin | | 62 | rotation keyform angle |
| 21 | warp keyform count | | 63 | rotation keyform origin x |
| 22 | warp grid points | | 64 | rotation keyform origin y |
| 23 | warp divisions A | | 65 | rotation keyform scale |
| 24 | warp divisions B | | 71 | keyform positions |
| 25 | rotation keyform binding | | 72 | parameter binding refs |
| 26 | rotation keyform begin | | 73 | keyform binding begin |
| 27 | rotation keyform count | | 74 | keyform binding count |
| 28 | rotation **base angle** | | | |
| 33 | drawable ids | | 75 | parameter key begin |
| 34 | drawable keyform binding | | 76 | parameter key count |
| 35 | drawable keyform begin | | 77 | parameter keys |
| 36 | drawable keyform count | | 78 | vertex UVs |
| 40 | drawable parent deformer | | 79 | vertex indices |
| 43 | drawable vertex counts | | 87 | draw-order object list (the tie-break order, §6) |
| 44 | drawable vertex offsets (floats) | | 90 | glue ids |
| 45 | drawable index offsets | | 91-100 | glue, see below |
| 46 | drawable index counts | | | |
| 68 | drawable keyform opacity | | 69 | drawable keyform draw order |
| 39 | drawable part index | | 41 | drawable texture index |
| 42 | drawable flags (**bytes**) | | 47 | drawable mask begin |
| 48 | drawable mask count | | 80 | drawable mask list |

Draw-order groups, sections 81 to 88, sit beside it. Per group (count slot 18):

| # | Contents |
|---|---|
| 81 | group object begin (into the flat object arrays) |
| 82 | group object count |
| 83 | group total object count (counting nested groups) |
| 84 | group maximum draw order |
| 85 | group minimum draw order |

Per object, across all groups (count slot 19):

| # | Contents |
|---|---|
| 86 | object type: 0 art mesh, 1 part |
| 87 | object index: the drawable (or part) it names |
| 88 | object self index: the group a part object opens, -1 for none |

Only 81, 82, 86, 87 and 88 are read; 83 to 85 are not. The reading is
positional, taken from an independent MIT-licensed MOC3 reader's section table,
and its indices agree with every index this document established on its own.
Sections 84 and 85 equal the maximum and minimum of section 69 over the whole
model on all six real models measured, which fits the grouping reading.

A wrong index here fails loudly rather than returning a plausible model: each
section is required to hold exactly the declared number of well-formed entries.

### How the drawable side tables were identified

| # | Width | Contents | How it was established |
|---|---|---|---|
| 39 | u32 | part index per drawable | `u32::MAX` for none; the values group exactly as the part names do — every eye mesh shares the part named `eye`, every front-hair mesh the one named `hair_front` |
| 41 | u32 | texture index per drawable | all zero on every model seen, which is what a single-page model looks like and confirms the one-page assumption the emitter makes |
| 42 | **byte** | drawable constant flags | one byte per drawable; only two values occur, 4 on 529 meshes and 6 on 39. Bit 2 is double-sided and bit 1 multiply-blend — the meshes carrying 6 include one the model itself names as a shadow |
| 47 | u32 | first clipping mask per drawable | a running offset |
| 48 | u32 | clipping mask count per drawable | begin plus count equals the next drawable's begin for **all 567 adjacent pairs**, and the last lands exactly on count slot 17 — the same closure argument that fixed the drawable tables |
| 80 | u32 | the flat mask list, as drawable indices | the only section in three models whose length matches count slot 17 and whose every entry is a valid drawable index. Then confirmed by meaning rather than by shape: it clips `eye_r01` and `eye_r02` to `eye_r03`, which is an iris and a highlight clipped to their own eye white |
| 70 | u32 | drawable keyform position offsets | the offsets the padding rule of §3 already predicts, stored explicitly; useful as a cross-check |

Section 42 is a **byte** array, not a word array. A scan that assumes four-byte
entries walks straight past it, which is why it was missed on the first pass.

Sections 4 to 9 are per *part*: 5 is the identity, 4, 6, 7 and 8 are constant on
every model seen, and 9 is the parent part index with `u32::MAX` at a root.
Notably **none of them orders the parts**, which matters for §6. The full part
layout is under "Parts" in §6.

### Glue: sections 91 to 100 and count slots 21 and 22

A glue stitches vertex pairs of two meshes that hang off unrelated deformer
chains. Authoring tools use it where one surface was split into several meshes
(a torso skinned in two pieces, cloth cut along a seam). Section 90 holds the
ids and count slot 20 the number of glues; the rest was located by the same
closure arithmetic as everything else.

| # | Width | Contents | Per |
|---|---|---|---|
| 91 | u32 | keyform binding index | glue |
| 92 | u32 | keyform begin, into section 100 | glue |
| 93 | u32 | keyform count | glue |
| 94 | u32 | mesh A, a drawable index | glue |
| 95 | u32 | mesh B, a drawable index | glue |
| 96 | u32 | pair-list begin, into sections 98 and 99 | glue |
| 97 | u32 | pair-list entry count, always even | glue |
| 98 | f32 | weights | pair-list entry |
| 99 | **u16** | vertex indices | pair-list entry |
| 100 | f32 | glue keyform intensities | glue keyform |

Count slot 21 is the total number of pair-list entries and slot 22 the total
number of glue keyforms. Entries come in twos: entry `2p` is the vertex on mesh
A and its weight, entry `2p + 1` the vertex on mesh B and its weight. Section 99
is `u16`, like the drawable index table in 79, not `u32`.

What established it, on one real model with 122 glues:

* Every section's size fits its declared count, and each glue's begin plus
  count equals the next glue's begin, ending exactly on slot 21 (2008 entries)
  for the pair lists and on slot 22 (122) for the keyforms.
* Every pair-list count is even, and every vertex index lies inside the mesh it
  indexes — all 1004 pairs.
* At the default pose, the two vertices of every one of the 1004 pairs
  **coincide exactly** (gap 0 on all 122 glues). A misread table would not do
  that.
* The two weights of every pair sum to one.
* The section order agrees with the community-published MOC3 layout, which also
  agrees with every index already pinned above.

**The application rule**, verified by rendering rather than by a reference
runtime. With `I` the glue's intensity, blended from its keyforms through its
keyform binding exactly as any other element's keyforms are, and both positions
read before either is written:

```text
A' = A + (B - A) * weight_a * I
B' = B + (A - B) * weight_b * I
```

Applied after every drawable has been carried up its deformer chain, before the
y flip. Without it, posing from parameters alone (no motion) opens a transparent
gap of about 0.011 units between two torso meshes at a single parameter value,
and the worst gap over all five of that model's motions is 0.0126 units. With it
the gap is closed to float noise.

Not yet exercised by any model seen: an intensity other than 1 and a binding
with more than one keyform. The synthetic tests cover both by construction.

**Weights need not sum to 1.** On the first real model every pair did, but on
two of three models measured some pairs sum below 1 (down to 0 for a fully
released pair). The Cubism Editor lets an artist partly or fully "release"
glued vertices (<https://docs.live2d.com/en/cubism-editor-manual/glue>), so
such a pair is meant to stay open: with intensity `I` its gap after glue is
`|1 - I(wa + wb)|` times its gap before, whichever weight belongs to which
mesh. That is a feature of the data, not a decoding error; the real-asset test
checks it as an identity rather than asserting closure.

**A vertex can be in two glues.** One was seen in one model (a vertex of a
chain mesh named by two neighbouring glues). Glues apply in file order, each
seeing the earlier result; no one-pass order closes both glues there (file
order leaves 0.024 canvas units on the first, reverse order 0.062 on the
second, simultaneous reads 0.006 and 0.061). The order is a guess and is pinned
by a synthetic test marked unverified.

**Which weight belongs to which mesh** is supported, not settled. A pair's
share `wa / (wa + wb)` rises along the line from mesh A's rest centroid to
mesh B's in all 282 glues (across the three models) whose weights vary and in
none the other way, and swapping the weights increases triangle flips caused by
glue (from 0 to 43 over motions on one model). That is evidence from seam
geometry, not a reference runtime.

**Hidden partners.** A glue is applied whatever the opacity of either mesh,
and no opacity rule is added to it. The glues that once looked wrong, with a
visible mesh pulled by up to 0.6 canvas units towards a transparent partner,
were pairs of alternate pieces that the artist hides by fading a deformer; see
"Deformer opacity" in section 6. Once that fade reaches the drawables, the
pre-glue gap of every pair whose two meshes are both shown is under 0.1 canvas
units. A glue toward a mesh that is only partly faded still pulls a visible mesh
by more than 0.005 canvas units during a crossfade (largest seen 0.29, at an
opacity of 0.06), which is taken as authored.

## 3. What was confirmed, and against what

Each of these was checked against something independent of the guess itself.

**Counts and identifiers — checked against Unity.** These bundles carry the
Cubism Unity integration's own components beside the MOC3: one `CubismPart`,
`CubismDrawable` or `CubismParameter` object per element, each named for its
identifier. Comparing the two sides matched exactly on both the counts (195
parts, 601 drawables, 849 parameters) and on every identifier string in all
three sets.

**Which array is minimum, maximum, default.** Three unlabelled float arrays.
Only one assignment satisfies `min <= default <= max` for all 849 parameters,
and the result is textbook Cubism: `ParamAngleX` spans ±30, `ParamEyeLOpen`
runs 0 to 1.2 with a default of 1.

**Canvas field order.** Settled by the origin being exactly half the size.

**Drawable tables — checked by arithmetic that has to close.** The per-drawable
offsets are cumulative, so each must equal the previous plus its own size, and
the last must account for exactly the totals the count table declares. On a real
model both close exactly, and all 27756 triangle indices land inside their own
mesh.

**The keyform pool's division — checked by prediction, not by fitting.** Walking
the pool — warp deformers first, each keyform padded to a multiple of eight
points, then drawables under the same rule — reproduces every one of the 9554
stored offsets and ends precisely on the declared total. A wrong padding rule or
a wrong ordering misses on the very first deformer.

**Warp grid orientation.** Two division counts arrive with nothing to say which
is rows and which is columns, and for a square grid it cannot matter. Reading
every non-square grid across six models both ways settled it: with
`divisions.1 + 1` points to a stored row, 713 of 729 grids are perfectly
monotone lattices and most of the rest are coherently mirrored; the other
reading interleaves rows and leaves not one grid monotone.

**A correction, and a caution about determinants.** These coordinates were once
read the other way round — pairs swapped, angles negated, the lattice
transposed — on the strength of a determinant argument that turned out to be
backwards. The argument was: a drawable's mesh is the same shape in uv space as
in canvas space, so only a mirror flips the sign of the map between them, and
one mirror must be expected because texture rows run down while `y` runs up. The
second half is wrong for this pipeline: `a2d-unity` **flips Unity's bottom-up
rows** when it decodes a `Texture2D`, and that flip is what makes the two axes
agree. A correct model therefore fits *positive*, not negative.

Two lessons are worth keeping. A determinant detects a mirror and **nothing
else** — it is blind to rotation, so it can never confirm that a model is the
right way up. And a convention argued from first principles is worth less than
the same argument checked against the code that actually runs: the row flip was
forty lines away and had been read, and was still forgotten.

What no measurement here settled was whether the character is upright. It is
not: this one is drawn reclining, head to the right, which is why every attempt
to assert that a face should be level produced a wrong answer.

Anything not on this list is left unparsed rather than guessed at. The raw
section table is exposed so later work can extend the parser without
re-deriving the frame.

## 4. There is no resting pose in a MOC3

A drawable's coordinates are not stored. They are produced by blending that
drawable's keyforms according to the current parameter values, and the result
is in the space of whatever deforms the drawable — not in canvas space. A MOC3
therefore cannot be drawn without evaluating it.

### Parameters to keyforms

Each element (warp, rotation, drawable) names a *keyform binding*. A binding
lists one or more axes; each axis is a *parameter binding*, which names a
parameter and a strictly increasing list of keys inside that parameter's range.

The element's keyforms form a grid with one axis per binding, so the keyform
count equals the product of the axes' key counts — an identity checked on parse
for every element.

Evaluation is a multilinear blend: locate the parameter's value between two
adjacent keys on each axis, then blend the surrounding keyforms. Values outside
the key range clamp rather than extrapolate, since a parameter is already
clamped to its own range and the key list covers it.

**Keyform ordering is first-axis-fastest.** With axes `[A, B]`, keyform index is
`i_A + i_B * len(A)`.

*What established it:* an additive fit over each element's keyform values. A
rig's keyform grid is close to additively separable — each parameter
contributes its own offset — so fitting `v[i][j] ~ mu + r_i + c_j` and reading
off the residual says which reshape is the real grid. The comparison is fair in
a way an earlier smoothness argument was not: an `a x b` additive model and a
`b x a` one have the *same* number of free parameters over the *same* number of
samples, while counting neighbour pairs gives unequal axes incomparable totals.

Across the six models, 122 of the 128 decidable two-axis grids and all six
decidable three-axis grids come out first-axis-fastest, by a mean margin of
0.38 against 0.06 for the six exceptions. Grids whose axes are all the same
length decide nothing — the two reshapes are transposes of one another and the
fit is identical — which is why the earlier reading survived: it was checked on
a model where the informative case was never isolated.

One element settles it on its own without any statistics. A root rotation
deformer bound to a four-key axis and a five-key axis carries twenty keyforms
whose origins change with a period of **four**, not five, and whose x sweeps
monotonically within each group of four. The fastest axis has four keys, and
four is the length of the first axis.

### The deformer chain

Deformers form a forest; a drawable names one parent deformer and inherits the
chain above it. There are two kinds.

A drawable need not name a deformer at all: section 40 stores `0xFFFFFFFF` for a
mesh parented straight to the model root, and such a mesh is already in model
space. This is the same "none" sentinel used by a deformer's own parent
(section 16) and by a drawable's part (section 9), so it is the format's general
convention rather than a special case here.

*What established it:* reading the field as a plain index rejected two of the
six real models available, each naming deformer 4294967295 out of 220 and 456
respectively. Both load once the sentinel is honoured, and the meshes concerned
are ones with nothing above them to inherit. The failure is loud rather than
subtle only by luck — the value is far out of range, so it tripped a bounds
check instead of deforming something quietly.

A **warp deformer** is a grid of control points, stored row-major with
`divisions.1 + 1` points to a row. Its children live in the grid's unit square, and
posing a point is a bilinear lookup. Outside the square the edge cells are
*extended* rather than clamped, so geometry that overhangs a deformer keeps its
shape instead of collapsing onto the border.

A **rotation deformer** is a rigid frame: origin, angle in degrees, uniform
scale, opacity. It also carries a **base angle** — section 28, one float per
rotation deformer, constant rather than per keyform — and the frame's real
angle is the base plus whatever the keyforms blend to. Posing a point is
`origin + R(base + angle) · (point · scale)`.

*What established the base angle:* rendering. Without it, one model of six
posed its character a quarter turn over and outside her own canvas while the
scenery beside her — parented to the model root rather than to a deformer, and
so untouched by the chain — stayed exactly where it belonged. That split is
what made it findable: two halves of one file disagreeing about where the model
is. Adding section 28 to the blended angle puts her upright, seated at the
furniture the scenery draws, and does the same for the other five: it corrects
poses in all of them, including ones that had looked right.

The section is one float per rotation deformer, padded, in degrees, spanning
±360, and non-zero on 26 to 215 of the deformers in each of the six models — it
is ordinary data, not a rare special case. Reading it is optional: a layout
without it leaves every base at zero, which is exactly how the chain behaved
before the field was identified.

Deformers are resolved parents first, from the root down, and a drawable is then
carried through its resolved parent alone (section 4).

### Which way is up

**A MOC3 stores y running down the canvas**, the way an image's rows do. Every
space in the file agrees on this — keyform points, deformer grids, rotation
origins — so nothing inside the format needs converting; the flip belongs at the
boundary where a posed model leaves `formats/`, and `Moc3::pose` is where it is
applied.

*What established it:* not an argument from conventions, which got this wrong
once already. The Spine path is known to render upright, so the sign it produces
for the map from a mesh's texture coordinates to its posed positions is the
convention the renderer and texture pipeline actually implement. Measured the
same way on both:

| path | meshes | determinant positive | negative |
|---|---|---|---|
| Spine, 8 real rigs | 1276 | 3 | **1273** |
| Cubism, 6 real models | 1797 | **1771** | 26 |

The two disagree, and the Cubism models rendered upside down. Which side was
inverted — the positions or the texture coordinates — was settled the same way:
negating position y renders a coherent upright model, while flipping uv `v`
satisfies the determinant equally but shreds the art across meshes.

The earlier reasoning held that the sign should be positive because `a2d-unity`
flips Unity's bottom-up rows when it reads a `Texture2D`. That flip is real; the
conclusion drawn from it was not.

### How deformers compose

Deformers are resolved into canvas space **parents first**; a drawable is then
carried through its resolved parent only.

- **Warp.** Its grid is carried through its parent, so the lookup lands in
  canvas space. It adds no scale: its total scale is its parent's, one at the
  root.
- **Rotation under a rotation.** Origin through the parent, angles add, scales
  multiply.
- **Rotation under a warp.** Only a *position* and an *angle* come from the
  warp. The origin goes through the warp; the turn is the direction of a short
  probe along the warp's own up axis (`0.1` of its unit square, retried from
  the other side and shorter, up to ten times) against where an unbent warp
  would point it. Total scale is the parent's times the rotation's own. **A
  warp lends no scale**, and everything beneath the rotation stays rigid.
  If the warp is squashed so flat that no probe has a direction, the rotation
  keeps its origin and its own angle and the model keeps posing.

A point in a rotation's frame lands at `origin + R(angle) * (p * total scale)`,
where a rotation's total scale is the product of every rotation scale above it.
The root rotation is the pixels-to-units bridge: its scale is the reciprocal of
the canvas's pixels-per-unit (`0.00026192` against 3792 px/unit, reciprocal
3818, on one model).

**Evidence.**

- The Cubism Editor manual, on a warp parent with a rotation child: moving the
  warp's control points makes the rotation and its children "move and rotate
  rather than deform".
- *Texture proportions.* A rigid mesh -- one keyform, hung off a rotation -- is
  an exact similarity of its texture layout. Posed under this rule it stays
  one to within `1e-5` on three real models, across every sampled frame of
  every motion. Under the previous rule (a per-axis unit taken from the warp's
  bounding box, children pushed through the warp's bilinear lookup) a hem
  panel was 4.2:1 against its texture, and fabric strands pumped between about
  0.5 and 3.4 times their size within one motion.
- *Twin meshes.* Models carry identical local meshes on two independent chains
  (one through several warps and rotations, one under a root warp). Their posed
  sizes agree exactly under this rule and differed by up to 14% under the old.
- Glue closure at rest never got worse (a model's closed glues: 97 -> 98 of
  121; others unchanged).

An earlier version of this section said the exchange rate between a warp and a
rotation under it was the warp's grid extent, and that forcing the rate
isotropic "changes no model measurably". The second claim was false: on one
model it takes a hem panel from 4.2:1 to 1:1. What the earlier reading got right
is the observation it started from -- a rotation sitting at `(0.501, 0.500)`
under a warp, dead centre of the unit square, with its own child back at
pixel-sized coordinates. The origin is a position in the warp; the child lives
in the rotation's own space. What it got wrong was inferring that the warp
rescales that space.

Canvas fit could not have caught this: the old rule landed every drawable
inside its canvas. Only the texture could.

## 5. Stored defaults are not display values

A MOC3's parameter defaults are the values the *rig* rests at, not necessarily
the values a model is shown at.

One model of six ships a zoom parameter running 0 to 10 whose stored default is
8. That drives the root deformer's scale between two keyforms differing sixfold,
so at its own defaults the model comes out about five times too large: geometry
spanning 4.57 by 4.79 against a canvas of 1.00 by 1.35, with an ordinary
canvas-wide backdrop sheet measuring four and a half canvases across.

Wound back to 0, the same model measures 0.91 by 0.96 — a clean fit — and every
drawable is ordinary.

The display values live on the Unity side, one `CubismParameter` component per
parameter (a bundle with 350 parameters, 155 parts and 667 drawables carries
1177 GameObjects). **Recovering them belongs to the importer**, which is the
only layer allowed to know anything source-specific (`CLAUDE.md` §2).

Until that exists, a viewer should frame the **canvas** rather than the posed
geometry. The canvas does not move, so one mis-scaled drawable cannot shrink
everything else to nothing.

## 6. Opacity, draw order, and why a face can come out with no eyes

Two per-keyform tracks sit beside the coordinates and blend by exactly the same
weights.

**Opacity** (section 68) is one float per drawable keyform, every value inside
`[0, 1]`. Cubism hides a part by taking it to zero opacity rather than by
removing it, so a decoder that paints every drawable opaque covers whatever
should have shown through. A fully transparent drawable is not emitted at all.

**Draw order** (section 69) is the artist's per-drawable value and the thing
motions animate. Across six real models it runs 200 to 1000, most values at a
resting 500. It is blended with the same weights as the coordinates, and the
paint order is derived from it:

1. each drawable's blended draw order is rounded to the nearest integer
   (non-finite counts as 500);
2. drawables are painted back to front in ascending order;
3. drawables with an equal value are ordered by position in the draw-order
   object list (section 87): the earlier entry paints first.

Drawables the list does not name go last. Rounding rather than truncating
matters: the weights sum to one only within float error, so a resting 500 can
blend to 499.99997, and truncation would invent a reorder on a model whose draw
orders never vary.

What established it: the Unity renderer component serializes the render order
the official Cubism Core reported when the prefab was built. On four real
models those values form a permutation of `0..n`, and the rule above, evaluated
at the model's default parameters, reproduces them **exactly** — every pair of
drawables agrees (67,528 / 108,345 / 230,181 / 5,151 pairs). The alternatives
fail on the same data: section 87 painted as given differs in 362, 466, 0 and
101 positions; ties reversed in 362, 464, 678 and 90; ties by drawable index in
348, 465, 677 and 92. Ties are not rare — at defaults most adjacent steps along
section 87 are equal draw orders — so the tie direction is heavily exercised.
Two further real models carry renderer values that are not permutations (stale,
from another revision of the model), so the rule is unchecked on them.

Section 58 holds a per-part draw order on the same scale. It is 500 for all but
a few parts, and no part is in a draw-order group on any model seen, so it takes
no part in the ordering here. The editor manual says a part's draw order only
takes effect once the part is grouped.

### Deformer opacity: sections 59 and 61

Every deformer keyform carries an opacity as well: section 61 for rotations
(read alongside their angle and origin) and **section 59 for warps**. Section 59
is identified like section 68: exactly one float per warp keyform (count slot 7
equals the number of warp keyforms, and the room before section 60 holds that
many floats after padding), every value inside `[0, 1]`, only two or three
distinct values per model, and a few dozen to a hundred and forty zeros.

The rule, as implemented: a deformer's opacity is its blended own opacity times
its parent's, so it is the product of every ancestor's; a drawable shows its own
blended opacity times its parent deformer's. Geometry, glue and masks do not
change, and a drawable whose deformer is transparent still works as a mask, as
a transparent drawable always did.

What established it:

* The Live2D Inspector manual says a deformer's opacity changes the opacity of
  the art meshes that belong to it, and the modeling menu's "apply deformer to
  child elements" folds a deformer's opacity into its children.
* Real models hide alternate pieces this way. At rest, 91, 33 and 280 drawables
  are hidden across the three models measured, against 26, 2 and 52 by their own
  opacity alone. Rendered without the rule, a model shows duplicate hands,
  lanterns and shawl or ribbon variants stacked on the intended ones; with it,
  one of each.
* It explains the glue gaps: the pairs that drifted by up to 0.6 canvas units
  were alternate pieces hidden this way, and with the rule the worst pre-glue
  gap while both meshes of a glue show is 0.042, 0.012 and 0.089.

What is not established: whether ancestors combine by multiplication (rather
than a minimum, or only the direct parent counting), which rests on the manual's
wording and the renders; how a partial opacity between 0 and 1 should look,
which no reference was compared against; and the deformers' visibility and
enabled flags (sections 13 and 14, and 37 and 38 for drawables), which are not
read. A part's own opacity is also not applied; see "Parts" below.

### Parts: sections 2 to 9, 15 and 58

| Section | Type | Per part | Established by |
|---|---|---|---|
| 2 | u32 | runtime space | order of the part block in the open-source `moc3` crate's layout; not read here ("not read") |
| 3 | string | part id | read and used (`part_ids`) |
| 4 | u32 | keyform binding index | the same crate; on all six real models it names a binding with no axes, so no parameter drives a part |
| 5 | u32 | keyform begin | the same crate; the identity on all six |
| 6 | u32 | keyform count | the same crate; 1 per part on all six, and count slot 6 equals the part count |
| 7, 8 | u32 | visible, enabled | the same crate; 1 on every part of all six |
| 9 | u32 | parent part, `u32::MAX` at a root | the crate, and independently here: every index is in range and the trees are shallow (depth 0 to 3, up to 33 roots) |
| 15 | u32 | (per deformer) the part it belongs to | the crate (13 and 14 being its visible and enabled flags, as noted under Deformer opacity above); in range wherever set, `u32::MAX` on a handful of deformers per model |
| 58 | f32 | (per part keyform) draw order | see above |

**A part keyform holds a draw order and nothing else.** The `moc3` crate's
part-keyform layout has only section 58, and the real models agree (one keyform
per part, no axes). As supporting evidence only, section 59 directly after it is
already the warp keyforms' opacity, so nothing sits between them. In a MOC3 a
part's opacity is therefore not stored; it is a value the host or a pose sets at
run time. That the core folds it into the drawable opacity it reports is
inferred from how the renderers draw (they multiply only the drawable opacity
and the model opacity), not stated by any source read.

On the six real models examined, that run-time value has no source anywhere in
the bundle (the first point below is pinned by a test, the other two were
checked once by a throwaway probe of the six bundles and are not tested): no clip binds a `CubismPart` property (zero part-opacity bindings in
30 clips, those clips carrying only parameter values and, on some, a constant
model opacity); every `CubismPart` component in the prefabs stores opacity 1
(its body is an index and that float, nothing more); and the pose controller
component is present but no pose part component exists, so no pose group is
defined. Applying part opacity or pose would change nothing on screen for these
models, which is why neither is implemented. The importer now says so by name
when a clip does bind a part (part-opacity curves are counted and, where the
path resolves, the part ids listed), and `real_part_opacity.rs` pins that no
clip binds a part, and that the MOC3 and the prefab agree on the part count.

Open semantics, to settle only when a real asset needs them and against a
reference viewer:

- whether a part's opacity reaches drawables of its child parts (a product down
  the parent chain of section 9) or only drawables whose section 39 names that
  part;
- whether it also reaches drawables through a deformer's part (section 15). On
  real models a deformer's part differs from the part of the drawable under it
  for roughly 3 to 46 percent of drawables, so the two rules give different
  pictures;
- the combination with deformer opacity (most likely a plain product);
- pose itself: fade timing, which parts form a group, linked parts.

### Section 87 is the group's object list, not a paint sequence

Section 87 lists the drawables of the model's one draw-order group. Painting it
as given is wrong at rest on three of the four models checked (on the fourth
the list alone happens to be exact): it breaks ties and nothing else. Slot `k` is
*not* a per-drawable value saying where each one sits either — that reading is
the inverse permutation, which is what first shipped.

The inverse is dangerously plausible. It is still a permutation, it still paints
every drawable exactly once, and the result still looks broadly like the
character. What it does is scatter the paint order locally: on one model it put
the face's skin (`face_02`) over the eyes, which is why that model rendered with
a blank face, and it ordered one eye's lashes behind its own eye while the other
eye's sat in front — an asymmetry that no coherent order would produce, and the
clue that the reading was wrong rather than the data.

The fixture pins the direction with a *rotation* rather than a reversal,
because a reversal is its own inverse and could not tell the two apart.

All six real models have exactly one group, count slot 19 equal to the drawable
count, every object of type 0 with self index -1, and every drawable listed
exactly once; none repeats an entry. That shape reads straight through. Any
other (several groups, part objects, nested groups, a repeated or out-of-range
entry) is reported as a load note and reduced to the art-mesh objects in list
order, with any drawable left out appended in model order, so nothing goes
unpainted. A layout without sections 81 to 88 reads the first `n` entries of
section 87 and says so.

### Why the eyes were invisible

Two problems, both now fixed:

1. the irises were unclipped, drawing their full quads rather than being cut to
   their own eye whites (sections 47, 48 and 80);
2. section 87 was read as a per-drawable key rather than as a list, painting the
   face skin over the eyes.

The second was the one that mattered — the eyes were there the whole time,
behind the face's own skin.

A third suspect was investigated at length and was not a fault: the model looks
turned because **the character is drawn reclining**, head to the right. That was
twice diagnosed as a decoding error, and the second diagnosis was acted on
before being reverted. §3 records the argument and why it was wrong. Nothing in
the format says which way up a character stands; only someone who knows the
character can say.

### Repeating parameters: section 54

Cubism's editor can mark a parameter as repeating: its two ends join, so a value
past the maximum comes round to the minimum. The switch is stored in the MOC3,
one little-endian `u32` per parameter (count slot 5), 0 or 1, in **section 54**.
It is addressed and aligned exactly like sections 51 to 53, 56 and 57: 64-byte
aligned, `4 * n` bytes rounded up to 64, the padding zero. **Section 55** is one
`u32` per parameter of the same shape holding the editor's "decimal places" (3 on
every parameter of every model measured); it is display metadata, is not read,
and exists in the code only as a named constant.

What established it:

- Position. An independent MIT-licensed MOC3 reader lists nine parameter offsets
  in its oldest layout (v3.00): runtime space, ids, maximum, minimum, default,
  *is repeat*, *decimal places*, binding begin, binding count. That is sections
  49 to 57, and 50 to 53, 56 and 57 are the indices this document had already
  established on its own, which pins 54 and 55 by position.
- Values. On all six real models section 54 holds only 0 and 1 as whole `u32`s
  (the first bytes of one read `01 00 00 00 01 00 00 00`, not packed bytes), and
  its distance to section 55 equals the distance between the neighbouring
  parameter sections in the same file.
- Meaning. On the one model with repeating parameters, twelve are flagged. All
  twelve are rotation controls whose keyform angles cover a full turn over the
  range (-180, 0, 180 at keys -30, 0, 30 on `[-30, 30]`). A second, low-detail
  sibling of that model flags two parameters that have no bindings at all and are
  held at 0 by every motion: an inherited, inert flag. Four other models flag
  nothing, though each has parameters that drive a single-axis rotation over
  360 to 540 degrees and stay in range: the flag is an authored opt-in, not
  something geometry implies.
- The version. A header byte of 2 is the Core's "moc3 file version 3.3.00 -
  3.3.03" and these files carry the flag, so a 3.3 file has it. The Core's getter
  for it is much newer (Core 05.01.0000, 2025) than the data; no new MOC version
  came with it. Which editor version first wrote non-zero values is not known.

Reading (`parameter_repeats` on the IR, `Moc3::parse`): present gives one `bool`
per parameter; a section that runs past the file or holds a value other than 0 or
1 is an error (a wrongly read flag would wrap a parameter that should clamp); an
absent section gives `None` and a note. The package keeps the flags from format
version 6; an older package has none.

`CubismIr::repeating_parameters` returns exactly the flagged parameters when the
flags exist. Only with none (a source without the section, or a package older
than format 6) does it fall back to the inference this crate used before the flag
was found: a parameter repeats when some motion curve driving it has **every** key
on the same side of `[min, max]`, at least half the range beyond that end (a curve
that straddles the range, say -90 to 90, is a deliberate sweep and does not
count). With flags, `repeat_degradations` reports only a parameter that inference
would flag and the source does not (it is clamped, and the note says why); with
none it reports every inferred parameter. Playback folds a repeating parameter's
value into `[min, max)` (the maximum maps to the minimum) instead of clamping it.

How the inference measured against the flag, on all six real models: it finds a
subset of the flagged set every time and never a parameter the source leaves
unflagged. On the model that has them, it found six of the twelve flagged. The
other six climb from 0 by one to three ranges in one motion and return to 0, and
the original rule left them out on purpose: a ramp that visits the range and
comes back is as likely a value the author expected to saturate. They were
repeating parameters all along. Clamped they sit at the maximum for most of that
motion; wrapped they spin one to three turns. A looser rule would also have caught
two parameters on that model that are not flagged (one key just past the maximum,
a Hermite-size overshoot; one `[0, 1]` parameter with a single key at 2), so the
inference stays narrow.

Why wrapping matters, from the same model: clamped, a linear ramp 0 to 180 over
the clip freezes at 30 after a fraction of the clip and snaps back at the loop
wrap (a vertex move of 0.0357 canvas units over eleven drawables and an opacity
pop of 0.70); wrapped, the wrap is closed (0.0002, no opacity change).

Unverified:

- Whether the closed Core uses the flag inside its own update. The data proves the
  field is written; what the Core does with it is not observable here. The public
  Framework applies repeat when a parameter value is *set*, not during evaluation,
  and both official SDKs need an opt-in before they honour the editor's flag, so
  whether the source game's runtime wrapped these values or clamped them is open.
- The boundary. This crate folds into `[min, max)`, so a value exactly on the
  maximum (modulo the span) becomes the minimum. The Framework keeps the closed
  interval `[min, max]`: a value on the maximum stays there, and exact multiples
  land on the minimum above the range and on the maximum below it. The two
  differ only for a value exactly on a boundary, and matter only where the
  keyforms at the two ends differ (0.0199 canvas units at worst on the one model).
  Ours follows the editor manual's description (the maximum is the same point as
  the minimum); the Framework follows its code. Not changed.
- The editor manual says that with repeat on, the rightmost key is "no longer
  recognised" and the slider jumps to the leftmost point just before reaching it,
  and advises a key at the rightmost point anyway for a full rotation. Read as a
  statement about the value domain (the maximum is the minimum), not about
  rerouting keyform interpolation; that is an interpretation, unverified against
  the Core. Wrapping the value alone leaves a drawable whose keyforms differ
  between the two ends moving by that difference when a curve crosses the join.
- Crossfading between two motions takes the shortest way round a repeating
  parameter (then wraps). The Framework blends fades linearly in value space for
  repeating and non-repeating parameters alike, with no shortest arc. This is a
  deliberate divergence; with the flag read it now also applies to the six
  parameters the inference missed. See the motions entry in section 8.

## 7. How correctness is judged without a reference runtime

No reference *runtime* has been compared against. What the chain is judged by
now is **drawing the model with its own texture and looking at it**, plus a
handful of properties that must hold for any correct evaluation.

**Draw it and look.** This is the strongest criterion available and the only
one that has ever caught a whole-model rotation. Everything below is an
aggregate, and every aggregate here has at some point passed a pose that was
visibly wrong. Rendering three characters from one source shows each figure
whole, correctly textured, and placed against its own scenery inside its own
canvas.

**It lands in the right frame.** Across six models every drawable poses inside
its own canvas, and the extents sit comfortably within it rather than merely
inside a tolerance. Treat this as a smoke test, *not* as evidence: the base
angle of §4 was missing while this criterion read 1598 of 1805 — and worse, a
candidate rule tried during that investigation brought every model inside its
canvas while visibly wrecking a pose that had been correct. A rule can improve
canvas fit and still be wrong.

**Masks overlap what they clip.** A drawable is clipped to its masks, and
Cubism does not ship masks that clip nothing, so a mask whose posed bounds miss
the drawable's is evidence the two landed wrongly *relative to each other* —
which no per-drawable measure can see. This is the aggregate that came closest
to finding the base angle: with it missing, one model had 31 of 46 mask pairs
disjoint against 1 of 10 and 4 of 35 elsewhere. All six now sit at zero except
for twenty pairs on one model, all of them four hand-detail meshes masked to
parts the hand only reaches part-way through an animation. The rest pose is not
where every mask has to bite, so a non-overlapping pair is a lead rather than a
verdict.

**Two independently authored rigs agree.** A model and its own low-detail
variant — 601 drawables against 101, 849 parameters against 151 — render as the
same scene. A wrong chain would have to distort both identically.

**A warp's child space really is normalised.** Drawables under a warp carry
coordinates near one whether their parent's grid is in `[0, 1]` or in units
running to 64.

**No drawable is mirrored against the texture it samples.** This is the only
criterion that compares the geometry against something *outside* the deformer
chain. A mesh is the same shape in uv space as in canvas space, so the map
between them is a similarity, and only a mirror flips the sign of its
determinant. On the three models measured, 99%, 96% and 97% fit positive.

Positive, not negative: `a2d-unity` flips Unity's bottom-up rows when it decodes
a `Texture2D`, which cancels the usual v-runs-down convention. Getting that
backwards is what produced the reverted change in §3.

Note what this does **not** show. A determinant is invariant under rotation, so
this can never say a model is the right way up — only that it is not mirrored.
Treating it as an orientation check is precisely the mistake that was made.

The *angle* of that same similarity is worth nothing, and was tried. Across all
six models the per-drawable uv-to-position angles have a mean resultant length
of only 0.08 to 0.46, so they are barely concentrated at all: Cubism packs art
meshes into the atlas at whatever rotation fits, and there is no shared
orientation to measure against. A linear median over those angles reads near
zero and means nothing, because the values wrap — which is how the missing base
angle of §4 survived a check that appeared to confirm every subtree was
unrotated.

An earlier version of this document listed a fifth criterion — that sweeping
`ParamEyeBallX` and `ParamEyeBallY` gives two perpendicular, right-handed axes —
and it was wrong twice over. Those parameters are carried through the very chain
under test, so they cannot witness against it; and the wiring is not uniform
across rigs anyway. One model moves its pupils vertically for `ParamEyeBallX`
with its face plainly upright, and the handedness of the pair depends on which
way a rig takes positive to mean. The assertion has been removed rather than
weakened: one that fails on correct data is worse than none.

These are `crates/a2d-cli/tests/cubism_orientation.rs`, gated on
`A2D_FIXTURE_CUBISM` because extracted assets are never committed (§11).

### Drawing one, from the CLI

```bash
animated2d preview <bundle-or-package> -o frames/   # writes frames/pose.png
animated2d import  <bundle> -o character.a2dpack    # bundle -> package
```

`preview` takes a bundle directly *and* a package; both routes render the same
pose through the same code, which is what says a package is a faithful record
of the model rather than a re-interpretation of it. A real-asset test asserts
exactly that: the same bundle, posed from the MOC3 and posed from the package
written to disk, matches vertex for vertex.

`preview` without `-o` opens either in the desktop viewer, and plays the
motions the package carries (a bundle goes through the importer first, so the
motions recovered from its fade motion data come along). With `-o` it writes a
frame per regression timestamp of the default motion; a model with no motions
keeps a single rest-pose frame. Only texture page zero is sampled.

## 8. What is still unverified

- **No reference runtime.** The models now render as coherent scenes, which is
  much stronger than landing in the right frame, but a picture that looks right
  cannot show that a particular warp bends the way Live2D bends it, or that
  blending weights are right *between* keys rather than only on them.
- **Rotation under a warp.** The rule in section 4 is checked against texture
  proportions and the editor manual, not a reference runtime. Three points
  within it are open: the probe length (`0.1` of the unit square, from memory
  of the Cubism 2 SDK, not confirmed for later versions; `0.01` moves a vertex
  by at most `1.1e-3` canvas units on the worst model measured); warp inside
  warp, where the child's grid is carried through the parent rather than each
  point being walked through both (up to `0.02` canvas units apart at rest,
  `0.06` in motion); and glue where a gap exists at rest (the widest is `0.3`
  canvas units on one model), which pulls a mesh towards its partner and can
  still stretch it. Whether Cubism shows the same pull needs a reference. A
  glue with a gap at rest is mostly between alternate pieces that deformer
  opacity hides in that pose (section 6), so the pull is not on screen.
- **Deformer opacity.** Applied as a product down the deformer chain (section
  6). Unverified: multiplication against a minimum or the direct parent only;
  how a partial opacity should look; the visibility and enabled flags
  (sections 13, 14, 37, 38); and part opacity and pose, which are not applied (see "Parts" in section 6).
- **Motions.** The bundles' animations are Unity `AnimationClip`s in the
  compressed muscle-clip form, not `motion3.json`. Their parameter curves are
  recovered and carried in the package as keyed Hermite segments, so a model
  plays its own animations. Authored fades are not applied: the stored fade
  fields are kept, but playback does not ramp a curve, and the bundles examined
  carried no fade data to read. Switching motions crossfades over the host's
  `mix_duration` instead: a sine weight `0.5 - 0.5 cos(pi t)`, each side
  evaluated alone from the defaults and blended per parameter before physics
  (a parameter one side leaves alone fades to or from its default; a repeating
  one takes the shortest arc, where the Framework blends linearly). The outgoing motion keeps playing, a loop wrap
  does not fade, and a second switch mid-fade fades from the pose on show. The
  shape and the 150/300 ms lengths are the Cubism-family convention and the
  host's choice, not read from the assets (their Unity controllers specify 0.25 s
  linear transitions back to a default state); no reference runtime was compared.
- **Physics, pose files, expressions, hit areas.** All live outside the MOC3 and
  are not read.
- **Draw order: what is unverified.** Painting follows the rule in §6 and
  matches the official runtime's recorded order at rest on four models, but not
  yet anywhere a draw order is fractional or animated. Unchecked: whether Core
  rounds, truncates or floors a fractional blend (rounding is used; every value
  at rest is an exact integer); whether draw order blends linearly with the
  geometry weights (assumed, as the manual says draw-order keyforms behave like
  shape keyforms); and nested groups and part objects (type 1, a self index,
  section 58, section 83), which no model seen uses and which are reported
  rather than ordered. Two of the six models could not be checked at rest.
- **Multiple texture pages.** Section 41 gives each drawable a texture index and
  is read, but the emitter still puts every mesh on one page, because nothing
  loads a second one yet. It is all zero on every model seen.
- **Glue.** Decoded and applied (§2). Weights may sum below 1 (partial release,
  seen on two of three models). Unverified: intensities other than 1, which
  weight belongs to which mesh, the order of application when one vertex is in
  several glues, and whether a glue toward a partly faded mesh is weakened (a
  fully faded one is hidden by deformer opacity, see section 6).
