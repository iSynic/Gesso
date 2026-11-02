# nodes

The node types and every property they take, generated from the document schema.

A node is a JSON object with a `type`, an optional `name`, the common properties, the properties
of its type and, for containers, `children`. Leave out anything you don't need: every property
has a default (`opacity` 1, `visible` true, no fills, …).

## Which type to use

| Type | Use it for |
|---|---|
| `frame` | Screens, cards, buttons, rows: anything that contains other nodes, has a background or uses auto layout. Clips its children (`clip`, default true). |
| `text` | All text. `text: "…"` for one style; `spans` for mixed styles. |
| `rectangle`, `ellipse`, `polygon`, `line` | Shapes: dividers, avatars, dots, bars, icons. |
| `path` | Vector icons and chart lines, from SVG path data in local coordinates. |
| `group` | Several nodes moved together without a box of their own (its size follows its children). |
| `component`, `instance` | A reusable main component and its instances with overrides (see `components`). An instance has no `children` of its own: `component` names its component (by name or id) and `overrides` changes its content. |
| `image` | A bitmap from the document's assets (agents can't add assets yet). |
| `page` | A top-level canvas; insert with `insert(null, {type: "page", name})`. |
| `note` | A sticky note for comments. |

## Examples

```js
// label: Shapes and text
const row = insert(null, { type: "frame", name: "Shapes", autoLayout: { gap: 16, padding: 16, crossAlign: "center" }, fills: ["#fff"], children: [
  { type: "ellipse", name: "Avatar", width: 40, height: 40, fills: ["#c7d2fe"] },
  { type: "rectangle", name: "Bar", width: 120, height: 8, radius: 4, fills: ["#2563eb"] },
  { type: "polygon", name: "Star", sides: 5, starInnerRatio: 0.45, width: 24, height: 24, fills: ["#f59e0b"] },
  { type: "line", name: "Rule", width: 60, strokes: [{ paint: "#9ca3af", weight: 2 }] },
  { type: "path", name: "Check", width: 24, height: 24, paths: [{ d: "M4 12 L10 18 L20 6" }],
    strokes: [{ paint: "#16a34a", weight: 2.5, cap: "round", join: "round" }] },
  { type: "text", name: "Label", spans: [{ text: "Mixed " }, { text: "styles", fontWeight: 700, color: "#2563eb" }], style: { fontSize: 16 } },
] });
screenshot(row, { scale: 2 })
```

```js
// label: Gradient, shadow and a donut chart
const card = insert(null, { type: "frame", name: "Hero", width: 360, height: 200, radius: 16,
  fills: [{ type: "linearGradient", start: [0, 0], end: [1, 1], stops: [{ offset: 0, color: "#6366f1" }, { offset: 1, color: "#ec4899" }] }],
  effects: [{ type: "dropShadow", color: "#0f172a33", offset: [0, 8], blur: 24 }],
  children: [
    { type: "ellipse", name: "Track", x: 240, y: 60, width: 80, height: 80, arc: { start: 0, sweep: 360, innerRatio: 0.7 }, fills: ["#ffffff40"] },
    { type: "ellipse", name: "Value", x: 240, y: 60, width: 80, height: 80, arc: { start: -90, sweep: 250, innerRatio: 0.7 }, fills: ["#fff"] },
  ] });
get(card, { depth: 1, fields: ["name", "bounds"] })
```

Notes:
- `radius` is one number or `[topLeft, topRight, bottomRight, bottomLeft]`.
- Paints stack in order (first at the back). Gradient `start`/`end`/`center` are in the node's
  unit square: `[0, 0]` top-left, `[1, 1]` bottom-right.
- A stroke's `align` is `inside` (default), `center` or `outside`. Strokes go all the way
  round; there are no per-side borders. For a divider on one side, use a 1 px rectangle
  (`sizingH: "fill"`, `height: 1`) as a sibling in a vertical auto layout, or a 1 px `gap` over
  a grey parent fill (see `recipes`).
- `ellipse` `arc`: angles in degrees, 0° at 3 o'clock, positive `sweep` clockwise; so a donut
  segment starting at the top has `start: -90`.
- `path`: `paths[].d` is SVG path data in the node's own coordinates (0,0 is its top-left).
  `width`/`height` are its box. Setting them in a script doesn't scale the path, so size the
  box to the data, and write new `paths` to reshape it. People resizing a path in the app, and
  layout resizing one (constraints, auto layout), do scale its data with the box.
- Text `letterSpacing` and `lineHeight` are in pixels. Text has no padding: wrap it in an
  auto-layout frame to pad or align it.
- `effects`: `dropShadow` and `innerShadow` (`color`, `offset` [x, y], `blur`, `spread`),
  `layerBlur` and `backgroundBlur` (`radius`).
- Text: `style` is the base style; each span may override `fontFamily`, `fontSize`, `fontWeight`
  (100–900), `italic`, `color`, `decoration` and `link`. Fonts available headless: Inter (any
  weight); the app also has the system fonts.
- `autoResize` (text): `widthAndHeight` grows to fit one line, `height` (fixed width, wraps)
  or `none` (fixed box).

## Reference

<!-- generated: properties -->
