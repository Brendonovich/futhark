# Futhark plan

Futhark is a realtime, node-based visual programming environment in the spirit of TouchDesigner, built on
[Rune](https://github.com/anomalyco/rune) for UI and [wgpu](https://wgpu.rs) for all GPU work. Background research is
in `docs/research/touchdesigner.md`.

## Principles

- **Modern only.** No legacy features from TouchDesigner. That means no CPU surface geometry (SOPs), no Tscript, no
  OpenGL-era GLSL, and no "export" parameter mode (binds and expressions cover it).
- **Plain names.** Families are called Texture, Channel, Geometry, Data, Shader and Component, not TOP, CHOP and so on.
- **wgpu for every GPU operation**: texture ops, geometry, rendering and readback. Shaders are WGSL.
- **Client and server.** The engine runs in a server process, `futharkd`, that owns the project, the clock and the
  GPU. Everything else is a client of one protocol: the CLI, the desktop app, agents (MCP) and, later, other
  machines. Nothing is possible in one client that isn't possible through the protocol.
- **The desktop app is a special local client.** It edits through the protocol like any other client, but it sees
  operator viewers through shared GPU surfaces (IOSurfaces on macOS), so pictures reach the screen without a copy.
- **The engine is headless.** The graph, cooking and operators know nothing about the UI, and also run in-process
  without a server: offline renders, `check`, tests.

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
  - They are generated from the operator registry, so they can't drift from the code.
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

## Client and server

The architecture follows [Isocast](https://github.com/R44VC0RP/isocast) (`docs/shared-frames.md` and
`docs/remote-engines.md` there).

### Processes

- **`futharkd`** (`crates/server`) is the only process with engine state: the open project, the network, the clock,
  the GPU device and the frame loop. The engine runs on one thread that owns it; connection threads send it jobs.
- **Clients** find the local server at a socket path: `$FUTHARK_SOCKET`, otherwise `futharkd.sock` in the user's
  application support directory on macOS or `$XDG_RUNTIME_DIR/futhark/` on Linux. `connect_or_launch` starts the
  server detached if nothing answers, so the first client to need it brings it up.
- **The server outlives its clients.** Closing the desktop app leaves the project running (an installation keeps
  playing). It stops on `shutdown`, on a signal, or, with `--exit-on-stdin-close`, when its parent goes away (tests).
- **Not everything needs a server.** `futhark check`, offline `cook` and renders, and tests load projects with the
  engine in-process.

### Protocol

- **JSON-RPC 2.0, one JSON object per line**, over a Unix socket (owner-only). This is the one place JSON appears:
  it is a wire format for tools and agents, not a file format.
- **Methods**: `hello` (protocol version and capabilities), `status`, `create`, `open`, `save`, `close`, `types`
  (every operator type's schema), `get` (a snapshot of the network with its revision), `edit`, `sample` (an
  operator's current output), `watch`, `shared_frames` and `shutdown`.
- **Edits are batches.** `edit` takes a list (add, remove, rename, set a parameter, connect, disconnect, move) and an
  optional expected revision. The batch applies completely or not at all, and a stale revision fails with
  `WRONG_STATE`, so clients never overwrite each other's changes silently.
- **Events.** `watch` turns a connection into an event stream: `network_changed { revision }`, project opened,
  closed and saved. Events carry revisions, not deltas; clients `get` again.
- **Errors are for agents**: a stable code plus a message naming what was wrong and what would be valid.
- **Versioning**: `hello` carries a protocol number. New methods are additive; an unknown method is
  `METHOD_NOT_FOUND`.
- **Streams take over a connection.** `watch` and `shared_frames` reply once, then the connection carries only that
  stream. Clients open separate connections for calls, events and each viewer.

### Shared frames

How the desktop app sees viewers (`crates/surfaces`):

- **The server owns a ring of IOSurfaces per viewer** (2–8 slots, `Bgra8Unorm`, sRGB-tagged), sized to the viewer's
  maximum size with the picture's aspect. It renders the operator's texture into the next free slot with wgpu (the
  IOSurface is imported as a Metal texture), and announces it only after the GPU has finished.
- **Surfaces cross processes once, as Mach send rights.** The server registers a per-process Mach service and offers
  each ring under a one-time token. The client fetches the ring by token, then every frame is a small message naming
  a slot: `frame { generation, slot }`. The client sends `release { generation, slot }` when it no longer shows it.
- **A slow client only stalls its own viewer.** When every slot is held the server skips that viewer's frames.
- **The client shows a slot without copying**: Rune wraps the IOSurface (`PixelBuffer::from_iosurface`) and draws it
  in a `NativeSurface`.
- **Other platforms and remote clients** get encoded frames (H.264, or still images to start) instead. D3D12 shared
  handles on Windows can come later.

### Remote clients (later)

Other machines connect over TCP with mutual TLS and pinned keys, paired once with a short code shown on the server
(Isocast's design). Local-only methods (`shutdown`, opening arbitrary paths) are refused to them.

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

Rust is the single source of truth for operator types. Each operator type is a Rust type whose parameters are a
struct with a derive:

```rust
#[derive(Params)]
pub struct LfoParams {
    pub wave: Wave, // a #[derive(Menu)] enum
    /// Cycles per second.
    #[param(default = 1.0, min = 0.0, soft_max = 10.0)]
    pub frequency: f64,
    #[param(default = "chan1")]
    pub name: String,
}

#[derive(Default)]
pub struct Lfo;

impl ChannelOp for Lfo {
    type Params = LfoParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/lfo",
        label: "LFO",
        doc: "A repeating wave over time, one sample per frame.",
        inputs: 0..=0,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, params: &LfoParams) -> Result<Channels, CookError> { ... }
}
```

Each family has its own operator trait (`ChannelOp` now; `TextureOp` and the others as they arrive) with a context
suited to it. The registry erases the family so the engine cooks every operator the same way.

- **The derive produces a static parameter schema** from the struct: name, type, default, range, menu items, label,
  and docs taken from doc comments. The engine, the editor's inspector, the KDL reader and writer, and Luau all read
  that schema. Nothing is declared twice.
- **The registry is an explicit list** of operator types in `crates/ops`, with no link-time registration magic.
- **Generated artifacts come from the running registry.** `futhark defs` writes the Luau definition files for
  luau-lsp, and `futhark docs` writes the operator reference. Both are rebuilt by the editor as needed, and CI checks
  that committed copies are current.
- **Schemas are validated in tests.** Every registered operator is checked for defaults within range, unique names
  and valid menu defaults.
- Custom parameters on components are project data, so they live in KDL with the same parameter types.

## Architecture

Workspace:

- `crates/graph`: the network model (operators, paths, wires, parameters, components), dirty tracking, the cook
  scheduler and the clock.
- `crates/macros`: the `Params` and `Menu` derives.
- `crates/gpu`: the wgpu context, texture pools, WGSL helpers and readback.
- `crates/ops`: the built-in operators of every family.
- `crates/project`: reading and writing the KDL project format, the file watcher and reload.
- `crates/surfaces`: shared GPU surfaces: IOSurfaces handed over as Mach ports on macOS.
- `crates/protocol`: the protocol's types: methods, results, events, errors.
- `crates/server`: `futharkd`, the server.
- `crates/client`: the client library used by the CLI and the desktop app.
- `crates/cli`: the `futhark` binary: a client of the server, plus offline `check`, `cook` and renders.
- `crates/script`: the Luau runtime (mlua), expression compilation and evaluation, and generation of the definition
  files and sourcemap.
- `apps/desktop`: the Rune desktop app, a client of the server, including:
  - a network editor (a canvas with pan and zoom, nodes, wires)
  - a viewer for each family (textures through shared frames)
  - a parameter inspector
  - output windows and perform mode

Rune: pin `anomalyco/rune` to an exact `rev` on `dev`.

## Milestones

1. **Skeleton** (done): the workspace, the engine-owned headless wgpu context, the `futhark` CLI, and CI that runs tests
   offline and headless.
2. **Graph core** (done): operators, wires, the `Params` derive, pull cooking and dirty propagation, and reading and writing
   KDL. Channel operators (Constant, LFO, Noise, Math) are the first test bed.
3. **Server and textures**: `futharkd`, the protocol and the client library, with the CLI as a client. Texture
   operators (Constant, Ramp, Noise, Transform, Level, Composite, Shader, File In) with golden-image tests. Shared
   frames over IOSurfaces.
4. **Desktop app**: a Rune client with the network editor, texture viewers through shared frames, channel viewers,
   and a parameter inspector.
5. **Data and expressions**: tables and text, Luau expressions and binds, Channel ↔ Texture ↔ Data converters, and
   the script editor with luau-lsp IntelliSense.
6. **3D**: the Geometry family, Shader family, Render, Camera, Light, and instancing.
7. **Output and I/O**: Window components, perform mode, OSC and MIDI in and out, audio in and out.
