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

A wrong index here fails loudly rather than returning a plausible model: each
section is required to hold exactly the declared number of well-formed entries.

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

## 6. How correctness is judged without a reference runtime

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

## 7. What is still unverified

- **No reference render.** Landing in the right frame shows the chain composes;
  it cannot show that a particular warp bends the way Live2D bends it, or that
  blending weights are right *between* keys rather than only on them.
- **Motions.** The bundles' animations are Unity `AnimationClip`s in the
  compressed muscle-clip form, not `motion3.json`. They are not decoded, so a
  model poses and draws but does not play its own animation.
- **Physics, pose files, expressions, hit areas.** All live outside the MOC3 and
  are not read.
- **Multiple texture pages.** Every drawable is assumed to sample page zero; the
  per-drawable texture index is not decoded.
- **Glue.** Identifiers are read; the constraint is not applied.
