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
| _(none yet)_ | | |

## Visual regression

Spec §17.3 asks for renders at fixed timestamps (`0.0s / 0.25s / 0.5s / 1.0s`)
compared by image or framebuffer hash. The runtime half is in place:
`GenericSpineModel::pose_at` scrubs to an exact time without looping or firing
events, and `animated2d preview` prints the mesh count and bounds at those four
timestamps. The image comparison itself needs the GPU renderer, which is not
built yet — see the roadmap in `README.md`.
