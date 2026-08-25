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
| 33 | drawable ids | | 75 | parameter key begin |
| 34 | drawable keyform binding | | 76 | parameter key count |
| 35 | drawable keyform begin | | 77 | parameter keys |
| 36 | drawable keyform count | | 78 | vertex UVs |
| 40 | drawable parent deformer | | 79 | vertex indices |
| 43 | drawable vertex counts | | 87 | drawable draw order |
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

**Keyform ordering is last-axis-fastest.** With axes `[A, B]`, keyform index is
`i_A * len(B) + i_B`. Confirmed on a two-axis model whose two orderings select
keyforms differing by a factor of four in scale: the last-axis-fastest reading
is the one that renders correctly.

### The deformer chain

Deformers form a forest; a drawable names one parent deformer and inherits the
chain above it. There are two kinds.

A **warp deformer** is a grid of control points. Its children live in the grid's
unit square, and posing a point is a bilinear lookup. Outside the square the
edge cells are *extended* rather than clamped, so geometry that overhangs a
deformer keeps its shape instead of collapsing onto the border.

A **rotation deformer** is a rigid frame: origin, angle in degrees, uniform
scale, opacity. Posing a point is `origin + R(angle) · (point · scale)`.

Constraints are applied in the order the chain is walked, from the drawable
outward to the root.

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
artist's per-drawable value, and it is **not** the same thing as the resolved
back-to-front sequence in section 87: the two disagree on 567 of 568 drawables
in one real model. Turning draw orders into a render order needs the part tree,
so section 87 is what the emitter sorts by, and section 69 is decoded and
carried but not yet used.

That distinction has a visible consequence. On one model the eyes do not
appear, and the reason is not that the meshes are absent: all 28 are present,
correctly placed, at full opacity, and named `eye_r01`, `eyelash_down_r03`,
`eyelids_r02` and so on. Chasing it down produced three findings, only one of
which is fixed.

**Fixed: the eyes were unclipped.** `eye_r01` and `eye_r02` -- an iris and a
highlight -- each declare one mask, and it is `eye_r03`, their own eye white.
Masks are now read and emitted, and the irises clip correctly.

**Not fixed: the face skin draws in front of them.** `face_02` is a solid mesh
whose atlas region is 68% opaque, and section 87 puts it at 534 against eye
meshes at 519 to 526. Suppressing that one mesh reveals the eyes immediately.

Section 87 is the only permutation of the drawables in the whole file, so there
is no other candidate for a render order, and it is not internally coherent: the
right eye's lashes sit at 509 to 513, *behind* that eye at 519 to 521, while the
left eye's sit at 527 to 533, in front of its own. So the resolved order must be
computed rather than read. In Cubism that computation involves the part tree,
but the part sections carry no ordering: sorting part-major by part index does
better on hand-written ordering rules than section 87 does (6 of 8 against 5 of
8) and is still wrong, and no per-part section holds an order. **This is
unsolved.**

**Not fixed, and probably the deeper problem: the face is laid out on a rotated
axis.** Taking the posed centres of the facial meshes, the brow-to-eye-to-nose
-to-mouth axis runs along decreasing `x` while the right-to-left axis runs along
`y` -- the face's own vertical is the screen's horizontal. But sweeping
`ParamEyeBallX` moves the pupils along `x`, which in that layout is the face's
*vertical*. The geometry and the parameter wiring disagree by a quarter turn,
and they cannot both be right. Whichever is wrong, a defect that rotates the
head would also explain why nothing in the face lands where a render order
expects it.

That last point is why no render-order rule should be guessed at until it is
settled: fitting an ordering to geometry that is itself suspect would bake the
error in.

## 7. How correctness is judged without a reference runtime

No reference render has been compared against, so the chain is judged by
properties that must hold for any correct evaluation.

**It lands in the right frame.** Across six models, 2130 of 2131 drawables pose
inside their own canvas, and the exception is the zoom case of §5. The chain is
deep enough that a wrong composition does not land anywhere plausible — an
earlier version was out by four orders of magnitude.

**Two independently authored rigs agree.** A model and its own low-detail
variant — 601 drawables against 101, 849 parameters against 151 — render as the
same scene. A wrong chain would have to distort both identically.

**A warp's child space really is normalised.** Drawables under a warp carry
coordinates near one whether their parent's grid is in `[0, 1]` or in units
running to 64.

**The pupil axes stay square and right-handed.** Sweeping `ParamEyeBallX` and
`ParamEyeBallY` and measuring which way each moves the pupils gives two axes
that are perpendicular and right-handed in every model, with the responding
drawables in total agreement. A chain that transposed a grid, mirrored a warp or
sheared a rotation could not produce that.

Their *absolute* angle varies between models — 0, -60 and -95 degrees — and that
is head tilt in the artwork, not error: a reclining character is drawn
reclining. This is why the assertion is on the pair rather than on either axis
alone, and it is a correction of an earlier conclusion that read the varying
angle as a quarter-turn bug. The measurement also has to average *unit* vectors
rather than displacements, or a single mis-scaled drawable speaks for the whole
model.

These are `crates/a2d-cli/tests/cubism_orientation.rs`, gated on
`A2D_FIXTURE_CUBISM` because extracted assets are never committed (§11).

## 8. What is still unverified

- **No reference render.** Landing in the right frame shows the chain composes;
  it cannot show that a particular warp bends the way Live2D bends it, or that
  blending weights are right *between* keys rather than only on them.
- **Motions.** The bundles' animations are Unity `AnimationClip`s in the
  compressed muscle-clip form, not `motion3.json`. They are not decoded, so a
  model poses and draws but does not play its own animation.
- **Physics, pose files, expressions, hit areas.** All live outside the MOC3 and
  are not read.
- **Render order.** Section 87 places the face skin in front of the eyes and
  orders the two eyes' lashes inconsistently with each other. No section holds a
  part order, and part-major sorting does not fix it either. See §6.
- **Head orientation.** The facial meshes are laid out on an axis a quarter turn
  from the one the eye parameters move along. See §6.
- **Multiple texture pages.** Section 41 gives each drawable a texture index and
  is read, but the emitter still puts every mesh on one page, because nothing
  loads a second one yet. It is all zero on every model seen.
- **Glue.** Identifiers are read; the constraint is not applied.
