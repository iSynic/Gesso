# FigCraft evaluation (2026-10-28)

Source: https://github.com/storytold/figcraft at commit `da1cf8d`. First read through the GitHub
API, then cloned and built locally (in the session scratchpad, not in this repo) and benchmarked.

## Hands-on benchmark (Windows 11, i5-11600KF 6C/12T, RTX 3080, release builds)

**Build:** HEAD doesn't build as published. The desktop app needs `craft-menu`, which isn't in the
pinned `craft-libs` revision. To build `figcraft-cli` I dropped the desktop app, web app, server
and egui UI crates from the workspace. A clean release build of the CLI took 251 s and produced a
38 MB binary.

**Its own harness** (`figcraft-cli perf --layers 10000,50000,100000`; its synthetic 360x240 screens
are much lighter than ours; 1440x900 viewport; mean ms):

| Operation | 10k | 50k | 100k |
|---|---:|---:|---:|
| build + normalize (like open) | 162 | 366 | 755 |
| load .figcraft | 49 | 238 | 478 |
| render @100% | 5.8 | 5.6 | 5.7 |
| render zoomed to fit (LOD) | 7.8 | 12.0 | 17.4 |
| render zoomed to fit, no LOD | 21.6 | 75.3 | 122.5 |
| pan 40 px @100% (retained) | 1.2 | 1.6 | 2.0 |
| move one layer + re-render @100% | 0.5 | 4.1 | 8.3 |
| hit test (µs) | 44 | 172 | 338 |
| node.set fill on one layer | 0.13 | 1.8 | 4.0 |
| edit main component (instances follow) | 2.9 | 15.6 | 35.1 |
| select all + move every screen | 3.6 | 19.6 | 47.7 |

Per-edit cost grows roughly linearly with document size (0.13 ms at 10k, 4 ms at 100k).

**Our 100k-node spike scene** (identical items, built as FigCraft nodes by
`scratchpad/figcraft-src/fc-bench`; icons approximated by 5-point stars; system fonts, Segoe UI).
It's rendered through FigCraft's canvas path (`Retained` + LOD, tiny-skia, multi-threaded bands) at
1920x1080 over the same 960-frame scenario. Two runs; `results/figcraft_scene100k_run*.json`:

| Phase | FigCraft p50 / p95 (ms) | Vello retained p50 / p95 (ms) | Skia GL retained p50 / p95 (ms) |
|---|---|---|---|
| Pan at 100% | 8.3–8.9 / 12.2–12.3 | 2.3 / 3.1 | 2.8 / 3.7 |
| Zoom 5%→800% (new scale every frame) | 41–43 / 88–95 | 2.9 / 7.7 | 11.1 / 196 |
| Pan at 15% | 4.2–6.2 / 6.1–8.7 | 4.0 / 5.1 | 54.7 / 62.9 |
| Edit (recolour + nudge one card) | 12–21 / 19–32 | 4.9 / 5.7 | 2.0 / 3.0 |

Also measured: document build + normalize 570–730 ms, RSS 318 MB after building, and hit-test
~200 µs per point at 100k nodes.

FigCraft's numbers exclude uploading and presenting the frame (about 1 ms). The GPU numbers include
GPU completion and present.

**Reading:** FigCraft's retained CPU canvas handles panning well, because it renders only the
uncovered strip. It misses 60 fps whenever the scale changes, since every zoom step is a full CPU
render (41 ms median). Its edit cost also grows with the document. Both match its own gap #7. The
rendering in the screenshots matches ours (bidi, CJK, shadows, images): `results/png/figcraft_*.png`.

## First read (from the GitHub API)

## What it is
- Pure-Rust, clean-room design tool, **MIT OR Apache-2.0** (same as ours, so code reuse is allowed
  with copyright notice + NOTICE/ATTRIBUTION carried over). ~109k lines, 18 crates, early alpha,
  very active (pushed today), 3.7k stars.
- Covers most of our v1 feature list already: auto layout (incl. wrap, grid, hug/fill/fixed,
  min/max), components/variants/instances with overrides, variables with modes, vector networks,
  booleans, effects, rich text with bidi, SVG/PNG/JPG/WebP/PDF export, undo, autosave/recovery,
  AccessKit accessibility, CLI, MCP server, sandboxed WASM plugins, collaboration preview.

## Architecture vs our brief

| Area | FigCraft | Our brief / spikes | Fit |
|---|---|---|---|
| Layering | Engine crates never depend on UI; enforced by `cargo xtask layers` | Same principle | Borrow the idea and the xtask check |
| Document | `imbl` persistent map, `Arc` snapshots for undo, integer ids | Slotmap + stable random 128-bit ids, explicit op log (CRDT-ready) | Different; their snapshot undo is simple and proven, but ids/ops don't meet our CRDT goal as-is |
| Mutations | 455 commands in one registry; UI, CLI, control channel, MCP, plugins all call it | One transaction API + ops | Strong match in spirit; commands sit above an op layer |
| Layout | Own auto-layout engine (~1.8k LOC), not Taffy | Taffy behind a trait | Theirs encodes design-tool semantics (hug/fill, baselines, absolute children); worth studying for our mapping layer |
| Text | HarfRust + skrifa + unicode-bidi, own line layout | Parley vs cosmic-text spike | Alternative candidate; Parley also uses HarfRust internally |
| Renderer | **tiny-skia CPU only**, banded/parallel, retained image, dirty regions, LOD | GPU first (Vello/Skia) + CPU reference | Their own gap #7: "CPU canvas where Figma uses the GPU", est. 30-60 h for a GPU path held to the CPU renderer as oracle |
| Agent API | MCP: `run_command`, `batch` (one undo step, `$ref` between steps), `create_node`, `set_properties`, `get_node`, `export`, `screenshot`, `pointer`/`key` input, file-access roots | QuickJS `execute` as one transaction | Their tool surface is a good baseline; `execute` would sit on top of the same command layer |
| File format | `.figcraft` = zip of `document.json` + images | Plain JSON + schema + sidecar assets, git-friendly | Different (zip is not diffable) |
| Robustness | `unwrap`/`expect`/`panic` denied by clippy, `unsafe` forbidden, panic guard rolls back a command | Not specified | Borrow these lints and the guard pattern |

Their published performance (`docs/gaps.md`, `cargo xtask bench`): full 1440x900 render 7-15 ms
at 10k layers; a 50k-layer page ~8 ms zoomed out with LOD; single-layer edit at 10k layers ~0.2 ms.
That's fine at 10k but leaves no headroom for our 100k / 60-120 fps target on the CPU. Our Vello
spike draws a 100k-node page at p95 ≤ 8 ms in every phase.

## Things we must not take
- `crates/format/src/fig*` (reads/writes `.fig` via the reverse-engineered kiwi schema) and
  `crates/format/src/sketch*`. Our brief forbids formats from commercial tools.
- The REST API JSON importer maps a commercial tool's public API. It may be allowed under "public
  specifications", but that's your call.
- ArtCraft brand assets (`docs/brand/`, trademark-licensed) and the app icon.

## Risks if we depend on or fork it
- Pinned **forks of egui and winit** plus `craft-libs` and `krilla` from their git repos (patched
  via `[patch.crates-io]`). Forking FigCraft inherits those forks.
- Alpha quality by its own estimate (~49% to real-world use). Test depth is unknown until we can
  build it.
- Its data model, ids and file format are the parts least aligned with our brief, and they're the
  hardest to change later.

## Options
1. **Fork FigCraft** (strip `.fig`/`.sketch`), then add a Vello GPU canvas (keeping tiny-skia as the
   oracle), QuickJS `execute` over its command registry, and JSON-schema'd plain-JSON files.
   Fastest path to a full-featured editor. We'd inherit its document model, UI forks and
   architecture.
2. **Our own core per the brief, borrowing selectively** (with attribution): `geom` (shape
   builders, smoothed corners), `color`, `pathops` (booleans via linesweeper), the auto-layout
   semantics, the SVG/PDF export walkers, the MCP tool design (`batch` + `$ref`, file-access roots),
   the lint/guard/layering conventions, and the command-registry pattern. Keeps our performance
   architecture and CRDT-ready ops.
3. Use it only as a reference.

## Recommendation
Option 2, now backed by measurements. On our scene FigCraft misses the 60 fps target while zooming
(p50 41 ms vs Vello's 2.9 ms) and on edits at 100k (p95 19–32 ms vs 5.7 ms). It already pans
well. Its per-edit cost grows linearly with document size, which points at its document and
normalize model rather than only the renderer. A fork would mean replacing the renderer, fixing
the scaling of edits, changing ids and file format, and keeping its egui/winit forks building
(HEAD doesn't build today). That's most of the engine. The L0–L4 crates and its conventions lift
out cleanly and are worth borrowing with attribution, and tiny-skia/its renderer is a good
candidate for our CPU reference and golden-image oracle.
