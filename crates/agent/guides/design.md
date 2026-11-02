# design

Practices that make screens look deliberate: spacing, type, colour, alignment, naming, design tokens, reuse and placement.

## Spacing

Use one scale for every gap and padding: **4, 8, 12, 16, 24, 32, 48, 64**. Related things sit
closer than unrelated ones: a label 4–8 px from its field, fields 16 px apart, sections 32–48 px
apart. Inside a card use 16–24 px padding; a phone screen has 16–24 px side margins, a desktop
page 32 px or more.

## Type

Pick a few sizes and reuse them, for example:

| Role | Size / weight | Line height |
|---|---|---|
| Display | 32–40 / 700 | 1.2× |
| Heading | 20–24 / 600 | 1.3× |
| Body | 14–16 / 400 | 1.5× |
| Label, caption | 12–13 / 500 | 1.4× |

Set `lineHeight` in pixels (`style: {fontSize: 16, lineHeight: 24}`) for multi-line text. One
font family (Inter) with weights for hierarchy is enough. Body text wraps best at 45–75
characters: give paragraphs `sizingH: "fill"` inside a column of limited width.

## Colour

- A neutral background (`#ffffff`, `#f9fafb`), neutral text (`#111827` primary, `#6b7280`
  secondary), one accent (`#2563eb`) for primary actions and links, and status colours only for
  status (`#16a34a` success, `#dc2626` error, `#d97706` warning).
- Contrast: body text needs at least 4.5:1 against its background, large text 3:1. Grey text on
  white should be no lighter than `#6b7280`.
- Borders and dividers are light (`#e5e7eb`); shadows are soft (`#0f172a14`–`#0f172a26`,
  blur 8–24, small y offset). Frames clip their children, so a shadow on a child is cut off at
  the parent's edge: give the parent `clip: false` or enough padding.
- One primary button per view; secondary actions are outlined or plain text.

## Alignment and structure

- Everything sits on the spacing scale and lines up with something. Auto layout does this for
  you; avoid free positioning in UI.
- Hit targets are at least 40 px tall on desktop and 44 px on touch screens.
- Corner radius is consistent: e.g. 6–8 for controls, 12–16 for cards.
- Common sizes: phone 390 × 844, tablet 834 × 1194, desktop 1440 × 900.

## Naming

Name every frame by what it is: `Login`, `Login/Form`, `Email field`, `Primary button`. Names
make `find({name: …})` work for you and for the next person. Keep layer order meaningful (top
to bottom in the tree is reading order in auto layout).

## Design tokens (variables)

Define colours and numbers once and bind properties to them, so a theme changes in one place.
`setVariables` takes collections by name. A collection has modes (e.g. Light and Dark); each
variable has a value per mode (missing modes use the first). A value can be an alias of another
variable: `{alias: "blue/600"}`. Calling it again for a collection replaces that collection,
keeping ids for names that stay, so bindings remain valid; other collections are untouched.
`getVariables()` returns them all in the same form, with ids added and `values` keyed by mode
name. (The file format keys values by mode id, as the `nodes` reference shows; scripts use
names.)

Bind a property path to a variable by name: `bindings: {"fills/0/color": "Theme/accent"}`
(`Collection/name`, or just the name when it is unique). A path names a property and the steps
into it: `fills/0/color`, `strokes/0/paint/color`, `style/color`, `opacity`, `radius`. Give the
property in the same call (`fills: ["#000"]` with the binding above, or any `style` for
`style/color`, since a partial style is completed). The engine then writes the variable's value
into the property, and rewrites it whenever the variable or the mode changes.

Modes apply: a page, frame, component or instance picks a mode per collection with
`modes: {"Theme": "Dark"}`, and its contents (instances included) take that mode's values. So a
dark variant of a screen is a copy with `modes` set; see `components` for an example.

```js
// label: Theme tokens
setVariables([{ name: "Theme", modes: ["Light", "Dark"], variables: [
  { name: "bg", type: "color", values: { Light: "#ffffff", Dark: "#111827" } },
  { name: "text", type: "color", values: { Light: "#111827", Dark: "#f9fafb" } },
  { name: "accent", type: "color", values: { Light: "#2563eb", Dark: "#60a5fa" } },
  { name: "radius", type: "number", values: { Light: 12 } },
] }]);
const card = insert(null, { type: "frame", name: "Themed card", width: 280, fills: ["#ffffff"], bindings: { "fills/0/color": "bg", radius: "radius" },
  autoLayout: { direction: "vertical", gap: 8, padding: 20 }, children: [
    { type: "text", text: "Tokens", style: { fontSize: 18, fontWeight: 600 }, bindings: { "style/color": "Theme/text" } },
    { type: "rectangle", height: 4, sizingH: "fill", fills: ["#2563eb"], bindings: { "fills/0/color": "accent" } },
  ] });
getVariables()[0].variables.map((v) => v.name)
```

## Reuse

Anything that appears more than twice (buttons, list rows, cards, inputs, nav items) should be
a component, with instances where it is used: one edit then updates them all, and overrides
change the text or colour per use. See `components`.

## Placing new work

- New screens go in free space: `const p = findEmptySpace(1440, 900); insert(null, {..., x: p.x,
  y: p.y})`. To place next to a screen, pass `near: {x, y}` (e.g. its right edge).
- Don't move or restyle what the user made unless asked. Read it first (`get` with `fields`),
  and match its spacing, colours and fonts when you add to it.
- After each change, `screenshot` the part you changed and check: alignment, overflow (text cut
  off or wrapping badly), contrast, consistent spacing.
