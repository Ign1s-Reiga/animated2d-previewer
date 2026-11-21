# Tests

## Asset policy

**Extracted game assets and proprietary SDK binaries are never committed.**

Everything the committed test suite runs against is synthetic: either
hand-authored text (Spine JSON skeletons, atlas files) or generated at test time
(the Spine binary writer in `crates/a2d-spine/src/binary/mod.rs`, the PNG builder
in `crates/a2d-cli/tests/support/mod.rs`). A synthetic fixture exercises the same
code paths as a real one and can be read and reviewed in a diff.

Real assets belong in `tests/fixtures/local/`, which is gitignored.

## Running the suite

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

## Kinds of test

| Kind | Where | What it covers |
| --- | --- | --- |
| Unit | `#[cfg(test)]` in each module | binary parsers, version detection, bone transforms, weighted skinning, interpolation, Bezier evaluation, draw order, clipping, atlas parsing, name normalization |
| Round-trip | `crates/a2d-pack/src/package.rs` | a skeleton using every timeline and attachment type survives `IR → model.bin → IR` unchanged, and re-encodes to identical bytes |
| Golden | `crates/a2d-cli/tests/pipeline.rs` | `source asset → importer → IR → deterministic serialize` compared against a committed expected manifest |
| Robustness | `crates/a2d-spine/src/binary/{mod,v4}.rs`, `crates/a2d-pack/src/package.rs` | every truncation and single-byte corruption of a fixture must return an error, never panic |
| Architecture | `crates/a2d-cli/tests/architecture.rs` | the layering rules in `CLAUDE.md` §3, enforced rather than reviewed |
| GPU | `crates/a2d-render/tests/render.rs` | clear, tint, blend modes, draw order, stencil clipping, read-back, buffer growth — all against real pixels |
| Visual regression | `crates/a2d-cli/tests/visual.rs` | fixed timestamps rendered through the whole stack; determinism and movement |
| Visual regression (Cubism) | `crates/a2d-cli/tests/visual_cubism.rs` | the same timestamps for the Cubism family, plus hand-computed geometry: keyform blending, a Hermite segment and a stepped segment, asserted on emitted vertices and on rendered pixel centroids |
| Real-asset | `crates/a2d-unity/tests/real_bundle.rs`, `crates/a2d-cli/tests/{unity_inspect,real_moc3}.rs` | a real Unity bundle parses, every object is addressable, the inventory holds what §12 asks for, and the MOC3 inside it reads with sane identifiers and parameter ranges — `#[ignore]`, gated on `A2D_FIXTURE_CUBISM` |
| Cubism clip motions | `crates/a2d-cli/tests/real_clip_motions.rs` | every `AnimationClip` in a real bundle parses to the exact byte; a bundle with no fade data yields one motion per clip; every curve targets a parameter the MOC3 has; keys are finite and ordered; the package round-trips and validates — `#[ignore]`, gated on `A2D_FIXTURE_CUBISM` (one bundle file, not a directory) |
| Unity Spine | `crates/a2d-cli/tests/unity_spine.rs` | a real Unity bundle yields a skeleton and atlas that the existing decoder reads with no Unity-specific handling — `#[ignore]`, gated on `A2D_FIXTURE_SPINE_BUNDLE` |
| Cubism assembly | `crates/a2d-cli/tests/cubism_orientation.rs` | a posed model is coherent — its pupil axes stay square and right-handed — and fits its canvas once un-zoomed; asserted from Cubism's conventional parameter meanings rather than from a reference image — `#[ignore]`, gated on `A2D_FIXTURE_CUBISM` |
| Constraint geometry | `crates/a2d-runtime/src/spine/pose.rs` | IK, the four transform-constraint modes, and path constraints against hand-computed geometry — a straight path and a square one have exactly known arc lengths |
| Viewer behaviour | `crates/a2d-desktop/src/{config,state,tray}.rs` | config persistence and clamping, drag/scale/selection, tray id mapping — all without opening a window |
| Subprocess | `crates/a2d-cli/tests/viewer_process.rs` | the real binary runs a hidden window, renders and presents frames, and writes settings on the way out |
| Cubism preview | `crates/a2d-cli/tests/cubism_preview.rs`, `viewer_process.rs` | a synthetic Cubism package opens in a hidden window and presents frames; `preview -o` writes a frame per regression timestamp of its motion, or one rest-pose frame without motions |
| Visible window | `crates/a2d-cli/tests/viewer_process.rs` | the visible draw trigger (`request_redraw` / `RedrawRequested`) that hidden runs bypass — `#[ignore]`, gated on `A2D_VISIBLE_VIEWER_TESTS` |

## Tests that need a real asset

Gate them behind `#[ignore]` **and** an environment variable naming the asset, so
the suite still passes for someone who does not have it:

```rust
#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_SPINE to a directory"]
fn a_real_character_imports_and_poses() {
    let Ok(dir) = std::env::var("A2D_FIXTURE_SPINE") else { return };
    // ...
}
```

Run them with:

```bash
A2D_FIXTURE_SPINE=tests/fixtures/local/spine cargo test --workspace -- --ignored
```

Document each new variable in the table below.

| Variable | Points at | Used by |
| --- | --- | --- |
| `A2D_REQUIRE_GPU` | any value; turns a missing-GPU skip into a failure | every GPU test |
| `A2D_BASELINE_DIR` | a directory of baseline frames | `a2d-cli --test {visual,visual_cubism}` |
| `A2D_CONFIG_DIR` | a directory to keep `config.json` in, instead of the per-user one | the viewer; `a2d-cli --test viewer_process` |
| `A2D_FIXTURE_SPINE_BUNDLE` | a Unity AssetBundle holding a Spine rig | `a2d-cli --test unity_spine` |
| `A2D_FIXTURE_CUBISM` | a Unity AssetBundle holding a Cubism model | `a2d-unity --test real_bundle`, `a2d-cli --test {unity_inspect,real_moc3,real_clip_motions,cubism_orientation}` |

## Visual regression

Spec §17.3 asks for renders at fixed timestamps (`0.0s / 0.25s / 0.5s / 1.0s`)
compared by image or framebuffer hash. `crates/a2d-cli/tests/visual.rs` does
this for the Generic Spine family, driving the whole stack: source assets →
importer → IR → runtime → renderer → pixels.

### Why no pixel baseline is committed

Rasterisation differs by a least significant bit between GPUs and driver
versions, so a baseline recorded on one machine fails on every other one. A test
that fails for reasons unrelated to the change is worse than no test. The
always-on assertions are therefore the properties that hold on *any* correct
renderer:

- rendering is deterministic — the same pose twice is byte-identical;
- the animation moves — distinct timestamps produce distinct frames;
- the character is drawn — pixels are covered, and not the whole frame.

### Pinning exact pixels locally

Point `A2D_BASELINE_DIR` at a directory. The first run records the baselines;
later runs compare against them with a 2/255 per-channel tolerance.

```bash
A2D_BASELINE_DIR=tests/fixtures/local/baseline cargo test -p a2d-cli --test visual
```

Delete a baseline file to re-record it.

### Cubism

`crates/a2d-cli/tests/visual_cubism.rs` is the Cubism counterpart. Its model is
`support::cubism_deformation_package`: two triangles, each driven by a two-key
keyform blend, so a wrong blend, a Hermite segment sampled linearly, a stepped segment
treated as linear, or an un-flipped y moves pixels by a computable amount.

- Triangle A (lower half) slides by `(p + 30) / 60` as `ParamAngleX` eases from
  -30 to 30 over one second with zero slopes. At 0.25 s the parameter is
  -20.625 (blend weight 0.15625); linear interpolation would give 0.25.
- Triangle B (upper half) rises by `0.3 * (p + 30) / 60` and is driven by a
  stepped curve, so it stays put at 0.0, 0.25 and 0.5 s and has moved by 1.0 s.

Beyond the properties `visual.rs` checks (drawn, deterministic, moves, fresh
model and backward scrub reproduce frames), two tests assert the hand-computed
numbers: `posed_geometry_matches_the_hand_computed_blend` on the emitted
vertices (CPU only, runs without a GPU), and
`rendered_triangles_land_where_the_blend_puts_them` on pixel centroids (1.5 px
slack). `A2D_BASELINE_DIR` works the same way; files are named `cubism_idle_*`.

## Whether a Cubism model is assembled coherently

A model can parse cleanly, pose with no unstable drawable, and still come out
mirrored, sheared, or several times too large. Nothing structural notices, and
there is no reference image to compare against.

Cubism's parameter names are conventional, which is the way in: `ParamEyeBallX`
slides the pupils left and right, `ParamEyeBallY` up and down. The tempting
assertion — that the first must therefore move them along the screen's x axis —
is wrong, because it only holds for a character drawn upright. Measured across
three real models the pupil axes came out at 0, -60 and -95 degrees, every one
of them with total agreement between drawables. That is artwork, not error: a
reclining character is drawn reclining.

What holds regardless of the pose is that the two axes stay **perpendicular and
right-handed**. A chain that transposed a warp grid, mirrored a deformer or
sheared a rotation breaks that; a tilted head does not.

Size is checked separately, and needed its own discovery. A model's stored
parameter defaults are not necessarily its display values: one of the three
ships a zoom parameter defaulted to 8 of 10, which scales the whole model by
about five and makes an ordinary canvas-wide backdrop measure four and a half
canvases across. Winding the parameters that drive the root deformer back to
their minimum gives the widest view, and there every model sits inside its own
canvas.

```bash
A2D_FIXTURE_CUBISM=tests/fixtures/local/bundle cargo test -p a2d-cli --test cubism_orientation -- --ignored
```

## The window

`a2d-desktop` splits deliberately: `config`, `state` and `tray` hold the
behaviour a user notices and are unit-tested with no window involved, while
`app` is a thin `winit` layer that only translates events into calls on them.

Two things are out of reach even so. An event loop cannot be created off the
main thread, so a window cannot be opened from inside the test harness; and the
save-on-quit path only runs when the viewer actually shuts down.
`crates/a2d-cli/tests/viewer_process.rs` covers both by launching the built
binary as a subprocess:

```bash
cargo test -p a2d-cli --test viewer_process
```

Two flags make that possible:

- `--exit-after <seconds>` quits the viewer through exactly the same path as
  Esc, the tray and the close button, so the run exercises the real shutdown
  rather than a test-only one. The clock starts when the window is up, not at
  process start — a cold device can take seconds to create.
- `A2D_CONFIG_DIR` redirects settings into a scratch directory, so a test never
  touches the real per-user `config.json`.

`preview` also prints how many frames it presented. That count is the assertion
the window gap needed: zero frames means the window never drew, which no
in-process test could have noticed.

These skip, like the other GPU tests, on a machine with no adapter or no desktop
session.

## Capturing the window to look at it

`PrintWindow` is not reliable against this window: it reads the redirection
surface, and a wgpu surface presented through the flip model often leaves that
blank, so a correct frame captures as a blank one. Three separate renders were
misread as failures that way.

Capture the window's screen rectangle instead — the window is always-on-top, so
nothing overlaps it:

```powershell
$r = ...            # GetWindowRect on the process's MainWindowHandle
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
```

Better still, prefer `preview -o <dir>`, which renders through the same viewer
and writes deterministic PNGs with no window involved.

## GPU tests

`crates/a2d-render/tests/render.rs` and the visual regression tests need a
graphics adapter. Where none is available they skip with a note, so the suite
still passes on a headless box.

Set `A2D_REQUIRE_GPU=1` to turn that skip into a failure — which is what CI on a
machine that *should* have a GPU wants:

```bash
A2D_REQUIRE_GPU=1 cargo test --workspace
```

## Tests that open a visible window

The subprocess viewer tests run with `--hidden`, which draws from `about_to_wait`
rather than through `RedrawRequested`. One opt-in test launches a visible window
to cover the real trigger. It opens a visible window; run before touching the
redraw path:

```bash
A2D_VISIBLE_VIEWER_TESTS=1 cargo test -p a2d-cli --test viewer_process -- --ignored
```

### Windows transparency

On Windows the viewer is transparent only when it presents through a
DirectComposition visual on DX12 (a swapchain bound to a window handle is opaque
there; see the `a2d-desktop` crate doc). The default, hidden test
`the_windows_viewer_presents_through_directcomposition_with_premultiplied_alpha`
asserts the summary line reports `on Dx12 with PreMultiplied alpha` and that the
"no transparent surface mode" note is absent. Like the other viewer tests it
only skips when the viewer *exits non-zero* with an environmental message (no
adapter, no window). A viewer that fails to start currently exits 0 with
`Presented 0 frames`, so on a machine with no GPU or desktop this test fails on
its assertions rather than skipping; `A2D_REQUIRE_GPU` only makes the non-zero
exit case a failure too.

If the DirectComposition swapchain cannot be created or configured (the
configure is checked with wgpu error scopes), the viewer falls back to the
plain window surface and this test fails, which is the intent.

That a hidden window reports `PreMultiplied` does not prove the pixels are
see-through. There is no automated pixel test: it needs a visible window over a
known backdrop. The manual check is to fill a borderless window with magenta,
open the viewer over it (position it with `config.json`, using `A2D_CONFIG_DIR`),
and read a pixel of the viewer window away from the character with `GetPixel`:
magenta (255, 0, 255) means transparent, black means the opaque fallback.
