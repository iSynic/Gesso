# Stage 2 report: core document crate (2026-10-28)

Deliverable 2 from the brief: the document model, operations, undo/redo, the JSON format with a
schema, and tests.

Measured on the same machine as Stage 1: Windows 11, i5-11600KF (6C/12T), 32 GB, release build,
Rust 1.98. Raw results are in `results/stage2/bench_run{1,2,3}.json`, from three runs in separate
processes. Where a figure differs between runs I give the range.

## What was built

| Path | What it holds |
|---|---|
| `Cargo.toml` | Product workspace: edition 2024, MIT OR Apache-2.0, `unsafe` forbidden, `unwrap`/`expect`/`panic` denied in library code. `spikes/` stays a separate workspace |
| `crates/doc` (≈2.9k lines) | Ids, property types, node kinds, variables, the document arena with validation, primitive ops with inverses, transactions with rollback, undo/redo with coalescing, commit observers, JSON commands for agents, and a seeded document generator (feature `testing`) |
| `crates/format` (≈0.3k lines) | `document.json` read/write, the `.canvas` folder bundle with content-addressed assets and atomic save, migrations, and the JSON Schema |
| `crates/format/schema/document.v1.schema.json` | Generated schema. A test fails if it is stale |
| `fuzz/` | Two cargo-fuzz targets: `open_json` (arbitrary bytes as a file) and `apply_ops` (arbitrary op lists). They compile; running them needs nightly |
| `.github/workflows/ci.yml` | Clippy, tests, 5,000 property cases, licenses and 2 min of fuzzing per target. Manual trigger only, not active (no remote) |
| `docs/file-format.md` | Human-readable spec of format v1 |

## Benchmarks

**Document ops on a 100,000-node document** (`stage2_bench`, ms):

| Operation | p50 | p95 |
|---|---|---|
| Update one property (in its own transaction, with undo entry) | 0.0008–0.0010 | 0.0022–0.0040 |
| `update_json` with two properties (parse, validate, apply) | 0.003 | 0.0035–0.0042 |
| One transaction of 500 updates | 0.12–0.13 | 0.17–0.21 |
| Undo of that transaction / redo | — | 0.13–0.18 / 0.15–0.17 |
| Move a card to another artboard | 0.0014–0.0015 | 0.012–0.017 |
| Delete a card, then undo | — | 0.016–0.019 |
| Deep copy of a 901-node subtree (new ids) | 0.94–0.96 | 1.13–1.45 |
| Build 100k nodes through 12.6k transactions | 148–158 total | |

Criterion (CI regression gate):

| Bench | Time |
|---|---|
| `update_one_prop_100k` | 480 ns |
| `tx_500_updates_then_undo_100k` | 140 µs |
| `from_data_10k` | 9.0 ms |
| `to_json_10k` | 27 ms |
| `from_json_10k` | 43 ms |

Edit cost doesn't depend on document size; arena and hash lookups are O(1). For comparison,
FigCraft measured 4 ms per property edit at 100k nodes.

**Files** (generated documents with realistic cards: text, images, a component instance with an
override, variables):

| Document | `document.json` | Compact JSON | Open, p50 (p95) ms | Parse / build and validate | Save | Memory added |
|---|---|---|---|---|---|---|
| **50 MB** (64,502 nodes) | 49.9 MB | 17.8 MB | **282–351** (296–508) | 252–296 / 30–49 | 0.4–0.76 s | 57 MB |
| 100k nodes | 77.4 MB | 27.7 MB | 436–476 (468–517) | 389–421 / 47–49 | 0.35–0.50 s | 87 MB |
| 115k nodes, long text | 91.9 MB | 34.8 MB | 517–589 (543–787) | 464–534 / 58–63 | 0.37–0.47 s | 106 MB |

**Against the brief:**
- Open a 50 MB document in under 2 s: met, at 0.51 s worst case.
- Memory under 1 GB: met for the document model, at 57 MB. Layout, text and render caches will
  add to this in later stages.
- Parsing dominates the open time. It runs at about 190 MB/s and is single-threaded. If later
  stages need the headroom, parsing pages in parallel or avoiding serde's `flatten` buffering are
  easy wins.

## Tests

**28 tests pass:**
- 4 unit tests (ids),
- 14 behaviour tests (ops, rules, undo, components, variables, assets, commands),
- 2 property tests,
- 8 format tests.

The two property tests check:
- **Random transactions:** random mixes of valid and invalid inserts, deletes, updates, moves and
  copies across pages, components and instances. After every transaction the invariants hold, and
  a failed transaction changes nothing. Undoing everything restores each earlier state exactly,
  and redoing reaches the end state.
- **Random documents:** JSON → arena → JSON round-trips byte-for-byte.
- They passed 20,000 cases locally, against 256 by default. CI is set to 5,000.

The format tests cover:
- byte-identical re-save,
- a schema freshness check,
- a fixture with every node kind, paint, effect and variable type, validated against the schema,
- the v0 → v1 migration,
- clear errors for bad JSON, newer versions, bad ids and structure,
- the bundle with asset deduplication and missing-asset reporting,
- an exact expected-output test for readability.

Other checks:
- `cargo clippy --workspace --all-targets -D warnings` is clean.
- `cargo deny check licenses` passes. New dependencies: slotmap (Zlib), schemars, getrandom,
  rustc-hash, thiserror, sha2, imagesize (all MIT/Apache); proptest, criterion, jsonschema,
  tempfile and memory-stats are dev-only.

## Bugs found and fixed along the way

- **Hand-written ids that start with a letter above `7` overflow 128 bits.** The fixture had them.
  The schema pattern now requires `^[0-7]…` and the error message says why.
- **Property-test bookkeeping:** a transaction that inserts and then deletes still makes an undo
  step. Behaviour is correct; the test was fixed to expect it.

## Deviations and open items

- **Fuzzing was not run here**, because there is no nightly toolchain on this machine. The targets
  compile, the property tests cover the same ground, and the CI workflow runs libFuzzer for 2 min
  per target.
- **The file loader accepts unknown fields.** serde's `flatten` ignores them, so a typo in a
  hand-edited file is dropped silently. The agent commands do reject unknown fields. Proposal: a
  strict mode for the CLI's `validate` command in Stage 6, using the schema.
- **No fractional indices yet.** Moves are index-based, which is enough until sync.
- **No `xtask layers` check yet.** There are only two crates; it arrives with the third.
- **Instance override resolution** is stored but not evaluated yet (Stage 7, as planned).

## Decision for you (optional, reversible)

Pretty-printing makes files **2.8× larger**: 100k nodes is 77 MB instead of 28 MB. It's what you
chose for readable diffs, and opening stays well inside the budget.

Any JSON reader accepts either form, so this can change later without a format version bump. An
alternative is one node per line, which diffs as cleanly at roughly compact size. I've kept 2-space
pretty-printing unless you say otherwise.

## Next: Stage 3 (needs your go-ahead)

Layout (Taffy behind `LayoutEngine`, design-semantics mapping, incremental dirty propagation from
`ChangeSet`) and text (Parley: shaping cache and parallel shaping at open), with the benchmark
harness built from the spike scenarios.
