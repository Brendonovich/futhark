# Futhark plan

Futhark is a realtime, node-based visual programming environment in the spirit of TouchDesigner, built on
[Rune](https://github.com/anomalyco/rune) for UI and [wgpu](https://wgpu.rs) for all GPU work. Background research is
in `docs/research/touchdesigner.md`.

## Principles

- **Modern only.** No legacy features from TouchDesigner. That means no CPU surface geometry (SOPs), no Tscript, no
  OpenGL-era GLSL, and no "export" parameter mode (binds and expressions cover it).
- **Plain names.** Families are called Texture, Channel, Geometry, Data, Shader and Component, not TOP, CHOP and so on.
- **wgpu for every GPU operation**: texture ops, geometry, rendering and readback. Shaders are WGSL. Rune also renders
  through wgpu (30.x), so the engine and the UI share a single `wgpu::Device` and textures never leave the GPU to be
  shown.
- **The engine is headless.** The graph, cooking and operators know nothing about the UI. The editor is a thin Rune
  client, and the engine can also run without a window: offline renders, tests, a CLI.

## Families

Each operator belongs to one family, and the family decides what flows along its wires.

| Family | TD equivalent | Lives on | Data |
| --- | --- | --- | --- |
| **Texture** | TOP | GPU | One `wgpu::Texture` (2D, 3D or array) with a format and resolution. |
| **Channel** | CHOP | CPU | Named channels of `f32` samples sharing a sample rate and a start/end range. |
| **Geometry** | POP | GPU | Point, vertex and primitive buffers with typed attributes (`P`, `N`, `Color`, `Tex`, custom). |
| **Data** | DAT | CPU | Text, or a table of string cells. |
| **Shader** | MAT | GPU | A material: a WGSL pipeline plus its parameters, applied to Geometry when rendered. |
| **Component** | COMP | — | A container holding a network. It exposes inputs and outputs through In/Out operators and custom parameters. Kinds include Base, Geometry, Camera, Light, Window and the panel types. |

## Type system

- Wires connect operators of the same family. Crossing families takes either an explicit converter (Channel → Texture,
  Texture → Channel, Data → Channel, Channel → Geometry, Texture → Geometry, …) or a reference parameter that names
  another operator by path.
- A wire is a reference, not a copy. Downstream operators read their inputs' outputs directly, and GPU families pass
  resources by handle.
- Parameters are typed: float/int vectors of 1–4 components, toggle, menu, string, operator reference and pulse.
  Each parameter is in one of three modes: constant, expression or bind.
- GPU → CPU readback is explicit and asynchronous. It either arrives one frame late (the default) or stalls on purpose.

## Evaluation

- **Pull-based and lazy.** An edit marks an operator dirty. Each frame, roots pull: visible viewers, open output
  windows, and outputs to devices. An operator cooks only when it is pulled and has a reason to cook (an input or
  reference cooked, a parameter changed, or it depends on time).
- **Automatic dependency capture.** Reading another operator's data or parameters while cooking or evaluating an
  expression records a dependency.
- **One global clock** with a target frame rate and a realtime flag (drop frames, or process every frame for offline
  work). Components can also have local clocks. Time-dependent Channel operators output exactly the samples since the
  last cook.
- Cooking starts single-threaded on the main thread. The graph should be built so independent branches can later cook
  in parallel; nothing global or mutable is reachable from an operator's cook.

## Expressions and scripting

[Luau](https://luau.org), embedded through [mlua](https://crates.io/crates/mlua), is used for both parameter
expressions and scripting (callbacks, script operators). It was chosen over Rhai for speed: it is roughly 10× faster
in [script-bench-rs](https://github.com/khvzak/script-bench-rs) and 2–3× faster than CPython. It is also sandboxed by
design and proven at scale (Roblox). The cost is a C++ dependency, which mlua vendors and builds.

- **Expressions are compiled once.** Each expression is compiled to bytecode when it is edited and run on cook. Its
  result is cached until one of its dependencies changes.
- **Simple expressions skip the VM.** Constants, a bare parameter or channel reference, and basic arithmetic on those
  are recognised up front and run on a native path. This is the same trick as TouchDesigner's "optimized"
  expressions.
- **Dependencies are captured through accessors.** The graph is exposed to Luau as userdata (`op`, `parent`, `me`,
  `time`, parameter and channel access). Each access records a dependency for the operator being cooked.
- **Scripts are sandboxed.** Luau sandbox mode makes globals and the standard library read-only. Interrupts enforce a
  time budget per call. There is no filesystem or OS access, and `require` resolves only to Data operators in the
  project.
- **Types.** Scripts may use Luau type annotations, and the editor ships type definitions for the graph API.
- **One VM per cook thread.** Lua states are not shared across threads. When cooking goes parallel, each worker gets
  its own VM, and compiled bytecode is shared between them.
- GPU work stays in WGSL. Luau never runs per pixel or per point.

## Rendering

- Texture operators record into a shared `wgpu::CommandEncoder` per frame, drawing from a pool of textures keyed by
  size and format.
- By default a Texture operator inherits its resolution and format from its first input.
- A generic **Shader texture** operator runs user WGSL, either as a fragment pass over a full-screen triangle or as a
  compute pass. Inputs, parameters and time are bound automatically.
- 3D works the TouchDesigner way. A **Render** texture operator takes Camera, Light and Geometry components and
  outputs a Texture. Each Geometry component holds a Geometry network and a Shader, and supports instancing with
  per-instance attributes from any family.

## Architecture

Planned workspace:

- `crates/graph`: the network model (operators, paths, wires, parameters, components), dirty tracking, the cook
  scheduler and the clock. No GPU code.
- `crates/gpu`: the wgpu context, texture and buffer pools, WGSL helpers and readback.
- `crates/ops`: the built-in operators of every family.
- `crates/project`: saving and loading projects and reusable components.
- `crates/cli`: a headless runner (render a project to files, run tests).
- `apps/editor`: the Rune desktop app, including:
  - a network editor (a canvas with pan and zoom, nodes, wires)
  - a viewer for each family (textures drawn straight from the shared device through Rune effect slots)
  - a parameter inspector
  - output windows and perform mode

Rune: pin `anomalyco/rune` to an exact `rev` on `dev`. Its wgpu is `~30.0.1`; ours must match.

## Milestones

1. **Skeleton**: the workspace, a headless wgpu context, and CI that runs tests offline and headless.
2. **Graph core**: operators, wires, parameters, pull cooking and dirty propagation, with Channel operators (Constant,
   LFO, Noise, Math) as the first test bed.
3. **Textures**: Constant, Ramp, Noise, Transform, Level, Composite, Shader, and File In. Headless golden-image tests
   via readback.
4. **Editor**: a Rune app with the network editor, texture and channel viewers, and a parameter inspector, all on
   the shared device.
5. **Data and expressions**: tables and text, expressions and binds, Channel ↔ Texture ↔ Data converters.
6. **3D**: the Geometry family, Shader family, Render, Camera, Light, and instancing.
7. **Output and I/O**: Window components, perform mode, OSC and MIDI in and out, audio in and out.

## Open questions

- **Device ownership.** Either the engine creates the device and Rune adopts it, or the engine takes Rune's `Gpu`.
  The second works today through the compositor hook. The first is needed to run headless and windowed with the same
  code.
- **Project file format.** A text format that diffs well (one file per component?) versus a single file.
