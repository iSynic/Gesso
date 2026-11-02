// Shell spike (b) frontend: HTML properties panel + WASM/WebGPU Vello canvas, synced both ways.
import init, { Canvas } from './pkg/shell_canvas.js';

const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
const status = (s) => ($('status').textContent = s);
const FONTS = ['segoeui.ttf', 'segoeuib.ttf', 'YuGothR.ttc', 'msyh.ttc', 'malgun.ttf', 'seguiemj.ttf', 'arial.ttf'];

const el = $('c');
const dpr = window.devicePixelRatio || 1;
el.width = Math.round(el.clientWidth * dpr);
el.height = Math.round(el.clientHeight * dpr);

const tStart = performance.now();
await init();
const fonts = [];
for (const f of FONTS) {
  const buf = await invoke('read_font', { name: f });
  if (buf.byteLength) fonts.push(new Uint8Array(buf));
}
const tFonts = performance.now();
const canvas = await Canvas.create(el, fonts, 10000);
const tCreate = performance.now();

let dirty = true;
let pendingInput = null; // performance.now() of the earliest input not yet on screen
const lat = { drag: [], panel: [] };

function hex(c) { return '#' + c.slice(0, 3).map((v) => v.toString(16).padStart(2, '0')).join(''); }
function showProps() {
  const p = JSON.parse(canvas.props());
  $('none').hidden = !!p;
  $('props').hidden = !p;
  if (!p) return;
  $('nid').textContent = `Node ${p.id}`;
  for (const k of ['x', 'y', 'w', 'h']) if (document.activeElement !== $(k)) $(k).value = Math.round(p[k] * 100) / 100;
  $('fill').value = hex(p.fill);
}
function applyPanel() {
  const c = $('fill').value;
  canvas.set_props(+$('x').value, +$('y').value, +$('w').value, +$('h').value, parseInt(c.slice(1, 3), 16), parseInt(c.slice(3, 5), 16), parseInt(c.slice(5, 7), 16));
}
for (const k of ['x', 'y', 'w', 'h', 'fill']) {
  $(k).addEventListener('input', () => { pendingInput ??= { t: performance.now(), kind: 'panel' }; applyPanel(); dirty = true; });
}

let dragging = false;
const pos = (e) => [e.offsetX * dpr, e.offsetY * dpr];
el.addEventListener('pointerdown', (e) => { el.setPointerCapture(e.pointerId); dragging = canvas.pointer_down(...pos(e)) >= 0; showProps(); dirty = true; });
el.addEventListener('pointermove', (e) => { if (!dragging) return; pendingInput ??= { t: e.timeStamp, kind: 'drag' }; canvas.drag_to(...pos(e)); dirty = true; });
el.addEventListener('pointerup', () => { dragging = false; canvas.pointer_up(); });

// Resolves once the GPU has finished submission `n` (wgpu's on_submitted_work_done callback).
const tick = () => new Promise((r) => { const ch = new MessageChannel(); ch.port1.onmessage = r; ch.port2.postMessage(0); });
async function gpuDone(n) { while (Canvas.gpu_done_count() < n) await tick(); }

let firstFrame = null;
async function frame(ts) {
  if (dirty) {
    dirty = false;
    showProps();
    const input = pendingInput;
    pendingInput = null;
    const rstart = performance.now();
    const n = canvas.render();
    firstFrame ??= performance.now();
    if (input) {
      const submitted = performance.now();
      await gpuDone(n);
      const gpu = performance.now();
      // Next animation frame ≈ the frame containing this work is being composited.
      const nextRaf = await new Promise((r) => requestAnimationFrame(r));
      lat[input.kind].push({ to_submit: submitted - input.t, to_gpu_done: gpu - input.t, to_next_raf: nextRaf - input.t, render_cpu: submitted - rstart, render_to_gpu_done: gpu - rstart });
    }
  }
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
status(`${canvas.node_count()} nodes · ${canvas.adapter_info()}\nwasm+fonts ${(tFonts - tStart).toFixed(0)} ms, scene+GPU init ${(tCreate - tFonts).toFixed(0)} ms`);

// ---- scripted session (--auto) ------------------------------------------------------------
const pct = (v) => { const s = [...v].sort((a, b) => a - b); const at = (p) => s[Math.round((s.length - 1) * p)]; return { n: s.length, p50: at(0.5), p95: at(0.95), max: s[s.length - 1] }; };
const nextFrame = () => new Promise((r) => requestAnimationFrame(r));

if (await invoke('auto_mode')) {
  for (let i = 0; i < 10; i++) await nextFrame();
  // IPC round trip (webview -> Rust -> webview).
  const ipc = [];
  for (let i = 0; i < 200; i++) { const t = performance.now(); await invoke('ping', { x: i }); ipc.push(performance.now() - t); }
  // Select a node at the same relative spot as the native spike.
  let p = [el.width * 0.3 * (1600 / (el.clientWidth + 260)), el.height * 0.4];
  canvas.pointer_down(...p);
  dirty = true;
  await nextFrame(); await nextFrame();
  // 300 drag steps, each a synthetic input handled the same way as a real pointermove.
  for (let i = 0; i < 300; i++) {
    p = [p[0] + 1, p[1] + (i % 2 ? -0.5 : 0.5)];
    pendingInput ??= { t: performance.now(), kind: 'drag' };
    canvas.drag_to(...p);
    dirty = true;
    await nextFrame(); await nextFrame();
  }
  canvas.pointer_up();
  // 200 panel edits through the real input element and its event handler.
  for (let i = 0; i < 200; i++) {
    $('w').value = 200 + (i % 40);
    $('w').dispatchEvent(new Event('input'));
    await nextFrame(); await nextFrame();
  }
  const sum = (arr) => ({ to_submit: pct(arr.map((s) => s.to_submit)), to_gpu_done: pct(arr.map((s) => s.to_gpu_done)), to_next_raf: pct(arr.map((s) => s.to_next_raf)), render_cpu: pct(arr.map((s) => s.render_cpu)), render_to_gpu_done: pct(arr.map((s) => s.render_to_gpu_done)) });
  const out = {
    shell: 'Tauri 2 (WebView2) + WASM vello 0.11 on WebGPU + HTML panel', nodes: canvas.node_count(), adapter: canvas.adapter_info(),
    canvas_px: [el.width, el.height], dpr, user_agent: navigator.userAgent,
    startup_ms: { wasm_init_and_fonts: tFonts - tStart, scene_and_gpu_init: tCreate - tFonts, first_frame_since_navigation: firstFrame },
    ipc_ping_roundtrip: pct(ipc), drag: sum(lat.drag), panel_edit: sum(lat.panel),
  };
  status(JSON.stringify(out.drag.to_gpu_done));
  await invoke('save_results', { json: JSON.stringify(out, null, 2) });
  await invoke('quit');
}
