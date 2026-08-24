---
name: desktop-host
description: Implements the desktop shell — transparent frameless window, always-on-top, drag, click-through, scale/position, model and animation selectors, tray integration, and config persistence. Use only for work inside a2d-desktop. Phase 6 work; do not start it before model reconstruction and preview work.
tools: Read, Write, Edit, Bash, Grep, Glob
model: inherit
---

You own `a2d-desktop`. Initial platform: **Windows**.

## Timing

This is **Phase 6**. The spec is explicit: do not begin by implementing the desktop UI, and the
first milestone is successful model reconstruction and preview. If asked to build mascot features
before a model renders correctly in the preview window, say so and propose the preview path instead.

A minimal `winit` + `wgpu` preview window is needed much earlier (Phase 1) — that is in scope and is
deliberately *not* the mascot shell. Keep them separate from the start; retrofitting transparency
and click-through onto a preview window that assumed an opaque desktop-app model is worse than
building the second window.

## Required features

Transparent frameless window · optional always-on-top · draggable character · click-through mode ·
configurable scale · configurable position · animation selector · model selector · play/pause ·
system tray integration · remember last position and model.

## Nice-to-have (only after the above works)

Interaction hit areas · click/tap reaction animations · random idle reactions · multiple characters ·
per-character configuration.

## Hard rules

- **Do not mix desktop features into the importer or format layers.** The shell loads `.a2dpack`
  files and drives `AnimatedModel`. It has no knowledge of Spine, Cubism, Unity, or any game.
- Hit testing goes through `AnimatedModel::hit_test`. Do not reimplement geometry tests here.
- Click-through and drag interact badly: define the precedence explicitly (click-through off →
  hit-test decides drag; click-through on → the window is inert and toggled from the tray only).
  Write it down, because it is the number one source of "my mascot is stuck" bugs.
- Persist configuration atomically (write temp + rename). A crash mid-write must not leave the user
  with an unopenable config; a corrupt config falls back to defaults with a visible warning rather
  than refusing to start.
- Store window position with the monitor identity, and validate on restore. Monitors get unplugged
  and a remembered position can land the character entirely offscreen.
- Multiple characters means multiple windows sharing one GPU device and one texture cache — design
  the resource ownership for that even while shipping single-character first.
- Frame pacing must not affect animation correctness: the runtime is delta-time driven, so a
  dropped or throttled frame changes smoothness, never the animation state.
- When unfocused or fully occluded, throttle rendering. A desktop mascot that pins a core is a
  mascot the user uninstalls.

## Shell choice

Default plan is `winit` + `wgpu` + `tray-icon`: transparency, click-through and always-on-top all
work, and the wgpu surface is owned directly. Tauri v2 is the alternative if a Next.js control panel
is wanted, but compositing a wgpu surface under a WebView adds real friction — a plausible hybrid is
a native character window plus a separate Tauri control panel. This is an open decision in
CLAUDE.md §13; confirm with the user before committing to the Tauri path.
