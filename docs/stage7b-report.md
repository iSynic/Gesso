# Stage 7b report: export, SVG import, clipboard, images (2026-10-29)

Checkpoint 7b of Stage 7. Checkpoint 7c (text editing on the canvas, pen tool, accessibility)
has **not** been started; this report pauses for your go-ahead.

**Machine:** the same as before. Windows 11, Intel Core i5-11600KF (6C/12T), 32 GB,
**NVIDIA GeForce RTX 3080 (Vulkan)**, release build, Rust 1.98.

**Raw results** are in `results/stage7b/run{1,2,3}.json` and `.log`: the full `bench` suite
three times, gated against Stage 6's `run2.json`. `results/stage7b/before-atlas-patch/` holds
three earlier runs, made before the Vello fix described below. Ranges are min–max across runs.

## Summary

- **Export works from every entry point:** script `exportImage`, CLI `canvas export`, and the
  app's File ▸ Export selection… plus an Export section in the properties panel. Formats are PNG,
  JPEG, WebP (lossless), SVG and PDF.
  - SVG draws text as outlines.
  - PDF keeps text as real, selectable text, using embedded subset fonts with Unicode maps.
- **SVG import works:** script `insert(parent, {type: "svg", svg})`, File ▸ Place image or SVG…,
  dropping a file, and pasting SVG text. Anything it can't import exactly is reported as a
  warning.
- **Clipboard:**
  - Copy still writes our layers payload.
  - **Copy as PNG** and **Copy as SVG** are new.
  - Paste accepts layers, SVG text or a bitmap.
- **Images:** place them, drop them, or paste them. Decoding now happens in the background:
  frames draw placeholders and never wait.
- **Gate met:** no frame over 16 ms while 100 images decode. The worst frame was **7.7–10.6 ms**.
  Getting there took two fixes, one of them a small addition to our Vello patch (below).
- **No regressions against 7a.** No metric is slower than 7a in all three runs. Effect-heavy
  frames got faster thanks to the Vello fix (50 blurs and 50 shadows: 5.0 → 2.2–3.1 ms p95).
  The gate still flags the same misses as in 7a (details under Benchmarks).
- **Tests:** a round-trip oracle against resvg, PDF structure checks and goldens, import
  goldens, and a `svg_import` fuzz target with a stable-toolchain mutation test. All pass.

## What was built

| Path | What it holds |
|---|---|
| `crates/export` (new) | `collect` (full-detail display lists in export space), `raster` (PNG/JPEG/WebP, up to 16,384 px per side), `svg` writer, `pdf` writer (pdf-writer + subsetter), `import` (usvg), `glyphs` (skrifa outlines). Sits between `render-vello` and `editor` in the layer check. |
| `crates/render` | Exact builder mode (no LOD or greeking, full effects, full-resolution images). Background image decoding: `Scene::set_image_waker`, `frame_complete` for screenshots, a separate decode pool, at most 8 decodes taken in per frame. |
| `crates/text` | Glyph runs carry their source characters (for PDF Unicode maps). |
| `crates/editor` | `place` module: place SVG or images, centred on the drop point or the view, into the selected frame, in one undo step. `Session::import_svg`, `add_asset`, and in-memory assets for untitled documents. Commands Place (Ctrl+Shift+K), Copy as PNG (Ctrl+Shift+C), Copy as SVG, Export (Ctrl+Shift+E). |
| `apps/desktop` | File ▸ Place image or SVG…, Export selection…; Edit ▸ Copy as PNG / SVG; drop files on the canvas; paste SVG text or bitmaps; Export section in the properties panel; the event loop wakes when a decode finishes. |
| `apps/cli` | `canvas export <doc> [--page] [--node]… --format F [--scale S] [--quality Q] [--padding P] -o OUT`. |
| `crates/script`, guides | `exportImage` takes every format and `quality`; `insert` takes `{type: "svg"}` (warnings go to the logs). The `api` guide is updated. |
| `third_party/vello` | The image atlas is no longer recreated when a frame needs less of it (below). |
| `crates/bench` | New `export_formats` scenario. |
| `fuzz/` | New `svg_import` target. |
| `docs/` | `architecture.md` (export and import, background decoding, layer order), `licenses.md` (regenerated), `third_party/README.md`, this report. |

**New crates (all cached, permissive):**
- pdf-writer 0.15 and subsetter 0.2.6 (MIT OR Apache-2.0);
- usvg 0.48.1 with roxmltree, svgtypes, simplecss, data-url and fontdb;
- `image`'s JPEG and WebP codecs;
- resvg 0.47, as a test-only dependency.

`cargo deny check licenses`, `cargo xtask layers` and `cargo clippy --workspace --all-targets
-D warnings` are clean.

### What export and import do with things that don't map exactly

**SVG export:**
- Shadows and layer blur become SVG filters.
- Sweep gradients and background blur become embedded PNGs.
- Text becomes outlines, so it looks the same everywhere. Keeping `<text>` is a later option,
  as planned.

**PDF export:**
- Shapes, gradients and images stay vector. Text stays real text.
- Effects are drawn into images at export resolution: shadows, blurs, sweep and repeating
  gradients, and gradients with per-stop transparency.
- **The shadow images contain only the shadow.** The shape and its text are still drawn as
  vectors on top, so text in shadowed cards stays selectable.

**SVG import:**
- A clip path that is a (rounded) rectangle becomes a clipping frame.
- **Content under any other clip path is drawn by our renderer into an image, with the clip
  applied, and you get a warning** (the plan's "rasterised").
- Filters, masks and patterns are dropped, with a warning.
- Text is converted to outlines with Inter. Unknown font families fall back to Inter.

### Choices I made (please say if you want them otherwise)

1. **No direct dependency on arboard.** egui-winit 0.37 already uses arboard for text and
   bitmaps (`PasteImage`, `copy_image`), so the app goes through egui-winit.
   - Copy as SVG puts the SVG on the clipboard as plain text. Most design tools and editors read
     it that way.
   - The dedicated `image/svg+xml` clipboard format isn't written.
2. **SVG import keeps the SVG's own coordinates.** An SVG exported at 2× imports at twice the
   size, as the file says.
3. **Pasted bitmaps keep their pixel size,** scaled down to 80 % of the view if larger. Dropped
   files land where they are dropped; pasted ones land in the centre of the view.
4. **WebP is lossless only.** The cached `image` crate has no lossy WebP encoder. A lossy
   encoder needs a new crate, which means a download; tell me if you want it.

## Benchmarks

### The export_formats scenario (new)

| Metric | p50 | p95 | Gate |
|---|---|---|---|
| SVG export, 192-node screen (1.4 MB, text outlined) | 70–75 ms | 73–84 ms | measured |
| PDF export, same screen (91 KB, text embedded) | 22–24 ms | 23–27 ms | measured |
| JPEG 3840×2731 of that screen | 221–238 ms | 225–243 ms | measured |
| PDF of 125k nodes on one page (10.8 MB) | 2.56–2.74 s | 2.59–2.85 s | measured |
| … of which building the display lists | 1.35–1.49 s | | |
| Import a 10k-path SVG (663 KB) and insert it | 215–237 ms | 220–243 ms | measured |
| … of which conversion to nodes | 149–160 ms | | |
| Paste 2,512 nodes (602 KB payload), with layout | 17–20 ms | 19–26 ms | measured |
| **Frames while 100 images (1024² JPEG) decode** | 3.8–5.0 ms | 5.6–7.0 ms | **max 7.7–10.6 ms < 16 ms ✅** |

- **100 images:** all are drawn after 1.0–1.1 s. For comparison, decoding them before drawing
  makes the first frame take 0.75–0.77 s.
- **The 125k-node PDF:** the generator's largest page holds about 24k nodes, so this
  benchmark draws every page of a 100k-node document onto one PDF page. The pages overlap,
  which doesn't affect the timing.

### Three fixes the measurements forced

1. **A shadowed-card PDF took 30–40 s.**
   - **Cause:** each card's shadow was rendered to an image separately, about 10k renders.
   - **Fix:** identical shadows (same size, blur, colour and sub-pixel position) now share one
     image, and the distinct ones are rendered in parallel before the writing pass.
   - **Result:** 2.6–2.8 s, and the file shrank from 40 MB to 10.8 MB.
   - **Related fix found on the way:** a font that can't be embedded used to be re-checked on
     every text run. It is now checked once.
2. **Frames reached 590 ms while images decoded.**
   - **Cause:** the decode jobs filled the shared thread pool, so the frame's own parallel work
     queued behind them.
   - **Fix:** decoding now runs on its own pool, using a third of the cores.
3. **One frame took 44–50 ms when the first images arrived.**
   - **Cause:** almost all of it was GPU time. Vello recreates its image atlas whenever the size
     a frame needs changes. Frames with only placeholders need 1×1, so the first frame with
     images rebuilt the whole atlas.
   - **Fix:** a small addition to our existing Vello patch keeps the atlas when it's already big
     enough. The shader clamps reads to each image's own area, so the extra space is never
     sampled.
   - **Checks:** the GPU test `images_survive_frames_without_them` and the parity tests pass.
     Effect-heavy frames also got faster, because they use the atlas too.

### The rest of the suite

- **No metric is slower than 7a in all three runs.** I checked by comparing the 7a and 7b runs
  directly.
- **Faster than 7a in all three runs** (the atlas fix):

  | Metric (p95) | 7a | 7b |
  |---|---|---|
  | `frame_50_blurs_50_shadows` | 5.0–5.1 ms | 2.2–3.1 ms |
  | `frame_10_background_blurs` | 6.4–7.8 ms | 4.8–5.6 ms |

- **The gate against Stage 6 still flags the same misses as in 7a** (not new):
  - `full_layout_10k_cold/warm_text`;
  - `drag_artboard_per_frame` against our 8 ms gate (it is within the 16 ms target);
  - `find_query_100k` against our 5 ms gate;
  - `component_edit_1000_instances_to_frame` (open from 7a, run 1 only).
- **Noise:** the regression count against Stage 6 swings from 26 to 71 between identical runs.
  Run 1 was the noisiest. This is the same noise described in the 7a report.

## Tests added

- **`export/tests/svg_oracle.rs`:** every render fixture, exported to SVG, is drawn by resvg and
  compared with our renderer. Mean difference ≤ 1.1.
- **`export/tests/svg_import.rs`:**
  - Eight hand-written SVGs, imported and drawn by us, match resvg's rendering of the same
    files. Mean difference 0.02–1.14.
  - Every fixture, exported and then imported back, matches the original, except those whose
    warnings name a dropped filter, pattern or rotated image.
  - Fixtures with non-rectangular clips now pass through the image fallback, for example
    `strokes_align`, `clipping` and `blend_modes`.
- **`export/tests/pdf.rs`:** structure checks (fonts embedded, Unicode maps present, page
  boxes) plus content-stream goldens.
- **`export/tests/svg_import_mutations.rs`:** 1,200 deterministic byte mutations of the SVG
  fixtures. None panics, and every one that imports inserts valid nodes. This is the
  `svg_import` fuzz property, runnable on stable Rust. The libFuzzer target itself needs
  nightly, which isn't installed; `cargo check` on the fuzz crate passes.
- **`render/tests/async_images.rs`:**
  - frames draw a placeholder until the decode lands;
  - the waker fires when a decode finishes;
  - `frame_complete` waits for pending decodes;
  - without a waker, frames decode first.
- **`editor/tests/place.rs`:** SVG and images land centred or at the drop point, inside a
  selected frame, as one undo step, and identical image bytes are stored once.
- **`script/tests/api.rs`:** an SVG insert, its warning in the logs, and every export format.

## Checked by hand

- **PDFs in the browser's viewer (PDFium):** shadows, blurs, gradients, images, blend modes
  and text all look right. Text can be selected.
- **SVGs:** checked by the resvg oracle, an independent renderer, rather than by eye in a
  browser (the preview pane here doesn't open local SVG files). Opening a few from
  `target/svg-oracle/` in Chrome or Firefox is worth doing.
- **CLI:** wrote all five formats.
- **Not yet done:** pasting a PNG and an SVG from another app, and dropping a file onto the
  running app. Each piece is covered by tests, but not the round trip through the OS
  clipboard. It is quick to try:
  1. take a screenshot with Snipping Tool, then press Ctrl+V on the canvas;
  2. copy an `.svg` file's text, then press Ctrl+V;
  3. drag a `.png` onto the canvas.

## Not done in 7b (as planned, or noted above)

- Lossy WebP (needs a crate download).
- `<text>` in SVG export.
- SVG filters, masks and patterns on import.
- The `image/svg+xml` clipboard format.

**Next:** 7c (text editing on the canvas with IME, the pen tool and path editing,
accessibility). Waiting for your go-ahead.
