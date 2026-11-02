# api

Every function a script can call, with its parameters, return value and errors.

All functions are globals. `delete` and `export` are reserved words in JavaScript, so those two
are `remove(id)` and `exportImage(...)` (also `doc.delete` and `doc.export`). Ids are strings.
A function that fails throws an `Error` whose message starts with its name.

## Creating and changing

**`insert(parentId, node, index?)` → id.** Adds `node` (with its `children`, as deep as you like)
under `parentId`, or on the current page when `parentId` is `null`. `index` is the position among
the siblings (default: last, i.e. on top). Returns the new root's id. Pages are inserted with
`insert(null, {type: "page", name: "…"})`. `insert(parentId, {type: "svg", svg: "<svg…>", name?,
x?, y?})` imports SVG source as a clipping frame of path nodes (text outlined; filters, masks
and patterns dropped, and listed in the logs as `svg import: …`).

**`update(id, props)` → null.** Changes properties. `style` (text), `autoLayout`, `bindings`
and `modes` merge into the existing values, so `update(t, {style: {fontSize: 18}})` keeps the font and
colour. Other properties replace (`fills: [...]` replaces all fills). `text: "…"` replaces a
text node's content.

**`replace(id, node)` → id.** Swaps a node for a new one in the same place.

**`move(id, parentId, index?)` → null.** Reparents (`null`: the current page). Inside the new
parent the node keeps its `x`/`y`.

**`copy(id, parentId?, overrides?, index?)` → id.** Duplicates a node and its children with new
ids, under `parentId` (default: the same parent), then applies `overrides` like `update`.

**`remove(id)` → null.** Deletes a node and its children.

**`createComponent(ids, name?)` → id** and **`detach(id)` → id.** Make a component from nodes,
and turn an instance back into a frame; see `components`.

Shorthands accepted wherever nodes or properties are written:
- `fills: ["#2563eb"]` for solid fills; `strokes: ["#e5e7eb"]` for a 1 px solid stroke, or
  `strokes: [{paint: "#e5e7eb", weight: 2}]`;
- colours as `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`;
- `text: "…"` for a text node's content, and a partial `style` (unset fields come from the
  defaults: Inter, 14 px, regular, black);
- `autoLayout.padding` as one number, `[vertical, horizontal]`, or `[top, right, bottom, left]`;
- `bindings: {"fills/0/color": "Theme/primary"}`: variables by name; `modes: {"Theme": "Dark"}`:
  collections and modes by name;
- instances: `component: "Button"` by name, and `overrides: {"Label": {text: "Buy"}}` by name
  path inside the component (see `components`).

New nodes get defaults where you leave a size out: text hugs its content, an auto-layout frame
hugs its children, and shapes or plain frames are 100 × 100.

```js
// label: Card with a button
const card = insert(null, { type: "frame", name: "Card", width: 320, fills: ["#fff"], radius: 12,
  autoLayout: { direction: "vertical", gap: 12, padding: 24 }, children: [
    { type: "text", name: "Title", text: "Upgrade", sizingH: "fill", style: { fontSize: 20, fontWeight: 600 } },
    { type: "frame", name: "Button", fills: ["#2563eb"], radius: 8, autoLayout: { padding: [10, 16] },
      children: [{ type: "text", text: "Continue", style: { color: "#fff", fontWeight: 600 } }] },
  ] });
const title = find({ name: "Title", within: card })[0];
update(title, { text: "Upgrade to Pro", style: { color: "#111827" } });
const second = copy(card, null, { name: "Card 2", x: 400 });
get(second, { fields: ["name", "bounds"] })
```

## Reading

**`get(id, {depth?, fields?})` → node or `null`.** One node as JSON: `id`, `type`, `name`, its
properties as `insert` takes them, `text` (text nodes), `parentId`, `bounds` ({x, y, width,
height} in page coordinates, after layout) and `childIds`. `depth: 1` (or more) nests the
children as full nodes instead. `fields: ["name", "bounds"]` returns only those (plus `id` and
`type`); use it to keep results small. An instance's children are its expanded content, with
`instanceId` and `instancePath` on each node (see `components`). Properties at their default are left out (e.g. an
`autoLayout` with the default `mainAlign` has no `mainAlign`; read it as
`n.autoLayout.mainAlign ?? "start"`). `bindings` come back as variable ids; `getVariables()`
maps ids to names.

**`get(query, options?)` → array of nodes.** The nodes matching a query (below).

**`find(query)` → array of ids.** Like `get(query)` but only the ids.

**`find(fn)` → array of ids.** Calls `fn(summary)` for every node on the current page and keeps
those where it returns true. A summary has `id`, `type`, `name`, `x`, `y`, `width`, `height`,
`visible`, `parentId`, `bounds` and, for text, `text`. Prefer a query object: it's much faster.

A **query** is an object; every condition given must hold:

| Field | Matches |
|---|---|
| `type` | a type or a list: `"text"`, `["frame", "component"]` |
| `name` | exact name, or a pattern with `*`: `"Card *"` |
| `text` | text content containing this (case-insensitive) |
| `within` | descendants of this node id |
| `page` | `"current"` (default), `"all"`, or a page id |
| `visible` | `true` or `false` |
| `limit` | at most this many (default 1000) |
| `id` | that one node |

```js
insert(null, { type: "frame", name: "Card A", autoLayout: { padding: 16 }, children: [{ type: "text", text: "Revenue" }] });
insert(null, { type: "frame", name: "Card B", y: 150, autoLayout: { padding: 16 }, children: [{ type: "text", text: "Users" }] });
({
  cards: find({ type: "frame", name: "Card *" }).length,
  revenue: get({ text: "revenue" }, { fields: ["text", "parentId"] }),
  wide: find((n) => n.type === "text" && n.width > 40).length,
})
```

## Placing and looking

**`findEmptySpace(width, height, {near?, margin?})` → {x, y}.** A spot on the current page where
a `width` × `height` box is at least `margin` (default 100) away from every top-level node.
Without `near` it is to the right of all content, top-aligned; with `near: {x, y}` it is the
free spot closest to that point.

**`screenshot(ids?, {scale?, padding?})` → {image, width, height}.** Renders nodes (one id or a
list; `null` for the whole page) on the current page to a PNG that is attached to the result.
`scale` is pixels per unit (default 1, capped so the longest side is at most 2048 px).

**`exportImage(ids, format, path, {scale?, padding?, quality?})` → {path, width, height, bytes}.**
Writes a file: `format` is `"png"`, `"jpeg"` (on white; `quality` 1–100, default 90), `"webp"`
(lossless), `"svg"` (text as outlines) or `"pdf"` (vector, with selectable text). `ids` is one
id, a list, or `null` for the whole page. Up to 16,384 px per side. The path must be inside the
allowed folders: for the MCP server its `--root` folders (default: the folder it started in);
for `canvas`, the current folder, the `--root` folders and the document's folder; for the app,
the folder it started in and the document's folder. Relative paths start in the first of them.

`screenshot` captures the box of the nodes (plus `padding`), so anything overlapping that box
appears too, over the page background.

```js
const spot = findEmptySpace(390, 844);
const screen = insert(null, { type: "frame", name: "Phone", x: spot.x, y: spot.y, width: 390, height: 844, fills: ["#f9fafb"] });
screenshot(screen, { scale: 0.5 })
```

## Pages, selection, variables, output

- `page` (the current page id), `selection` (ids), `viewport` ({x, y, width, height, zoom}):
  read-only globals.
- `setPage(id)`: switch the current page (queries and `insert(null, …)` use it).
- `select(ids)`: select nodes in the app (the user sees them highlighted); `select([])` clears.
- `setVariables(collections)` / `getVariables()`: design tokens (see `design`).
- `print(...values)`: adds a line to `logs` in the result.

```js
const p = insert(null, { type: "page", name: "Mobile" });
setPage(p);
const n = insert(null, { type: "text", text: "On page 2" });
select(n);
({ page, selection })
```

## Results and errors

The result of `execute` has `value` (the last expression, as JSON), `logs`, `created` (new ids),
`changes`, `undoLabel`, `ms`, and any screenshots as images. `changes` counts nodes inserted
(a tree counts once), deleted and moved, and property updates, including the positions and
sizes the layout computed, so one `insert` of a screen can report hundreds of updates. A thrown error rolls the
whole script back; see `start` for the error format and retries.
