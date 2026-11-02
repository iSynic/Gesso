# File format, version 2

Status: **v2, 2026-10-29** (v1 decided 2026-10-28; v2 changes override targets and adds
`modes`, see "Versions and migration"). The machine-readable schema is
`crates/format/schema/document.v2.schema.json`. A test keeps it in sync with the Rust types:
`UPDATE_SCHEMA=1 cargo test -p format` regenerates it. An example that uses every node kind is
`crates/format/tests/fixtures/all-kinds.canvas/`.

## Bundle

A document is a folder whose name ends in `.canvas`:

```
My Design.canvas/
  document.json                 the whole document (UTF-8 JSON)
  assets/<sha256>.<ext>         images, named by the SHA-256 of their bytes
```

Assets:
- Assets are **content-addressed**. Identical images are stored once, and a file never changes
  after it is written.
- `ext` follows the MIME type: `png`, `jpg`, `webp`, `gif` or `svg`.
- The writer never deletes asset files. An unreferenced asset is harmless; a cleanup command will
  come later.
- If a listed asset file is missing, the document still opens, and the reader reports the asset as
  missing.

Saving and conventions:
- **Saving is atomic.** The writer writes `document.tmp`, flushes it to disk, then renames it over
  `document.json`.
- No compression, no encryption and no binary sections.

## `document.json`

```json
{
  "formatVersion": 2,
  "generator": "ai-design-engine 0.1.0",
  "pages": [ { "id": "…", "type": "page", "name": "Home", "children": [ … ] } ],
  "variables": [ … ],
  "assets": { "<sha256>": { "mime": "image/png", "width": 640, "height": 400 } }
}
```

Writing conventions. These make the file diff cleanly, and re-saving an unchanged document gives
byte-identical output:
- `formatVersion` is always the first key.
- Keys appear in a fixed order (the order the Rust structs declare them).
- Field names are camelCase.
- **A field equal to its default is omitted.** For example `opacity: 1`, `visible: true`,
  `rotation: 0` and empty lists don't appear. Readers fill in defaults for missing fields.
- Pretty-printed with a 2-space indent, LF line endings and a trailing newline. Numbers use the
  shortest form that round-trips, so `100` is written `100.0`.

Ordering and coordinates:
- **Pages and children are listed in z-order.** The first child is drawn first, at the back.
- Units are CSS pixels.
- `x`/`y` are relative to the parent's top-left corner.
- `rotation` is in degrees, clockwise, around the node's centre.

## Ids

- Every node, variable, collection and mode has a **128-bit random id**, written as **22 base62
  characters** (`0-9A-Za-z`), e.g. `3rXcZxKUPc288GrMSPbDIq`.
- An id must be at most 2^128−1, so the first character is `0`–`7`.
- Ids are stable for the node's whole life: across saves, undo and copy-paste between documents.
  A copy gets new ids.
- Tests and agents can request deterministic ids from a seed.

## Nodes

Every node has:
- `id` and `type`,
- an optional `name`,
- the common properties below,
- the fields for its type,
- `children` (only for containers).

| `type` | Container | Type-specific fields |
|---|---|---|
| `page` | yes; top level only | `background` |
| `frame` | yes | `autoLayout`, `clip` (default true) |
| `group` | yes | — |
| `component` | yes | `autoLayout`, `description` |
| `instance` | no (its content comes from the component) | `component` (id), `overrides` [`{target, prop, value}`] |
| `rectangle`, `line` | no | — |
| `ellipse` | no | `arc` {`start`, `sweep`, `innerRatio`} |
| `polygon` | no | `sides` (≥ 3), `starInnerRatio` |
| `path` | no | `paths` [{`d`: SVG path data, `fillRule`}] |
| `text` | no | `spans` [{`text`, style overrides, `link`}], `style`, `align`, `verticalAlign`, `autoResize` |
| `image` | no | `asset`, `fit` (`fill`/`fit`/`crop`/`tile`) |
| `note` | no | `text` |

**Common properties:**

| Group | Fields |
|---|---|
| Position and size | `x`, `y`, `rotation`, `flipX`, `flipY`, `width`, `height` |
| Sizing | `sizingH`/`sizingV` (`fixed`/`hug`/`fill`), `minWidth`, `maxWidth`, `minHeight`, `maxHeight`, `constraints` {`h`, `v`} |
| Appearance | `radius` (number or `[tl, tr, br, bl]`), `fills`, `strokes`, `effects`, `opacity`, `blend`, `visible`, `locked` |
| Layout and tokens | `layoutChild` {`absolute`, `grow`, `alignSelf`, `gridCell`}, `bindings`, `modes` (page, frame, component, instance only) |

**Value types:**

- **Paint:** `{type: solid|linearGradient|radialGradient|angularGradient|image, …, opacity, visible, blend}`.
  Gradient geometry is given in the node's unit square.
- **Colour:** `#rrggbb` or `#rrggbbaa` (sRGB, 8 bits per channel).
- **Stroke:** `{paint, weight, align: inside|center|outside, dash, cap, join}`.
- **Effects:** `dropShadow` / `innerShadow` {`color`, `offset`, `blur`, `spread`}, `layerBlur` /
  `backgroundBlur` {`radius`}.
- **Auto-layout:** `{direction: horizontal|vertical|grid, gap, crossGap, padding [t, r, b, l], mainAlign, crossAlign, wrap, columns, rows}`.
  Grid tracks are `{fixed: n}`, `{fr: n}` or `"auto"`.
- **Bindings:** a map from a property path (`opacity`, `fills/0/color`, `style/color`, `text`)
  to a variable id. The first step must be a node property; a deeper step that doesn't fit the
  node (a missing fill, a value of the wrong type) leaves the binding dormant.
- **Modes:** a map from a variable collection id to one of its mode ids. A node uses the
  nearest choice on itself or an ancestor (crossing into instances), else the collection's
  default mode.
- **Overrides** on instances: `target` is an **id path**. Each step names a node inside the
  component reached so far: the first is inside the instance's component; if it is a nested
  instance, the next step is inside that instance's component, and so on. `[]` is the instance
  itself. `prop`/`value` are as in an update op. A (target, prop) pair appears at most once.
  An override of a property wins over the component's binding of it: the binding is dropped
  for that instance (a `kind` override drops bindings into kind fields, e.g. `style/color`).
  An override of `bindings` rebinds instead. An instance's `fills`, `strokes` and `radius` are
  its content root's, so they are stored as `[]` overrides.

### How the fields render (Stage 4)

The renderers (`render`, `render-cpu`, `render-vello`) read the fields this way. A reader that
draws the format should do the same.

- **Transform:** `x`/`y` are relative to the parent. Rotation and flips are applied about the
  node's centre.
- **Colour:** blending and gradient interpolation use sRGB-encoded values, as CSS does.
- **Blur:** a `blur` (shadows) or `radius` (layer and background blur) of B is a Gaussian with
  **σ = B / 2**, the CSS convention.
- **Drop shadow:** the blurred silhouette of what the node draws, offset by `offset`, grown by
  `spread`, tinted with `color`. A transparent fill casts nothing.
- **Inner shadow:** the inverse of the shape's alpha, shrunk by `spread`, offset, blurred, tinted
  and clipped to the shape.
- **Background blur:** blurs what is already drawn under the node, then draws the node on top.
- **Strokes:** `center` straddles the outline. `inside` and `outside` lie wholly inside or
  outside it, at the full `weight`. Open paths and lines are always drawn centred.
- **Line:** runs from (0, height / 2) to (width, height / 2).
- **Polygon and star:** points on the ellipse inscribed in the box, the first at the top.
- **Image fit:** `fill` covers the box (cropped, centred), `fit` is contained in it (letterboxed),
  `crop` stretches the image to the box, `tile` repeats it at its native size.
- **Radius:** per-corner radii are clamped so adjacent corners never overlap.
- **Frame:** with `clip`, children are clipped to the frame's shape. The frame's strokes draw
  after its children.
- **Instance:** shows its component's content with the overrides applied, laid out at the
  instance's size (constraints and auto layout as in the component), clipped to it.
- **Bindings:** the file stores each bound property's resolved value too, so a reader that
  doesn't evaluate variables still draws what the editor shows.

## Variables

```json
"variables": [{
  "id": "…", "name": "Theme",
  "modes": [{"id": "…", "name": "Light"}, {"id": "…", "name": "Dark"}],
  "defaultMode": "…",
  "variables": [{"id": "…", "name": "surface", "type": "color",
                 "values": {"<light id>": "#ffffff", "<dark id>": {"alias": "<variable id>"}}}]
}]
```

- **Types:** `color` (hex string), `number`, `string` and `boolean`.
- A mode with no value falls back to the collection's default mode.
- Aliases may cross collections. Alias cycles and type mismatches are rejected on load.

## Validation on load

The document is rejected, with a message naming the node, if:
- an id is duplicated or malformed,
- a page is not at the top level, or a non-page is,
- a non-container has children,
- an instance points at something that isn't a component,
- a component contains an instance of itself, directly or through other components,
- an asset or a bound variable is unknown, or a binding path doesn't start at a node property,
- `modes` names an unknown collection or mode, or sits on a kind that doesn't take it,
- an instance overrides the same (target, prop) twice,
- path data is invalid,
- a polygon has fewer than 3 sides,
- a number is not finite.

## Versions and migration

- A reader parses older files as untyped JSON, applies the migration steps in order
  (`crates/format/src/migrate.rs`), and only then reads them into the current types.
- A file with no `formatVersion` is treated as version 0, the pre-release layout. Version 0 used
  `nodes` where v1 uses `pages`, and `cornerRadius` where v1 uses `radius`.
- **v1 → v2:** an override `target` id becomes a one-step path (`"L1"` → `["L1"]`); a target
  naming the component root becomes `[]`. The `modes` field is new and optional. Fixture:
  `crates/format/tests/fixtures/v1.json`.
- Files from a **newer** version are refused with a clear error rather than half-read.
- Any change to the format raises `formatVersion` and adds a migration step and a fixture.

## Size and speed (measured, see `docs/stage2-report.md`)

- About 810 bytes per node pretty-printed; 64.5k realistic nodes make a 50 MB file.
- Opening that file takes 0.28–0.35 s (p50 over runs) and adds 57 MB of memory.
- Compact (unindented) JSON would be 2.8× smaller.
