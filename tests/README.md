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
| Robustness | `crates/a2d-spine/src/binary/mod.rs`, `crates/a2d-pack/src/package.rs` | every truncation and single-byte corruption of a fixture must return an error, never panic |
| Architecture | `crates/a2d-cli/tests/architecture.rs` | the layering rules in `CLAUDE.md` §3, enforced rather than reviewed |
| GPU | `crates/a2d-render/tests/render.rs` | clear, tint, blend modes, draw order, stencil clipping, read-back, buffer growth — all against real pixels |
| Visual regression | `crates/a2d-cli/tests/visual.rs` | fixed timestamps rendered through the whole stack; determinism and movement |
| Constraint geometry | `crates/a2d-runtime/src/spine/pose.rs` | IK, the four transform-constraint modes, and path constraints against hand-computed geometry — a straight path and a square one have exactly known arc lengths |
| Viewer behaviour | `crates/a2d-desktop/src/{config,state,tray}.rs` | config persistence and clamping, drag/scale/selection, tray id mapping — all without opening a window |
| Subprocess | `crates/a2d-cli/tests/viewer_process.rs` | the real binary opens a window, presents frames, and writes settings on the way out |

## Tests that need a real asset

Gate them behind `#[ignore]` **and** an environment variable naming the asset, so
the suite still passes for someone who does not have it:

```rust
#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_AEONS to a directory"]
fn a_real_character_imports_and_poses() {
    let Ok(dir) = std::env::var("A2D_FIXTURE_AEONS") else { return };
    // ...
}
```

Run them with:

```bash
A2D_FIXTURE_AEONS=tests/fixtures/local/aeons cargo test --workspace -- --ignored
```

Document each new variable in the table below.

| Variable | Points at | Used by |
| --- | --- | --- |
| `A2D_REQUIRE_GPU` | any value; turns a missing-GPU skip into a failure | every GPU test |
| `A2D_BASELINE_DIR` | a directory of baseline frames | `a2d-cli --test visual` |
| `A2D_CONFIG_DIR` | a directory to keep `config.json` in, instead of the per-user one | the viewer; `a2d-cli --test viewer_process` |

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

## GPU tests

`crates/a2d-render/tests/render.rs` and the visual regression tests need a
graphics adapter. Where none is available they skip with a note, so the suite
still passes on a headless box.

Set `A2D_REQUIRE_GPU=1` to turn that skip into a failure — which is what CI on a
machine that *should* have a GPU wants:

```bash
A2D_REQUIRE_GPU=1 cargo test --workspace
```
