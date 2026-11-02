// The last step of the batched effects (see effects.wgsl): each region's working image →
// premultiplied RGBA8 at its slot, tinted for shadows. Same `Region` and `Step` as effects.wgsl.

struct Region {
    slot: vec2<u32>,
    size: vec2<u32>,
    shift: vec2<i32>,
    radius: i32,
    mode: u32,
    color: vec4<f32>,
    kernel: vec2<u32>,
    levels: u32,
    pad: u32,
}

struct Step {
    list: u32,
    level: u32,
    horizontal: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read> regions: array<Region>;
@group(0) @binding(2) var<storage, read> lists: array<u32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var work: texture_2d<f32>;
// The effects' source (an inner shadow's mask).
@group(0) @binding(5) var original: texture_2d<f32>;
@group(0) @binding(6) var out8: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn finish(@builtin(global_invocation_id) id: vec3<u32>) {
    let r = regions[lists[step.list + id.z]];
    if id.x >= r.size.x || id.y >= r.size.y {
        return;
    }
    let at = vec2<i32>(r.slot + id.xy);
    let v = textureLoad(work, at, 0);
    let color = r.color;
    var o: vec4<f32>;
    switch r.mode >> 8u {
        case 1u: {
            let al = clamp(v.x, 0.0, 1.0) * color.a;
            o = vec4<f32>(color.rgb * al, al);
        }
        case 2u: {
            let a = textureLoad(original, at, 0).a;
            let al = clamp((1.0 - v.x) * a, 0.0, 1.0) * color.a;
            o = vec4<f32>(color.rgb * al, al);
        }
        default: {
            o = clamp(v, vec4<f32>(0.0), vec4<f32>(1.0));
        }
    }
    textureStore(out8, at, o);
}
