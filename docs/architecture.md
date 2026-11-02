# Architecture (v7, 2026-10-30)

Status: **stack confirmed 2026-10-28** (Vello, Parley, native winit+wgpu+egui, Taffy, rquickjs); file format v1 decided; `doc` and `format` crates built in Stage 2; `text`, `layout`,
`bench` and `xtask` built in Stage 3; `index`, `render`, `render-cpu` and `render-vello` built in
Stage 4; `editor` and `apps/desktop` (the canvas app) built in Stage 5; `script`, `agent`, `apps/cli`
and `apps/mcp` (the agent API) built in Stage 6. It reflects the
measurements in `docs/spike-report.md` and the stage reports (`docs/stage2-report.md` to
`docs/stage6-report.md`), and the borrowing decision in
`docs/figcraft-evaluation.md`. Sections for later stages are still drafts.

## 1. Goals and constraints

- AI-native design canvas. Agents and humans use the same transaction API, and every change is
  undoable and scriptable.
- Performance targets (brief):
  - 60 fps pan/zoom at 100k nodes.
  - Edit → frame < 16 ms.
  - Re-layout of a 10k subtree < 50 ms.
  - Open a 50 MB doc in < 2 s using < 1 GB.
  - Hit-test < 1 ms.
  - Internal gate on the reference desktop: **p95 ≤ 8 ms**.
- One Rust core shared by the desktop app, a headless CLI and MCP server, and later WASM.
- Clean room: public specs and open-source libraries only. No formats or code from commercial
  tools.
- MIT OR Apache-2.0. Dependencies must be permissive; this is enforced by `cargo deny`.

## 2. Workspace and crate boundaries

```
crates/
  geom        points, rects, affine, bezier helpers, smoothed corners   (borrow: FigCraft geom)
  color       colour spaces, blending, paint types                      (borrow: FigCraft color)
  pathops     boolean ops, outline/stroke expansion                     (borrow: FigCraft pathops)
  doc         node arena, properties, ids, ops, transactions, undo, op log
  layout      LayoutEngine trait + Taffy implementation + design-semantics mapping
  text        Parley wrapper: font db, shaping cache, rich spans, cursor/selection geometry
  index       spatial index (rstar): world-space visual bounds, culling and hit-test queries
  render      display list, scene building, fragments, zoom bands, LOD, image cache, Renderer
              trait, the reference CPU blur
  render-cpu    vello_cpu: export, headless rendering, golden images
  render-vello  GPU renderer (wgpu + Vello) with our own effect compute passes
  editor      UI-free editor core: session, selection and scope, hit-testing, tools, transforms,
              snapping, layers model, property specs, clipboard payload, autosave journal
  script      rquickjs runtime, sandbox limits, JS API bindings over `doc` commands
  agent       request/response protocol (the command registry), headless host, IPC, guides
  format      JSON (de)serialisation, schema, migrations, asset store
  export      SVG/PDF writers over the display list, PNG/JPEG/WebP, SVG import (usvg)
apps/
  desktop     winit + wgpu + egui shell over `editor` (UI only; no document logic)
  cli         `canvas`: render, validate, run, state, schema, guide
  mcp         `canvas-mcp`: MCP server (stdio) over `agent`, forwarding to the app's socket
xtask         layering check, bench gate, license report
```

**Rules:**

- Crates are layered L0 → L4 (geom/color → pathops → doc/text/layout/index → render/
  format/editor → script/agent → apps).
- `cargo xtask layers` fails the build on a dependency that isn't strictly downward. The
  layers it enforces today: `doc` < `text` < `layout` < `index` < `format` < `render` <
  `render-cpu` < `render-vello` < `export` < `editor` < `script` < `agent` < `bench`,
  `desktop`, `cli`, `mcp`, `xtask`. Every crate
  must be listed, so a new crate has to be placed deliberately. This idea is borrowed from FigCraft.
- No UI or windowing types below `apps/`.
- `core` = everything in `crates/`. It must compile for `wasm32-unknown-unknown`, except
  `render-vello`'s surface creation and platform font discovery, which sit behind features.
- Clippy denies `unwrap`/`expect`/`panic` in library crates. `unsafe` is forbidden outside a short
  allowlist. A panic guard around each command rolls back the transaction. All three conventions
  are borrowed from FigCraft.

## 3. Document model (`doc`) — built in Stage 2

- **Storage:** a `slotmap` arena of nodes. Each node also has a stable random **128-bit id**
  (`NodeId`, 22 base62 characters), used in files, ops and the agent API. An `FxHashMap<NodeId, NodeKey>`
  maps between them. A component → instances index makes "who uses this component" O(1).
- **Node kinds:** page, frame (optional auto-layout), group, rectangle, ellipse (arcs), polygon/star,
  line, path (SVG path data), text (rich spans), image, component, instance (overrides), note.
  A `boolean` kind is still to come (it needs pathops).
- **Properties:**
  - One `CommonProps` struct for every node: transform, size and sizing modes with min/max,
    constraints, radius, fills, strokes, effects, opacity, blend, visible, locked, layout-child
    props, and variable bindings.
  - Kind-specific data lives in `NodeKind`.
  - Updates address one top-level property via `PropKey`/`PropValue`, generated by one macro so
    the struct and the two enums can't drift. `Kind` replaces the kind payload but can't change
    the node type.
- **Tree:** parent pointer plus an ordered `Vec` of children (z-order). Moves are index-based.
  Fractional indices will be added when CRDT sync is built; they don't need to be in the file
  format.
- **Validation** runs on every op and on load:
  - container and page rules, no cycles,
  - instances point at components, no component contains itself (also checked through other
    components),
  - assets and variables exist,
  - path data parses, numbers are finite.
- **Components (Stage 7a):** instances store overrides as (id path, `PropValue`) and stay
  childless in the file. Their content is **derived state**: the layout deriver expands each
  instance into **virtual nodes** in the same arena (`Node.virt = Some(Virtual { instance,
  source })`), with deterministic ids (`virtual_id(scope, source)`), so render, index,
  hit-testing and the editor treat them as ordinary nodes.
  - Virtual nodes are never in the undo log or the file (`to_tree`/`to_data` skip them); ops
    on them are refused (`DocError::VirtualNode`).
  - `Tx::update` on a virtual node becomes `set_override(instance, path, value)`; on a real
    instance, `fills`/`strokes`/`radius` become `[]` overrides (the content root's look).
    `update_raw` bypasses both.
  - `Document::set_expansion` diffs new content into the existing virtual nodes in place, so a
    component edit reports only the properties that changed per instance.
  - Commands: `create_component`, `detach` (keeps the instance id), `reset_overrides`,
    `remap_ids` for copy/paste (instances and override paths follow copied components).
- **Variables and modes (Stage 7a):** `modes` on page/frame/component/instance choose a mode per
  collection; a node uses the nearest choice (crossing into instances). Bound values are written
  into real nodes by the deriver as ordinary ops in the same transaction, so the file holds what
  is drawn.

## 4. Transactions, ops and undo — built in Stage 2

- **Single mutation entry point:** `Store::transact(meta, |tx| …)`. Ops apply immediately, so
  reads inside the transaction see earlier writes. If the closure returns an error, every applied
  op is rolled back.
- **Primitive ops** (`Op`, serialisable for the op log):
  - `Insert{parent, index, node}`, `Delete{id}` (captures the subtree)
  - `Update{id, prop, value}`, `Move{id, parent, index}`
  - `SetVariables`, `PutAsset`
- Applying an op returns its inverse. Undo applies the inverses in reverse; redo re-applies the
  forward ops.
- A **coalesce key** merges consecutive transactions into one undo step, e.g. the steps of a drag.
- Each transaction returns a `ChangeSet` (inserted / deleted / updated(prop) / moved / variables /
  assets). That is the input for layout, index and render invalidation.
- **`on_commit` observers** receive every commit: origin (user, agent, script, undo, redo,
  system), label, ops and changes. They feed autosave and, later, sync.
- **Commands:** `doc::commands` adds agent-shaped helpers on `Tx`:
  - `insert_json`, `update_json`, `replace_json`, `move_node`, `copy`, `delete`, `get`, `find`,
    `set_variables`, `put_asset`.
  - JSON input is checked against the schema's field lists, so a misspelt property is an error,
    not silently ignored.
  - The named, schema-described command registry for the CLI and MCP (`agent::Request`, built
    in Stage 6) sits on top of these.

## 5. Incremental pipeline

```
op committed → dirty sets → layout (dirty subtrees) → text (re-shape changed runs)
             → index (update moved bounds) → render (invalidate fragments) → frame
```

### Instances and bindings, built in Stage 7a (`layout::instances`, `layout::bindings`)

The deriver runs three steps per transaction (and on undo/redo through `observe`, whose derived
changes are merged into the result):
1. **Bindings** of real nodes whose variables, bindings, modes or ancestry changed are resolved
   and written as ops.
2. **Expansion** of dirty instances (a component edit dirties every instance of it, upward
   through nesting). Content is built in parallel for many instances (it only reads the
   document), then diffed into the document in order. Overrides apply before bindings;
   constraints are applied at the instance's size; nested instances get their own overrides,
   then the outer ones.
3. **Settle**: the usual layout passes, which also lay out auto-layout and text inside virtual
   content; a result that changes a component's size re-expands its instances.

A property test checks that after random edits the expansions equal a fresh load's, a full pass
from scratch changes no value, and undo/redo restore every expansion
(`layout/tests/proptest_instances.rs`); another that an instance renders pixel-identical to its
detached copy (`render-cpu/tests/instances.rs`).

### Layout, built in Stage 3 (`crates/layout`)

- **Results live in the document.** A `Deriver` hook on `Store` runs at the end of every
  transaction:
  - it lays out what the `ChangeSet` touched;
  - it writes changed `x`/`y`/`width`/`height` back as ops in the same transaction, so they share
    its undo step and files and agents see real positions.
- **Undo and redo** restore laid-out values exactly. They only resync the engine's caches
  (`Deriver::observe`) and write nothing.
- **Passes until settled.** One pass can trigger another (a hugging frame grows, so its
  constrained children move), so passes repeat until nothing changes.
  - Values are compared with a tolerance of one rounding step (1/100 px) before writing, so a
    settled document produces no ops.
  - Typical edits settle in 2–3 passes; there is a cap of 32.
- **On open, stored values are trusted.** `LayoutDeriver::prepare` warms the caches (parallel
  text measuring, Taffy mirrors) without writing. `derive_all` relays out everything explicitly
  (CLI, font changes).
- **Auto layout runs on Taffy** behind the `LayoutEngine` trait.
  - Each auto-layout root (a container with auto layout whose parent has none) gets a mirror tree
    of its participating nodes. Mirrors are built lazily, on the first edit that touches them.
  - Property edits restyle single mirror nodes, and Taffy's cache redoes only the dirty path.
    Text edits mark one leaf dirty.
  - Structural edits (insert, delete, move, auto layout added or removed) rebuild that root's
    mirror. That costs about 27 ms for a 10k-node root; making it incremental is a known
    follow-up.
- **Design semantics** are mapped onto Taffy styles in one module (`semantics.rs`):
  - direction, gap, padding, alignment, wrap and grid tracks;
  - fixed, hug and fill on each axis (fill on the main axis is grow with basis 0; `grow` is a
    weighted fill);
  - min/max, absolute children, hidden children leaving the flow;
  - text measured through the text engine, respecting auto-resize;
  - a group's size always comes from its children.
- **Outside auto layout:**
  - constraints reposition children when a plain frame is resized (left, right, left+right,
    centre, scale);
  - groups fit the union of their children and rebase them so nothing moves on the canvas;
  - text auto-resize sets width and/or height.
- **Not handled yet:** rotation inside auto-layout flow (ignored), constraints on absolute
  children of auto-layout frames, and instance contents (Stage 7).

### Text, built in Stage 3 (`crates/text`)

- **Fonts:** Parley + fontique. Bundled Inter (OFL-1.1) is always registered, and the family name
  "Inter" always means the bundled face, so default text measures the same on every machine.
  System fonts are added for other families and fallback; tests use bundled fonts only.
- **Two caches,** both keyed by a hash of (spans, style, alignment, wrap width):
  - **sizes** (width, height, first baseline): small and kept for every text measured. This is
    all layout needs.
  - **full layouts** (glyphs, lines): only for text being drawn or edited, capped and evicted by
    generation.
  - Keeping glyphs for every text at open cost 650 MB on the 50 MB document; keeping sizes only
    cost 165 MB.
- **At open,** text is measured in parallel (rayon, one font/layout context per thread) at the
  widths layout will ask for.
- **Editing geometry** for Stage 5: hit-test to caret, caret rectangle, selection rectangles.
- Missing families and right-to-left runs are reported per text.

- **Spatial index:**
  - rstar R-tree over visual bounds (including shadow/blur outsets), bulk-loaded on open and
    updated incrementally on move.
  - Updates walk top-down from each changed subtree (Stage 7c): nodes on the way to a changed
    descendant are recomputed, subtrees whose placement only translated are shifted in place
    (clips recomputed from stored clip boxes), and inserted, moved or re-shown subtrees are
    rebuilt. When the touched subtrees hold over 1,024 nodes and an eighth of the page, the
    page's R-tree is bulk-reloaded once instead of updated item by item.
  - Used for culling, hit-testing, marquee and snapping candidates.
- **Render:**
  - A display list is built per **fragment**. Today a fragment is an artboard or top-level frame;
    Stage 2 will split large ones into finer subtrees.
  - Each fragment is encoded once per **zoom band** (six bands, each spanning 2×: edges at
    0.06, 0.125, 0.25, 0.5 and 1), with LOD rules for the band's lowest zoom:
    - skip items < 0.5 px,
    - text drawn as greeked bars below a threshold,
    - simple shapes substituted.
  - Each frame appends the cached fragments for visible groups with the camera transform.
  - An edit invalidates only the fragments that contain the edited node.
  - A per-frame encode budget, plus keeping the nearest band's list until the new one is ready,
    removes the zoom spikes and prevents the buffer-limit crash seen without culling. The budget
    covers text shaping too: fragments with an older list are shaped and built a few at a time,
    and at least one is built per frame so a band change always completes.

## 6. Rendering — built in Stage 4

**Pipeline.** `doc` → `index` (culling) → `render::Scene` (fragments and their display lists) →
a `Frame` → a `Renderer` backend.

- **Display list.** A flat list of commands (`PushLayer`/`Pop`, `Fill`, `Stroke`, `Glyphs`,
  `Image`, `BlurredRRect`, `PushEffect`/`PopEffect`), each with its transform. Paths, brushes,
  glyph runs and images live in shared `Arc` tables, so fragments are cheap to reuse. Both
  backends consume the same list, and so do the SVG and PDF writers (Stage 7b).
- **Fragments.** A fragment is a page's top-level node. A subtree over 2,000 nodes is split into
  child fragments that carry their ancestors' clips. A root with opacity, blend or effects isn't
  split, because those wrap its whole content. Fragment ids are global, so backends can cache by
  id across scenes.
  - Lists are cached per zoom band (six, each 2× wide: ≥ 1, 0.5–1, 0.25–0.5, 0.125–0.25,
    0.06–0.125, < 0.06), each with its own version. Until a band's list is built, the nearest
    band's list is drawn.
  - LOD is decided at the band's lower edge, so a band spanning 2× bounds the error: greeked text
    is under 8 px on screen, and from 100 % on only text under 4 px is greeked. Full effects
    (offscreen shadows, layer and background blurs) are drawn from 0.25; below that, only the
    blurred-rect shadow.
  - `Scene::apply(doc, &ChangeSet)` marks fragments dirty, or replans when structure changes.
  - When 32 or more visible fragments must be built in one frame (e.g. every instance of an
    edited component), they are built in parallel after text and images are prefetched: the
    builders only read the caches (`Caches::Read`), and a fragment that still misses text is
    built on the main thread instead.
  - An instance draws only its children (the content root carries its look); its layer
    properties (opacity, blend, effects) wrap them.
  - **LOD:** items under 0.5 px on screen are skipped, text under 4 px is greeked, and blurs or
    shadows with σ under 0.5 px are dropped.
- **Colour.** Blending and gradient interpolation use sRGB-encoded values, as CSS and browsers
  do. Targets are `Rgba8Unorm`.
- **`Renderer` trait:** `render(&Frame, &mut Target) -> RenderStats`. Implementations:
  - `render-cpu`: vello_cpu, multithreaded. It is the reference renderer and is used for export
    (`render_png`).
  - `render-vello`: Vello 0.11 on wgpu, headless or on a caller's device, with readback for tests
    and screenshots.
- **Effects.** σ = blur / 2 (the CSS convention) for shadows and blurs.
  - Drop shadows on an opaque box (rectangle-like node, opaque normal fill, strokes inside,
    children clipped or none) use the blurred-rect primitive.
  - Every other drop shadow is the blurred silhouette of the drawn content: an offscreen render,
    then alpha → spread (dilation) → offset → blur → tint.
  - Inner shadows invert, erode by the spread, offset, blur and mask with the shape.
  - Layer blur blurs the node's rendered content. Background blur blurs a snapshot of what is
    already drawn under the node.
  - The blur itself is `render::blur`: separable Gaussian, ×2 downsampling per octave while
    σ > 8 px, then upsampling. The GPU passes (`render-vello/src/effects.wgsl`) are step-for-step
    the same, so the two backends agree within rounding.
- **GPU effect batching (`render-vello`).**
  - Offscreen regions are deferred: a placeholder image is drawn and the region is queued.
  - Before a scene that uses them is rendered, the ready regions are shelf-packed into one atlas
    (slots aligned to the deepest octave) and rendered with a single Vello call.
  - Their effects then run as one compute submission. Each step is a few dispatches over all
    regions, grouped by power-of-two size class: 12 dispatches for 100 effects, not 450.
  - Placeholders are pointed at their results with `override_image`, using sub-rectangle origins.
- **Run cache (`render-vello`).** Effect-free runs of a fragment are encoded into a
  `vello::Scene` once per (fragment, band, version) and appended with the camera transform each
  frame.
  - Runs of every band are kept, least recently drawn evicted beyond 128 MB of encoding, so
    zooming back to a band doesn't re-encode the view. Superseded versions go after 2 frames.
  - A run can close a layer an earlier run opened (an effect splits the list between them). Such
    pops go to the frame's scene directly, since Vello ignores a pop with nothing open in the
    run's own scene.
  - The frame's top-level scene is kept between frames, so its buffers (several MB for a
    whole-page view) keep their capacity.
- **Patched Vello** (`third_party/`, through `[patch.crates-io]`; see its README):
  - `vello_encoding` keeps 4,096 gradient ramps instead of 64, and counts a revision when they
    change;
  - `vello` reuses its scene-packing buffer and keeps the ramp texture between frames,
    re-uploading it only on a new revision.
- **Vello 0.11 workaround.** Vello's image atlas can drop the pixels of override images and of
  images not drawn in the previous render. So override placeholders are marked dirty before every
  render, and so are user images that weren't used in the render just before. Vello issue to watch;
  remove the workaround when an update fixes it.
- **Images.** Decoded on rayon into a byte-capped cache keyed by asset hash, with a 2× mip chain
  for low zoom. A neutral placeholder is drawn until an image is ready.
  - Headless scenes (screenshots, export, tests) decode before drawing.
  - The app gives its scene a waker (Stage 7b): decodes then run as rayon jobs, frames never
    wait, and fragments drawn with a placeholder are noted per asset. A finished decode wakes
    the event loop; the next frame takes it in (at most 8 per frame, so a burst's GPU uploads
    spread out) and rebuilds only those fragments. Decodes run on their own pool (a third of
    the cores), so the frame's parallel work never queues behind them.
    `Scene::frame_complete` waits for pending decodes (screenshots of an interactive session).

### Export and import, built in Stage 7b (`crates/export`)

- **Shared walk.** `export::collect` builds each node's display list with an *exact* builder (no
  LOD, greeking or tiny-item skipping, full effects, full-resolution images) and places it in
  export space: world units, translated to the content's integer bounds.
- **Raster:** `render-cpu` over those lists, then PNG, JPEG (on white, quality setting) or WebP
  (lossless only: the cached `image` crate has no lossy encoder). Up to 16,384 px per side.
- **SVG writer:** a full `matrix()` per shape; gradients in user space with the brush
  transform; image fills as clipped `<use>` of a `<defs>` image, or a `<pattern>` when tiled;
  shadows and layer blur as SVG filters; text as outlined glyph paths (skrifa), so it looks the
  same everywhere. Sweep gradients and background blur are rasterised into embedded PNGs.
- **PDF writer (pdf-writer):** groups with opacity or blend become isolated transparency-group
  XObjects; gradients are shadings inside a clip; images carry an SMask. Text stays text: each
  font instance (Inter's variable axes included) is subset by `subsetter` and embedded as a
  CID font with a ToUnicode map, so it is selectable and searchable. Effects, sweep gradients,
  repeating gradients and per-stop alpha are rasterised by `render-cpu` at export resolution.
- **SVG import (usvg):** shapes become path nodes with their transform baked in; gradients are
  mapped into the node's unit square; groups with an id, opacity or blend are kept; a
  (rounded) rectangle clip becomes a clipping frame, and content under any other clip path is
  drawn by our own renderer into an image with the clip applied; one image under one clip
  becomes a shape with an image fill; text is outlined with the bundled Inter (unknown
  families fall back to it). Filters, masks and patterns are dropped. Every loss is a warning.
- **Entry points:** script `exportImage(ids, format, path, opts)` and `insert(parent, {type:
  "svg", svg})`; CLI `canvas export`; the app's File ▸ Export selection…, Place image or SVG…,
  drop, paste (layers, SVG text or a bitmap, through egui-winit's arboard clipboard) and Copy
  as PNG / SVG.

## 7. Threading

- **Main/UI thread:** winit events, egui, input → commands. It never waits on I/O.
- **Render:** the frame is encoded on the main thread from cached fragments, which is cheap (0.2 ms
  for 10k nodes in the shell spike). Fragment re-encoding and the GPU submit can move to a render
  thread if profiling asks for it.
- **Worker pool (rayon):** text shaping, layout of large dirty roots, image decode, export, file
  load/save, fragment re-encode.
- The document is single-writer (main thread). Workers read immutable snapshots or receive owned
  inputs.
- **Autosave worker (Stage 5):** one thread per session appends journal batches and writes the
  base snapshot; the main thread only hands it compacted ops (0.06 ms after a 120-frame drag).

## 8. File format — decided (v1, now v2)

Full specification: `docs/file-format.md`. Schema: `crates/format/schema/document.v2.schema.json`.

- **Container:** a `Name.canvas/` folder with `document.json` and content-addressed
  `assets/<sha256>.<ext>`.
- **Nodes** are a nested tree with `children` in z-order.
- **Ids** are 22-character base62 strings.
- The JSON has a stable key order, omits default values, and is pretty-printed, so re-saving an
  unchanged document is byte-identical.
- Saves are atomic (temp file plus rename).
- **Migrations** run on untyped JSON before typed parsing, and newer versions are refused.
- **Measured:** a 50 MB file (64.5k nodes) opens in 0.28–0.35 s, and 100k nodes (77 MB) in
  0.44–0.48 s.

## 9. Agent API — built in Stage 6

**Crates:**
- `script`: the rquickjs sandbox and the JS document API over `editor::Session`.
- `agent`: the protocol shared by every front end. `Request`/`Response` (serde + schemars), the
  `Host` trait, the headless `Workspace` host, the IPC framing, the JSON Schema validator and the
  guides (embedded with `include_str!`). `agent::Request` is the named, schema-described command
  registry that §4 promised; the separate `commands` crate in §2 was not needed.
- `apps/cli` (`canvas`) and `apps/mcp` (`canvas-mcp`) are thin front ends over `agent`.

**Tools** (MCP; the CLI has the same commands, one per run):
- `execute`: a JavaScript program run as **one undoable transaction**. A fresh QuickJS context per
  call (0.26 ms), 100 ms CPU by default (up to 10 s), 64 MB memory, no modules, files only
  through `export` under the allowed roots. A thrown error rolls everything back and returns
  `{message, line, column, sourceLine, scriptId}`; `retry` with line `edits` re-runs a failed
  script without resending it.
- `get_state`, `screenshot` (CPU renderer, so it doesn't depend on the window), `read_guide`,
  `validate` (all problems with JSON pointers, then the document's own checks), `open`, `save`.

There are no typed per-command tools and no `batch`: one `execute` covers both and keeps the
surface small, which is what the brief asks for.

**The JS API** is a thin layer over `doc::commands`, so scripts can't bypass validation or undo:
`insert` (whole trees), `update`, `replace`, `move`, `copy`, `delete`, `get`, `find`,
`setVariables`/`getVariables` (by name), `findEmptySpace`, `screenshot`, `export`, `select`,
`setPage`, `print`. Arguments and results cross as JSON. Agent-friendly shorthands (`"#hex"`
paints, `[t, r, b, l]` padding, variable names) are normalised in `script` before the commands
see them, and inserts get sensible defaults (hugging text, 100×100 shapes).

**Queries** (`get`/`find`) are matched natively. A whole-page query scans the node storage and
restores document order only for the matches; `within` walks the subtree.

**Routing:**
- `canvas-mcp` connects to a running desktop app over a local socket (a named pipe on Windows):
  u32 little-endian length + JSON frames, a handshake with the protocol version, one request in
  flight per connection.
- The desktop app answers on its UI thread (`UserEvent::Agent`), so agent edits are ordinary
  transactions: they show live, Ctrl+Z undoes a whole script, autosave and the layers panel see
  them.
- With no app running, `canvas-mcp` uses an in-process headless `Workspace`. A lost connection
  is reported as an error, never replayed.
- **File access** is restricted to the allowed roots (`--root`, the current folder, the open
  document's folder), canonicalised. This idea is borrowed from FigCraft.

**Guides** (`start`, `api`, `nodes`, `layout`, `design`, `recipes`) are served by `read_guide`.
Every JS example in them runs in a test, and the property lists in `nodes` are generated from the
schema, so they can't drift.

## 10. App shell — built in Stage 5

**Shell (`apps/desktop`):**
- Native winit 0.30 + wgpu + egui 0.37 (AccessKit on). Vello renders the canvas into an offscreen
  texture that egui shows as an image in its central panel; egui then draws everything to the
  surface in one pass. Panels are fixed: toolbar and menus on top, layers left, properties
  right, status bar (an AccessKit live region announcing the selection) at the bottom. Both side
  panels resize and collapse; there is no docking.
- egui uses the bundled Inter, with Segoe UI, Segoe UI Symbol and Yu Gothic as fallbacks on
  Windows. egui 0.37 ships no font data of its own by default.
- **Frames on demand:** after input, while a gesture runs, while egui animates, or while
  fragments are stale. Re-encoding gets a 4 ms budget per frame, so a zoom-band change finishes
  over several frames and the old band is drawn meanwhile. A single fragment (2,000 nodes) can
  overshoot it on its first visit (see the Stage 5 report).
- Overlays (hover outline, selection box and handles, size label, marquee, smart guides, gap
  marks, distance labels, reorder indicator, drop target, frame name labels) are drawn with
  egui's painter in screen space, from data the editor returns.
- Files: `*.canvas` bundles through rfd dialogs, recent files, a dirty-state title, and a prompt
  on quit, New or Open with unsaved changes.
- `desktop --script S.json --out R.json` replays scripted input through the same controller as
  real input and records input → GPU-done latency per frame. It is used for the Stage 5
  latency numbers, screenshots and a GPU-gated test.

**Editor core (`crates/editor`)**, UI-free so Stage 6 (MCP `get_state`) and a web build can
reuse it:
- `Session`: store and layout deriver, render scene and index, page, camera per page, selection
  and scope, layers model, bundle path, dirty flag. Every edit goes through
  `Session::transact`, undo or redo, which bring the scene, index and layers up to date.
- **Selection:** a click picks the topmost hit and climbs to the outermost ancestor inside the
  scope. Top-level frames are see-through, so their children are picked directly; a frame is
  selected by its name label or its empty area. Double-click enters a container, Esc climbs out,
  Ctrl picks the deepest node, and Shift toggles. A press inside a selected node drags the
  selection, while a click without a drag picks what is under the pointer.
- **Hit-testing:** an index point query, then an exact test in local space:
  - fills by winding, strokes within half their weight, edges of unfilled shapes, boxes for
    frames, text and images;
  - a 4 px screen tolerance;
  - clipping ancestors respected, locked and hidden nodes skipped.
- **Transforms** are gestures that record their start placements and rewrite them from the
  total delta as coalesced transactions, so a gesture is one undo step:
  - move (Alt duplicates, Ctrl suppresses reparenting);
  - eight resize handles in the node's rotated frame (Shift keeps the aspect ratio, Alt resizes
    from the centre, dragging past the far edge flips);
  - rotate outside the corners (Shift snaps to 15°);
  - nudge with the arrow keys.
- **Auto layout:** flow children of an auto-layout frame show a reorder indicator while dragged
  and move on drop.
- **Snapping** within 6 screen px, in world space, against:
  - edges and centres of the parent and nearby siblings;
  - equal spacing (centring between neighbours or repeating an existing gap);
  - whole pixels when nothing else snaps.

  Guides, gap marks and distance labels come back as `Feedback` for the overlay.
- **Properties panel:** generated from `inspect::SECTIONS` (path, label, widget, default). Reads
  show "Mixed" across a multi-selection, writes apply to all, and a number drag coalesces into
  one undo step.
- **Layers model:** a flat list of visible rows, rebuilt lazily when a change touches the tree's
  shape or a name. The panel draws only the rows on screen.
- **Clipboard:** a tagged JSON payload ("ai-design-engine/nodes v2") holding subtrees and their
  world transforms. Paste goes in place when visible, otherwise at the viewport centre, with
  fresh ids.
- **Autosave** is an op journal:
  - The session records every committed op.
  - Two seconds after the last edit, runs of property updates are compacted and appended by a
    worker to `<data dir>/ai-design-engine/recovery/<key>.journal.jsonl`.
  - The journal applies to the bundle on disk, or to a base snapshot written once when the
    session starts from an untitled or recovered document.
  - Saving starts a new journal.
  - On launch, journals whose bundle has not been saved since they started are offered for
    restore (base + replay; a torn last line is ignored). Stale ones are deleted.

### Canvas editing, built in Stage 7c (`editor::text_edit`, `editor::path_edit`, `editor::a11y`)

- **Rich text edits** are span-preserving functions in `doc::richtext` (insert, delete,
  replace, restyle and slice byte ranges). Results are normalised: equal neighbours merge, empty
  spans go, and one span always remains so an emptied text keeps its style. `Tx::edit_spans`
  writes them as a `Kind` update, so text inside an instance becomes an override like any edit
  there. The file format is unchanged.
- **Text editing on the canvas** (`TextEdit`): double-click a text (the word under the pointer
  is selected), press Enter on it (all selected), or create one with the Text tool (its
  placeholder selected).
  - Caret, selection and hit-testing use the same Parley layout the renderer draws, from the
    shared text cache.
  - Keys: arrows, Ctrl+arrows by word, Home/End by line, Ctrl+Home/End, Shift to extend,
    Backspace/Delete (Ctrl by word), Enter for a newline, Ctrl+A, Ctrl+B/I/U (toggling over a
    range, or for what is typed next), Esc or Ctrl+Enter to leave. Copy, cut and paste use
    plain text.
  - Double-click selects a word and drag extends by words; triple-click selects everything.
  - Typing coalesces into one undo step per word: a space after a letter, a pause over a
    second, a caret move, or switching between typing and deleting starts a new step.
  - While editing, the properties panel's font, size, weight, italic, colour and decoration
    fields read and set the selected characters.
  - Leaving a text that is empty deletes it.
- **Input methods:** the canvas sets egui's `IMEOutput` (which egui-winit turns into
  `set_ime_allowed` and `set_ime_cursor_area`) at the caret while editing. Composition text is
  written into the document inside an open transaction, so the text reflows around it, and is
  underlined. Each update replaces the composed text inside that transaction; the commit
  replaces it with the committed text and commits the transaction as the typing step (same
  coalescing as typing), so each event lays out once. Cancelling rolls it back. Keys go to
  the input method while it composes.
- **Pen tool (P):** click for corners, drag for a smooth point with mirrored handles (Alt moves
  only the outgoing one), Shift for 45° steps, click the first point to close, Enter or Esc to
  finish. Points snap like drawn shapes. The whole path is one undo step.
- **Path edit mode** (double-click or Enter on a path): drag points (the selection moves
  together) and handles (a smooth point's opposite handle turns with it unless Alt is held),
  press a segment to add a point there (curves split exactly), Delete removes points,
  double-click toggles smooth and corner, arrows nudge. One undo step per drag.
  - Paths are parsed into anchors with in and out handles; quadratic segments become cubics
    when edited, and a control point on its own anchor reads as no handle.
  - After each edit the node's box is refitted to the outline. The data is shifted to start at
    the origin (on a 1/1000 grid, so the shift survives writing) and the node's position moves
    to match, with rotation and flips kept, so nothing jumps on the canvas.
- **Accessibility:** the canvas is a focusable AccessKit `Canvas` node named after the page;
  its description is the selection plus the keyboard hint, and its value is the text being
  edited. Its children are the layers around the selection (its siblings, or the page's
  top-level layers), up to 200, each with a role (group, label, image, graphic), name, value
  for text, screen bounds and selected state; a screen reader's click or focus selects one.
  Keyboard: Tab and Shift+Tab move to the next and previous sibling, Enter goes into a
  container (or edits a text or path), Shift+Enter goes to the parent. The canvas takes
  keyboard focus when pressed and locks Tab, arrows and Esc while focused. Panel controls with
  no telling text (symbol buttons, bare checkboxes, colour swatches, number fields and
  drop-downs) carry accessible names.

## 11. Testing and benchmarks

- Unit and property tests per crate. Op/inverse round-trip property tests: apply then undo
  restores the document byte-for-byte.
- Rendering: golden images on vello_cpu (30 fixtures, 1–2/255 tolerance), hand-computed pixel
  probes, an incremental-equals-fresh property test over random edits and undo, 1-vs-12-thread
  determinism, and a local GPU-vs-CPU parity test (`GPU_TESTS=1`). Layout fixtures with expected
  rects.
- **Benchmark harness:** `cargo run -p bench --release -- run`. Scenarios implement a `Scenario`
  trait, so Stage 4 can add render scenarios. Output is JSON with host info and p50/p95/max per
  metric.
  - `--baseline FILE[,FILE…]` (slowest p95 per metric across the files) fails the run when a
    p95 is more than 10% (`--threshold`) and 0.05 ms
    (`--floor`) slower.
  - Reference numbers are stored in `results/stageN/`.
  - Hosted CI compares a pull request with its base branch on the same runner, because hosted
    machines can't be compared with the reference desktop.
- Editor: unit tests, scenario tests for selection, transforms and tools, and property tests:
  random gestures then undo-all restore the document exactly; the layers model equals a fresh
  rebuild after random edits.
- Canvas app: scripted sessions (`apps/desktop/sessions/*.json`) for latency, and a GPU-gated
  test (`GPU_TESTS=1`) that runs edits through the real app, undoes everything and compares the
  saved documents byte for byte.
- `cargo deny check` (licenses, advisories) in CI.

## 12. Borrowed from FigCraft (MIT OR Apache-2.0, with attribution)

- **Code:** geom, color, pathops, auto-layout semantics (as reference for the Taffy mapping),
  SVG/PDF export walkers.
- **Conventions:** command registry, MCP batch/`$ref`, file-access roots, lint set, panic guard,
  `xtask layers`.
- **Not taken:** `.fig`/`.sketch` readers and writers, the REST-API importer (pending your call),
  brand assets, egui/winit forks.
- Copied files keep their headers. `NOTICE`/`ATTRIBUTION.md` lists the source commit (`da1cf8d`).

## 13. Risks (see the spike report for detail)

1. Vello 0.x: buffer limits and API churn. The GPU blur is now ours (Stage 4); Vello 0.11's
   image atlas needs a workaround for override images (see §6).
2. Zoom-band re-encode spikes: spread over frames with a 4 ms budget in Stage 5, but a single
   2,000-node fragment can still take ~29 ms on its first visit (text shaping).
3. Vello's own CPU time (packing and glyph resolve) is 8–11 ms at whole-page views and 15–20 ms
   at the zoom where text stops being greeked. This is the main remaining frame-time cost.
4. Numbers so far come from one high-end machine only.
5. IME on the canvas is built (Stage 7c) but was tested only with synthetic events, not with the Windows Japanese and Chinese input methods by hand.
6. Text shaping cost at open for many unique strings.
7. File size: pretty-printed JSON is 2.8× larger than compact (see the Stage 2 report).
