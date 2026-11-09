# Patched third-party crates

## `vello_encoding` 0.11.0

A copy of the crates.io release of `vello_encoding` 0.11.0 (MIT OR Apache-2.0, © the Vello
Authors; license files included), used through `[patch.crates-io]` in the workspace
`Cargo.toml`.

**Changes:**
- `RETAINED_COUNT` in `src/ramp_cache.rs` is raised from 64 to 4096.
- `Ramps` gains a `revision` field, bumped whenever the ramp cache's data changes. The patched
  `vello` below uses it to skip re-uploading unchanged ramps.
- `BumpSizes` (in `src/config.rs`) holds the sizes of the buffers the GPU allocates into while
  rendering, which upstream fixes. `RenderConfig::with_sizes` and `BufferSizes::with_sizes` take
  them; `new` uses upstream's sizes. Added 2026-11-09 for the robust memory patch below.

**Why:** upstream keeps at most 64 gradient colour ramps between frames. A scene that draws more
distinct gradients than that rebuilds most of its ramps every frame, at about 10 µs each. The
Stage 4 pan benchmark draws about 2,400 distinct gradients at 5 % zoom, so Vello spent about
23 ms of CPU per frame on ramps. With the patch it is under 1 ms. See `docs/stage4-report.md`.

**To drop it:** delete this folder and the `[patch.crates-io]` entry once an upstream Vello
release caches more ramps, or lets the caller set the count.

## `vello` 0.11.0

A copy of the crates.io release of `vello` 0.11.0 (Apache-2.0 OR MIT, © the Vello Authors;
license files included), also used through `[patch.crates-io]`. Added 2026-10-29, after the
Stage 5 performance pass.

**Changes** (in `src/render.rs`, `src/lib.rs`, `src/util.rs` and `src/wgpu_engine.rs`, marked
`Local patch`):
- **The packing buffer is reused.** Upstream packs the scene into a new `Vec` every frame; at a
  whole-page view that is 4.5 MB of fresh, page-faulting memory, about 2.2–2.7 ms. The renderer
  now keeps the buffer in a `Persistent` struct and takes it back from the recording after it
  runs.
- **The gradient-ramp texture is kept between frames.** Upstream creates, uploads and destroys
  it every frame (about 0.8 ms with 2k ramps). It is now re-created only when the ramp cache's
  `revision` or size changes. Frames without gradients keep the old texture bound.
- **The image atlas never shrinks** (added in Stage 7b). Upstream recreates the atlas texture
  whenever the size it needs changes; frames that draw no images ask for 1×1, so the first frame
  with images after them recreated the full atlas, a 30–50 ms stall (seen while images decode in
  the background and placeholders are drawn first). An atlas at least as large as needed is now
  kept. The fine shader clamps atlas reads to each image's own extent, so the extra space is
  never sampled.
- **Robust dynamic memory** (added 2026-11-09). Upstream gives the GPU fixed-size buffers for
  tiles, path segments and per-tile command lists, and a frame that needs more draws nothing at
  all. In a large window at mid zoom on a real document (2400×1350 at 42 % and 50 %), the canvas
  went blank while the app's overlays kept moving, so frame titles seemed to slide off their
  frames. Now `render_to_texture` copies each frame's bump counters back without waiting.
  `Renderer::poll_memory` reads the ones that have arrived; when a frame overflowed, it grows
  the buffers (to a power of two with a quarter to spare, capped by the device's limits) and
  returns true so the caller draws again. `memory_pending` says a check is still in flight.
  Buffers never shrink. Stages run out one after another, so a heavy frame can take two or three
  redraws to settle. `util::RenderContext` now requests the adapter's own storage-buffer limits
  instead of the 128 MiB default. `render_full` and `render_encoding_full` were removed as
  unused.
- The public `low_level::Render::render_encoding_coarse` keeps its signature and frees the ramp
  texture after the fine pass, as before.

**Test:** `gradient_ramps_survive_frames` in `crates/render-vello/tests/parity.rs` alternates
fixtures with different gradients, and none, on one renderer and compares each with the CPU
renderer. It fails if the texture is not re-uploaded when the ramps change.
`a_frame_that_overflows_vellos_buffers_is_redrawn_whole` in the same file draws a frame that
needs more than the starting buffers, checks that `poll_memory` reports it, and compares the
settled frame with the CPU renderer.

**To drop it:** delete this folder and its `[patch.crates-io]` line once upstream reuses these
resources. The `vello_encoding` `revision` field goes with it.
