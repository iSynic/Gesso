# Stage 3 report: layout and text, with the benchmark harness (2026-10-29)

Deliverable 3 from the brief. Measured on the same machine as before: Windows 11, Intel Core
i5-11600KF (6C/12T), 32 GB, release build, Rust 1.98. Text scenarios use bundled Inter plus the
system's fallback fonts, as the app will. Raw results: `results/stage3/run{1,2,3}.json` (plus
`.log`), three full runs in separate processes. Ranges are min–max across runs. No GPU numbers
yet (Stage 4).

## What was built

| Path | What it holds |
|---|---|
| `crates/doc` (changed) | `Deriver` hook on `Store`: derived ops join the transaction's undo step; a failure rolls the transaction back; undo/redo only call `observe`. `GenConfig.auto_layout_heavy` adds a 10k-node auto-layout page |
| `crates/text` (≈0.4k lines) | Font database with bundled Inter (OFL-1.1) plus system fonts; shaping with Parley; size cache and full-layout cache; parallel measuring; caret, hit-test and selection geometry; missing-family and RTL reporting |
| `crates/text/fonts/` | `InterVariable.ttf`, `InterVariable-Italic.ttf` (Inter 4.1, unmodified), `OFL.txt` |
| `crates/layout` (≈1.0k lines) | `LayoutEngine` trait; `TaffyLayout` (per-root mirror trees, incremental restyle); `semantics.rs` (design semantics → Taffy styles); constraints and group bounds; `LayoutDeriver`, which writes results back |
| `crates/bench` (≈0.6k lines) | Harness CLI: `list`, `run --scenario --out --baseline --threshold --floor`. Six scenarios behind a `Scenario` trait. Replaces `crates/format/examples/stage2_bench.rs` (removed) |
| `xtask` | `cargo xtask layers`: fails if a crate depends on anything but a lower layer (`doc` < `text` < `layout` < `format` < `bench`, `xtask`); every crate must be placed |
| `NOTICE`, `docs/licenses.md` | Product-workspace license list (regenerated) plus the bundled font. The spike list moved to `docs/licenses-spikes.md` |
| `.github/workflows/ci.yml` | Adds `xtask layers`, 5,000 layout property cases, and a bench gate that compares a pull request with its base on the same runner. Still inactive |

### How layout works

Full description in `docs/architecture.md` §5. In short:

- **Write-back:** at the end of each transaction the deriver lays out what changed and writes
  `x`/`y`/`width`/`height` as ops in the same transaction.
- **Passes:** it repeats until nothing changes (typical edits settle in 2 passes).
- **Tolerance:** values are compared within 1/100 px before writing, so a settled document gets
  no ops and re-saves byte-identically.
- **Auto layout** runs on Taffy, one mirror tree per auto-layout root:
  - mirrors are built on first use;
  - property edits are incremental;
  - structural edits rebuild that root's mirror.

## Benchmarks

**Against the brief's targets** (p95 unless noted, ms):

| Scenario | Result | Brief | Internal gate |
|---|---|---|---|
| Open a laid-out 50 MB document end to end (read, parse, build, measure all text in parallel, lay out everything) | **678–728** | < 2,000 | |
| … memory added by that open | **165–175 MB** | < 1 GB | |
| Re-layout of a 10k-node auto-layout tree: root resized (everything re-wraps) | **7.4–9.7** | < 50 | ≤ 15 ✓ |
| … full layout from scratch, text sizes cached (mirror build + Taffy) | 29.7–45.0 (p50 27–28) | < 50 ✓ | ≤ 15 ✗ |
| … full layout from scratch, cold text (first shaping of 5,003 paragraphs) | 178–197 | (see note) | |
| Keystroke in a text inside that tree, in a 110k-node document, end to end | **0.27–0.43** | | ≤ 2 ✓ |
| Fixed-size leaf resized inside that tree | 0.28–0.42 | | ≤ 2 ✓ |
| Undo / redo of the root resize | 6.8–7.3 / 7.2–9.9 | < 50 | ≤ 15 ✓ |
| Insert / delete a card in that tree (mirror rebuild) | 34–40 / 33–43 (p50 28) | < 50 ✓ | |
| Plain frame with 200 constrained children resized | 0.04–0.05 | | ≤ 2 ✓ |
| Property that layout doesn't read (opacity) | 0.001 | | |

**Open, broken down** (50 MB, 64.5k nodes, p50 ms):

| Step | Time |
|---|---|
| Read and parse | 262–283 |
| Build and validate | 27–30 |
| Measure all text (27.8k sizes, parallel) and lay out 2,700 auto-layout roots | 364–385 |
| **Total** | **657–700** |

The opened document's stored positions all matched the computed layout (0 differences), so
opening writes nothing.

**Text** (10k distinct labels, cold, p50 ms; runs 1–2, run 3 below):

| | Time |
|---|---|
| Sequential | 129–132 |
| Parallel (12 threads) | 47–50 |
| All cached | 0.3 |

**Stage 2 scenarios, ported** (same metrics as `docs/stage2-report.md`; these show no change):
- update one property: 1 µs p50;
- 500-op transaction: 0.11–0.12 ms;
- open 50 MB without layout: 273–296 ms;
- open 100k nodes without layout: 420–495 ms.

**Notes on these numbers:**
- **Run 3 was disturbed** at the end: its `edit` scenario's first full layout (3.2 s vs 1.9 s) and
  its `text` scenario (2–3.6× slower) are outliers I can't attribute to the code. Ranges above
  include run 3 except the text table. The other scenarios of run 3 agree with runs 1–2.
- **Cold full layout (178–197 ms) is shaping, not layout.** It is the first shaping of 5,003
  multi-line paragraphs on one thread inside Taffy's measure callback. In the app this cost is
  paid at open, in parallel (`prepare`), which is the 364–385 ms step above for a far bigger
  document. Warm full layout is the figure to compare with the brief.
- **Full layout from scratch misses my 15 ms internal gate** (27–28 ms p50).
  - About 8 ms is building the 10k-node Taffy mirror.
  - About 17 ms is Taffy computing layout for a tree with 40 grid sections and 5,000 text leaves
    (about 35k leaf measure calls, answered from a per-leaf memo).
  - The spike's 9–11 ms was the same shape but with arithmetic stand-ins for text and without
    building the tree.
  - A full layout only happens on open or explicitly; every edit measured is incremental.
- **First layout of an unlaid document** (1.2 s for 64.5k nodes, 1.9 s for 110k) writes
  135k–241k values in one transaction. Generated test documents need this once; documents saved by
  this engine don't.

## Tests

**59 tests pass** (28 from Stage 2, plus the deriver test in `doc`). New in Stage 3:

- **text (7):**
  - deterministic sizes with bundled Inter;
  - spans change metrics;
  - wrapping;
  - RTL and a missing family reported, with fallback still shaping;
  - caret ↔ hit-test round trip and selection rectangles;
  - cache hits, and parallel equals sequential;
  - measuring without keeping glyphs.
- **layout fixtures (16)**, hand-computed rects:
  - row with gap and padding, hugging;
  - fill sharing and cross-axis stretch, then reflow on resize;
  - space-between and centre;
  - wrap with a cross gap;
  - grid with fixed and fr tracks plus a placed cell;
  - absolute child;
  - min/max clamping fill;
  - nested hug propagating outward;
  - text hugging with Inter, then widening when edited;
  - standalone text height;
  - all five constraint modes, then undo;
  - fill frame driving constraints inside it;
  - group bounds;
  - one undo step, settled documents don't churn, edited positions snap back;
  - hidden children leave the flow;
  - typing reflows the 10k tree and matches a full layout.
- **layout unit (2):** constraint math and rect union.
- **layout property test** (the key invariant):
  - random sequences of up to 25 edits: sizes, sizing modes, visibility, absolute/grow, auto
    layout added, changed or removed, inserts of rects, text, frames and groups, deletes, moves,
    span edits;
  - after every edit, a fresh deriver's full layout writes nothing;
  - undo restores and redo reapplies the exact document;
  - **passed 20,000 cases**. Default 256; CI 5,000.
- **2 regression tests** for the two bugs that only showed at 10,000+ cases.
- **format round trip (2):** the all-kinds fixture and a generated auto-layout document, laid
  out, saved, re-opened, fully relaid out (0 changes) and re-saved byte-identically. Computed
  positions are in `document.json`.

**Other checks:**
- clippy `-D warnings` clean;
- `cargo deny check licenses` ok;
- `cargo xtask layers` ok, and it fails on a deliberately added `layout → format` dependency;
- **bench gate** behaves as intended:
  - `--baseline run1,run2,run3` passes for `ops` and `edit`;
  - a baseline with every p95 halved (a 2× slowdown) exits 1 and lists the regressions.

## Bugs found and fixed along the way

The property test found five layout bugs; the first two make the layout itself never settle:

1. **`grow` on a fixed-size child ratcheted forever.** The grown width was written back, became the
   next fixed width, and grew again. `grow` is now a weighted fill (basis 0).
2. **A group set to fill fought its own bounds fit.** A group's size now always comes from its
   children; flow may only position it.
3. **Removing auto layout from a root orphaned the auto-layout frames inside it.** They became
   roots that nothing laid out.
4. **A sizing change on text didn't update whether it wraps.**
5. **A node edited while hidden wasn't snapped back when shown.** Its cached rect was stale.

Two found while building:
- **"Inter" didn't resolve:** the variable font registers as "Inter Variable", so every text
  silently measured 0 wide. "Inter" now maps to the bundled face.
- **Leaves came out zero-sized:** Taffy 0.15's measure callback replaces the whole leaf layout,
  so leaves must go through `compute_leaf_layout`.

Two performance problems were also fixed:
- **Memory:** keeping full Parley layouts for every text cost **654 MB** at open. Splitting the
  cache into sizes (for layout) and full layouts (for drawing, capped) brought it to 165 MB.
- **Open time:** measuring text at min-content width in the parallel prefill took the 50 MB open
  from 1.0 s to 0.66 s.

## Deviations and open items

- **The bench gate uses the slowest of several baselines.** Gating on one stored run failed on
  noise (`copy_5_artboards` +17%; its p95 ranges 1.1–1.6 ms across runs). `--baseline` now takes
  several files and uses the slowest p95 per metric. Use all three Stage 3 runs.
- **Structural edits rebuild the whole mirror** (28 ms p50 to insert a card in a 10k tree). That
  is within the brief, but long enough to drop a frame on paste or duplicate. Inserting and
  removing subtrees in the Taffy tree directly is the fix; I suggest doing it before Stage 5's
  canvas interactions.
- **Full layout from scratch is 27 ms against my 15 ms internal gate** (brief: < 50 ms, met).
- **Not handled yet:**
  - rotation inside auto-layout flow (ignored);
  - constraints on absolute children of auto-layout frames;
  - instance contents (Stage 7; instances are fixed-size leaves).
- **Group bounds ignore children's rotation.** They use the unrotated boxes.
- **Fonts on other machines:** "Inter" is always the bundled face. Other families come from the
  system, so documents using them can measure differently elsewhere; missing families are reported
  per text for the UI to flag.
- **The layout semantics were written from public flexbox/grid behaviour and common design-tool
  conventions.** FigCraft's implementation was not consulted, and nothing was copied.

## Next: Stage 4 (needs your go-ahead)

Renderer behind the `Renderer` trait: Vello on the GPU, `vello_cpu` as the reference renderer, and
golden-image tests. Render scenarios join the harness with GPU info in the host block.
