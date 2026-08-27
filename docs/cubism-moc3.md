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
| 16 | deformer parent | | 56 | parameter binding begin |
| 17 | deformer type (0 warp, 1 rotation) | | 57 | parameter binding count |
| 18 | deformer index within its type | | 60 | warp keyform position offsets |
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
| 43 | drawable vertex counts | | 87 | the paint sequence |
| 44 | drawable vertex offsets (floats) | | 90 | glue ids |
| 45 | drawable index offsets | | | |
| 46 | drawable index counts | | | |
| 68 | drawable keyform opacity | | 69 | drawable keyform draw order |
| 39 | drawable part index | | 41 | drawable texture index |
| 42 | drawable flags (**bytes**) | | 47 | drawable mask begin |
| 48 | drawable mask count | | 80 | drawable mask list |

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
Notably **none of them orders the parts**, which matters for §6.

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

Constraints are applied in the order the chain is walked, from the drawable
outward to the root.

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

### The scale rule

**Every space in a model is canvas pixels, with one exception: a warp
deformer's children live in its unit square.** A rotation deformer sitting under
a warp is therefore the one place a unit conversion is needed.

The exchange rate is the warp's own grid extent — the grid spans the parent's
space while the children span zero to one — and it **accumulates**: a warp
nested in another warp measures its grid in that warp's units, not in pixels, so
the rates multiply down the chain. Missing the accumulation makes shallow chains
look almost right and deep ones wrong by a factor of a thousand.

Rates are taken from the *posed* grids rather than the setup ones, so a warp
that stretches carries its children with it.

The root rotation is the pixels-to-units bridge: its scale is the reciprocal of
the canvas's pixels-per-unit. On one model, scale `0.00026192` against a canvas
of 3792 px/unit — reciprocal 3818 — exactly as the transform predicts.

How the rule was found is worth recording, because it was not found by
statistics. Dumping one small model's deformer tree in full and reading it gave
three consecutive lines: a warp with a grid spanning `-627..686` pixels, a
rotation under it sitting at `(0.501, 0.500)` — dead centre of a unit square —
and *that* rotation's own child back at `(305.6, -48.5)`, pixels again.
`0.501 + 305.6/1312` lands at `0.734`, inside the square. Aggregate
correlation-hunting produced three separately confounded results before that;
one readable tree settled it.

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

**Draw order** (section 69) runs 499 to 1000 about a resting 510. This is the
artist's per-drawable value, and it is **not** the resolved back-to-front
sequence — that is section 87, and turning draw orders into one would need the
part tree. Section 58 holds a per-part draw order on the same 0-to-1000 scale,
but it is 500 for all but three parts on the model measured, so it separates
nothing. Section 69 is therefore decoded and carried but not yet used.

### Section 87 is a sequence, not a key

Slot `k` names the drawable painted `k`-th. It is *not* a per-drawable value
saying where each one sits — that reading is the inverse permutation.

The inverse is dangerously plausible. It is still a permutation, it still paints
every drawable exactly once, and the result still looks broadly like the
character. What it does is scatter the paint order locally: on one model it put
the face's skin (`face_02`) over the eyes, which is why that model rendered with
a blank face, and it ordered one eye's lashes behind its own eye while the other
eye's sat in front — an asymmetry that no coherent order would produce, and the
clue that the reading was wrong rather than the data.

The fixture pins the direction with a *rotation* rather than a reversal,
because a reversal is its own inverse and could not tell the two apart.

A model whose table repeats an entry rather than permuting cleanly — one of the
six does — has the unnamed drawables appended in model order, so nothing goes
unpainted.

### Why the eyes were invisible

Two problems, both now fixed:

1. the irises were unclipped, drawing their full quads rather than being cut to
   their own eye whites (sections 47, 48 and 80);
2. section 87 was read as a key rather than a sequence, painting the face skin
   over the eyes.

The second was the one that mattered — the eyes were there the whole time,
behind the face's own skin.

A third suspect was investigated at length and was not a fault: the model looks
turned because **the character is drawn reclining**, head to the right. That was
twice diagnosed as a decoding error, and the second diagnosis was acted on
before being reverted. §3 records the argument and why it was wrong. Nothing in
the format says which way up a character stands; only someone who knows the
character can say.

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

A Cubism package has no motions to play, so `preview` without `-o` refuses and
says why rather than opening an empty viewer.

## 8. What is still unverified

- **No reference runtime.** The models now render as coherent scenes, which is
  much stronger than landing in the right frame, but a picture that looks right
  cannot show that a particular warp bends the way Live2D bends it, or that
  blending weights are right *between* keys rather than only on them.
- **Motions.** A package carries none, so a Cubism model poses and draws but
  does not move. See below.
- **The unit conversion between a warp and a rotation under it.** It takes the
  warp's posed bounding box per axis, which is exact for an axis-aligned
  lattice and approximate for a turned one. Reading the setup grid instead, or
  forcing the rate isotropic, changes no model measurably, so nothing available
  here distinguishes them.
- **Motions.** The bundles' animations are Unity `AnimationClip`s in the
  compressed muscle-clip form, not `motion3.json`. They are not decoded, so a
  model poses and draws but does not play its own animation.
- **Physics, pose files, expressions, hit areas.** All live outside the MOC3 and
  are not read.
- **Animated draw order.** Section 69 is decoded but unused: turning the
  artist's per-drawable values into a render order needs the part tree, and the
  per-part orders in section 58 are equal on every model measured. The setup
  sequence in section 87 is used instead, so a model that reorders during an
  animation will not.
- **Multiple texture pages.** Section 41 gives each drawable a texture index and
  is read, but the emitter still puts every mesh on one page, because nothing
  loads a second one yet. It is all zero on every model seen.
- **Glue.** Identifiers are read; the constraint is not applied.
