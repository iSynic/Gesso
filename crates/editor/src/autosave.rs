//! Autosave and crash recovery, as an op journal.
//!
//! Every committed op (edits, undo, redo) is recorded by the session. Two seconds after the
//! last edit the batch is compacted (runs of property updates keep the last value per node and
//! property, so a 120-frame drag becomes one op per property) and handed to a worker thread,
//! which appends it to `<key>.journal.jsonl` as one line. The journal applies on top of a base:
//! the bundle on disk for a document opened and not yet edited, or otherwise a base snapshot
//! written once when the journal starts. The main thread never serialises the whole document
//! per autosave; a `Document::to_data` snapshot of the 50 MB document took 47 ms, too long
//! for a frame.
//!
//! Saving deletes the journal; on launch, journals whose base is still current are offered for
//! restore, which loads the base and replays the batches.

use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use doc::{NodeId, Op, PropKey, Store, TxMeta};
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

use crate::session::{Error, Session};

/// Quiet time after the last edit before the journal is written.
pub const DELAY: Duration = Duration::from_secs(2);

const JOURNAL_SUFFIX: &str = ".journal.jsonl";
const BASE_SUFFIX: &str = ".base.json";
const META_SUFFIX: &str = ".meta.json";

/// The default recovery folder: `<local data dir>/ai-design-engine/recovery`.
pub fn default_dir() -> Option<PathBuf> {
    crate::dirs::data_local_dir().map(|d| d.join("ai-design-engine").join("recovery"))
}

/// What the journal applies to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Base {
    /// The bundle at `source`, as saved when the journal started.
    Bundle,
    /// `<key>.base.json`, written when the journal started.
    Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// The bundle the work belongs to; `None` for an untitled document.
    pub source: Option<PathBuf>,
    pub base: Base,
    /// When the journal started and when it was last appended to (ms since the Unix epoch).
    pub started_at_ms: u64,
    pub saved_at_ms: u64,
}

/// A journal found on launch.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    pub journal: PathBuf,
    pub meta: Meta,
}

/// Timings of the last journal batch, for the benchmark.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SnapshotStats {
    /// Main-thread work: taking and compacting the batch.
    pub clone_ms: f64,
    /// Ops in the batch before and after compaction.
    pub ops: usize,
    pub compacted: usize,
}

enum Job {
    /// Starts a journal: clears old files, writes the base snapshot if any.
    Begin { paths: Paths, base: Option<doc::DocumentData> },
    Append { paths: Paths, ops: Vec<Op>, meta: Meta },
    Discard { paths: Paths },
}

#[derive(Clone)]
struct Paths {
    journal: PathBuf,
    base: PathBuf,
    meta: PathBuf,
}

impl Paths {
    fn of(dir: &Path, key: &str) -> Paths {
        Paths { journal: dir.join(format!("{key}{JOURNAL_SUFFIX}")), base: dir.join(format!("{key}{BASE_SUFFIX}")), meta: dir.join(format!("{key}{META_SUFFIX}")) }
    }

    fn remove(&self) {
        // Meta first: without it a leftover journal is never offered.
        let _ = std::fs::remove_file(&self.meta);
        let _ = std::fs::remove_file(&self.journal);
        let _ = std::fs::remove_file(&self.base);
    }
}

pub struct Autosave {
    dir: PathBuf,
    key: String,
    source: Option<PathBuf>,
    base: Base,
    started_at_ms: u64,
    /// The session revision written last, and when a newer one was first seen.
    written: u64,
    pending_since: Option<(u64, Instant)>,
    worker: Option<(mpsc::Sender<Job>, thread::JoinHandle<()>)>,
    pub last: SnapshotStats,
}

/// A file key unique to this autosave: the bundle's name and path hash (or "untitled"), the
/// process and the time, so a new session never touches an earlier session's journal.
fn key_for(path: Option<&Path>) -> String {
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let stem = match path {
        Some(p) => {
            let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            let mut h = rustc_hash::FxHasher::default();
            p.hash(&mut h);
            let name = p.file_name().map(|n| n.to_string_lossy().chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(40).collect::<String>()).unwrap_or_default();
            format!("{name}-{:016x}", h.finish())
        }
        None => "untitled".to_string(),
    };
    format!("{stem}-{}-{t}", std::process::id())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn run(job: Job) {
    match job {
        Job::Begin { paths, base } => {
            paths.remove();
            if let Some(data) = base {
                let _ = write_atomic(&paths.base, format::data_to_json(&data).as_bytes());
            }
        }
        Job::Append { paths, ops, meta } => {
            let Ok(mut line) = serde_json::to_vec(&ops) else { return };
            line.push(b'\n');
            let appended = std::fs::OpenOptions::new().create(true).append(true).open(&paths.journal).and_then(|mut f| {
                f.write_all(&line)?;
                f.sync_data()
            });
            // The meta makes the journal visible to recovery, so it goes last.
            if appended.is_ok()
                && let Ok(m) = serde_json::to_vec_pretty(&meta)
            {
                let _ = write_atomic(&paths.meta, &m);
            }
        }
        Job::Discard { paths } => paths.remove(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Drops property updates that a later update in the same run overwrites. A run ends at any
/// structural op, so ids that are deleted and re-inserted (undo) keep their order.
pub fn compact(ops: Vec<Op>) -> Vec<Op> {
    let mut out: Vec<Op> = Vec::with_capacity(ops.len());
    let mut seen: FxHashSet<(NodeId, PropKey)> = FxHashSet::default();
    let mut run: Vec<Op> = Vec::new();
    let flush = |run: &mut Vec<Op>, seen: &mut FxHashSet<(NodeId, PropKey)>, out: &mut Vec<Op>| {
        seen.clear();
        let mut kept: Vec<Op> = run
            .drain(..)
            .rev()
            .filter(|op| match op {
                Op::Update { id, value } => seen.insert((*id, value.key())),
                _ => true,
            })
            .collect();
        kept.reverse();
        out.extend(kept);
    };
    for op in ops {
        if matches!(op, Op::Update { .. }) {
            run.push(op);
        } else {
            flush(&mut run, &mut seen, &mut out);
            out.push(op);
        }
    }
    flush(&mut run, &mut seen, &mut out);
    out
}

impl Autosave {
    /// Autosave for the session's document into `dir` (created if needed). A session opened
    /// from a bundle and not yet edited journals against the bundle; any other starts with a
    /// base snapshot (one `to_data` on this thread).
    pub fn new(dir: PathBuf, s: &mut Session) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        s.enable_journal();
        let _ = s.take_journal();
        let mut a = Autosave {
            key: key_for(s.path()),
            dir,
            source: s.path().map(Path::to_path_buf),
            base: Base::Bundle,
            started_at_ms: 0,
            written: s.revision(),
            pending_since: None,
            worker: None,
            last: SnapshotStats::default(),
        };
        a.begin(s);
        a
    }

    fn paths(&self) -> Paths {
        Paths::of(&self.dir, &self.key)
    }

    fn send(&mut self, job: Job) {
        let (tx, _) = self.worker.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let h = thread::spawn(move || {
                while let Ok(job) = rx.recv() {
                    run(job);
                }
            });
            (tx, h)
        });
        if tx.send(job).is_err() {
            self.worker = None;
        }
    }

    /// Starts a fresh journal against the session's current state.
    fn begin(&mut self, s: &Session) {
        let on_bundle = self.source.is_some() && !s.is_dirty();
        self.base = if on_bundle { Base::Bundle } else { Base::Snapshot };
        self.started_at_ms = now_ms();
        let base = (!on_bundle).then(|| s.doc().to_data());
        let paths = self.paths();
        self.send(Job::Begin { paths, base });
    }

    /// Call once per frame. Returns `true` when a batch was written.
    pub fn tick(&mut self, s: &mut Session, now: Instant) -> bool {
        let rev = s.revision();
        if rev == self.written {
            self.pending_since = None;
            return false;
        }
        let since = match self.pending_since {
            Some((r, t)) if r == rev => t,
            _ => {
                self.pending_since = Some((rev, now));
                return false;
            }
        };
        if now.duration_since(since) < DELAY {
            return false;
        }
        self.snapshot(s);
        true
    }

    /// Hands the ops committed so far to the worker now.
    pub fn snapshot(&mut self, s: &mut Session) {
        let t = Instant::now();
        let ops = s.take_journal();
        let n = ops.len();
        let ops = compact(ops);
        self.last = SnapshotStats { clone_ms: t.elapsed().as_secs_f64() * 1000.0, ops: n, compacted: ops.len() };
        self.written = s.revision();
        self.pending_since = None;
        if ops.is_empty() {
            return;
        }
        let meta = Meta { source: self.source.clone(), base: self.base, started_at_ms: self.started_at_ms, saved_at_ms: now_ms() };
        let paths = self.paths();
        self.send(Job::Append { paths, ops, meta });
    }

    /// The document was saved: the bundle is the new base and the journal starts over. The
    /// key follows the session's path, which may have changed with Save As.
    pub fn saved(&mut self, s: &mut Session) {
        self.discard();
        let _ = s.take_journal();
        self.key = key_for(s.path());
        self.source = s.path().map(Path::to_path_buf);
        self.written = s.revision();
        self.pending_since = None;
        self.begin(s);
    }

    /// Deletes this session's recovery files (queued behind pending writes).
    pub fn discard(&mut self) {
        let paths = self.paths();
        self.send(Job::Discard { paths });
    }

    /// Waits for queued writes (tests, shutdown).
    pub fn flush(&mut self) {
        if let Some((tx, h)) = self.worker.take() {
            drop(tx);
            let _ = h.join();
        }
    }
}

/// Journals in `dir` worth offering: untitled ones, and ones whose base bundle has not been
/// saved since the journal started (others are deleted).
pub fn find(dir: &Path) -> Vec<Recovery> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = name.strip_suffix(META_SUFFIX) else { continue };
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(meta) = serde_json::from_slice::<Meta>(&bytes) else { continue };
        let journal = dir.join(format!("{stem}{JOURNAL_SUFFIX}"));
        if !journal.exists() || (meta.base == Base::Snapshot && !dir.join(format!("{stem}{BASE_SUFFIX}")).exists()) {
            continue;
        }
        let current = match (&meta.source, meta.base) {
            (Some(src), Base::Bundle) => {
                let saved = std::fs::metadata(src.join(format::DOCUMENT_FILE)).and_then(|m| m.modified()).ok();
                let saved = saved.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
                saved.is_some_and(|t| t <= meta.started_at_ms)
            }
            _ => true,
        };
        if current {
            out.push(Recovery { journal, meta });
        } else {
            // The bundle was saved after this journal started, so it no longer applies.
            remove(&Recovery { journal, meta });
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.meta.saved_at_ms));
    out
}

/// Loads a journal's base and replays it. A torn last line (a crash mid-write) is ignored.
pub fn load(r: &Recovery) -> Result<doc::Document, Error> {
    let io = |p: &Path, e| format::FormatError::Io { path: p.display().to_string(), source: e };
    let base = match (r.meta.base, &r.meta.source) {
        (Base::Bundle, Some(src)) => format::Bundle::open(src)?.doc,
        _ => {
            let p = with_suffix(&r.journal, BASE_SUFFIX);
            let bytes = std::fs::read(&p).map_err(|e| io(&p, e))?;
            format::from_json(&bytes)?
        }
    };
    let text = std::fs::read_to_string(&r.journal).map_err(|e| io(&r.journal, e))?;
    let mut store = Store::new(base);
    for line in text.lines() {
        let Ok(ops) = serde_json::from_str::<Vec<Op>>(line) else { break };
        store.transact(TxMeta::user("Recover"), |tx| {
            for op in ops {
                tx.apply(op)?;
            }
            Ok(())
        })?;
    }
    Ok(store.into_doc())
}

fn with_suffix(journal: &Path, suffix: &str) -> PathBuf {
    let name = journal.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(JOURNAL_SUFFIX)).unwrap_or("recovery");
    journal.with_file_name(format!("{name}{suffix}"))
}

/// Deletes a journal and its base and meta.
pub fn remove(r: &Recovery) {
    Paths { journal: r.journal.clone(), base: with_suffix(&r.journal, BASE_SUFFIX), meta: with_suffix(&r.journal, META_SUFFIX) }.remove();
}
