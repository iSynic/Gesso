# layout

Auto layout, sizing (fixed, hug, fill), alignment, wrapping, grids and constraints.

Use auto layout for anything with rows, columns or padding: cards, buttons, lists, forms, nav
bars, whole screens. The layout is computed when you insert or update, so `get(id).bounds` and
screenshots in the same script already show the result.

## Auto layout

Give a `frame` (or `component`) an `autoLayout` object:

| Field | Values | Default |
|---|---|---|
| `direction` | `"horizontal"`, `"vertical"`, `"grid"` | `"horizontal"` |
| `gap` | space between children (between columns in a grid) | 0 |
| `crossGap` | space between wrapped lines, or between grid rows | 0 |
| `padding` | `16`, `[vertical, horizontal]`, or `[top, right, bottom, left]` | 0 |
| `mainAlign` | `"start"`, `"center"`, `"end"`, `"spaceBetween"` (along the direction) | `"start"` |
| `crossAlign` | `"start"`, `"center"`, `"end"`, `"stretch"`, `"baseline"` | `"start"` |
| `wrap` | `true` to wrap a horizontal row onto more lines | false |
| `columns`, `rows` | grid tracks: `{fr: 1}`, `{fixed: 120}` or `"auto"` | |

Children are placed in order; their `x`/`y` are ignored. To pin one child anywhere inside the
frame (a badge, a close button), give it `layoutChild: {absolute: true}` and an `x`/`y`.

## Sizing

Each node has `sizingH` (width) and `sizingV` (height):

- `"fixed"`: uses `width`/`height` as given.
- `"hug"`: shrinks to fit the content. For auto-layout frames (their children plus padding) and
  text.
- `"fill"`: stretches to fill the parent's auto layout. Along the parent's direction it shares
  the free space with other `fill` children (`layoutChild: {grow: 2}` takes a double share);
  across it, it takes the full width/height inside the padding.

Typical combinations:
- A screen: fixed width and height. A card in a list: `sizingH: "fill"`, `sizingV: "hug"`.
- A button: hug both ways, with padding.
- Text that should wrap inside a card: `sizingH: "fill"` (it wraps at the card's inner width and
  grows in height).
- A spacer that pushes the rest to the end of a row: an empty frame with `sizingH: "fill"` (or
  use `mainAlign: "spaceBetween"`).

`minWidth`, `maxWidth`, `minHeight`, `maxHeight` bound any of these.

```js
// label: Toolbar row
const bar = insert(null, { type: "frame", name: "Toolbar", width: 480, fills: ["#fff"], strokes: ["#e5e7eb"],
  autoLayout: { direction: "horizontal", gap: 8, padding: [12, 16], crossAlign: "center" }, children: [
    { type: "text", name: "Title", text: "Projects", style: { fontSize: 18, fontWeight: 600 } },
    { type: "frame", name: "Spacer", sizingH: "fill", height: 1 },
    { type: "frame", name: "Search", width: 160, height: 32, radius: 6, strokes: ["#d1d5db"],
      autoLayout: { padding: [0, 10], crossAlign: "center" }, children: [{ type: "text", text: "Search", style: { color: "#9ca3af" } }] },
    { type: "frame", name: "New", radius: 6, fills: ["#111827"], autoLayout: { padding: [8, 12] },
      children: [{ type: "text", text: "New project", style: { color: "#fff", fontWeight: 600 } }] },
  ] });
get(bar, { depth: 1, fields: ["name", "bounds"] })
```

## Grids

`direction: "grid"` lays children out in `columns` (and optional `rows`) in reading order.
`gap` separates columns and `crossGap` rows. A child with `sizingH: "fill"` fills its cell's
width; `layoutChild: {gridCell: [column, row]}` (0-based) places it in a given cell.

```js
// label: Stat grid
const grid = insert(null, { type: "frame", name: "Stats", width: 600, fills: ["#f9fafb"],
  autoLayout: { direction: "grid", columns: [{ fr: 1 }, { fr: 1 }, { fr: 1 }], gap: 16, crossGap: 16, padding: 16 },
  children: ["Revenue", "Orders", "Visitors", "Refunds", "Margin", "Churn"].map((label) => ({
    type: "frame", name: label, sizingH: "fill", radius: 8, fills: ["#fff"], strokes: ["#e5e7eb"],
    autoLayout: { direction: "vertical", gap: 4, padding: 16 }, children: [
      { type: "text", text: label, style: { fontSize: 12, color: "#6b7280" } },
      { type: "text", text: "1,234", style: { fontSize: 24, fontWeight: 700 } },
    ] })) });
get(grid, { fields: ["bounds"] })
```

## Nesting

Real screens are columns of rows of columns. Build the tree in one `insert`; each level gets
its own direction, gap and padding. Keep spacing consistent (see `design`).

## Without auto layout

Children of a plain frame keep their `x`/`y`. When that frame is resized, `constraints`
(`{h: "left" | "right" | "leftRight" | "center" | "scale", v: "top" | "bottom" | "topBottom" |
"center" | "scale"}`) say how each child follows. Use plain frames for free-form art (charts,
illustrations) and auto layout for UI.

```js
const chart = insert(null, { type: "frame", name: "Chart", width: 300, height: 160, fills: ["#fff"], children:
  [40, 90, 65, 120, 80].map((h, i) => ({ type: "rectangle", name: "Bar " + i, x: 20 + i * 56, y: 140 - h, width: 36, height: h, radius: [4, 4, 0, 0], fills: ["#3b82f6"] })) });
update(chart, { height: 200 });
get(chart, { depth: 1, fields: ["name", "y"] }).children[0]
```
