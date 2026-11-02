# Stage 1 spike report (2026-10-28)

All numbers were measured on **one machine**: Windows 11 Home 10.0.26200, Intel i5-11600KF
(6C/12T), NVIDIA RTX 3080 (driver 610.88), 32 GB RAM. They come from release builds with Rust 1.98.
Raw output is in `spikes/results/*.json` and screenshots are in `spikes/results/png/`.

This is a high-end desktop GPU. As agreed, I treat **≤ 8 ms p95 here** as the stand-in for "16 ms on
a mid-range laptop". Every spike binary writes JSON, so the same runs can be repeated on a laptop,
Mac or Linux box (see `.github/workflows/spikes.yml`, which is inactive).

Most runs were done once or twice, not three times as planned. Where two runs exist I give the
range. Treat single-run tails (max, p99) as indicative only.

## Summary of recommendations

| Decision | Recommendation | Confidence | Main evidence |
|---|---|---|---|
| GPU renderer | **Vello 0.11** (wgpu), with retained per-artboard fragments, R-tree culling and LOD | Medium-high | Every phase p95 ≤ 8 ms at 100k nodes. Skia GL misses badly when zoomed out (55 ms p50) |
| CPU / reference renderer | **vello_cpu** for headless export. Keep **tiny-skia** (via FigCraft-style code) as a second golden-image oracle | Medium | vello_cpu 4K detail 30 ms, overview 182 ms. Same scene encoding as the GPU path |
| Text | **Parley 0.12** + fontique | High | 1.7–10× faster than cosmic-text on every case. Correct per-script fallback and RTL |
| Layout | **Taffy 0.15** behind our own `LayoutEngine` trait, with design-tool semantics (hug/fill, etc.) mapped on top | High | 9.1k nodes: full layout 9–11 ms, one-leaf edit 0.7 ms (target is < 50 ms per 10k subtree) |
| App shell | **Native: winit + wgpu + egui** | Medium-high | Input → GPU done 3.1 ms p50 vs about 18 ms in Tauri. One language. No WebGPU-in-webview risk |
| Scripting | **rquickjs 0.14** (QuickJS) | High | 496-node program 2.7 ms. CPU and memory limits and line numbers all work |
| Borrowing from FigCraft | Our own core, borrowing selected crates and conventions with attribution (option 2) | High | See `docs/figcraft-evaluation.md` |

**Decisions I need from you:** renderer (Vello), text (Parley), app shell (native egui). Details
and risks follow.

---

## 1. Renderer: Vello vs Skia

**Scene** (`spikes/common/src/scene.rs`, seeded): 100,007 items on 1440×1024 artboards. That is
37,762 rects with radii, 6,207 ellipses, 12,449 bezier icon paths, 37,347 text runs
(Latin/CJK/Arabic/emoji), 6,242 images, 6,294 drop shadows and 246 layer blurs.

**Scenario:** a 1920×1080 window, vsync off, 960 frames:

- Pan at 100%: 300 frames.
- Continuous zoom 5% → 800%: 300 frames, a new scale every frame.
- Pan at 15% overview: 300 frames, about 13k items visible.
- Edits: 60 frames, recolour and nudge one card per frame.

Frame time runs from frame start to GPU completion (Vello: `device.poll` wait; Skia: `glFinish`).

**Frame time, ms, p50 / p95**

| Mode | Pan 100% | Zoom 5→800% | Pan 15% | Edit |
|---|---|---|---|---|
| **Vello, retained fragments** | 2.30 / 3.14 | 2.90 / **7.68** (max 44.9) | 3.99 / 5.08 | 4.87 / 5.73 |
| Vello, immediate (cull + LOD) | 1.52 / 2.01 | 2.53 / 27.6 | 7.21 / 8.34 | 1.94 / 2.39 |
| Skia GL, immediate (cull) | 2.45 / 3.48 | 12.5 / 226 | 54.5 / 61.9 | 2.05 / 3.52 |
| Skia GL, SkPicture per artboard | 2.77 / 3.70 | 11.1 / 196 | 54.7 / 62.9 | 1.99 / 2.96 |
| Vello, **no culling** | crash: a storage buffer binding exceeds the 128 MB wgpu limit | | | |

| Other | Vello | Skia |
|---|---|---|
| Startup to first frame (warm) | 709 ms (device and surface 586 ms) | 393–657 ms |
| Peak RSS | 190–217 MB | 428–445 MB |
| Release `dist` binary size | 11.8 MB | 7.3 MB |
| Clean build | not timed cleanly (shared target dir); no external toolchain needed | 60 s with the prebuilt binaries skia-safe downloads. Building from source needs LLVM, Ninja and Python |
| Backend used on Windows | Vulkan (wgpu). DX12 and Metal are available | OpenGL (D3D, Metal and Vulkan need other features) |
| Layer blur | **Not supported on the GPU path** (246 items drawn sharp) | Supported (image-filter blur) |
| Drop shadow | `draw_blurred_rounded_rect` (rounded rects only) | Mask-filter blur, any shape |

**CPU export at 4K** (3840×2160, 5 runs, p50):

| | Detail 100% (306 items) | Overview 15% (12,951 items) |
|---|---|---|
| vello_cpu 0.3 | 30 ms | 182 ms |
| Skia raster (1 thread) | 20 ms | 5,790 ms (an outlier I didn't investigate; probably per-item mask-filter shadows) |
| FigCraft tiny-skia (1920×1080, 12 threads) | about 8 ms pan / 41 ms full | — |

**Reading the numbers:**

- With a culling R-tree, both GPU renderers are fast at 100%. They separate when many items are on
  screen.
  - Skia's per-draw CPU cost makes overview panning 55 ms, and recording pictures doesn't help.
  - Vello stays at 4–8 ms because its cost is mostly GPU compute over the encoded scene.
- Vello has two weak spots:
  1. **CPU encoding** when nothing is cached. That's the immediate-mode zoom p95 of 27.6 ms.
     Retained per-artboard fragments, re-encoded only on edit or zoom-band change, bring it to
     7.7 ms. The remaining zoom max (45 ms) comes from frames where many artboards re-encode at
     once when crossing a band. Next step: re-encode in the background or spread it over several
     frames.
  2. **Unbounded scenes:** without culling it hits the wgpu buffer binding limit and crashes. So
     culling and LOD are **mandatory, not optional**. The renderer must also budget the encoded
     size per frame.
- Edit cost in retained Vello (4.9 ms) is higher than immediate (1.9 ms) because an edit
  re-encodes the whole artboard fragment (about 240 items). Finer fragments (per card or frame
  subtree) will fix this.
- **Fidelity gaps in Vello** that need product work:
  - **Layer blur:** options are a separate render-to-texture blur pass, or vello_hybrid/vello_cpu
    filters as they mature.
  - **Shadows on non-rect shapes:** same.
  - Both are in the risk list.
- Visual check (`png/vello_*`, `png/skia_*`): same geometry, text, bidi and images. Anti-aliasing
  is comparable at 100%.

**Recommendation: Vello.**

- It's the only option that met ≤ 8 ms p95 in every phase.
- It's pure Rust: no C++ toolchain or prebuilt-binary dependency.
- It uses the same wgpu stack as egui and as WASM/WebGPU.
- It has a matching CPU renderer (vello_cpu) that takes the same encoding.

Skia is the fallback if blur fidelity can't be solved in time, but its overview performance would
need a tile cache.

## 2. Text: Parley vs cosmic-text

Measured in the same process on Windows system fonts (`results/text_run1.json`):

| Case | Parley 0.12 | cosmic-text 0.19 |
|---|---|---|
| Cold start (font discovery + first layout) | **3.2 ms** | 32.1 ms |
| 10k short labels, shape + layout | **66.9 ms** | 113.4 ms |
| 100 long paragraphs, wrapped at 600 px | **19.5 ms** | 60.7 ms |
| 100 rich paragraphs (5 styles/spans) | **21.8 ms** | 75.0 ms |
| Edit latency, one keystroke in a 2,000-word paragraph (full relayout) | **1.93 ms** | 4.96 ms |

**Mixed-script line** ("Hello مرحبا 你好 こんにちは 안녕 😀"), runs reported:

- **Parley:** Segoe UI / **RTL run for Arabic** / Yu Gothic UI for Han and kana / Malgun Gothic /
  Segoe UI Emoji.
- **cosmic-text:** Microsoft YaHei UI for the Chinese and Yu Gothic for the Japanese (a reasonable
  locale-free choice), Malgun Gothic and Segoe UI Emoji. But **no run was marked RTL**: its
  run-level API doesn't expose bidi level the way we queried it.
- Both produce correct-looking output in the renderer screenshots.

**API fit:**

- Parley has a span-style builder, cluster and cursor hit-testing, selection geometry, and shares
  `peniko`/`skrifa` with Vello (glyph runs go straight to `draw_glyphs`).
- cosmic-text has a richer built-in editor and swash rasterization, which we don't need with a GPU
  glyph renderer.

**Recommendation: Parley.**

**Caveat:** the 100k scene only has 39 distinct text layouts, reused across items, so the renderer
benchmark doesn't measure shaping 37k unique strings. At 66.9 ms per 10k labels, shaping 37k unique
labels is about 250 ms single-threaded. That must be parallel and cached at document open (it fits
the < 2 s target on 12 threads).

## 3. Layout: Taffy

The tree has 9,101 nodes: 100 sections, 60 wrapping flex rows and 40 grids, 10 cards each. Text
leaves use a measure function. Results are in `results/layout_run*.json`.

| Case (p50 / p95 ms) | Fixed-size cards | Fill-heavy (flex-grow everywhere) |
|---|---|---|
| Build tree | 8.3 | 6.9 |
| Full layout | 9.3 / 10.9 | 11.4 / 15.9 |
| Text leaf edit (`mark_dirty` + relayout) | 0.72 / 1.33 | 0.76 / 1.27 |
| Icon style edit | 0.43 / 0.79 | 0.54 / 0.94 |
| Root resize | 2.2 / 5.1 | 4.0 / 8.9 |

The target is < 50 ms to re-lay-out a 10k-node subtree. A full layout is 5× under it, and
incremental edits are 50× under. Yoga wasn't needed.

**Gaps to cover in our mapping layer:**

- Design-tool semantics aren't 1:1 with CSS flex: hug/fill/fixed, "space between" with
  auto-spacing, absolute children inside auto-layout, baseline alignment, and min/max on text.
- FigCraft's auto-layout semantics (about 1.8k LOC, MIT/Apache) are a good reference for that
  mapping.

**Recommendation: Taffy**, behind a `LayoutEngine` trait.

## 4. App shell: native egui vs Tauri

Both shells show the same thing: a 10k-node Vello canvas (retained) and a properties panel
(x/y/w/h/fill) synced both ways. Each runs a scripted session of 300 drag steps and 200 panel edits.

| Metric (p50 / p95 ms) | Native: winit 0.30 + wgpu 30 + vello + egui 0.37 | Tauri 2 (WebView2) + Rust→WASM vello on WebGPU + HTML panel |
|---|---|---|
| Input → GPU done, drag | **3.06 / 3.93** (no vsync), 5.9 / 6.3 (vsync) | 12.6 / 19.4 |
| Render start → GPU done | about 3.0 | 6.1–7.5 / 7.5–10.3 |
| Input → next composited frame (rAF) | — | 18.1 / 24.2 |
| Panel edit → GPU done | 3.04 / 3.74 | 17.5 / 22.6 |
| UI cost per frame | egui 0.12 ms | DOM work is not separable |
| Canvas encode (CPU) | 0.18 ms (native) | 2.3 ms (WASM) |
| IPC round trip | n/a (same process) | 1.1–1.2 / 1.7–1.9 |
| Startup to first frame | 657 ms | 817–905 ms (WASM + font fetch alone: 558–610 ms) |
| RSS | 135 MB | not comparable (multi-process webview) |
| Release binary | 13.6 MB | 9.6 MB, plus the 3.0 MB WASM; relies on the system WebView2 |
| Languages | Rust | Rust + JS/HTML (+ wasm-bindgen glue) |

**Qualitative notes:**

- **Tauri:**
  - WASM can't see system fonts, so every font must be shipped or read over IPC.
  - The document lives either in WASM, which means two builds of the core, or in the Rust host,
    which means IPC on every edit.
  - Latency is tied to the webview's rAF/compositor cycle (about +1 frame).
  - WebGPU in WebView2 worked out of the box. WebGPU in WKWebView (macOS) and WebKitGTK (Linux)
    is, as far as I know, not reliably available. That's from my own knowledge and **not verified
    here**, and it is the biggest cross-platform risk for this option.
  - Its strength is the UI layer: HTML/CSS panels are faster to build and look native-ish.
- **Native egui:**
  - One process, one language, and the core runs at native speed.
  - egui 0.37 has AccessKit accessibility.
  - Its weaknesses: immediate-mode widgets look less polished, there's no built-in docking,
    complex text inputs are weaker than the web's, and IME needs testing.
  - FigCraft ships on egui (with forks of egui and winit), which shows a full editor UI is
    feasible. Its need for forks is also a warning.
- **Development effort** for the spikes (rough estimate): native took about 1 hour (one API-drift fix in egui
  0.37). Tauri took about 2.5 hours (wasm-pack, font loading, two build steps, latency
  instrumentation).

**Recommendation: native winit + wgpu + egui.** The canvas is the product, and the native path
halves latency and keeps a single Rust codebase. Keep the UI layer thin and data-driven (panels
described by the command and property schema) so a web UI could be added later for the WASM build
without touching the core.

## 5. Scripting: rquickjs

The spike binds a stub document API (insert/update/get/find/print, JSON-marshalled) to a Rust
store. Results are in `results/script_run1.json`.

| Metric | Result |
|---|---|
| Runtime create | 0.09 ms |
| First context (with prelude) | 0.28 ms |
| Warm context | 0.11 ms p50 |
| 496-node build program (55 cards), end to end | **2.69 / 3.07 ms** p50 / p95 |
| FFI no-op call | 0.096 µs |
| `insert` via JSON marshalling | 4.45 µs |
| CPU limit (interrupt handler, 100 ms budget, infinite loop) | Stopped at 100.06 ms, `interrupted` error |
| Memory limit (64 MB) | `out of memory` after 86 ms. Runtime stays usable |
| Error location | `eval_script:3:1` (line:col reported) |

**Recommendation: rquickjs.** Run `execute` as one undoable transaction. Use a fresh context per
call (0.1 ms) for isolation, with CPU and memory limits enforced as above. JSON marshalling is fine
at this scale. Typed `FromJs`/`IntoJs` can come later if profiling asks for it.

## 6. FigCraft comparison (same 100k scene)

Full details are in `docs/figcraft-evaluation.md`. FigCraft is a tiny-skia CPU retained canvas
with LOD, multi-threaded bands, 1920×1080, two runs:

| Phase p50 / p95 | FigCraft | Vello retained |
|---|---|---|
| Pan 100% | 8.3–8.9 / 12.2 | 2.3 / 3.1 |
| Zoom | 41–43 / 88–95 | 2.9 / 7.7 |
| Pan 15% | 4.2–6.2 / 6.1–8.7 | 4.0 / 5.1 |
| Edit | 12–21 / 19–32 | 4.9 / 5.7 |

Other FigCraft measurements:

- Document build: 570–730 ms. RSS: 318 MB. Hit-test: about 200 µs.
- Its own harness at 100k: per-property edit 4 ms (it grows linearly with document size), and
  main-component edit 35 ms.

**Conclusion:** borrow its well-separated L0–L4 crates and conventions (geometry, colour, path
booleans, auto-layout semantics, SVG/PDF export walkers, MCP `batch`/`$ref` design, file-access
roots, lint/panic-guard/layering checks) with attribution. Don't fork it. Don't take its
`.fig`/`.sketch` code or brand assets.

## Targets vs measured (this machine)

| Brief target | Measured | Status |
|---|---|---|
| 60 fps pan/zoom at 100k nodes | Vello retained: p95 3.1 / 7.7 / 5.1 ms (pan / zoom / overview) | Met here. Zoom max 45 ms during band crossings needs smoothing. Laptop still to be measured |
| Edit → frame < 16 ms | 5.7 ms p95 (100k, renderer only). Shell input → GPU done 3.9 ms p95 (10k) | Met. Not yet measured end to end with layout + text at 100k |
| Re-layout of a 10k subtree < 50 ms | 9–11 ms full, 0.7 ms incremental | Met |
| Open a 50 MB doc < 2 s, < 1 GB | Not measured (no file format yet). Scene gen + shaping 50 ms with 39 unique layouts. FigCraft load 478 ms at 100k | **Open:** Stage 2 benchmark once the format exists |
| Hit-test < 1 ms | Not separately timed in our spikes (the shells use an rstar point query). FigCraft at 100k: 200–338 µs | Likely; time it in Stage 2 |

## Risks

1. **Vello maturity:**
   - No GPU layer blur, and only rounded-rect shadows.
   - Crashes on unbounded scenes (buffer limits).
   - The API changes every release (0.x).
   - Mitigation: renderer trait, mandatory culling and encode budget, a blur pass we own, pinned
     versions, and vello_cpu as the oracle.
2. **Zoom band re-encode spikes** (45 ms max). Mitigation: background encoding, finer fragments,
   and keeping the previous band's fragment until the new one is ready.
3. **Single-machine numbers.** A high-end GPU flatters every GPU option. Laptop iGPU and Apple
   Silicon runs are needed before the targets count as met.
4. **egui UI polish and IME.** Must be prototyped early in Stage 2: the properties panel, layers
   tree, and text editing with IME.
5. **Text shaping at document open** for many unique strings. Parallelise and cache by
   (string, style).
6. **Cross-platform WebGPU** (only affects a future web build or the Tauri option): not verified.

## Deviations from the brief and plan

- **Skia backend:** ran on OpenGL, not D3D, because skia-safe's D3D feature needs extra setup. GL
  is representative of Skia's CPU-side cost, which is what dominated.
- **tiny-skia** was measured through FigCraft's renderer rather than a separate
  `render-tinyskia` spike.
- Most runs were done once or twice instead of three times. There are no criterion benches; I used
  timed loops with p50/p95.
- The text spike measured run-level fallback and bidi, not rendered PNG review of every corpus
  item. Rendered text from both renderers is in the screenshots.
- **Shell spike** used 10k nodes, as planned, not 100k.
- CI workflow written but inactive (no remote).
