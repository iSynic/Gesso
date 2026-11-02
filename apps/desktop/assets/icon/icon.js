// label: Gesso application icon
// The painted G, generated stroke by stroke. Run with:
//   canvas run icon.js --timeout 20000 -o gesso-icon.canvas
// then make_icon.py exports the sizes.

const S = 944, k = S / 1000;
let seed = 7;
const rnd = (a = 0, b = 1) => { seed = (seed * 1664525 + 1013904223) >>> 0; return a + (b - a) * (seed / 4294967296); };
const R = 268, cx = 500, cy = 496;
let line_ = [], N = 0, nor = [], HW = 68;
function use(kind) {
  line_ = [];
  if (kind === "arc") {
    HW = 68;
    for (let i = 0; i <= 250; i++) { const t = (56 + 302 * i / 250) * Math.PI / 180; line_.push([cx + R * Math.cos(t), cy - R * Math.sin(t)]); }
  } else {
    HW = 56;
    for (let i = 0; i <= 90; i++) { const u = i / 90; line_.push([806 - 226 * u, 526 + 9 * Math.sin(u * 3.1) - 5 * u]); }
  }
  N = line_.length;
  nor = line_.map((_, i) => { const p = line_[Math.max(0, i - 5)], q = line_[Math.min(N - 1, i + 5)]; const x = q[0] - p[0], y = q[1] - p[1], l = Math.hypot(x, y) || 1; return [-y / l, x / l]; });
}
const f = (v) => (v * k).toFixed(1);
function strand(o, t0, t1, amp = 1.5) {
  const ph = rnd(0, 6.28), fr = rnd(0.02, 0.06); let d = "";
  for (let i = Math.max(0, t0 | 0); i <= Math.min(N - 1, t1 | 0); i += 3) {
    const w = o + amp * Math.sin(ph + i * fr);
    d += (d ? "L" : "M") + f(line_[i][0] + nor[i][0] * w) + " " + f(line_[i][1] + nor[i][1] * w);
  }
  return d;
}
const whole = (t0 = 0, t1 = N - 1) => strand(0, t0, t1, 0);
const path = (name, d, stroke, extra = {}) => ({ type: "path", name, x: 0, y: 0, width: S, height: S, paths: [{ d }], strokes: [stroke], ...extra });
const bundle = (name, count, range, paint, weight, opacity, t = () => { const a = rnd(0, N * 0.8); return [a, a + rnd(40, 150)]; }) => {
  let d = "";
  for (let i = 0; i < count; i++) { const [a, z] = t(); d += strand(rnd(-range, range), a, z); }
  return path(name, d, { paint, weight: weight * k, cap: "round", join: "round", align: "center" }, { opacity });
};

const OUTER = insert(null, { type: "frame", name: "Gesso icon", x: 0, y: 0, width: 1024, height: 1024, clip: true, fills: [{ type: "solid", color: "#00000000" }] });
const ID = insert(OUTER, { type: "frame", name: "Tile", x: 40, y: 40, width: S, height: S, radius: 215 * k, clip: true,
  fills: [{ type: "linearGradient", start: [0.15, 0], end: [0.85, 1], stops: [{ offset: 0, color: "#3d59d8" }, { offset: 0.55, color: "#1b2776" }, { offset: 1, color: "#0c1136" }] }] });
insert(ID, { type: "rectangle", name: "Glow", x: 0, y: 0, width: S, height: S, blend: "screen",
  fills: [{ type: "radialGradient", center: [0.28, 0.16], radius: [0.8, 0.8], stops: [{ offset: 0, color: "#86a2ff77" }, { offset: 1, color: "#86a2ff00" }] }] });
{
let a = "", c = "";
for (let v = 0; v <= 1000; v += 9) { a += `M0 ${f(v)}L${S} ${f(v)}`; c += `M${f(v)} 0L${f(v)} ${S}`; }
insert(ID, path("Weave warp", a, { paint: "#ffffff", weight: 2.4 * k, cap: "butt", align: "center" }, { opacity: 0.035, blend: "overlay" }));
insert(ID, path("Weave weft", c, { paint: "#000000", weight: 2.4 * k, cap: "butt", align: "center" }, { opacity: 0.08, blend: "overlay" }));
insert(ID, { type: "rectangle", name: "Vignette", x: 0, y: 0, width: S, height: S, blend: "multiply",
  fills: [{ type: "radialGradient", center: [0.5, 0.46], radius: [0.78, 0.78], stops: [{ offset: 0.55, color: "#ffffff00" }, { offset: 1, color: "#04061c" }] }] });
}
{
const cast = (name, x, y, r, o) => { insert(ID, path(name, whole(10, N - 8), { paint: "#02041a", weight: 2 * HW * k, cap: "round", join: "round", align: "center" }, { x: x * k, y: y * k, opacity: o, effects: [{ type: "layerBlur", radius: r * k }] })); };
use("arc");
cast("Cast shadow", 14, 40, 34, 0.62);
cast("Contact shadow", 5, 12, 9, 0.6);
}
{const PART = "arc";
const P = PART === "arc" ? "Arc" : "Bar";
use(PART);
const end0 = PART === "arc" ? 16 : 18, end1 = PART === "arc" ? N - 12 : N - 16;
const cap = "butt";
if (PART === "bar") {
  insert(ID, path("Bar shadow on arc", whole(4, N - 8), { paint: "#3d2f1c", weight: 2 * HW * k, cap: "butt", align: "center" }, { x: 5 * k, y: 16 * k, opacity: 0.32, blend: "multiply", effects: [{ type: "layerBlur", radius: 12 * k }] }));
  insert(ID, path("Bar cast shadow", whole(40, N - 6), { paint: "#02041a", weight: 2 * HW * k, cap: "round", align: "center" }, { x: 8 * k, y: 24 * k, opacity: 0.55, effects: [{ type: "layerBlur", radius: 18 * k }] }));
}
const wide = { weight: (2 * HW - 2) * k, cap, join: "round", align: "center" };
insert(ID, path(P + " body", whole(end0, end1), { paint: { type: "linearGradient", start: [0.1, 0.05], end: [0.9, 0.95], stops: PART === "arc" ? [{ offset: 0, color: "#fffdf7" }, { offset: 0.6, color: "#f6efe0" }, { offset: 1, color: "#e4d8bf" }] : [{ offset: 0, color: "#fffdf7" }, { offset: 1, color: "#f8f1e2" }] }, ...wide }));
insert(ID, path(P + " form shadow", whole(end0, end1), { paint: { type: "linearGradient", start: [0.25, 0.15], end: [0.85, 0.95], stops: [{ offset: 0.2, color: "#b49f7800" }, { offset: 1, color: "#8c7650cc" }] }, ...wide }, { blend: "multiply", opacity: PART === "arc" ? 1 : 0.22 }));
for (const [name, o, c, w, a, r] of [["edge shade", HW * 0.76, "#a8946f", 24, 0.5, 9], ["outer shade", -HW * 0.82, "#a8946f", 16, 0.4, 7], ["crest light", -HW * 0.26, "#ffffff", 36, 0.75, 14]]) {
  insert(ID, path(P + " " + name, strand(o, end0 + 4, end1 - 4, 1), { paint: c, weight: w * k * HW / 68, cap: "round", join: "round", align: "center" }, { opacity: a, effects: [{ type: "layerBlur", radius: r * k }] }));
}
}
{const PART = "arc";
use(PART); seed = PART === "arc" ? 21 : 34;
const P = PART === "arc" ? "Arc" : "Bar", d = PART === "arc" ? 1 : 0.4;
const span = () => { const a = rnd(0, N * 0.8); return [a, a + rnd(N * 0.15, N * 0.6)]; };
insert(ID, bundle(P + " grooves deep", 46 * d, HW * 0.9, "#8f7a57", 3.2, 0.42, span));
insert(ID, bundle(P + " grooves fine", 90 * d, HW * 0.95, "#a8946f", 1.5, 0.5, span));
insert(ID, bundle(P + " ridges broad", 34 * d, HW * 0.85, "#ffffff", 5, 0.75, span));
insert(ID, bundle(P + " ridges fine", 110 * d, HW * 0.94, "#fffefa", 1.7, 0.9, span));
}
{
seed = 99;
use("arc");
// The brush lands loaded on the arc: strands fan out past the body and dry gaps show the ground.
insert(ID, bundle("Arc head strands", 60, 68, "#fbf6ea", 4.6, 1, () => { const a = rnd(0, 30); return [a, a + rnd(30, 70)]; }));
insert(ID, bundle("Arc head dry gaps", 22, 66, "#1a2570", 2.2, 0.85, () => { const a = rnd(4, 34); return [a, a + rnd(12, 46)]; }));
let dots = "";
for (let i = 0; i < 22; i++) {
  const j = rnd(0, 34) | 0, o = rnd(-105, 105), px = (line_[j][0] + nor[j][0] * o + rnd(-40, 70)) * k, py = (line_[j][1] + nor[j][1] * o - rnd(0, 46)) * k, r = rnd(1.2, 5);
  dots += `M${px - r} ${py}A${r} ${r} 0 1 0 ${px + r} ${py}A${r} ${r} 0 1 0 ${px - r} ${py}Z`;
}
insert(ID, { type: "path", name: "Spatter", x: 0, y: 0, width: S, height: S, paths: [{ d: dots }], fills: [{ type: "solid", color: "#f7f1e3" }], opacity: 0.9 });
}
{const PART = "bar";
const P = PART === "arc" ? "Arc" : "Bar";
use(PART);
const end0 = PART === "arc" ? 16 : 18, end1 = PART === "arc" ? N - 12 : N - 16;
const cap = "butt";
if (PART === "bar") {
  insert(ID, path("Bar shadow on arc", whole(4, N - 8), { paint: "#3d2f1c", weight: 2 * HW * k, cap: "butt", align: "center" }, { x: 5 * k, y: 16 * k, opacity: 0.32, blend: "multiply", effects: [{ type: "layerBlur", radius: 12 * k }] }));
  insert(ID, path("Bar cast shadow", whole(40, N - 6), { paint: "#02041a", weight: 2 * HW * k, cap: "round", align: "center" }, { x: 8 * k, y: 24 * k, opacity: 0.55, effects: [{ type: "layerBlur", radius: 18 * k }] }));
}
const wide = { weight: (2 * HW - 2) * k, cap, join: "round", align: "center" };
insert(ID, path(P + " body", whole(end0, end1), { paint: { type: "linearGradient", start: [0.1, 0.05], end: [0.9, 0.95], stops: PART === "arc" ? [{ offset: 0, color: "#fffdf7" }, { offset: 0.6, color: "#f6efe0" }, { offset: 1, color: "#e4d8bf" }] : [{ offset: 0, color: "#fffdf7" }, { offset: 1, color: "#f8f1e2" }] }, ...wide }));
insert(ID, path(P + " form shadow", whole(end0, end1), { paint: { type: "linearGradient", start: [0.25, 0.15], end: [0.85, 0.95], stops: [{ offset: 0.2, color: "#b49f7800" }, { offset: 1, color: "#8c7650cc" }] }, ...wide }, { blend: "multiply", opacity: PART === "arc" ? 1 : 0.22 }));
for (const [name, o, c, w, a, r] of [["edge shade", HW * 0.76, "#a8946f", 24, 0.5, 9], ["outer shade", -HW * 0.82, "#a8946f", 16, 0.4, 7], ["crest light", -HW * 0.26, "#ffffff", 36, 0.75, 14]]) {
  insert(ID, path(P + " " + name, strand(o, end0 + 4, end1 - 4, 1), { paint: c, weight: w * k * HW / 68, cap: "round", join: "round", align: "center" }, { opacity: a, effects: [{ type: "layerBlur", radius: r * k }] }));
}
}
{const PART = "bar";
use(PART); seed = PART === "arc" ? 21 : 34;
const P = PART === "arc" ? "Arc" : "Bar", d = PART === "arc" ? 1 : 0.4;
const span = () => { const a = rnd(0, N * 0.8); return [a, a + rnd(N * 0.15, N * 0.6)]; };
insert(ID, bundle(P + " grooves deep", 46 * d, HW * 0.9, "#8f7a57", 3.2, 0.42, span));
insert(ID, bundle(P + " grooves fine", 90 * d, HW * 0.95, "#a8946f", 1.5, 0.5, span));
insert(ID, bundle(P + " ridges broad", 34 * d, HW * 0.85, "#ffffff", 5, 0.75, span));
insert(ID, bundle(P + " ridges fine", 110 * d, HW * 0.94, "#fffefa", 1.7, 0.9, span));
}
{
seed = 55;
use("bar");
// The crossbar lifts away dry.
insert(ID, bundle("Bar tail strands", 40, 56, "#f8f2e4", 4.4, 1, () => { const z = N - 1 - rnd(0, 13); return [z - rnd(10, 26), z]; }));
insert(ID, bundle("Bar head strands", 44, 54, "#f8f2e4", 4.4, 1, () => { const a = rnd(0, 13); return [a, a + rnd(10, 28)]; }));
insert(ID, bundle("Bar dry gaps", 16, 54, "#141c5c", 2.2, 0.85, () => { const a = N - rnd(8, 30); return [a, a + rnd(6, 20)]; }));
}
{
insert(ID, { type: "rectangle", name: "Rim light", x: 0, y: 0, width: S, height: S, radius: 215 * k,
  strokes: [{ paint: { type: "linearGradient", start: [0.3, 0], end: [0.7, 1], stops: [{ offset: 0, color: "#ffffff88" }, { offset: 0.4, color: "#ffffff14" }, { offset: 1, color: "#00000055" }] }, weight: 7 * k, align: "inside" }] });
select([]);
}

// Small sizes (32 px and under): the same G and tile with the detail taken out.
const SM = insert(null, { type: "frame", name: "Gesso icon small", x: 1100, y: 0, width: 1024, height: 1024, clip: true, fills: [{ type: "solid", color: "#00000000" }] });
insert(SM, { type: "rectangle", name: "Tile", x: 16, y: 16, width: 992, height: 992, radius: 200,
  fills: [{ type: "linearGradient", start: [0.15, 0], end: [0.85, 1], stops: [{ offset: 0, color: "#3a56d4" }, { offset: 1, color: "#141c5c" }] }] });
insert(SM, { type: "path", name: "G", x: 0, y: 0, width: 1024, height: 1024, paths: [{ d: "M716 268 A300 300 0 1 0 812 530 L590 530" }],
  strokes: [{ paint: "#fbf7ec", weight: 158, cap: "butt", join: "miter", align: "center" }] });
null
