# Cross-version divergences

Each entry: the versions affected, and how the IR resolves it so the runtime never learns the
source version.

## Rotate timelines: shortest-arc interpolation

- **3.x** interpolates a rotate timeline along the shortest arc. The difference between
  consecutive key angles is wrapped into [-180, 180) before the curve percent is applied; an exact
  +180 difference maps to -180. The wrap and its tie rule come from reading the 3.8 runtime's
  formula, not from a real export; that real 3.8 rigs contain key pairs that only make sense under
  a shortest-arc rule was observed. Unverified: earlier 3.x runtimes may have used a while-loop
  wrap that keeps +180 (this affects only exact +/-180 differences).
- **4.0 and later** removed the wrap and use key values as written.
- **2.x** is believed to wrap as well, but that is unverified, so the 2.x JSON family is left
  unchanged rather than guessed at.
- **IR:** follows 4.x, interpolating key values directly. The 3.x decoders (binary and JSON)
  unwrap the keys (`normalize::unwrap_rotate_keys`, crate-private): key 0 is kept, every later key becomes the
  previous unwrapped value plus the wrapped raw difference. This is exact for linear and Bezier
  spans, because 3.x applies the curve percent to the wrapped difference, which equals a lerp
  between the unwrapped keys. Stepped spans are unaffected. Other 3.x timelines are not wrapped
  (shear and transform-constraint mixes are plain values) and are not touched.
- Packages written before format 10 keep the raw angles; `validate` notes them when a key pair
  steps by `>= 180` or `< -180` degrees (mirroring the wrap interval).

## Known gap

Crossfades in `a2d-runtime` lerp rotation linearly, so after unwrapping, an accumulated spin (for
example keys 0/120/240/0 becoming 0..360) that crossfades to a pose near 0 turns a full extra
circle. Shortest-arc rotation mixing is not implemented in the runtime yet (follow-up).
