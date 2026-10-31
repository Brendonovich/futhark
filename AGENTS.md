# Futhark

A TouchDesigner-style realtime node-based environment built on Rune and wgpu. Read `PLAN.md` for the design and
`docs/research/touchdesigner.md` for background.

## Layout

- `crates/graph`: the network model, parameters, the operator registry and the pull-based cook engine. No GPU code.
- `crates/macros`: the `Params` and `Menu` derives, re-exported by `futhark-graph`.
- `crates/ops`: the built-in operators, and `registry()` listing them.
- `crates/project`: reading and writing the KDL project format.
- `crates/gpu`: the engine-owned wgpu context and readback.
- `crates/cli`: the `futhark` binary.
- `examples/`: example projects. Tests check each loads cleanly and saves back byte for byte.

## Commands

- `scripts/check`: everything CI runs (fmt, clippy, tests). Done means it passes.
- `cargo run -p futhark -- cook examples/hello /mix --frames 10` cooks an operator and prints its output.
- `cargo run -p futhark -- check <project>` reports anything that doesn't load cleanly.
- `cargo run -p futhark -- gpu` shows the GPU adapter the engine picks.

## Rules

- No legacy TouchDesigner features (no CPU surface geometry, no Tscript, no export parameter mode, no GLSL).
- Use plain family names: Texture, Channel, Geometry, Data, Shader, Component. Never TOP, CHOP, POP, DAT, MAT, COMP
  in code or UI.
- All GPU work goes through wgpu, and shaders are WGSL. Match Rune's wgpu version exactly.
- The engine stays headless. UI code lives only in `apps/desktop`.
- Every capability goes through the server's protocol, so the CLI, the desktop app and agents can all do it. The CLI
  covers every method.
- Pin every dependency to an exact version (Rune to a `rev`).
- Tests run offline and headless.
- A new operator goes in `crates/ops` and in `registry()`. The schema test checks its parameters.
