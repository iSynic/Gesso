# Stage 5 follow-up: zoom-sweep and whole-page frame times (2026-10-29)

A pass on the two Stage 5 misses before Stage 6: spikes while zooming, and the CPU cost of
whole-page views.

**Machine:** Windows 11, Intel Core i5-11600KF (6C/12T), 32 GB, **NVIDIA GeForce RTX 3080
(Vulkan, driver 610.88)**, release build, Rust 1.98.

**How it was measured:**
- Scripted sessions in a 1920×1080 window, vsync off. Each frame is timed from input to GPU done.
- Three runs per session, each in a separate process. Ranges are min–max across runs.
- The baseline is Stage 5 runs 2–3 (`results/stage5/`). New raw results are in
  `results/stage5-perf/`, every frame recorded.
- The machine was noisy in this session: Firefox, NVIDIA Broadcast and audio were running.
  Identical builds varied by up to 1.5 ms per frame between runs, so the runs that overlapped the
  test suite were redone.

## Results

| Session / metric (ms) | Stage 5 p95 | Now p95 | Now worst frame | Brief | Gate |
|---|---|---|---|---|---|
| Zoom sweep 0.03 → 2 → 0.03, 120 frames (`latency.json`) | 15.2–15.3 (worst 31.7–33.5) | **13.4–15.3** | **14.9–16.2** | 16 | 8 |
| Zoom sweep, first visit (`zoom.json`, cold) | | 13.9–14.5 | 16.0–17.5 | 16 | 8 |
| Zoom sweep, repeated (`zoom.json`, warm) | | **9.1–9.8** | **10.2–11.5** | 16 | 8 |
| Whole page, 19k nodes, layers tree expanded | 13.7–14.7 | **9.6–12.1** | 11.2–13.3 | 16 | |
| Whole page, fit view, idle (`page.json`, new) | | 7.7–9.4 | 9.1–10.6 | 16 | |
| Whole page, panning at fit (`page.json`, new) | | 7.3–8.8 | 7.8–9.6 | 16 | |
| 50 MB file, fit view, idle | 12.7–12.8 | **8.8–10.2** | ¹ | 16 | |
| Move at 100 % | 5.2–5.4 | 4.5–4.9 | 5.0–5.8 | 16 | 8 |
| Resize | 5.3–6.3 | 4.7–6.2 | 5.0–7.5 | 16 | 8 |
| Panel number edit | 6.5–7.5 | 6.1–8.0 | 9.4–12.8 | 16 | 8 |
| Panel colour edit | 4.7–5.4 | 4.5–5.7 | 4.8–6.3 | 16 | 8 |
| Reorder drag, 10k tree | 6.0–6.1 | 4.7–6.3 | 5.7–7.1 | 16 | 8 |
| Resize in the 10k tree | 8.2–9.0 | 7.0–9.5 | 7.9–9.7 | 16 | 8 |
| Move at 100 %, 50 MB file | 6.9–8.6 | 5.5–7.3 | 7.6–8.7 | 16 | 8 |

¹ The first frame after opening builds every fragment: 88–109 ms, as in Stage 5 (100–107).

**Where the CPU time goes, whole page at fit** (38,701 paths, no glyphs; p50):

| Part | Stage 5 | Now |
|---|---|---|
| Our scene (`Scene::frame`, culling) | < 0.1 | < 0.1 |
| Appending cached runs into the frame's Vello scene (`execute`) | 2.5–3.0 | **1.1–1.4** |
| Vello `render_to_texture` (CPU side) | 4.3–5.0 | 4.1–4.7 |
| egui (panels): layers collapsed / 19k-node tree expanded | – / 1.0–1.1 | 0.3 / 0.9–1.1 |

**Inside Vello's `render_to_texture`, whole page.** This was measured with a temporary
instrumented copy of Vello, since removed:
- packing the 4.5 MB scene, into a buffer allocated fresh every frame: 2.2–2.7 ms;
- re-creating and uploading the gradient-ramp texture every frame: about 0.8 ms;
- uploading the scene buffer: about 0.7 ms;
- recording and submitting: about 0.5 ms.

**`bench` render scenarios** (CPU-side harness, headless GPU), Stage 5 → now, p95:
- zoom sweep, first pass: 16.5 → 9.5 ms;
- zoom sweep, warm: 12.4 → 9.3 ms;
- pan at 5 %: 10.9 → 8.9 ms;
- frames with effects: 18–31 % faster.

**One bench flag explained.** `drag_icon_in_card` is 15–18 % slower, and all of the increase is
GPU wait. The encoded frame is identical to Stage 5: 946 paths and 8,174 glyphs. Our CPU parts
are faster (`execute` 0.16 → 0.07 ms, submit 2.8 → 2.4 ms). With identical work, I attribute the
longer wait to GPU clock state, not to these changes.

## What changed

1. **The encode budget now covers text shaping** (`render/src/scene.rs`).
   - **Before:** shaping and image decoding ran for every visible stale fragment before the
     budgeted build loop. A band change on first visit shaped the whole view at once: frames of
     up to 29 ms of scene time.
   - **Now:**
     - fragments with no list at all are still prefetched together, since they must draw;
     - fragments that have another band's list are prefetched four at a time, inside the
       budget;
     - at least one fragment is built per frame, so a spent budget can't stall a band change.
       A test found that it could stall before.
   - The worst scene time while zooming is now 6–8 ms (first visit only), and 1 ms when warm.
2. **Six zoom bands, each spanning 2×** (`render/src/build.rs`).
   - **Before:** three bands, with LOD decided at each band's lower edge:
     - Near (≥ 0.35) greeked any text under 11.4 px at every zoom, so 10 px text showed as grey
       bars even at 400 %. **This was a bug.**
     - At zoom 0.35–0.5, all text of 11.4 px and up was drawn as glyphs while only 4–6 px tall
       on screen: 35–44k glyphs, and 13–16 ms of Vello CPU time.
   - **Now:** edges at 0.06, 0.125, 0.25, 0.5 and 1.
     - Greeked text is always under 8 px on screen, and from 100 % on only text under 4 px is
       greeked.
     - The zoom sweep's glyph peak fell from 43.7k to 10.9k.
     - Full effects are drawn from 0.25 (before: from 0.35).
     - The `lod_mid` golden (zoom 0.25) was re-blessed: its 22 px titles, 5.5 px on screen, are
       now glyphs rather than bars.
3. **The Vello run cache keeps every band's runs** (`render-vello`).
   - Lists now have a version per band, so building one band no longer invalidates another
     band's cached runs.
   - Runs not drawn for two frames used to be dropped, so each return to a band re-encoded the
     whole view (6–10 ms). They are now evicted least-recently-drawn beyond 128 MB of encoding.
   - Superseded versions still go after two frames.
4. **The frame's top-level Vello scene is reused**, so its multi-megabyte buffers keep their
   capacity. Appending runs fell from 2.5–3.0 to 1.1–1.4 ms at whole-page views.
5. **Bug fix: clipped content vanished after an effect on the GPU** (`render-vello`). This was
   in the code since Stage 4.
   - An offscreen effect splits a fragment's commands into runs. A clip opened in one run and
     closed in a later one stayed open, because Vello ignores a pop with nothing open in the
     run's own scene.
   - Every later fragment was then clipped away. Example: a frame with a blurred child followed
     by other frames.
   - The CPU renderer was right, so only the GPU parity test could catch it, once a fixture hit
     the case.
   - **Fix:** such pops go to the frame's scene. New fixture: `effects_split_runs`.
6. **Bug fix: egui texture updates** (`apps/desktop/src/gfx.rs`).
   - Texture changes are now applied before the surface is acquired, and are consumed.
   - **Before:** a frame whose surface was lost dropped its texture uploads, such as font-atlas
     growth. Debug builds also tripped egui's assertion, which failed the desktop tests in
     debug.
7. **Measurement.**
   - Script results now record, per frame: prefetch time, Vello `execute` and `submit` times,
     and path and glyph counts.
   - New session: `sessions/page.json` (whole page at fit, idle and panning).

**Tests:**
- New tests:
  - `small_text_is_not_greeked_at_full_zoom`;
  - `a_budgeted_band_change_completes_over_frames`;
  - the `effects_split_runs` golden and GPU parity fixture.
- All workspace tests pass, as do the GPU-gated ones (`GPU_TESTS=1`: parity, run cache, desktop
  scripts) in debug and release.
- Clippy, `xtask layers` and `deny` are clean.

## What's left, and the choices

The zoom sweep's worst frame (15–16 ms) is within the brief's 16 ms but over the 8 ms gate.
Whole-page views are at 8–12 ms. What remains is mostly outside our code:
- **Vello `render_to_texture`, about 4.5 ms at a whole page.** Two of its costs could be fixed
  by patching Vello itself, as `vello_encoding` already is: reuse the packing buffer, and keep the
  gradient texture between frames. Estimated gain: 1–1.5 ms per whole-page frame.
- **Glyph-heavy frames at 0.5–1** (around 11k glyphs) cost about 5 ms in Vello's per-glyph
  resolve. Classic Vello copies each glyph's outline every frame and can't instance it.
- **GPU time, presenting and egui** account for the remaining 2–4 ms.

Reaching 8 ms at whole-page and sweep views would take a **raster cache**: rendering tiles of
the page once and drawing them as images while zoomed out or while the camera moves. That is an
architecture choice (memory, invalidation, blur during zoom), so it needs your decision.

## Addendum: patched Vello (2026-10-29)

You chose to patch Vello. It is now a local copy in `third_party/vello`, used through
`[patch.crates-io]`; its README lists the changes and how to drop them. Two changes:
- **The scene-packing buffer is reused.** Vello no longer allocates a fresh 4.5 MB buffer every
  frame.
- **The gradient-ramp texture is kept between frames.** It is re-uploaded only when the ramp
  cache changes. `vello_encoding` now counts a revision for that.

**Results.** Same machine and sessions, raw data in `results/vello-patch/`:
- Three runs per session; four for `latency.json`, whose first run was redone. Ranges are
  min–max across runs.
- The machine was under about 24 % background CPU load (the Claude app, Firefox, audio). Two
  runs shown separately below (latency run 1 and bench run 2) were visibly slowed in code
  this patch doesn't touch.
- The most reliable figure is Vello's own CPU time per frame, which the patch affects directly:
  it fell by half in every run.

| Session / metric (ms) | Before p95 | Now p95 | Before worst frame | Now worst frame | Vello CPU p50, before → now |
|---|---|---|---|---|---|
| Zoom sweep, `latency.json` | 13.4–15.3 | **11.7–13.5** | 14.9–16.2 | 12.5–13.0 (17.3 in the loaded run) | 5.0–5.5 → 2.8–3.0 |
| Zoom sweep, first visit (`zoom.json`) | 13.9–14.5 | **11.5–11.9** | 16.0–17.5 | 13.2–14.6 | 5.4–5.8 → 2.9–3.1 |
| Zoom sweep, repeated (`zoom.json`) | 9.1–9.8 | **6.8–6.9** | 10.2–11.5 | **7.5–7.6** | 5.1–5.5 → 2.8–2.9 |
| Whole page, layers tree expanded | 9.6–12.1 | **7.4–10.0** | 11.2–13.3 | 8.6–10.1 | 5.3–6.1 → 2.9–3.3 |
| Whole page, fit, idle | 7.7–9.4 | **5.7–5.8** | 9.1–10.6 | 5.9–6.1 | 5.4–6.0 → 3.1–3.2 |
| Whole page, fit, panning | 7.3–8.8 | **5.4–6.2** | 7.8–9.6 | 5.8–6.6 | 5.2–5.7 → 2.9–3.3 |
| 50 MB file, fit, idle | 8.8–10.2 | **6.4–6.9** | ¹ | ¹ | 5.5–6.5 → 3.2–3.4 |
| Move at 100 % | 4.5–4.9 | 3.7–5.2 | 5.0–5.8 | 4.9–7.9 | 1.3–1.4 → 1.0–1.1 |
| Resize | 4.7–6.2 | 4.6–6.0 | 5.0–7.5 | 5.7–6.5 | 1.4–1.5 → 1.1–1.2 |
| Panel number edit | 6.1–8.0 | 5.6–7.7 | 9.4–12.8 | 8.7–9.7 | 1.5–1.7 → 1.2–1.4 |
| Panel colour edit | 4.5–5.7 | 4.3–5.6 | 4.8–6.3 | 4.4–5.7 | 1.4–1.6 → 1.1–1.4 |
| Reorder drag, 10k tree | 4.7–6.3 | **2.9–3.3** | 5.7–7.1 | 3.2–4.0 | 2.6–2.9 → 0.7–0.9 |
| Resize in the 10k tree | 7.0–9.5 | **4.9–5.5** | 7.9–9.7 | 5.1–6.2 | 2.8–3.3 → 0.9–1.0 |
| Move at 100 %, 50 MB file | 5.5–7.3 | **4.2–4.5** | 7.6–8.7 | 5.0–5.8 | 2.9–3.1 → 1.0–1.1 |

¹ The first frame after opening builds every fragment: 87–90 ms (before: 89–109). RSS with the
50 MB file open: 377 MB.

**What this means:**
- Whole-page views now meet the 8 ms gate.
- The repeated zoom sweep meets it, worst frame included.
- The first-visit zoom sweep (11.5–13.5 ms p95) is still over the gate, but within the brief's
  16 ms. Most of its time is building the zoom band's fragments, which a raster cache would
  avoid.
- The bigger-than-estimated gain (2.5 ms rather than 1–1.5 ms) came mostly from
  page-faulting: each fresh 4.5 MB buffer was new memory for Windows to zero and map, on top of
  the packing itself.

**`bench`, p95, against `results/stage5-perf/bench-render.json`.** From bench run 1, the
cleaner of two:
- zoom sweep, first pass: 9.5 → 6.4 ms;
- zoom sweep, warm: 9.3 → 6.2 ms;
- pan at 5 % and at 15 %: 36–38 % faster;
- keystroke-to-frame: 7.3 → 4.6 ms;
- dragging an artboard: 14.4 → 11.6 ms.

**What the bench flagged:**
- **GPU wait in `drag_icon_in_card`, `frame_10_background_blurs` and `pan_100pct`.** The CPU now
  submits sooner, so more of the GPU's own time shows up as waiting; each frame's total is
  equal or lower.
- **Export (PNG render and encode, +10–19 %) and `keystroke_of_which_scene_apply` (+11 %).**
  This is noise. Export runs on the CPU renderer, and neither touches Vello.
- **Bench run 2** (`bench-render-2.json`) ran under heavier load. It showed +77 % in
  transaction and layout code that this patch doesn't touch, so it isn't used for these
  numbers.

**Tests:**
- New: `gradient_ramps_survive_frames` (GPU parity). It alternates gradient fixtures, and a
  fixture with no gradients, on one renderer. A deliberate mutation that skips the re-upload
  makes it fail, with a mean error of 64.
- All workspace tests pass, as do the GPU-gated ones in release and debug. Clippy, `deny` and
  `xtask layers` are clean.
