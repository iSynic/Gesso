//! Offscreen effects as compute passes (`effects.wgsl`, `finish.wgsl`), step for step the same as
//! `render::blur`: the kernel weights come from [`render::blur::kernel`], the octave rule from
//! [`render::blur::OCTAVE_SIGMA`].
//!
//! Effects are planned into a [`Batch`] over regions of one source texture and submitted together:
//! each step runs as a few dispatches over all regions (grouped by size), not one per region.

use std::num::NonZeroU64;

use render::Process;
use render::blur::{OCTAVE_SIGMA, kernel};
use rustc_hash::FxHashMap;
use vello::wgpu;
use wgpu::util::DeviceExt;

/// A pooled texture and its default view.
#[derive(Clone)]
pub(crate) struct Tex {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

/// Pooled textures, reused across frames. Everything taken during a frame goes back at its end.
#[derive(Default)]
pub(crate) struct Pool {
    free: FxHashMap<(u32, u32, bool), Vec<Tex>>,
    used: Vec<((u32, u32, bool), Tex)>,
}

/// Free textures kept between frames, in bytes.
const POOL_BUDGET: u64 = 256 << 20;

impl Pool {
    /// A `w × h` texture: rgba32float working image (`float`), else rgba8unorm usable as a Vello
    /// target, an effect output and an image override.
    pub(crate) fn take(&mut self, device: &wgpu::Device, w: u32, h: u32, float: bool) -> Tex {
        let key = (w.max(1), h.max(1), float);
        let t = self.free.get_mut(&key).and_then(Vec::pop).unwrap_or_else(|| {
            let usage = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(if float { "effect work" } else { "surface" }),
                size: wgpu::Extent3d { width: key.0, height: key.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: if float { wgpu::TextureFormat::Rgba32Float } else { wgpu::TextureFormat::Rgba8Unorm },
                usage: if float { usage } else { usage | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST },
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Tex { texture, view }
        });
        self.used.push((key, t.clone()));
        t
    }

    /// Returns this frame's textures; drops free ones beyond the budget.
    pub(crate) fn recycle(&mut self) {
        for (k, t) in self.used.drain(..) {
            self.free.entry(k).or_default().push(t);
        }
        let bytes: u64 = self.free.iter().map(|((w, h, f), v)| *w as u64 * *h as u64 * if *f { 16 } else { 4 } * v.len() as u64).sum();
        if bytes > POOL_BUDGET {
            self.free.clear();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Prepare,
    Morph,
    Down,
    Blur,
    Up,
    Finish,
}

/// One effect on a region of the batch's source.
struct Region {
    slot: (u32, u32),
    size: (u32, u32),
    premul: bool,
    p: Process,
}

/// Effects planned for one submission, all on regions of one source texture (an atlas of
/// offscreen regions, or a backdrop snapshot). Results land at the same slots of one output.
pub(crate) struct Batch {
    src: Tex,
    regions: Vec<Region>,
}

impl Batch {
    pub(crate) fn new(src: Tex) -> Self {
        Batch { src, regions: Vec::new() }
    }

    /// Plans `p` on the `size` region at `slot` (straight alpha unless `premul`). Slots must be
    /// multiples of `1 << levels(p)` for every region in the batch, so octaves never overlap.
    pub(crate) fn add(&mut self, slot: (u32, u32), size: (u32, u32), premul: bool, p: Process) {
        self.regions.push(Region { slot, size, premul, p });
    }
}

fn sigma(p: &Process) -> f64 {
    match *p {
        Process::Blur { sigma } | Process::DropShadow { sigma, .. } | Process::InnerShadow { sigma, .. } => sigma,
    }
}

/// Whether `p` blurs at all.
fn blurs(p: &Process) -> bool {
    sigma(p) >= 0.1
}

/// The octaves `p` downsamples by before blurring.
pub(crate) fn levels(p: &Process) -> u32 {
    let mut s = sigma(p);
    let mut n = 0;
    while blurs(p) && s > OCTAVE_SIGMA {
        s /= 2.0;
        n += 1;
    }
    n
}

pub(crate) struct Effects {
    layout: wgpu::BindGroupLayout,
    finish_layout: wgpu::BindGroupLayout,
    prepare: wgpu::ComputePipeline,
    morph: wgpu::ComputePipeline,
    down: wgpu::ComputePipeline,
    blur: wgpu::ComputePipeline,
    up: wgpu::ComputePipeline,
    finish: wgpu::ComputePipeline,
    /// Kernel weights by σ (bits), kept across frames.
    kernels: FxHashMap<u64, Vec<f32>>,
}

/// `Step` in the shaders.
const STEP: u64 = 16;
/// Offset between dispatches' steps in the shared uniform buffer.
const STRIDE: u64 = 256;

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn storage_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D2 },
        count: None,
    }
}

fn buffer_entry(binding: u32, ty: wgpu::BufferBindingType, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: dynamic, min_binding_size: None }, count: None }
}

/// One dispatch: a step over a list of regions.
struct Dispatch {
    step: Step,
    level: u32,
    horizontal: bool,
    /// Offset of the region list in the lists buffer.
    list: u32,
    count: u32,
    /// Largest written size in the list.
    size: (u32, u32),
    /// The textures read and written, as indices into the batch's textures.
    tex: (usize, usize),
}

/// What the planner needs to know about a region.
struct Info {
    size: (u32, u32),
    levels: u32,
    blurs: bool,
    morphs: bool,
}

fn size_at((w, h): (u32, u32), k: u32) -> (u32, u32) {
    (w.div_ceil(1 << k), h.div_ceil(1 << k))
}

/// Adds a step's dispatches over `members`: one per power-of-two size class, so small regions
/// don't run the largest one's workgroups.
#[allow(clippy::too_many_arguments)]
fn dispatch(out: &mut Vec<Dispatch>, lists: &mut Vec<u32>, info: &[Info], step: Step, level: u32, horizontal: bool, tex: (usize, usize), members: impl Iterator<Item = usize>) {
    let mut classes: Vec<(u32, Vec<u32>, (u32, u32))> = Vec::new();
    for i in members {
        let s = size_at(info[i].size, if step == Step::Up { level - 1 } else { level });
        let class = s.0.max(s.1).next_power_of_two();
        match classes.iter_mut().find(|c| c.0 == class) {
            Some(c) => {
                c.1.push(i as u32);
                c.2 = (c.2.0.max(s.0), c.2.1.max(s.1));
            }
            None => classes.push((class, vec![i as u32], s)),
        }
    }
    for (_, members, size) in classes {
        out.push(Dispatch { step, level, horizontal, list: lists.len() as u32, count: members.len() as u32, size, tex });
        lists.extend(members);
    }
}

fn bytes<T: Copy, const N: usize>(v: &[T], to: impl Fn(T) -> [u8; N]) -> Vec<u8> {
    v.iter().flat_map(|x| to(*x)).collect()
}

impl Effects {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let common = [buffer_entry(0, wgpu::BufferBindingType::Uniform, true), buffer_entry(1, read, false), buffer_entry(2, read, false), buffer_entry(3, read, false)];
        let mut entries = common.to_vec();
        entries.extend([texture_entry(4), storage_entry(5, wgpu::TextureFormat::Rgba32Float)]);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("effects"), entries: &entries });
        let mut entries = common.to_vec();
        entries.extend([texture_entry(4), texture_entry(5), storage_entry(6, wgpu::TextureFormat::Rgba8Unorm)]);
        let finish_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("effects finish"), entries: &entries });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("effects"), source: wgpu::ShaderSource::Wgsl(include_str!("effects.wgsl").into()) });
        let finish_module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("effects finish"), source: wgpu::ShaderSource::Wgsl(include_str!("finish.wgsl").into()) });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("effects"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let fpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("effects finish"), bind_group_layouts: &[Some(&finish_layout)], immediate_size: 0 });
        let pipeline = |layout: &wgpu::PipelineLayout, module: &wgpu::ShaderModule, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(layout),
                module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Effects {
            prepare: pipeline(&pl, &module, "prepare"),
            morph: pipeline(&pl, &module, "morph"),
            down: pipeline(&pl, &module, "down"),
            blur: pipeline(&pl, &module, "blur"),
            up: pipeline(&pl, &module, "up"),
            finish: pipeline(&fpl, &finish_module, "finish"),
            kernels: FxHashMap::default(),
            layout,
            finish_layout,
        }
    }

    /// Records and submits a batch. Returns the premultiplied rgba8unorm output, each region's
    /// result at its slot, and the number of dispatches.
    pub(crate) fn submit(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pool: &mut Pool, batch: Batch) -> Option<(Tex, usize)> {
        if batch.regions.is_empty() {
            return None;
        }
        let (w, h) = (batch.src.texture.width(), batch.src.texture.height());

        // Regions as the shaders see them (`Region`, 64 bytes), and their kernels end to end.
        let mut regions: Vec<u8> = Vec::with_capacity(batch.regions.len() * 64);
        let mut weights: Vec<f32> = Vec::new();
        let mut placed: FxHashMap<u64, u32> = FxHashMap::default();
        let mut info = Vec::with_capacity(batch.regions.len());
        for r in &batch.regions {
            let (alpha_only, color, shift, radius, finish) = match r.p {
                Process::Blur { .. } => (false, [0.0; 4], (0, 0), 0, 0u32),
                Process::DropShadow { color, dx, dy, spread, .. } => (true, color, (dx, dy), spread, 1),
                // The inner shadow's working value is the source alpha shrunk by the spread (its
                // inverse grown), so the morph runs the other way.
                Process::InnerShadow { color, dx, dy, spread, .. } => (true, color, (dx, dy), -spread, 2),
            };
            let levels = levels(&r.p);
            let mut kernel_at = (0u32, 0u32);
            if blurs(&r.p) {
                let s = sigma(&r.p) / f64::from(1u32 << levels);
                let k = self.kernels.entry(s.to_bits()).or_insert_with(|| kernel(s));
                let at = *placed.entry(s.to_bits()).or_insert_with(|| {
                    weights.extend_from_slice(k);
                    (weights.len() - k.len()) as u32
                });
                kernel_at = (at, k.len() as u32);
            }
            let mode = u32::from(!r.premul) | u32::from(alpha_only) << 1 | finish << 8;
            regions.extend(bytes(&[r.slot.0, r.slot.1, r.size.0, r.size.1], u32::to_le_bytes));
            regions.extend(bytes(&[shift.0, shift.1, radius], i32::to_le_bytes));
            regions.extend(mode.to_le_bytes());
            regions.extend(bytes(&color, f32::to_le_bytes));
            regions.extend(bytes(&[kernel_at.0, kernel_at.1, levels, 0], u32::to_le_bytes));
            info.push(Info { size: r.size, levels, blurs: blurs(&r.p), morphs: alpha_only && (radius != 0 || shift != (0, 0)) });
        }
        if weights.is_empty() {
            weights.push(0.0);
        }

        // Textures: 0 the source, 1 the output, then per level k its working image a[k] and,
        // where something blurs or morphs at that level, its partner b[k].
        let max_level = info.iter().filter(|i| i.blurs).map(|i| i.levels).max().unwrap_or(0);
        let morphs = info.iter().any(|i| i.morphs);
        let out = pool.take(device, w, h, false);
        let mut tex = vec![batch.src.clone(), out.clone()];
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for k in 0..=max_level {
            let (lw, lh) = size_at((w, h), k);
            a.push(tex.len());
            tex.push(pool.take(device, lw, lh, true));
            let partner = (k == 0 && morphs) || info.iter().any(|i| i.blurs && i.levels == k);
            b.push(partner.then(|| {
                tex.push(pool.take(device, lw, lh, true));
                tex.len() - 1
            }));
        }

        let mut lists: Vec<u32> = Vec::new();
        let mut ds: Vec<Dispatch> = Vec::new();
        let all = 0..info.len();
        dispatch(&mut ds, &mut lists, &info, Step::Prepare, 0, false, (0, a[0]), all.clone());
        if let (true, Some(b0)) = (morphs, b[0]) {
            dispatch(&mut ds, &mut lists, &info, Step::Morph, 0, true, (a[0], b0), all.clone());
            dispatch(&mut ds, &mut lists, &info, Step::Morph, 0, false, (b0, a[0]), all.clone());
        }
        for k in 1..=max_level {
            let at = all.clone().filter(|&i| info[i].blurs && info[i].levels >= k);
            dispatch(&mut ds, &mut lists, &info, Step::Down, k, false, (a[k as usize - 1], a[k as usize]), at);
        }
        for k in 0..=max_level {
            if let Some(bk) = b[k as usize] {
                let at = |i: &usize| info[*i].blurs && info[*i].levels == k;
                dispatch(&mut ds, &mut lists, &info, Step::Blur, k, true, (a[k as usize], bk), all.clone().filter(at));
                dispatch(&mut ds, &mut lists, &info, Step::Blur, k, false, (bk, a[k as usize]), all.clone().filter(at));
            }
        }
        for k in (1..=max_level).rev() {
            let at = all.clone().filter(|&i| info[i].blurs && info[i].levels >= k);
            dispatch(&mut ds, &mut lists, &info, Step::Up, k, false, (a[k as usize], a[k as usize - 1]), at);
        }
        dispatch(&mut ds, &mut lists, &info, Step::Finish, 0, false, (a[0], 1), all);

        let mut steps = vec![0u8; ds.len() * STRIDE as usize];
        for (i, d) in ds.iter().enumerate() {
            let at = i * STRIDE as usize;
            steps[at..at + STEP as usize].copy_from_slice(&bytes(&[d.list, d.level, u32::from(d.horizontal), 0], u32::to_le_bytes));
        }
        let init = |label, contents: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage });
        let steps = init("effect steps", &steps, wgpu::BufferUsages::UNIFORM);
        let regions = init("effect regions", &regions, wgpu::BufferUsages::STORAGE);
        let lists = init("effect lists", &bytes(&lists, u32::to_le_bytes), wgpu::BufferUsages::STORAGE);
        let weights = init("blur weights", &bytes(&weights, f32::to_le_bytes), wgpu::BufferUsages::STORAGE);

        // A bind group per (finish, textures); dispatches differ only in the step offset.
        let mut groups: Vec<((bool, usize, usize), wgpu::BindGroup)> = Vec::new();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("effects") });
        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("effects"), timestamp_writes: None });
            for (i, d) in ds.iter().enumerate() {
                let finish = d.step == Step::Finish;
                let key = (finish, d.tex.0, d.tex.1);
                let g = match groups.iter().position(|g| g.0 == key) {
                    Some(g) => g,
                    None => {
                        let view = |t: usize| wgpu::BindingResource::TextureView(&tex[t].view);
                        let mut entries = vec![
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &steps, offset: 0, size: NonZeroU64::new(STEP) }) },
                            wgpu::BindGroupEntry { binding: 1, resource: regions.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: lists.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: weights.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 4, resource: view(d.tex.0) },
                        ];
                        if finish {
                            entries.extend([wgpu::BindGroupEntry { binding: 5, resource: view(0) }, wgpu::BindGroupEntry { binding: 6, resource: view(d.tex.1) }]);
                        } else {
                            entries.push(wgpu::BindGroupEntry { binding: 5, resource: view(d.tex.1) });
                        }
                        let layout = if finish { &self.finish_layout } else { &self.layout };
                        groups.push((key, device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &entries })));
                        groups.len() - 1
                    }
                };
                cp.set_pipeline(match d.step {
                    Step::Prepare => &self.prepare,
                    Step::Morph => &self.morph,
                    Step::Down => &self.down,
                    Step::Blur => &self.blur,
                    Step::Up => &self.up,
                    Step::Finish => &self.finish,
                });
                cp.set_bind_group(0, &groups[g].1, &[(i as u64 * STRIDE) as u32]);
                cp.dispatch_workgroups(d.size.0.div_ceil(8), d.size.1.div_ceil(8), d.count);
            }
        }
        queue.submit([enc.finish()]);
        Some((out, ds.len()))
    }
}
