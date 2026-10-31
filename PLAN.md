# Futhark plan

Futhark is a realtime, node-based visual programming environment in the spirit of TouchDesigner, built on
[Rune](https://github.com/anomalyco/rune) for UI and [wgpu](https://wgpu.rs) for all GPU work. Background research is
in `docs/research/touchdesigner.md`.

## Principles

- **Modern only.** No legacy features from TouchDesigner. That means no CPU surface geometry (SOPs), no Tscript, no
  OpenGL-era GLSL, and no "export" parameter mode (binds and expressions cover it).
- **Plain names.** Families are called Texture, Channel, Geometry, Data, Shader and Component, not TOP, CHOP and so on.
- **wgpu for every GPU operation**: texture ops, geometry, rendering and readback. Shaders are WGSL. Rune also renders
  through wgpu (30.x), so the engine creates a single `wgpu::Device` that Rune adopts, and textures never leave the
  GPU to be shown.
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

### Script tooling

IntelliSense comes from [luau-lsp](https://github.com/JohnnyMorganz/luau-lsp), run as a language server. It provides
autocomplete, hover, signature help, type errors, go to definition, rename and semantic tokens.

- **Definition files describe the graph API.** They cover the globals (`op`, `me`, `parent`, `time`) and a class for
  each operator type with its typed parameters.
  - They are generated from the Pkl operator definitions, so they can't drift from the code.
- **A sourcemap describes the operator tree.** luau-lsp's Rojo-style sourcemap is generated from the project's
  network, so `parent().noise1.par.` completes real nodes and parameters. It is regenerated when the network changes.
  luau-lsp requires its sourcemap to be JSON, so it is written to the gitignored `.futhark/` cache and never
  committed.
- **In the editor**, the editor ships a luau-lsp binary at a pinned version and talks LSP to it over stdio. The
  editor draws completion popups, hover cards and diagnostics with Rune. Highlighting comes from semantic tokens.
- **In outside editors** (VS Code, Zed, Neovim), the same definition files and sourcemap give full IntelliSense on
  the project's `.luau` files.
- Paths passed as strings (`op("../noise1")`) are completed by the editor itself, because the type checker can't see
  into strings. Typed access (`parent().noise1`) is the preferred style.
- Linking Luau's analysis library directly (in process, completions from the live graph) is a possible later step if
  regenerating the sourcemap ever lags behind editing.

## Rendering

- Texture operators record into a shared `wgpu::CommandEncoder` per frame, drawing from a pool of textures keyed by
  size and format.
- By default a Texture operator inherits its resolution and format from its first input.
- A generic **Shader texture** operator runs user WGSL, either as a fragment pass over a full-screen triangle or as a
  compute pass. Inputs, parameters and time are bound automatically.
- 3D works the TouchDesigner way. A **Render** texture operator takes Camera, Light and Geometry components and
  outputs a Texture. Each Geometry component holds a Geometry network and a Shader, and supports instancing with
  per-instance attributes from any family.

## GPU device

The engine creates the wgpu instance, adapter, device and queue, and Rune adopts them.

- The engine creates its device the same way whether it runs headless (CLI, tests) or windowed, so GPU code has a
  single path.
- When windowed, the engine checks that its adapter supports the window's surface. It creates the device with the
  features and limits that both the engine and Rune need.
- Rune today creates its own `Gpu` per window (`Gpu::for_surface` in `window.rs`) and has no public way to accept
  one. `Gpu`'s fields are public, so the missing piece is a small upstream change: a run option that takes an
  `Arc<Gpu>` and uses it instead of creating one. That change is the first task of the Editor milestone.
- Engine textures are drawn in the UI through Rune effect slots and a compositor, on the same device, with no copies.

## Project files

Projects are git-first: a directory of text files is the project. There is no binary project file and no JSON.
Anything else on disk (caches, thumbnails, compiled shaders, an index) lives in a gitignored `.futhark/` folder and can
always be rebuilt.

```text
show/
  project.kdl          project settings: name, frame rate, realtime flag, the perform window
  root/                the root component
    component.kdl      its operators, parameters and wires
    layout.kdl         node positions and sizes in the network editor
    blur.wgsl          text content of Data operators, as real files
    controller.luau
    cues.tsv
    effects/           a child component is a subfolder with the same layout
      component.kdl
      layout.kdl
  media/               images, video, audio, models, fonts (Git LFS)
  .gitattributes       generated: LFS rules for media/
  .futhark/            gitignored cache
```

### Format

Component files are [KDL](https://kdl.dev) v2, written in a fixed house style. The writer enforces the style, so a file
looks the same however it was edited.

```kdl
ramp1 "texture/ramp"

noise1 "texture/noise" {
    input ramp1
    period 4
    amp expr="time.seconds * 0.5"
    seed bind="../controls/seed"
}

script1 "data/text" file="controller.luau"
```

- **One fact per line.** A parameter, a wire and a flag each get their own line, so separate edits touch separate
  lines.
- **Fixed order.** Operators are sorted by name and parameters follow their declaration order, so saving never
  reorders anything.
- **Defaults are omitted.** Only parameters that differ from their default are written.
- **Wires live on the receiving operator** as `input` lines, so connecting a wire changes one line of one operator.
- **Layout is separate.** Node positions live in `layout.kdl`, so dragging nodes around never conflicts with real
  edits.
- **Text content is a real file.** Data operators holding scripts, shaders or tables point at a sibling file with its
  natural extension (`.luau`, `.wgsl`, `.tsv`, `.md`). These files get proper diffs, highlighting and outside-editor
  support.
- **Components are folders.** Each component's network is its own file, so conflicts stay local to one network.
- **Names are identities.** Operators are referenced by name and path, not by generated IDs. Files stay readable, and
  a rename is an ordinary diff.

### Working with git

- **Saving writes only what changed.** Each component file is written atomically (write to a temporary file, then
  rename).
- **External changes reload live.** The engine watches the project directory. A `git pull`, `checkout` or a script
  edited in another editor reloads only the components that changed, and the running network picks them up.
- **Broken merges load anyway.** A wire or reference to a missing operator, an unknown parameter, or a type mismatch
  becomes an error shown on that operator. The project never refuses to open.
- **`futhark check`** validates a project headlessly (parse, references, types, expressions compile) for CI and
  pre-commit hooks.
- **Media** keeps human-readable paths under `media/` and is tracked with Git LFS. Operators refer to media by path
  relative to the project.
- **Reusable components** are just component folders. Copying one into another project, or adding it as a git
  submodule, is how components are shared.
- Undo history is kept in memory per session. Git is the long-term history.

## Operator definitions

Every operator type (its family, inputs, outputs, parameters with types, defaults, ranges, menus, labels and docs) is
declared in [Pkl](https://pkl-lang.org), not hand-written in Rust.

```pkl
noise = new TextureOp {
  label = "Noise"
  inputs = 0
  params {
    ["period"] = new Float { default = 1; min = 0; softMax = 10 }
    ["amp"] = new Float { default = 1 }
    ["seed"] = new Int { default = 0 }
    ["kind"] = new Menu { default = "perlin"; items { "perlin"; "simplex"; "worley" } }
  }
}
```

- **One source, many outputs.** A generator (`cargo xtask gen`) evaluates the Pkl definitions and produces:
  - typed Rust parameter structs and the operator registry
  - the Luau definition files for luau-lsp
  - the reference docs
- **Generated code is committed.** A normal `cargo build` doesn't need Pkl installed, and CI checks the generated
  files are up to date.
- **Pkl validates the definitions.** Schemas and constraints (`default` within `min..max`, menu defaults present in
  `items`, unique names) catch mistakes before any Rust is generated.
- Pkl is evaluated with the pinned `pkl` CLI, through [rpkl](https://crates.io/crates/rpkl) in the generator.
- Custom parameters on components are project data, so they live in KDL, not Pkl.

## Architecture

Planned workspace:

- `crates/graph`: the network model (operators, paths, wires, parameters, components), dirty tracking, the cook
  scheduler and the clock. No GPU code.
- `crates/gpu`: the wgpu context, texture and buffer pools, WGSL helpers and readback.
- `crates/ops`: the built-in operators of every family.
- `crates/project`: reading and writing the KDL project format, the file watcher and reload, and `check`.
- `defs/`: the Pkl operator definitions. `xtask` generates the Rust registry, Luau definitions and docs from them.
- `crates/script`: the Luau runtime (mlua), expression compilation and evaluation, and generation of the definition
  files and sourcemap.
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
4. **Editor**: the upstream Rune change to adopt an engine-created device, then a Rune app with the network editor,
   texture and channel viewers, and a parameter inspector. Projects save to KDL.
5. **Data and expressions**: tables and text, Luau expressions and binds, Channel ↔ Texture ↔ Data converters, and
   the script editor with luau-lsp IntelliSense.
6. **3D**: the Geometry family, Shader family, Render, Camera, Light, and instancing.
7. **Output and I/O**: Window components, perform mode, OSC and MIDI in and out, audio in and out.
