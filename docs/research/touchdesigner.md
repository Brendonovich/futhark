# TouchDesigner: how it works

Research notes for Futhark. Sources are the docs.derivative.ca wiki (build 2025.30000), read November 2026.

## Operator families

Everything is an operator (OP). There are seven built-in families. Within each family, generators have no inputs and
filters have one or more.

| Family | Runs on | Data |
| --- | --- | --- |
| **TOP** (Texture) | GPU | One 2D texture (or 3D / 2D-array). Per-OP pixel format (8-bit fixed by default, 16/32-bit float, 1–4 channels) and resolution. |
| **CHOP** (Channel) | CPU | Named channels of float32 samples sharing one sample rate and one start/end interval, plus extend conditions for reads outside the interval. |
| **POP** (Point, new) | GPU | Point / vertex / primitive lists with typed attributes (P, N, Color, Tex, custom), groups as bit flags. Unchanged attributes are shared with upstream by reference. Custom logic is GLSL compute. |
| **SOP** (Surface, legacy) | CPU | Houdini-style geometry: points with attributes, primitives (polys, NURBS, Bezier, meshes, metaballs). |
| **DAT** (Data) | CPU | Free text (scripts, GLSL, JSON) or a table of string cells. |
| **MAT** (Material) | GPU | A shader (Phong, PBR, Constant, GLSL) applied to geometry. |
| **COMP** (Component) | — | A container holding a network of OPs. 3D objects (Geometry, Camera, Light), 2D panels (Container, Button, Slider), and misc (Base, Time, Replicator, Window, Engine). |

## Type system

- **Wires connect operators of the same family only**, left to right. A wire does not copy data; it tells the
  downstream OP where to pull from.
- **Hierarchy wires** run top to bottom between 3D-object or panel COMPs and set parent/child transforms or layout.
- **COMPs get connectors** from In/Out OPs inside them (In TOP, Out CHOP, …), ordered by name.
- **Crossing families** has three mechanisms:
  1. **Converter OPs**: TOP→CHOP, CHOP→TOP, CHOP→DAT, DAT→CHOP, SOP→CHOP, CHOP→SOP, and to/from POP for all of them.
     They define layouts, e.g. CHOP→TOP puts samples on X and channel groups (R/RG/RGB/RGBA) on Y, as float32.
  2. **References**: path-typed parameters that point at an OP of any family (a Geometry COMP's Material, the
     Select TOP, instancing sources). These draw as dashed lines.
  3. **Parameter bindings**: expressions, exports and binds (see Parameters).
- GPU→CPU transfers (TOP→CHOP, POP→DAT) choose **Immediate**, which stalls the frame, or **Next frame**, which
  doesn't stall but is a frame late.

## Parameters

- Types: int/float in groups of 1–4 (XY, XYZ, RGBA), toggle, menu, string, OP path, pulse, and Python object.
- Each parameter has **four modes**, each with its own stored value:
  - **Constant**
  - **Expression**: Python. Common forms are compiled by a fast built-in evaluator ("Optimized") that skips the
    interpreter. Results are cached.
  - **Export**: a CHOP channel or DAT cell pushes into the parameter.
  - **Bind**: a two-way link to a master (another parameter, a table cell, a channel).
- **Custom parameters** live on COMPs and Script OPs, on custom pages. They are capitalised (`Speed`), while
  built-in ones are lowercase. This is how components expose an API (`parent().par.Speed`).
- CHOP parameters can be evaluated per channel (`me.chanIndex`).

## Cooking (evaluation)

- **Pull-based and lazy.** Changing something only marks an OP dirty; it cooks when something requests its data.
- Things that pull every frame: visible viewers, displayed panels and windows, and output OPs (OSC Out, NDI Out,
  Audio Device Out, Movie File Out…).
- **An OP cooks only if it has both a request and a reason.**
  - Requests come from downstream wires, parameter references, viewers, export targets, or `op.cook()`.
  - Reasons are an input or referenced OP cooking, a parameter change, an expression result possibly changing, being
    time-dependent, or user interaction (panels).
  - The OP asks its inputs first. If none of them cooked and it has no other reason, it skips its own cook.
- Some OPs always cook when asked (Render TOP, *In OPs), because checking for changes would cost more than cooking.
- Cook control:
  - Null CHOP "Selective" only propagates when values or names actually change, which cuts downstream recooks.
  - A COMP's Cooking flag freezes its whole network.
- **Dependency tracking is automatic** for wires, references, and anything read during an expression or Script OP
  cook. Plain Python state needs `tdu.Dependency` to be tracked.

## Time

- **Frame-based global clock.** `project.cookRate` (default 60) is the target FPS and the default CHOP sample rate.
- **Realtime flag**: on means drop frames to keep up with wall time; off means process every frame, for offline
  rendering.
- **Component time**: any COMP can have its own Time COMP clock (play, rate, range, loop, tempo).
- **CHOP time slicing**: each cook, a time-sliced CHOP produces exactly the samples since the last cook. That may be
  more than one frame's worth when frames drop, so audio stays continuous. Slices never overlap or leave gaps, all
  time-sliced CHOPs share the same interval, and a slice is capped at 200 ms.

## Rendering

- **Graphics API**: Vulkan since 2022.20000, when OpenGL was removed. macOS runs on MoltenVK over Metal.
- **TOP cooks are asynchronous.** The CPU records GPU commands and moves on, so CPU cook time is not GPU time.
- **Resolution and format** inherit from the input by default ("Use Input"), with multipliers (½×, 2×), fit,
  limit, or a custom size.
- **GLSL TOP**: fragment shader over a full-screen quad, or compute mode. GLSL 4.60. The engine injects helpers
  (`TDOutputSwizzle`, sampler arrays for inputs, uniforms from parameters).
- **3D pipeline**: a Render TOP takes Camera COMP(s), Light COMPs and Geometry COMPs. Each Geometry COMP holds a POP
  or SOP network with a render flag and a MAT. The result is a TOP, so 3D renders are just another image in the
  graph.
- **Multi-pass**: Render Pass TOP reuses the color/depth buffers of a previous render. Multiple render targets come
  from GLSL MATs.
- **Instancing**: a Geometry COMP takes per-instance attributes (translate, rotate, scale, color, texture index)
  from any family. CHOP channels, DAT columns, POP/SOP point attributes and TOP pixels all map to attributes.

## Runtime and threading

- Cooking and Python run on a **single main thread**. Vulkan was adopted partly to enable threaded cooking later.
- I/O runs on internal threads: movie decode (disk → decode → upload), video devices, audio.
- **Engine COMP** runs a `.tox` in a separate process (TouchEngine). It exchanges only TOP, CHOP and DAT data and
  custom parameters, buffered so the two sides can cook at different rates.
- **Profiling**:
  - Performance Monitor: a one-frame snapshot of cook times and order.
  - Per-OP info popup: cook time and cook count.
  - Probe: live CPU/GPU time and memory.
  - Perform CHOP: fps and dropped frames.

## Architecture and scripting

- **Hierarchy**: COMPs nest, addressed by path (`/project1/container1/text1`, relative `../x`).
  - `.toe`: whole project
  - `.tox`: one reusable component
- **Clones**: a COMP whose Clone Master is set mirrors the master's internals. Inputs, top-level parameter values
  and "immune" children can differ.
- **Replicator COMP**: a for-loop over OPs. It makes one copy of a master per table row and only recreates rows that
  changed.
- **Python**:
  - The `td` module provides `me`, `op()`, `parent()`, `absTime` and global shortcuts (`op.Name`).
  - Each OP type is a Python class.
  - **Extensions** attach Python classes to COMPs (`comp.ext.Name`, or promoted methods called directly on the COMP).
- **Callback DATs**:
  - Execute DAT (`onStart`, `onFrameStart`, `onFrameEnd`, …)
  - CHOP / DAT / Parameter / Panel / OP Execute DATs
  - Script TOP/CHOP/SOP/DAT/POP, which cook from Python (NumPy) and record their dependencies automatically.
- **Native plugins**: C++ custom OPs for TOP, CHOP, POP, SOP and DAT.

## UI

- **Network editor**: the main pane. A pannable, zoomable graph that you dive into COMPs to edit.
- **Viewers**: each family has one (image, channel graph, 3D, table/text, panel). They appear inside nodes, in panes
  or in floating windows. An active viewer accepts interaction.
- **Panel COMPs**: Container, Button, Slider, Field, List, Table and others. This is how custom UIs and control
  surfaces are built, and panel values feed back into the graph as CHOPs.
- **Window COMP**: an OS window showing any panel or viewer on a chosen monitor, with vsync options.
- **Perform mode**: hides the editor and shows only the output window. TouchPlayer is perform-only.

## I/O

External data comes in as the family that fits it:

| Data | Family | Examples |
| --- | --- | --- |
| Continuous control | CHOPs | OSC In, MIDI In, DMX/Art-Net/sACN In, Audio Device In (always time-sliced) |
| Discrete messages | DATs | OSC In DAT, MIDI Event DAT, one row per message |
| Video | TOPs | Movie File In, Video Device In, NDI In, Syphon/Spout (GPU texture sharing), Touch In/Out (TD-to-TD) |
| Outputs | Out OPs | Out OPs cook every frame and act as pull roots |
