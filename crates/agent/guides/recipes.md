# recipes

Complete, working examples to adapt: a form, a settings list, a nav bar and a dashboard.

Each one is a single `execute` call. They define small helper functions that return node
objects, then insert one tree. Copy the helpers you need; change names, text and colours.

## Shared helpers

Text and button helpers keep styles consistent. (Each recipe below repeats the ones it uses, so
it runs on its own.)

```js
const text = (t, size = 14, weight = 400, color = "#111827", extra = {}) =>
  ({ type: "text", name: t.slice(0, 24), text: t, style: { fontSize: size, fontWeight: weight, color }, ...extra });
const button = (label, primary = true) => ({
  type: "frame", name: label + " button", radius: 8, fills: [primary ? "#2563eb" : "#ffffff"],
  strokes: primary ? [] : ["#d1d5db"], autoLayout: { padding: [10, 16], mainAlign: "center", crossAlign: "center" },
  children: [text(label, 14, 600, primary ? "#ffffff" : "#111827")],
});
const b = insert(null, button("Save"));
get(b, { fields: ["bounds"] }).bounds
```

## Sign-in form

```js
// label: Sign-in screen
const text = (t, size = 14, weight = 400, color = "#111827", extra = {}) =>
  ({ type: "text", name: t.slice(0, 24), text: t, style: { fontSize: size, fontWeight: weight, color }, ...extra });
const field = (label, placeholder) => ({
  type: "frame", name: label + " field", sizingH: "fill", autoLayout: { direction: "vertical", gap: 6 }, children: [
    text(label, 13, 500, "#374151"),
    { type: "frame", name: "Input", sizingH: "fill", height: 44, radius: 8, fills: ["#ffffff"], strokes: ["#d1d5db"],
      autoLayout: { padding: [0, 12], crossAlign: "center" }, children: [text(placeholder, 14, 400, "#9ca3af")] },
  ],
});
const spot = findEmptySpace(390, 844);
const screen = insert(null, { type: "frame", name: "Sign in", x: spot.x, y: spot.y, width: 390, height: 844, fills: ["#f9fafb"],
  autoLayout: { direction: "vertical", padding: [96, 24], gap: 32 }, children: [
    { type: "frame", name: "Header", sizingH: "fill", autoLayout: { direction: "vertical", gap: 8 }, children: [
      text("Welcome back", 28, 700),
      text("Sign in to continue to your workspace.", 15, 400, "#6b7280", { sizingH: "fill" }),
    ] },
    { type: "frame", name: "Form", sizingH: "fill", autoLayout: { direction: "vertical", gap: 16 }, children: [
      field("Email", "you@example.com"),
      field("Password", "••••••••"),
      text("Forgot password?", 13, 500, "#2563eb"),
      { type: "frame", name: "Sign in button", sizingH: "fill", height: 48, radius: 8, fills: ["#2563eb"],
        autoLayout: { mainAlign: "center", crossAlign: "center" }, children: [text("Sign in", 15, 600, "#ffffff")] },
    ] },
    { type: "frame", name: "Footer", sizingH: "fill", autoLayout: { mainAlign: "center", gap: 4 }, children: [
      text("New here?", 13, 400, "#6b7280"), text("Create an account", 13, 600, "#2563eb"),
    ] },
  ] });
screenshot(screen, { scale: 0.5 })
```

## Settings list

Rows with a label, a description and a control on the right; sections with headings.

```js
// label: Settings page
const text = (t, size = 14, weight = 400, color = "#111827", extra = {}) =>
  ({ type: "text", name: t.slice(0, 24), text: t, style: { fontSize: size, fontWeight: weight, color }, ...extra });
const toggle = (on) => ({ type: "frame", name: on ? "Toggle on" : "Toggle off", width: 44, height: 24, radius: 12,
  fills: [on ? "#2563eb" : "#d1d5db"], autoLayout: { padding: 2, mainAlign: on ? "end" : "start" },
  children: [{ type: "ellipse", name: "Knob", width: 20, height: 20, fills: ["#ffffff"] }] });
const row = (title, desc, on) => ({ type: "frame", name: title, sizingH: "fill", fills: ["#ffffff"],
  autoLayout: { padding: [14, 16], gap: 16, crossAlign: "center" }, children: [
    { type: "frame", name: "Text", sizingH: "fill", autoLayout: { direction: "vertical", gap: 2 }, children: [
      text(title, 14, 500), text(desc, 13, 400, "#6b7280", { sizingH: "fill" })] },
    toggle(on),
  ] });
const section = (heading, rows) => ({ type: "frame", name: heading, sizingH: "fill", autoLayout: { direction: "vertical", gap: 8 }, children: [
  text(heading.toUpperCase(), 12, 600, "#6b7280"),
  { type: "frame", name: heading + " rows", sizingH: "fill", radius: 12, strokes: ["#e5e7eb"], fills: ["#e5e7eb"],
    autoLayout: { direction: "vertical", gap: 1 }, children: rows },
] });
const spot = findEmptySpace(640, 600);
const page = insert(null, { type: "frame", name: "Settings", x: spot.x, y: spot.y, width: 640, fills: ["#f9fafb"],
  autoLayout: { direction: "vertical", gap: 32, padding: 32 }, children: [
    text("Settings", 24, 700),
    section("Notifications", [row("Email", "News and product updates.", true), row("Push", "Mentions and replies.", false)]),
    section("Privacy", [row("Public profile", "Anyone can see your profile.", true), row("Analytics", "Share anonymous usage data.", false)]),
  ] });
screenshot(page, { scale: 0.5 })
```

The divider lines come from the list's grey fill showing through a 1 px `gap` between white
rows, so they stay right when rows are added.

## Nav bar

```js
// label: Top nav
const text = (t, size = 14, weight = 400, color = "#111827") => ({ type: "text", name: t, text: t, style: { fontSize: size, fontWeight: weight, color } });
// The bottom divider is a 1 px rectangle under the bar (strokes would box all four sides).
const nav = insert(null, { type: "frame", name: "Nav", width: 1440, fills: ["#ffffff"], autoLayout: { direction: "vertical" }, children: [
  { type: "frame", name: "Bar", sizingH: "fill", height: 64, autoLayout: { padding: [0, 32], gap: 32, crossAlign: "center" }, children: [
    { type: "frame", name: "Logo", autoLayout: { gap: 8, crossAlign: "center" }, children: [
      { type: "rectangle", name: "Mark", width: 24, height: 24, radius: 6, fills: ["#2563eb"] }, text("Acme", 16, 700)] },
    { type: "frame", name: "Links", autoLayout: { gap: 24 }, children: [
      text("Dashboard", 14, 600), text("Projects", 14, 500, "#6b7280"), text("Team", 14, 500, "#6b7280")] },
    { type: "frame", name: "Spacer", sizingH: "fill", height: 1 },
    { type: "ellipse", name: "Avatar", width: 32, height: 32, fills: ["#c7d2fe"] },
  ] },
  { type: "rectangle", name: "Divider", sizingH: "fill", height: 1, fills: ["#e5e7eb"] },
] });
get(nav, { depth: 2, fields: ["name", "bounds"] }).children[0].children.map((c) => c.name + " @ " + c.bounds.x)
```

## Dashboard

A sidebar, a header, a row of stat cards and a bar chart drawn from rectangles.

```js
// label: Analytics dashboard
const text = (t, size = 14, weight = 400, color = "#111827", extra = {}) =>
  ({ type: "text", name: t.slice(0, 24), text: t, style: { fontSize: size, fontWeight: weight, color }, ...extra });
const card = (name, children, extra = {}) => ({ type: "frame", name, sizingH: "fill", radius: 12, fills: ["#ffffff"],
  strokes: ["#e5e7eb"], autoLayout: { direction: "vertical", gap: 8, padding: 20 }, children, ...extra });
const stat = (label, value, delta, up) => card(label, [
  text(label, 13, 500, "#6b7280"), text(value, 28, 700),
  text((up ? "▲ " : "▼ ") + delta, 12, 600, up ? "#16a34a" : "#dc2626"),
]);
const data = [32, 45, 38, 52, 61, 48, 70, 66, 74, 58, 80, 92];
const chart = { type: "frame", name: "Bars", sizingH: "fill", height: 200,
  autoLayout: { gap: 12, crossAlign: "end" },
  children: data.map((v, i) => ({ type: "rectangle", name: "Bar " + (i + 1), sizingH: "fill", height: v * 2, radius: [4, 4, 0, 0],
    fills: [i === data.length - 1 ? "#2563eb" : "#bfdbfe"] })) };
const navItem = (label, active) => ({ type: "frame", name: label, sizingH: "fill", radius: 8, fills: active ? ["#eff6ff"] : [],
  autoLayout: { padding: [8, 12] }, children: [text(label, 14, active ? 600 : 500, active ? "#1d4ed8" : "#374151")] });
const spot = findEmptySpace(1440, 900);
const dash = insert(null, { type: "frame", name: "Dashboard", x: spot.x, y: spot.y, width: 1440, height: 900, fills: ["#f9fafb"],
  autoLayout: { direction: "horizontal" }, children: [
    { type: "frame", name: "Sidebar", width: 240, sizingV: "fill", fills: ["#ffffff"], strokes: ["#e5e7eb"],
      autoLayout: { direction: "vertical", gap: 4, padding: [24, 16] }, children: [
        text("Acme", 18, 700, "#111827", { name: "Logo" }), { type: "frame", name: "Gap", width: 1, height: 16 },
        navItem("Overview", true), navItem("Reports", false), navItem("Customers", false), navItem("Settings", false)] },
    { type: "frame", name: "Main", sizingH: "fill", sizingV: "fill", autoLayout: { direction: "vertical", gap: 24, padding: 32 }, children: [
      { type: "frame", name: "Header", sizingH: "fill", autoLayout: { crossAlign: "center" }, children: [
        text("Overview", 24, 700), { type: "frame", name: "Spacer", sizingH: "fill", height: 1 },
        text("Last 30 days", 13, 500, "#6b7280")] },
      { type: "frame", name: "Stats", sizingH: "fill", autoLayout: { gap: 16 }, children: [
        stat("Revenue", "$48,210", "12.4%", true), stat("Orders", "1,284", "3.1%", true),
        stat("Visitors", "21,930", "1.8%", false), stat("Conversion", "5.9%", "0.4%", true)] },
      card("Revenue chart", [text("Monthly revenue", 16, 600), chart], { autoLayout: { direction: "vertical", gap: 16, padding: 24 } }),
    ] },
  ] });
screenshot(dash, { scale: 0.5 })
```

## Changing what exists

Read, then change in place. For example, make every primary button in a screen rounder:

```js
const screen = insert(null, { type: "frame", name: "Screen", autoLayout: { gap: 8, padding: 16 }, children: [
  { type: "frame", name: "Save button", radius: 8, fills: ["#2563eb"], autoLayout: { padding: [10, 16] }, children: [{ type: "text", text: "Save" }] },
  { type: "frame", name: "Send button", radius: 8, fills: ["#2563eb"], autoLayout: { padding: [10, 16] }, children: [{ type: "text", text: "Send" }] },
] });
const buttons = find({ name: "* button", within: screen });
for (const id of buttons) update(id, { radius: 999 });
get({ name: "* button", within: screen }, { fields: ["name", "radius"] })
```
