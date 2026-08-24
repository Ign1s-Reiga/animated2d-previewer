# CLAUDE.md

Guidance for Claude Code working in this repository.
The authoritative product spec is `docs/requirements.md`. This file is the *operational* contract.

---

## 1. What this project is

A reusable desktop 2D character viewer/runtime that plays animated character assets extracted
from multiple games **without** depending on each game's original runtime.

Source ecosystems: **Live2D Cubism** and **Spine** (multiple historical versions), including
Unity-packaged variants. Target games for importers: 放置少女 (Depose Girls), AEONS ECHO, NIKKE.

Primary objective: **correct character playback for desktop viewing.** Nothing else.

**Current phase:** Phase 1 complete for the Spine path; Phase 2 feature-complete but its
goal is unverified without real assets; Phase 6 implemented.

Implemented: `a2d-core`, `a2d-spine` (atlas, detection, JSON 2.x/3.x/4.x, binary 3.x),
`a2d-runtime` (transforms, timelines, skinning, deform, IK, transform constraints in all four
modes, path constraints, mixing,
idle), `a2d-pack`, `a2d-import` (generic + aeons_echo), `a2d-render` (wgpu, batching, blend
modes, stencil clipping, offscreen render + read-back), `a2d-desktop` (transparent frameless
window, drag, scale, click-through, always-on-top, tray, config persistence), `a2d-cli`.

Not implemented: `a2d-unity`, `a2d-cubism`.
Known gaps, all reported rather than silently ignored: Spine 4.x and 2.x binary layouts.

The Spine constraint set is now complete: IK, transform in all four modes, and path.
The transform modes and the path control-point layout are pinned by property tests and by
hand-computed geometry, not by comparison against the official runtime. The §11
cross-implementation check is still outstanding and is what would catch a term in the
wrong place or a misread vertex layout.

`a2d-cubism` is blocked on the Cubism Core decision in §13.1, which is why §12's first
task has not been started.

---

## 2. The one rule that outranks everything

```
Game Asset Bundle / Extracted Files
        ↓  importers/     (asset discovery + reconstruction ONLY)
Source Format Detector
        ↓  formats/       (version-specific decoding → IR)
Spine Decoder | Cubism Decoder
        ↓
Normalized Animated2D Model (IR)
        ↓  runtime/       (deterministic evaluation)
Runtime / Animator
        ↓  renderer/      (source-format-neutral primitives)
Renderer
        ↓  desktop/
Desktop Host
```

**Game-specific knowledge must never leak downstream of `importers/`.**
**Source-version-specific knowledge must never leak downstream of `formats/`.**

If a change requires the renderer to know the string `"nikke"`, or requires the runtime to know
`"spine 3.8.99"`, the design is wrong. Stop and fix the layer above instead.

---

## 3. Stack & workspace layout

Rust, cargo workspace. Rendering via `wgpu`. Desktop shell via `winit` + `tray-icon`.
(Rationale and alternatives: §13 Open Decisions.)

| Crate | Spec module | Responsibility | May depend on |
|---|---|---|---|
| `a2d-core` | `core/` | IR types, math, animation data model, io traits, error types | — |
| `a2d-spine` | `formats/spine/` | version detect, v2/v3/v4 decoders, normalize → Generic Spine IR | core |
| `a2d-cubism` | `formats/cubism/` | moc3/motion3/physics decode, normalize → Generic Cubism model | core |
| `a2d-unity` | (support) | Unity serialized file / AssetBundle object graph reading | core |
| `a2d-import` | `importers/` | generic + depose_girls + aeons_echo + nikke importers | core, unity, spine, cubism |
| `a2d-pack` | (support) | `.a2dpack` read/write, manifest, deterministic serialization | core |
| `a2d-runtime` | `runtime/` | skeleton/param evaluation, timelines, constraints, mixing | core |
| `a2d-render` | `renderer/` | wgpu device, texture cache, batching, clipping/masks | core |
| `a2d-desktop` | `desktop/` | window, transparency, drag, click-through, tray, config | core, runtime, render, pack |
| `a2d-cli` | `tools/` | `animated2d` binary: inspect / import / validate / preview | everything |

**Dependency direction is one-way and enforced by review.**
`a2d-render` must not depend on `a2d-import`, `a2d-spine`, `a2d-cubism`, or `a2d-unity`.
`a2d-runtime` must not depend on `a2d-import` or `a2d-unity`.
If you need something from the wrong direction, move the type into `a2d-core`.

---

## 4. Non-negotiable implementation rules

From the spec (§19), plus repo-specific additions:

1. Do **not** tightly couple game importers to rendering.
2. Do **not** add a new renderer per game. One renderer, zero game branches.
3. Do **not** convert Spine assets to Cubism assets, or vice versa.
4. Normalize source data **before** runtime playback. The runtime consumes IR only.
5. Keep source-version-specific logic inside decoders (`a2d-spine/src/v3/`, etc.).
6. Prefer explicit typed data models over loosely typed maps.
   `HashMap<String, serde_json::Value>` must not appear in any public runtime or renderer API.
7. Add tests with **every** parser/runtime feature, in the same commit.
8. Preserve unknown fields when practical (`raw_extras`), but never let them reach runtime APIs.
9. Fail loudly on ambiguous format/version detection. Never guess between two candidates.
10. Avoid speculative abstraction until **two** concrete implementations require it.
11. Optimize only after correctness and visual parity are established.
12. Never treat source-game behavior as the canonical runtime model.
13. No `unwrap()` / `expect()` / `panic!()` in library crates on data-dependent paths.
    Parsers return `Result`; invariant violations that cannot come from input may `debug_assert!`.
14. Do not commit game assets. See §11.

**Explicit non-goals — do not build these:** Spine Editor or Cubism Editor compatibility,
model editing/authoring, export back to proprietary project formats, battle behavior,
server emulation, 3D rendering, exhaustive source-engine feature parity before basic viewing works.

---

## 5. Core interface

Every model implementation exposes the shared trait (`a2d-core`):

```rust
pub trait AnimatedModel {
    fn update(&mut self, dt: Duration) -> Result<(), RuntimeError>;
    fn emit(&self, out: &mut RenderList);          // instead of render(): no GPU in core
    fn play_animation(&mut self, name: &str, opts: PlayOptions) -> Result<(), RuntimeError>;
    fn stop_animation(&mut self, name: &str);
    fn set_expression(&mut self, name: &str) -> Result<(), RuntimeError>;
    fn animations(&self) -> &[AnimationInfo];
    fn expressions(&self) -> &[ExpressionInfo];
    fn bounds(&self) -> Aabb;
    fn hit_test(&self, x: f32, y: f32) -> Option<HitAreaId>;
    fn dispose(&mut self);
}
```

Concrete impls: `GenericSpineModel`, `GenericCubismModel`.

`load()` from the spec is a constructor (`GenericSpineModel::load(pkg) -> Result<Self>`), not a
trait method — a trait method would force partially-initialized state.

`render()` from the spec is split: models **emit renderer-neutral primitives**, the renderer draws
them. This is what keeps the renderer source-format-neutral.

```rust
pub struct RenderMesh {
    pub vertices: Vec<Vec2>, pub uvs: Vec<Vec2>, pub indices: Vec<u16>,
    pub texture: TextureId, pub color: Rgba, pub dark_color: Option<Rgb>,
    pub blend_mode: BlendMode, pub clipping_mask: Option<MaskId>, pub z_order: u32,
}
```

**Do not force Spine and Cubism into a single low-level deformation model.**
They share `AnimatedModel` and `RenderMesh`. They share nothing below that.

---

## 6. Generic Spine IR

Source-version-independent. The runtime must never learn whether the source was 2.1, 3.8.99 or 4.1.

```
GenericSpineModel
├─ metadata      ├─ bones[]      ├─ slots[]     ├─ skins[]
├─ attachments[] ├─ constraints { ik[], transform[], path[] }
├─ animations[]  ├─ events[]     ├─ draw_order  └─ texture_atlases[]
```

- **Bone**: name, parent, local translation, rotation, scale, shear, transform/inherit mode.
- **Slot**: target bone, attachment ref, color, dark color, blend mode, draw order.
- **Attachments (priority order)**: Region → Mesh → Weighted/skinned mesh → Clipping → BoundingBox.
  Later: Point, Path.
- **Timelines (minimum)**: bone translate/rotate/scale/shear, slot color+alpha, attachment switch,
  draw order, mesh deform, events.
- **Interpolation**: linear, stepped, Bezier.
- **Constraints, in this order**: IK → Transform → Path.

Version decoders own all historical quirks. Each decoder translates *up* to the **latest** IR shape.
Prioritize only versions actually found in target assets: Spine 3.8.x, whatever AEONS ECHO uses,
whatever NIKKE lobby assets use. **Do not implement unused versions speculatively.**

A model must still load when unsupported constraint data exists — report, degrade, never corrupt.

---

## 7. Generic Cubism model

Separate normalized runtime model. **Cubism 3+ first** (放置少女 uses a modern Cubism Unity
integration). Cubism 2 only later, behind its own decoder/runtime adapter.

```
GenericCubismModel
├─ moc data / runtime model  ├─ parameters[]  ├─ parts[]     ├─ drawables[]
├─ textures[]                ├─ motions[]     ├─ expressions[]
├─ physics                   ├─ pose          └─ hit_areas[]
```

Note: Unity-imported Cubism animations often no longer exist as raw `motion3.json`. The importer
must recover parameter curves from Unity `AnimationClip` objects, plus fade motion assets.

---

## 8. Importers

Importers do **asset discovery and reconstruction only**. No rendering logic, no runtime logic.
Output = a generic source-format package, or a normalized `.a2dpack`.

- **depose_girls** — decrypted Unity AssetBundles / serialized assets. Detect Cubism assets under
  paths like `Assets/GirlsGame/Editor/Resources/Live2D/`; extract MOC3 payload from
  `CubismMoc`/TextAsset-like data; extract `Texture2D`; find `AnimationClip`s originating from
  `*.motion3.json`; inspect fade motions; reconstruct model metadata.
- **aeons_echo** — `*.skel.bytes`, `*.atlas.txt`, `*.png`. Pair skeleton/atlas/textures, normalize
  suffixes internally (`.skel.bytes` → `.skel`, `.atlas.txt` → `.atlas`), detect Spine version,
  hand off to the right decoder.
- **nikke** — **lobby / standing character models only.** Explicitly out of scope: shooting poses,
  battle rigs, weapons, aiming, combat VFX, burst scenes.

Normalized Cubism output shape:

```
character/
├─ model.json | internal manifest
├─ character.moc3
├─ textures/texture_00.png
├─ motions/
└─ metadata.json
```

---

## 9. `.a2dpack` — internal package format

The viewer loads packages, never raw game assets.

```
character.a2dpack/
├─ manifest.json   ├─ model.bin   ├─ textures/   ├─ animations/   └─ metadata/
```

```json
{
  "formatVersion": 1,
  "modelType": "spine",
  "sourceGame": "aeons_echo",
  "sourceFormat": "spine-3.8",
  "displayName": "CharacterName",
  "defaultAnimation": "idle",
  "textures": [],
  "animations": []
}
```

`model.bin` contains the **normalized IR**, never a raw source-game object graph.
Serialization must be **deterministic** (stable field order, sorted maps, fixed float formatting) —
golden tests depend on it. Bump `formatVersion` on any layout change and handle it in the loader.

---

## 10. Detection & error handling

**Detection never relies on file extensions alone.** Inspect content: MOC3 magic, Spine binary
version header, Spine JSON skeleton shape, atlas grammar, Unity serialized type metadata.
Importer-level detection may use path patterns, object names, Unity type names, neighboring assets,
bundle naming conventions. **Ambiguity is an error, not a coin flip.**

Errors must distinguish: unsupported format · unsupported source version · corrupt asset ·
missing texture · missing atlas · missing skeleton · unsupported runtime feature ·
game-specific reconstruction failure.

Never silently discard unsupported data. Prefer partial load + explicit degradation report:

```
Loaded with warnings:
- TransformConstraint timeline unsupported
- Event timeline ignored
- Missing expression: smile_02
```

Implement this as `LoadReport { warnings: Vec<Degradation> }` returned alongside the model, and
surface it in `animated2d inspect` and `validate`. A warning that no CLI surface prints is a bug.

---

## 11. Testing (mandatory)

| Kind | Covers |
|---|---|
| Unit | binary parsers, version detection, bone transforms, weighted skinning, interpolation, Bezier evaluation, draw order, clipping, atlas parsing, game-specific name normalization |
| Golden | `source asset → importer → IR → deterministic serialize` vs committed fixture |
| Visual regression | render fixed timestamps `0.0s / 0.25s / 0.5s / 1.0s`, compare framebuffer hash or image with tolerance |
| Cross-impl | compare geometry/appearance against official Spine / Cubism runtimes or known viewers |

Visual regression is the only thing that catches subtle deformation regressions. Treat a failing
image diff as a real failure until proven otherwise.

**Asset policy:** never commit extracted game assets or proprietary SDK binaries.
`tests/fixtures/` real assets live in a gitignored `tests/fixtures/local/`; committed fixtures are
synthetic or hand-authored minimal models plus serialized IR snapshots. If a test needs a real
asset, gate it behind `#[ignore]` + an env var and document it in `tests/README.md`.

---

## 12. Development order & first task

| Phase | Deliverable | Goal |
|---|---|---|
| 1 | atlas parser, skeleton decoder for one real target version, bones, slots, region + mesh + weighted mesh, basic timelines, GPU rendering | display one AEONS ECHO character correctly |
| 2 | deform, draw order, color, clipping, IK, transform constraints, mixing | idle animations play correctly across multiple characters |
| 3 | Unity bundle inspection, MOC3 extraction, Texture2D extraction, AnimationClip reconstruction, normalized Cubism package | display `zjwujiang_prefab`, play its idle |
| 4 | `GenericCubismModel : AnimatedModel` integrated into the shared viewer | one viewer, two runtimes |
| 5 | NIKKE importer (lobby/standing only) | one NIKKE idle model via Generic Spine runtime |
| 6 | transparent window, drag, click-through, tray, idle logic, config persistence | desktop mascot |

**First concrete task — do this before anything else:**

> Build an inspector for the provided decrypted `zjwujiang_prefab` Unity AssetBundle and output a
> structured inventory of all Cubism-related assets inside it.

Must identify: Unity version · Cubism MOC object / MOC3 payload · `Texture2D` assets ·
Prefab/GameObject hierarchy · `AnimationClip` names · Cubism fade motion data ·
`AnimatorController` references · original asset paths if retained.

Then implement an exporter reconstructing the minimum data needed to render the model outside Unity.

**Do not begin by implementing the desktop UI.** First milestone = successful model reconstruction
and preview.

**MVP is done when:** AEONS ECHO Spine character imports and displays · `zjwujiang_prefab` imports
and displays · idle works for both · both open in the same viewer UI · renderer has zero
game-specific branches · viewer loads `.a2dpack`, not raw assets · parser/runtime tests pass ·
at least one visual regression test exists per runtime family.

---

## 13. Open decisions — ask the user, do not decide silently

1. **Cubism Core — DECIDED (2026-08-24): independent MOC3 parser.**
   `a2d-cubism` decodes MOC3 in Rust and does **not** link Live2D's proprietary Cubism Core.
   This keeps the project redistributable and free of proprietary binaries (§11, §16), at the
   cost of reverse-engineering an undocumented binary format. Consequences to hold to:
   parameter and deformer evaluation must be derived and tested, not assumed; every
   unrecognised MOC3 section is reported as a `Degradation`, never skipped silently; and
   visual parity against a known-good viewer is the acceptance bar, per §11 cross-impl.
   Do not add Cubism Core as a fallback without asking again.
2. **Desktop shell.** Default plan is `winit` + `wgpu` + `tray-icon` (transparent, click-through,
   always-on-top all work, and the wgpu surface is owned directly). Tauri v2 is the alternative if
   a Next.js control panel is wanted — but compositing wgpu under a WebView adds real friction.
   Current plan: native window for the character, optional separate control panel later.
3. **Unity deserialization.** Third-party crates exist for Unity serialized files; maturity varies
   and must be verified against the actual bundle before adopting. Falling back to a
   purpose-built minimal reader in `a2d-unity` is acceptable — the importer boundary contains it.
4. **Language.** If Rust is rejected, the module boundaries in §3 are the part that must survive;
   the crate names are not.

---

## 14. Commands

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

cargo run -p a2d-cli -- inspect  <input>
cargo run -p a2d-cli -- import   <input> -o <output.a2dpack>
cargo run -p a2d-cli -- validate <package>
cargo run -p a2d-cli -- preview  <package>

UPDATE_GOLDEN=1 cargo test -p a2d-spine   # regenerate golden fixtures (review the diff!)
```

`validate` must check: missing textures · unresolved attachments · unsupported timeline types ·
invalid bone parents · invalid slot references · malformed atlas references · unsupported constraints.

---

## 15. Subagents

Delegate to the agent that owns the layer. Agents are in `.claude/agents/`.

| Agent | Owns |
|---|---|
| `architecture-guardian` | layering/dependency review — run before any merge that touches ≥2 crates |
| `format-detective` | detection heuristics, magic numbers, version sniffing, `inspect`/`validate` |
| `spine-decoder` | `a2d-spine` — version decoders → Generic Spine IR |
| `cubism-decoder` | `a2d-cubism` — moc3/motion3/physics/pose → Generic Cubism model |
| `unity-importer` | `a2d-unity`, `a2d-import` — bundle spelunking, asset reconstruction |
| `animation-runtime` | `a2d-runtime` — timelines, curves, skinning, constraints, mixing |
| `renderer-engineer` | `a2d-render` — wgpu, blending, masks, batching, high-DPI |
| `desktop-host` | `a2d-desktop` — transparent window, drag, click-through, tray, config |
| `test-engineer` | unit/golden/visual-regression harnesses, fixture hygiene |

When a task spans layers, do the work layer by layer with the owning agent, then run
`architecture-guardian` last.

---

## 16. Legal / ethical boundary

This project targets assets the user has extracted from software they own, for personal offline
viewing. Do not add DRM circumvention, license-check bypass, game-server communication, or account
automation. Do not commit or redistribute game assets or proprietary SDK binaries.
If a task drifts toward those, stop and say so.
