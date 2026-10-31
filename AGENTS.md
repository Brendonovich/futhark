# Futhark

A TouchDesigner-style realtime node-based environment built on Rune and wgpu. Read `PLAN.md` for the design and
`docs/research/touchdesigner.md` for background.

## Rules

- No legacy TouchDesigner features (no CPU surface geometry, no Tscript, no export parameter mode, no GLSL).
- Use plain family names: Texture, Channel, Geometry, Data, Shader, Component. Never TOP, CHOP, POP, DAT, MAT, COMP
  in code or UI.
- All GPU work goes through wgpu, and shaders are WGSL. Match Rune's wgpu version exactly.
- The engine stays headless. UI code lives only in `apps/editor`.
- Pin every dependency to an exact version (Rune to a `rev`).
- Tests run offline and headless.
