// Offscreen effects on the GPU, batched: the regions of an atlas go through each step together
// (workgroup z = position in the dispatch's region list). Each step is the matching step of
// `render::blur`, with the same arithmetic, so GPU and CPU results agree within rounding:
//
//   prepare → morph (horizontal) → morph (vertical, with the shadow offset)
//           → down × octaves → blur (horizontal) → blur (vertical) → up × octaves
//           → finish (finish.wgsl)
//
// Octave level k has its own working images, 2^k times smaller than the atlas; a region sits at
// `slot >> k` there with size `ceil(size / 2^k)` (slots are aligned, so regions never overlap).
// Working images are rgba32float; for shadows only the alpha is carried (splatted to all four
// channels). Outside a region everything is 0.

struct Region {
    slot: vec2<u32>,
    size: vec2<u32>,
    // Shadow offset in pixels (vertical morph only).
    shift: vec2<i32>,
    // Morph radius: > 0 grows, < 0 shrinks.
    radius: i32,
    // prepare: bit 0 premultiply the input, bit 1 keep alpha only.
    // finish: bits 8.. 0 blur (premultiplied colour), 1 drop shadow, 2 inner shadow.
    mode: u32,
    // Straight-alpha shadow colour.
    color: vec4<f32>,
    // Blur kernel: offset and length in `weights` (offsets -r..=r).
    kernel: vec2<u32>,
    // Octaves the blur runs at.
    levels: u32,
    pad: u32,
}

struct Step {
    // Where this dispatch's region list starts in `lists`.
    list: u32,
    // Octave level the step writes (up: the level it reads).
    level: u32,
    horizontal: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read> regions: array<Region>;
@group(0) @binding(2) var<storage, read> lists: array<u32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var src: texture_2d<f32>;
@group(0) @binding(5) var dst: texture_storage_2d<rgba32float, write>;

fn region(id: vec3<u32>) -> Region {
    return regions[lists[step.list + id.z]];
}

fn size_at(r: Region, k: u32) -> vec2<u32> {
    let m = (1u << k) - 1u;
    return (r.size + vec2<u32>(m)) >> vec2<u32>(k);
}

// A region-local pixel of `src` at level `k`; 0 outside the region.
fn load(r: Region, q: vec2<i32>, k: u32) -> vec4<f32> {
    let s = size_at(r, k);
    if q.x < 0 || q.y < 0 || q.x >= i32(s.x) || q.y >= i32(s.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(src, vec2<i32>(r.slot >> vec2<u32>(k)) + q, 0);
}

fn store(r: Region, p: vec2<u32>, k: u32, v: vec4<f32>) {
    textureStore(dst, (r.slot >> vec2<u32>(k)) + p, v);
}

fn outside(r: Region, p: vec2<u32>, k: u32) -> bool {
    let s = size_at(r, k);
    return p.x >= s.x || p.y >= s.y;
}

@compute @workgroup_size(8, 8)
fn prepare(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = region(id);
    if outside(r, id.xy, 0u) {
        return;
    }
    var c = load(r, vec2<i32>(id.xy), 0u);
    if (r.mode & 1u) != 0u {
        // Straight → premultiplied RGBA8, as the CPU path receives it.
        c = vec4<f32>(round(c.rgb * c.a * 255.0) / 255.0, c.a);
    }
    if (r.mode & 2u) != 0u {
        c = vec4<f32>(c.a);
    }
    store(r, id.xy, 0u, c);
}

// One pass of a square max (grow) or min (shrink) filter; the vertical pass also moves the
// result by `shift`. With radius 0 it only copies (and moves).
@compute @workgroup_size(8, 8)
fn morph(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = region(id);
    if outside(r, id.xy, 0u) {
        return;
    }
    let horizontal = step.horizontal != 0u;
    var at = vec2<i32>(id.xy);
    if !horizontal {
        at = at - r.shift;
    }
    let n = abs(r.radius);
    if n == 0 {
        store(r, id.xy, 0u, load(r, at, 0u));
        return;
    }
    let grow = r.radius > 0;
    let sz = vec2<i32>(r.size);
    if !horizontal && (at.x < 0 || at.y < 0 || at.x >= sz.x || at.y >= sz.y) {
        store(r, id.xy, 0u, vec4<f32>(0.0));
        return;
    }
    var v = select(1.0, 0.0, grow);
    for (var d = -n; d <= n; d++) {
        let q = select(at + vec2<i32>(0, d), at + vec2<i32>(d, 0), horizontal);
        let s = load(r, q, 0u).x;
        v = select(min(v, s), max(v, s), grow);
    }
    store(r, id.xy, 0u, vec4<f32>(v));
}

// 2×2 box average from level - 1 to level.
@compute @workgroup_size(8, 8)
fn down(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = region(id);
    let k = step.level;
    if outside(r, id.xy, k) {
        return;
    }
    let b = vec2<i32>(id.xy) * 2;
    let s = load(r, b, k - 1u) + load(r, b + vec2<i32>(1, 0), k - 1u) + load(r, b + vec2<i32>(0, 1), k - 1u) + load(r, b + vec2<i32>(1, 1), k - 1u);
    textureStore(dst, (r.slot >> vec2<u32>(k)) + id.xy, s * 0.25);
}

// One direction of the separable Gaussian at the region's last octave.
@compute @workgroup_size(8, 8)
fn blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = region(id);
    let k = step.level;
    if outside(r, id.xy, k) {
        return;
    }
    let horizontal = step.horizontal != 0u;
    let n = i32(r.kernel.y);
    let half = n / 2;
    let at = vec2<i32>(id.xy);
    var acc = vec4<f32>(0.0);
    for (var i = 0; i < n; i++) {
        let d = i - half;
        let q = select(at + vec2<i32>(0, d), at + vec2<i32>(d, 0), horizontal);
        acc += weights[r.kernel.x + u32(i)] * load(r, q, k);
    }
    store(r, id.xy, k, acc);
}

// Bilinear 2× upsample with clamped edges, from level to level - 1.
@compute @workgroup_size(8, 8)
fn up(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = region(id);
    let k = step.level;
    if outside(r, id.xy, k - 1u) {
        return;
    }
    let f = (vec2<f32>(id.xy) + 0.5) / 2.0 - 0.5;
    let f0 = floor(f);
    let t = f - f0;
    let i0 = vec2<i32>(f0);
    let hi = vec2<i32>(size_at(r, k)) - 1;
    let a = load(r, clamp(i0, vec2<i32>(0), hi), k);
    let b = load(r, clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), hi), k);
    let c = load(r, clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), hi), k);
    let d = load(r, clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), hi), k);
    let top = a * (1.0 - t.x) + b * t.x;
    let bot = c * (1.0 - t.x) + d * t.x;
    textureStore(dst, (r.slot >> vec2<u32>(k - 1u)) + id.xy, top * (1.0 - t.y) + bot * t.y);
}
