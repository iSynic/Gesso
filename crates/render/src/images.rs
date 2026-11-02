//! Decoded images, keyed by asset hash, with a mip chain for zoomed-out drawing.
//!
//! Decoding is synchronous by default (headless rendering and export need every image). With a
//! [`Waker`] set, [`ImageCache::request`] decodes on the thread pool instead: frames draw
//! placeholders meanwhile, and the waker is called as each decode finishes.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use doc::model::{AssetId, AssetMeta};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

use crate::list::Image;

/// Where image bytes come from: a `.canvas` bundle, or memory in tests.
pub trait AssetSource: Send + Sync {
    fn read(&self, id: &AssetId, meta: &AssetMeta) -> Option<Vec<u8>>;
}

impl AssetSource for format::Bundle {
    fn read(&self, id: &AssetId, meta: &AssetMeta) -> Option<Vec<u8>> {
        self.read_asset(id, meta).ok()
    }
}

/// Assets held in memory.
#[derive(Default)]
pub struct MemoryAssets(pub FxHashMap<AssetId, Vec<u8>>);

impl AssetSource for MemoryAssets {
    fn read(&self, id: &AssetId, _: &AssetMeta) -> Option<Vec<u8>> {
        self.0.get(id).cloned()
    }
}

/// No assets: every image draws as a placeholder.
pub struct NoAssets;

impl AssetSource for NoAssets {
    fn read(&self, _: &AssetId, _: &AssetMeta) -> Option<Vec<u8>> {
        None
    }
}

/// Smallest mip edge kept.
const MIN_MIP: u32 = 32;
/// Bytes of decoded pixels kept before least-recently used images are dropped.
pub const DEFAULT_BUDGET: usize = 512 << 20;

struct Entry {
    /// Level 0 is full size; each next level halves both sides.
    mips: Option<Vec<Arc<Image>>>,
    bytes: usize,
    used: u64,
}

/// Background decodes run on their own pool, a third of the cores (at least 2), so the frame's
/// parallel work (text shaping, fragment builds) never queues behind them on the global pool.
/// (The global pool if it can't be created.)
fn spawn_decode(job: impl FnOnce() + Send + 'static) {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| (n.get() / 3).max(2));
        rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("image-decode-{i}")).build().ok()
    });
    match pool {
        Some(p) => p.spawn(job),
        None => rayon::spawn(job),
    }
}

/// Called on a decode thread when an asynchronous decode finishes (to wake the UI).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

type Decoded = (AssetId, Option<Vec<Arc<Image>>>);

/// Background decodes a frame takes in at most.
const TAKE_PER_FRAME: usize = 8;

/// Decodes under way on the thread pool.
struct Jobs {
    tx: Sender<Decoded>,
    rx: Mutex<Receiver<Decoded>>,
    pending: FxHashSet<AssetId>,
    /// Finished, not yet taken in: a frame takes at most [`TAKE_PER_FRAME`], so a burst of
    /// decodes spreads its GPU uploads over several frames.
    ready: VecDeque<Decoded>,
    wake: Waker,
    /// Decoding synchronously for now (a complete frame).
    paused: bool,
}

pub struct ImageCache {
    entries: FxHashMap<AssetId, Entry>,
    bytes: usize,
    budget: usize,
    generation: u64,
    jobs: Option<Jobs>,
}

impl Default for ImageCache {
    fn default() -> Self {
        ImageCache { entries: FxHashMap::default(), bytes: 0, budget: DEFAULT_BUDGET, generation: 0, jobs: None }
    }
}

fn key(id: &AssetId, level: usize) -> u64 {
    let mut h = FxHasher::default();
    id.0.hash(&mut h);
    level.hash(&mut h);
    h.finish()
}

fn decode(id: &AssetId, bytes: &[u8]) -> Option<Vec<Arc<Image>>> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let mut mips = Vec::new();
    let mut cur = img;
    loop {
        let (w, h) = cur.dimensions();
        let next = (w / 2 >= MIN_MIP && h / 2 >= MIN_MIP).then(|| image::imageops::resize(&cur, w / 2, h / 2, image::imageops::FilterType::Triangle));
        mips.push(Arc::new(Image { key: key(id, mips.len()), width: w, height: h, rgba: peniko::Blob::new(Arc::new(cur.into_raw())) }));
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    Some(mips)
}

impl ImageCache {
    pub fn with_budget(budget: usize) -> Self {
        ImageCache { budget, ..Default::default() }
    }

    /// Decodes the given assets that aren't cached yet, in parallel, and waits for them.
    pub fn prefetch(&mut self, src: &dyn AssetSource, assets: &[(AssetId, AssetMeta)]) {
        let todo: Vec<&(AssetId, AssetMeta)> = assets.iter().filter(|(id, _)| !self.entries.contains_key(id)).collect();
        let decoded: Vec<Decoded> = todo.par_iter().map(|(id, meta)| (id.clone(), src.read(id, meta).and_then(|b| decode(id, &b)))).collect();
        for (id, mips) in decoded {
            self.insert(id, mips);
        }
        self.trim();
    }

    /// Makes [`ImageCache::request`] decode in the background, calling `wake` as each decode
    /// finishes; `None` goes back to decoding synchronously.
    pub fn set_waker(&mut self, wake: Option<Waker>) {
        self.jobs = wake.map(|wake| {
            let (tx, rx) = channel();
            Jobs { tx, rx: Mutex::new(rx), pending: FxHashSet::default(), ready: VecDeque::new(), wake, paused: false }
        });
    }

    pub fn is_async(&self) -> bool {
        self.jobs.as_ref().is_some_and(|j| !j.paused)
    }

    /// Decodes synchronously until [`ImageCache::restore_waker`] (background decodes already
    /// under way still land).
    pub(crate) fn take_waker(&mut self) -> bool {
        self.jobs.as_mut().is_some_and(|j| !std::mem::replace(&mut j.paused, true))
    }

    pub(crate) fn restore_waker(&mut self, taken: bool) {
        if let (true, Some(j)) = (taken, &mut self.jobs) {
            j.paused = false;
        }
    }

    /// Starts decoding the given assets that aren't cached or under way: on the thread pool when
    /// a waker is set (they draw as placeholders until [`ImageCache::poll`] picks them up),
    /// otherwise right away.
    pub fn request(&mut self, src: &Arc<dyn AssetSource>, assets: &[(AssetId, AssetMeta)]) {
        let Some(jobs) = self.jobs.as_mut().filter(|j| !j.paused) else {
            self.prefetch(&**src, assets);
            return;
        };
        for (id, meta) in assets {
            if self.entries.contains_key(id) || !jobs.pending.insert(id.clone()) {
                continue;
            }
            let (src, id, meta, tx, wake) = (src.clone(), id.clone(), meta.clone(), jobs.tx.clone(), jobs.wake.clone());
            spawn_decode(move || {
                let mips = src.read(&id, &meta).and_then(|b| decode(&id, &b));
                if tx.send((id, mips)).is_ok() {
                    wake();
                }
            });
        }
    }

    /// Takes in finished background decodes; returns their ids.
    pub fn poll(&mut self) -> Vec<AssetId> {
        self.collect(false)
    }

    /// Waits for every background decode under way; returns their ids.
    pub fn wait(&mut self) -> Vec<AssetId> {
        self.collect(true)
    }

    fn collect(&mut self, all: bool) -> Vec<AssetId> {
        let Some(jobs) = &mut self.jobs else { return Vec::new() };
        // Everything still out (pending, not yet received), or whatever has arrived.
        let out = jobs.pending.len() - jobs.ready.len();
        if let Ok(rx) = jobs.rx.lock() {
            if all { jobs.ready.extend(rx.iter().take(out)) } else { jobs.ready.extend(rx.try_iter()) }
        }
        let take = if all { jobs.ready.len() } else { jobs.ready.len().min(TAKE_PER_FRAME) };
        let done: Vec<Decoded> = jobs.ready.drain(..take).collect();
        for (id, _) in &done {
            jobs.pending.remove(id);
        }
        let mut ids = Vec::with_capacity(done.len());
        for (id, mips) in done {
            ids.push(id.clone());
            self.insert(id, mips);
        }
        if !ids.is_empty() {
            self.trim();
        }
        ids
    }

    /// Whether finished decodes are waiting to be taken in by a later frame.
    pub fn has_ready(&self) -> bool {
        self.jobs.as_ref().is_some_and(|j| !j.ready.is_empty())
    }

    /// Background decodes under way (or finished and not yet taken in).
    pub fn pending(&self) -> usize {
        self.jobs.as_ref().map_or(0, |j| j.pending.len())
    }

    fn insert(&mut self, id: AssetId, mips: Option<Vec<Arc<Image>>>) {
        if self.entries.contains_key(&id) {
            return;
        }
        let bytes = mips.as_ref().map_or(0, |m| m.iter().map(|i| i.rgba.len()).sum());
        self.bytes += bytes;
        self.entries.insert(id, Entry { mips, bytes, used: self.generation });
    }

    /// The mip of `id` closest to (but not smaller than) `want_w` pixels wide, if decoded.
    pub fn get(&mut self, id: &AssetId, want_w: f64) -> Option<Arc<Image>> {
        let e = self.entries.get_mut(id)?;
        e.used = self.generation;
        let mips = e.mips.as_ref()?;
        let mut pick = &mips[0];
        for m in mips.iter().skip(1) {
            if (m.width as f64) < want_w {
                break;
            }
            pick = m;
        }
        Some(pick.clone())
    }

    /// Like [`ImageCache::get`], without marking the entry used (for parallel readers).
    pub fn peek(&self, id: &AssetId, want_w: f64) -> Option<Arc<Image>> {
        let mips = self.entries.get(id)?.mips.as_ref()?;
        let mut pick = &mips[0];
        for m in mips.iter().skip(1) {
            if (m.width as f64) < want_w {
                break;
            }
            pick = m;
        }
        Some(pick.clone())
    }

    /// Full-size dimensions of a decoded asset.
    pub fn size(&self, id: &AssetId) -> Option<(u32, u32)> {
        self.entries.get(id)?.mips.as_ref().map(|m| (m[0].width, m[0].height))
    }

    pub fn is_cached(&self, id: &AssetId) -> bool {
        self.entries.contains_key(id)
    }

    /// Call once per frame.
    pub fn next_generation(&mut self) {
        self.generation += 1;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn trim(&mut self) {
        while self.bytes > self.budget {
            let Some(oldest) = self.entries.iter().filter(|(_, e)| e.bytes > 0).min_by_key(|(_, e)| e.used).map(|(id, _)| id.clone()) else { break };
            if let Some(e) = self.entries.remove(&oldest) {
                self.bytes -= e.bytes;
            }
        }
    }
}
