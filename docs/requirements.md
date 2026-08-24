# Animated2D Desktop Viewer — Implementation Requirements

## 1. Purpose

Build a reusable desktop 2D character viewer/runtime that can display animated character assets extracted from multiple games without depending on each game's original runtime.

The initial target ecosystems are:

- Live2D Cubism assets
- Spine assets across multiple historical versions
- Game-specific Unity-packaged variants of the above

Initial game-oriented importers should be designed with these titles in mind:

- 放置少女 / Houkai? (Depose Girls)
- AEONS ECHO
- NIKKE

The system must **not** attempt to convert Spine into Cubism directly. Instead, both formats must be normalized into internal runtime representations behind a shared high-level interface.

---

## 2. Core Design Principle

The architecture must strictly separate these concerns:

1. **Game-specific asset extraction**
2. **Source-format decoding**
3. **Normalization into internal IR**
4. **Animation evaluation**
5. **Rendering**
6. **Desktop-window behavior**

Game-specific knowledge must never leak into the renderer.

Target flow:

```text
Game Asset Bundle / Extracted Files
        ↓
Game Importer
        ↓
Source Format Detector
        ↓
┌──────────────────────┐
│ Spine Decoder        │
│ Cubism Decoder       │
└──────────────────────┘
        ↓
Normalized Animated2D Model
        ↓
Runtime / Animator
        ↓
Renderer
        ↓
Desktop Host
```

---

## 3. Non-Goals

Do not implement the following in the first versions:

- Full compatibility with the Spine Editor
- Full compatibility with Live2D Cubism Editor
- Editing or authoring models
- Exporting back to proprietary Spine/Cubism project formats
- Battle-specific game behavior
- Game server emulation
- Exact replication of every source-engine feature before basic viewing works
- Direct Spine → Cubism conversion
- 3D rendering

The primary objective is **correct character playback for desktop viewing**.

---

## 4. High-Level Architecture

Recommended module structure:

```text
src/
├─ core/
│  ├─ model/
│  ├─ animation/
│  ├─ math/
│  └─ io/
│
├─ formats/
│  ├─ spine/
│  │  ├─ detect/
│  │  ├─ v2/
│  │  ├─ v3/
│  │  ├─ v4/
│  │  └─ normalize/
│  │
│  └─ cubism/
│     ├─ detect/
│     ├─ cubism2/
│     ├─ cubism3plus/
│     └─ normalize/
│
├─ importers/
│  ├─ generic/
│  ├─ depose_girls/
│  ├─ aeons_echo/
│  └─ nikke/
│
├─ runtime/
│  ├─ spine/
│  ├─ cubism/
│  └─ common/
│
├─ renderer/
│  ├─ gpu/
│  ├─ texture/
│  ├─ clipping/
│  └─ batching/
│
├─ desktop/
│  ├─ window/
│  ├─ interaction/
│  └─ tray/
│
└─ tools/
   ├─ inspect/
   ├─ convert/
   └─ validate/
```

The exact language/framework can be chosen independently, but module boundaries must follow this separation.

---

## 5. Shared Runtime Interface

All animated model implementations must expose a shared interface equivalent to:

```text
IAnimatedModel
├─ load()
├─ update(deltaTime)
├─ render()
├─ playAnimation(name, options)
├─ stopAnimation(name)
├─ setExpression(name)
├─ getAnimations()
├─ getExpressions()
├─ getBounds()
├─ hitTest(x, y)
└─ dispose()
```

Source-specific details must remain behind concrete implementations.

Example implementations:

```text
GenericSpineModel : IAnimatedModel
GenericCubismModel : IAnimatedModel
```

Do not force Spine and Cubism into a single low-level deformation model.

---

## 6. Generic Spine IR

Implement a source-version-independent internal representation called **Generic Spine IR**.

Its purpose is to remove historical Spine version differences from the runtime.

Suggested structure:

```text
GenericSpineModel
├─ metadata
├─ bones[]
├─ slots[]
├─ skins[]
├─ attachments[]
├─ constraints
│  ├─ ik[]
│  ├─ transform[]
│  └─ path[]
├─ animations[]
├─ events[]
├─ drawOrder
└─ textureAtlases[]
```

### 6.1 Bone

Must support:

- name
- parent
- local translation
- rotation
- scale
- shear if required
- inheritance/transform mode

### 6.2 Slot

Must support:

- target bone
- attachment reference
- color
- dark color when available
- blend mode
- draw order

### 6.3 Attachment Types

Initial implementation must prioritize:

- Region attachment
- Mesh attachment
- Weighted/skinned mesh
- Clipping attachment
- Bounding box attachment

Later support may include:

- Point attachments
- Path attachments

### 6.4 Animation Timelines

Support at minimum:

- bone translate
- bone rotate
- bone scale
- bone shear
- slot color/alpha
- attachment switching
- draw order
- mesh deformation
- event timeline

Interpolation support:

- linear
- stepped
- Bezier curves

### 6.5 Constraints

Support in this order:

1. IK constraints
2. Transform constraints
3. Path constraints

A model must still load if unsupported constraint data exists. Unsupported features must be reported explicitly instead of silently corrupting playback.

---

## 7. Spine Version Compatibility

The runtime must **not** parse every Spine version directly.

Instead use version-specific decoders:

```text
Spine 2.x ─┐
Spine 3.x ─┼→ Generic Spine IR
Spine 4.x ─┘
```

Each decoder is responsible for translating source semantics to the latest Generic Spine IR.

The Generic Spine runtime must never need to know whether the source was Spine 2.1, 3.8.99, 4.1, etc.

### Initial priority

Prioritize versions encountered in target games.

At minimum:

- Spine 3.8.x
- whichever Spine version is detected in AEONS ECHO
- whichever Spine version is detected in NIKKE lobby/character assets

Do not implement unused versions speculatively.

---

## 8. Generic Cubism Representation

Cubism must remain a separate normalized runtime model.

Suggested structure:

```text
GenericCubismModel
├─ moc data / runtime model
├─ parameters[]
├─ parts[]
├─ drawables[]
├─ textures[]
├─ motions[]
├─ expressions[]
├─ physics
├─ pose
└─ hitAreas[]
```

The first implementation should prioritize Cubism 3+ because 放置少女 uses a modern Cubism Unity integration.

Cubism 2 support may be added later behind a separate decoder/runtime adapter.

---

## 9. Game-Specific Importers

Game importers must only perform **asset discovery and reconstruction**.

They must output generic source-format packages and must not contain rendering logic.

### 9.1 Depose Girls Importer

Input examples:

- decrypted Unity AssetBundles
- extracted Unity serialized assets

Responsibilities:

- detect Cubism-related assets under paths similar to:

```text
Assets/GirlsGame/Editor/Resources/Live2D/
```

- extract MOC3 payload from Unity `CubismMoc`/TextAsset-like data
- extract Texture2D data
- detect Unity AnimationClips originating from `*.motion3.json`
- inspect fade motion assets
- reconstruct model metadata as needed
- optionally export a normalized Cubism package

Expected normalized output:

```text
character/
├─ model.json or internal manifest
├─ character.moc3
├─ textures/
│  └─ texture_00.png
├─ motions/
└─ metadata.json
```

Important:

Unity-imported Cubism animations may no longer exist as raw `motion3.json`. The importer must be able to recover animation parameter curves from Unity `AnimationClip` objects when possible.

### 9.2 AEONS ECHO Importer

Expected Spine-style source assets may include:

```text
*.skel.bytes
*.atlas.txt
*.png
```

Responsibilities:

- discover corresponding skeleton/atlas/texture sets
- normalize suffixes (`.skel.bytes` → `.skel`, `.atlas.txt` → `.atlas` internally)
- detect Spine version
- pass the reconstructed package to the correct Spine decoder

### 9.3 NIKKE Importer

Scope is limited to **desktop-viewable character / lobby models**.

Do not prioritize:

- shooting pose models
- battle-specific rigs
- weapons
- aiming systems
- combat VFX
- burst scene reconstruction

Responsibilities:

- find lobby/standing character animation assets
- reconstruct skeleton + atlas + textures
- detect Spine version
- hand package to Generic Spine decoder

---

## 10. Internal Package Format

Introduce a project-owned intermediate package format so the desktop viewer does not have to repeatedly parse game-specific Unity assets.

Example package extension:

```text
.a2dpack
```

Suggested structure:

```text
character.a2dpack/
├─ manifest.json
├─ model.bin
├─ textures/
├─ animations/
└─ metadata/
```

Manifest example:

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

`model.bin` should contain the normalized IR, not a raw source-game object graph.

Do not expose game-specific implementation details to the viewer.

---

## 11. Rendering Requirements

The renderer must support:

- textured triangle meshes
- alpha blending
- additive blending
- multiplicative blending if needed
- draw ordering
- mesh deformation
- weighted skinning
- masks/clipping
- per-slot opacity/color
- high-DPI scaling
- transparent backgrounds

The renderer should be source-format-neutral.

Recommended conceptual API:

```text
RenderMesh
├─ vertices
├─ uvs
├─ indices
├─ texture
├─ color
├─ blendMode
├─ clippingMask
└─ zOrder
```

Both GenericSpineRuntime and GenericCubismRuntime should ultimately emit renderer-friendly primitives.

---

## 12. Animation Runtime

Implement deterministic animation evaluation independent from rendering FPS.

Requirements:

- delta-time based evaluation
- looping animations
- one-shot animations
- animation queue
- crossfade/mixing
- random idle selection
- default idle selection
- playback speed
- pause/resume

Desktop mascot behavior can later use this runtime.

---

## 13. Desktop Viewer Requirements

Initial platform: Windows.

Required features:

- transparent frameless window
- optional always-on-top
- draggable character
- click-through mode
- configurable scale
- configurable position
- animation selector
- model selector
- play/pause
- system tray integration
- remember last position/model

Nice-to-have:

- interaction hit areas
- click/tap reaction animations
- random idle reactions
- multiple characters
- per-character configuration

Do not mix these features into the source-format importer layer.

---

## 14. CLI Tools

Provide developer-facing CLI utilities.

### inspect

```bash
animated2d inspect <input>
```

Output:

- detected game/source
- source animation format
- version
- contained textures
- animation names
- model bounds
- unsupported features

### import

```bash
animated2d import <input> -o <output.a2dpack>
```

### validate

```bash
animated2d validate <package>
```

Validation must check:

- missing textures
- unresolved attachments
- unsupported timeline types
- invalid bone parents
- invalid slot references
- malformed atlas references
- unsupported constraints

### preview

```bash
animated2d preview <package>
```

Open the package in the desktop viewer directly.

---

## 15. Detection Strategy

Do not rely only on file extensions.

Format detection must inspect actual file contents where possible.

Examples:

- Cubism MOC3 magic
- Spine binary version metadata/header
- Spine JSON skeleton structure
- atlas grammar
- Unity serialized type metadata

Game-specific importer detection should use:

- path patterns
- object names
- Unity type names
- neighboring assets
- source bundle naming conventions

---

## 16. Error Handling

Never silently discard unsupported data.

Errors must distinguish:

- unsupported format
- unsupported source version
- corrupt asset
- missing dependent texture
- missing atlas
- missing skeleton
- unsupported runtime feature
- game-specific reconstruction failure

Whenever possible, partially load models and report degraded features.

Example:

```text
Loaded with warnings:
- TransformConstraint timeline unsupported
- Event timeline ignored
- Missing expression: smile_02
```

---

## 17. Testing Requirements

Testing is mandatory.

### 17.1 Unit Tests

Cover:

- binary parsers
- version detection
- bone transforms
- weighted skinning
- animation interpolation
- Bezier evaluation
- draw order
- clipping
- atlas parsing
- game-specific naming normalization

### 17.2 Golden Tests

For known source models, maintain expected normalized outputs.

```text
source asset
  ↓ importer
normalized IR
  ↓ serialize
expected fixture
```

Use deterministic serialization for comparisons.

### 17.3 Visual Regression Tests

Render known frames at fixed timestamps:

```text
0.0s
0.25s
0.5s
1.0s
```

Compare screenshots or framebuffer hashes/tolerances.

This is critical for detecting subtle deformation regressions.

### 17.4 Cross-Implementation Validation

When possible, compare output against:

- official Spine runtime
- official/known Cubism runtime
- existing viewers

The same source animation at the same timestamp should produce nearly identical geometry/appearance.

---

## 18. Development Order

### Phase 1 — Generic Spine MVP

Implement:

- atlas parser
- skeleton decoder for one actual target Spine version
- bones
- slots
- region attachments
- mesh attachments
- weighted mesh
- basic animation timelines
- GPU rendering

Goal:

Display one AEONS ECHO character correctly.

### Phase 2 — Spine Animation Completeness

Add:

- deform timeline
- draw order
- color
- clipping
- IK
- transform constraints
- animation mixing

Goal:

Correctly play idle animations from multiple characters.

### Phase 3 — Depose Girls Cubism Import

Implement:

- Unity bundle object inspection
- MOC3 extraction
- Texture2D extraction
- Cubism Unity AnimationClip reconstruction
- normalized Cubism package creation

Goal:

Display the provided `zjwujiang_prefab` character and play at least its idle animation.

### Phase 4 — Generic Cubism Runtime Integration

Create `GenericCubismModel : IAnimatedModel` and integrate with the shared desktop viewer.

### Phase 5 — NIKKE Importer

Implement only lobby/standing character use cases.

Goal:

Display one NIKKE character idle model through Generic Spine Runtime.

### Phase 6 — Desktop Mascot Features

Add:

- transparent window
- drag
- click-through
- tray
- idle animation logic
- configuration persistence

---

## 19. Implementation Rules for Claude Code

Claude Code should follow these rules:

1. Do not tightly couple game importers to rendering.
2. Do not add a new renderer per game.
3. Do not convert Spine assets to Cubism assets.
4. Normalize source data before runtime playback.
5. Keep source-version-specific logic inside decoders.
6. Prefer explicit typed data models over loosely typed dictionaries/maps.
7. Add tests with every parser/runtime feature.
8. Preserve unknown fields when practical, but do not let unknown fields pollute runtime APIs.
9. Fail loudly on ambiguous format/version detection.
10. Avoid speculative abstraction until at least two concrete implementations require it.
11. Optimize only after correctness and visual parity are established.
12. Never use source game behavior as the canonical runtime model.

---

## 20. First Concrete Task

The first implementation task should be:

> Build an inspector for the provided decrypted `zjwujiang_prefab` Unity AssetBundle and output a structured inventory of all Cubism-related assets contained within it.

The inspector should identify at minimum:

- Unity version
- Cubism MOC object / MOC3 payload
- Texture2D assets
- Prefab/GameObject hierarchy
- AnimationClip names
- Cubism fade motion data
- AnimatorController references
- source asset paths if retained

Then implement an exporter that reconstructs the minimum data required to render the model outside Unity.

Do **not** begin by implementing the entire desktop UI.

The first milestone is successful model reconstruction and preview.

---

## 21. Definition of Done for MVP

The MVP is complete when all of the following are true:

1. An AEONS ECHO Spine character can be imported and displayed.
2. The provided 放置少女 `zjwujiang_prefab` can be imported and displayed.
3. Idle animation works for both models.
4. Both are opened through the same desktop viewer UI.
5. The renderer contains no game-specific branches.
6. The viewer loads normalized internal packages rather than raw game assets.
7. Automated parser/runtime tests pass.
8. At least one visual regression test exists for each runtime family.

