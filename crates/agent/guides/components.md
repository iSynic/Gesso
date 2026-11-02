# components

Reusable parts: a main component, instances of it with overrides, and themed variants through modes.

## Main components and instances

A `component` is a frame you reuse: build it once, then place `instance`s of it. An instance
shows the component's content at the instance's size; editing the component changes every
instance. Keep main components together, on a "Components" page or to the side of the screens.

- `insert(parent, {type: "instance", component: "Button"})`: `component` is the component's
  name (unique in the document) or id. Leave `width`/`height` out to use the component's size.
- `createComponent(ids, name?)` → id: turns nodes into a component. One frame is converted in
  place (same id); several nodes that share a parent are wrapped in a new component at their
  bounds.
- `detach(id)` → id: turns an instance into an ordinary frame with its content (same id), so it
  no longer follows the component.

When an instance is resized, its content follows the component's `constraints` and auto layout,
like a frame's children. An auto-layout component that hugs its content gives instances its
size when inserted; set `width`/`height` on the instance to change it.

## Overrides

An instance changes its content through **overrides**: properties of nodes inside the
component, set for this instance only. Write them by name, as a map from a node's **name path**
inside the component to properties (the same shorthands as `update`):

```js
// label: Button and instances
const btn = insert(null, { type: "component", name: "Button", fills: ["#2563eb"], radius: 8,
  autoLayout: { padding: [10, 16], gap: 8, crossAlign: "center" }, children: [
    { type: "ellipse", name: "Icon", width: 8, height: 8, fills: ["#fff"] },
    { type: "text", name: "Label", text: "Continue", style: { color: "#fff", fontWeight: 600 } },
  ] });
const row = insert(null, { type: "frame", name: "Actions", y: 80, autoLayout: { gap: 12 }, children: [
  { type: "instance", component: "Button", overrides: { "Label": { text: "Save" } } },
  { type: "instance", component: "Button", overrides: { "Label/text": "Delete", "": { fills: ["#dc2626"] }, "Icon/visible": false } },
] });
screenshot(row, { scale: 2 })
```

- A key is a name, or names joined by `/` going down (`"Card/Title"`); each step is the nearest
  node with that name below the previous one. `""` is the instance itself.
- `"Label/text": "Delete"` is short for `"Label": {text: "Delete"}`: when the whole key doesn't
  name a node, the last step is the property.
- An instance's `fills`, `strokes` and `radius` are its content's background: set them on the
  instance (`update(instance, {fills: [...]})` or the `""` key) and they override the
  component's. Opacity, effects, position and size are the instance's own.
- An override wins over a binding in the component: `"": {fills: ["#dc2626"]}` shows red even
  if the component's fill is bound to a variable (that binding is dropped for this instance).
  To keep a variant themed, override the binding instead:
  `"": {bindings: {"fills/0/color": "Theme/danger"}}` or `"Label": {bindings: {"style/color": "Theme/onDanger"}}`.
- Name the nodes inside a component so overrides can reach them; names must be unique enough
  that the path finds the right one.
- Overrides can change look and content (fills, text, visibility, style, size of a child), not
  structure: you can't add, remove or reorder children of an instance. Hide what you don't need
  (`visible: false`), or make a separate component.

## Reading and editing instance content

`get(instance, {depth: 3})` returns the content as children, laid out: the first child is the
content root (a frame at the instance's size), and below it the component's nodes. These nodes
have ids of their own (stable while the instance and component exist) and carry `instanceId`
(the instance showing them) and `instancePath` (the override target). `update` on one of those
ids writes an override; `move`, `remove` and `insert` into them fail.

`get(instance)` returns `component` as an id and `overrides` as stored: a list of
`{target, prop, value}` with `target` an id path (`[]` is the instance's look). A text `style`
or `spans` override is stored as a `kind` override holding the whole text node. `copy` of an
instance keeps its overrides.

`update(instance, {overrides: {...}})` adds or changes overrides by name; `overrides: []` (a
list) replaces them all, so `[]` resets the instance.

```js
const card = insert(null, { type: "component", name: "Stat", width: 200, fills: ["#fff"], radius: 12,
  autoLayout: { direction: "vertical", gap: 4, padding: 16 }, children: [
    { type: "text", name: "Label", text: "Label", style: { color: "#6b7280", fontSize: 13 } },
    { type: "text", name: "Value", text: "0", style: { fontSize: 24, fontWeight: 700 } },
  ] });
const a = insert(null, { type: "instance", component: card, x: 240, overrides: { Label: { text: "Revenue" }, Value: { text: "$12.4k" } } });
const content = get(a, { depth: 2, fields: ["name", "text"] }).children[0].children;
update(content[1].id, { style: { color: "#16a34a" } });
update(card, { radius: 16 }); // every Stat changes
get(a, { fields: ["overrides"] }).overrides.length
```

## Variants

There are no variant sets yet. Make one component per variant and name them as a family:
`Button/Primary`, `Button/Secondary`, `Button/Danger`; or one component whose differences are
overrides (colour, label, hidden icon). Prefer the second when the variants share a structure.

## Themes with modes

Variables (see `design`) have a value per mode. A page, frame, component or instance chooses a
mode per collection with `modes: {"Theme": "Dark"}`; everything inside it, including instance
content, uses that mode's values for its bindings. Nodes without a choice use their nearest
ancestor's, then the collection's default mode (its first, unless set). So a dark screen is the light screen with
`modes` set:

```js
// label: Themed screens
setVariables([{ name: "Theme", modes: ["Light", "Dark"], variables: [
  { name: "surface", type: "color", values: { Light: "#ffffff", Dark: "#111827" } },
  { name: "text", type: "color", values: { Light: "#111827", Dark: "#f9fafb" } },
  { name: "accent", type: "color", values: { Light: "#2563eb", Dark: "#60a5fa" } },
] }]);
insert(null, { type: "component", name: "Chip", x: -200, fills: ["#2563eb"], radius: 999, bindings: { "fills/0/color": "accent" },
  autoLayout: { padding: [4, 10] }, children: [{ type: "text", name: "Label", text: "New", style: { color: "#fff", fontSize: 12 } }] });
const light = insert(null, { type: "frame", name: "Screen/Light", width: 240, fills: ["#fff"], bindings: { "fills/0/color": "surface" },
  autoLayout: { direction: "vertical", gap: 12, padding: 24 }, children: [
    { type: "text", text: "Inbox", style: { fontSize: 20, fontWeight: 600 }, bindings: { "style/color": "text" } },
    { type: "instance", component: "Chip", overrides: { Label: { text: "3 new" } } },
  ] });
const dark = copy(light, null, { name: "Screen/Dark", x: 280, modes: { Theme: "Dark" } });
screenshot([light, dark], { scale: 1 })
```

To change one variable's value, edit what `getVariables()` returns and pass the collection back
to `setVariables` (a collection is replaced whole; ids are kept by name, so bindings stay).
`update(id, {modes: {Theme: null}})` removes a choice. `get` returns `modes` and `bindings` by id;
`getVariables()` maps ids to names.
