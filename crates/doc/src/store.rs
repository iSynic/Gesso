//! Transactions, undo/redo and the commit hook. A [`Store`] owns the document; every change is
//! a transaction of primitive ops that either applies completely or not at all.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::document::{DocError, Document, NodeKey, VirtualSpec};
use crate::id::{IdGen, NodeId};
use crate::op::{ChangeSet, Op};
use crate::props::PropValue;

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    User,
    Agent,
    Script,
    Undo,
    Redo,
    System,
}

#[derive(Debug, Clone)]
pub struct TxMeta {
    pub origin: Origin,
    pub label: String,
    /// Consecutive transactions with the same key merge into one undo step (e.g. a drag).
    pub coalesce: Option<String>,
}

impl TxMeta {
    pub fn new(origin: Origin, label: impl Into<String>) -> Self {
        TxMeta { origin, label: label.into(), coalesce: None }
    }
    pub fn user(label: impl Into<String>) -> Self {
        Self::new(Origin::User, label)
    }
    pub fn coalesce(mut self, key: impl Into<String>) -> Self {
        self.coalesce = Some(key.into());
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Doc(#[from] DocError),
    #[error("invalid input: {0}")]
    Input(String),
}

/// A committed change, passed to commit observers (autosave, op log, later sync).
#[derive(Debug)]
pub struct Commit<'a> {
    pub seq: u64,
    pub origin: Origin,
    pub label: &'a str,
    /// The ops that were applied, in order.
    pub ops: &'a [Op],
    pub changes: &'a ChangeSet,
}

#[derive(Debug, Clone)]
struct Entry {
    label: String,
    coalesce: Option<String>,
    ops: Vec<Op>,
    inverses: Vec<Op>,
}

/// An open transaction. Ops apply immediately, so reads see earlier writes.
pub struct Tx<'a> {
    pub(crate) doc: &'a mut Document,
    ids: &'a mut IdGen,
    ops: Vec<Op>,
    inverses: Vec<Op>,
    changes: ChangeSet,
}

impl Tx<'_> {
    pub fn doc(&self) -> &Document {
        self.doc
    }

    pub fn new_id(&mut self) -> NodeId {
        self.ids.next_id()
    }

    /// Applies one primitive op. On error nothing from this op is applied; the caller usually
    /// returns the error, which rolls back the whole transaction.
    pub fn apply(&mut self, op: Op) -> Result<(), DocError> {
        let forward = op.clone();
        let (inverse, change) = self.doc.apply(op)?;
        self.ops.push(forward);
        self.inverses.push(inverse);
        self.changes.push(change);
        Ok(())
    }

    /// Updates an instance's virtual content (see [`Document::set_expansion`]). Derived state:
    /// the changes are reported with the transaction's, but no op is recorded.
    pub fn set_expansion(&mut self, instance: NodeKey, specs: Vec<VirtualSpec>) -> ChangeSet {
        let mut c = ChangeSet::default();
        self.doc.set_expansion(instance, specs, &mut c);
        self.changes.extend(&c);
        c
    }

    /// Sets a prop on a virtual node without recording an op. False for a real node.
    pub fn set_virtual(&mut self, id: NodeId, value: PropValue) -> bool {
        match self.doc.set_virtual(id, value) {
            Some(c) => {
                self.changes.push(c);
                true
            }
            None => false,
        }
    }

    fn rollback(self) {
        for inv in self.inverses.into_iter().rev() {
            let undone = self.doc.apply(inv);
            debug_assert!(undone.is_ok(), "rollback failed: {undone:?}");
        }
    }
}

type Observer = Box<dyn FnMut(&Commit<'_>)>;

/// Derives data from user edits inside the same transaction, e.g. layout writing computed
/// positions back. Its ops join the transaction's undo step; if it fails, the transaction rolls
/// back. Undo and redo restore derived values exactly, so they only call [`Deriver::observe`].
pub trait Deriver {
    /// Called once when attached, and whenever the whole document is replaced. May set up
    /// derived state (instance content) in `doc`.
    fn attach(&mut self, doc: &mut Document);
    /// Called at the end of every transaction that changed something.
    fn derive(&mut self, tx: &mut Tx<'_>, changes: &ChangeSet) -> Result<(), Error>;
    /// Recomputes everything (e.g. after fonts change); emits ops only where results differ.
    fn derive_all(&mut self, tx: &mut Tx<'_>) -> Result<(), Error>;
    /// Called after undo and redo so the deriver can resync without emitting ops. It may update
    /// derived state (instance content) in `doc`; returns the changes that made.
    fn observe(&mut self, doc: &mut Document, changes: &ChangeSet) -> ChangeSet;
    /// For callers that need the concrete type (stats, configuration).
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

pub struct Store {
    doc: Document,
    ids: IdGen,
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    max_undo: usize,
    seq: u64,
    observers: Vec<Observer>,
    deriver: Option<Box<dyn Deriver>>,
    open: Option<Open>,
}

/// A transaction kept open across calls ([`Store::begin`] … [`Store::commit`]), for callers that
/// can't run their edits inside one closure, e.g. a script calling back into the host.
struct Open {
    meta: TxMeta,
    ops: Vec<Op>,
    inverses: Vec<Op>,
    changes: ChangeSet,
    /// Changes the deriver hasn't seen yet.
    underived: ChangeSet,
}

fn open_error() -> Error {
    Error::Input("a transaction is already open".into())
}

fn not_open() -> Error {
    Error::Input("no open transaction".into())
}

impl Default for Store {
    fn default() -> Self {
        Store::new(Document::new())
    }
}

impl Store {
    pub fn new(doc: Document) -> Self {
        Store { doc, ids: IdGen::Random, undo: VecDeque::new(), redo: Vec::new(), max_undo: 1000, seq: 0, observers: Vec::new(), deriver: None, open: None }
    }

    pub fn with_ids(mut self, ids: IdGen) -> Self {
        self.ids = ids;
        self
    }

    /// Replaces the id source, returning the previous one.
    pub fn set_ids(&mut self, ids: IdGen) -> IdGen {
        std::mem::replace(&mut self.ids, ids)
    }

    pub fn set_max_undo(&mut self, n: usize) {
        self.max_undo = n.max(1);
        while self.undo.len() > self.max_undo {
            self.undo.pop_front();
        }
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn into_doc(self) -> Document {
        self.doc
    }

    pub fn new_id(&mut self) -> NodeId {
        self.ids.next_id()
    }

    pub fn set_deriver(&mut self, mut d: Box<dyn Deriver>) {
        d.attach(&mut self.doc);
        self.deriver = Some(d);
    }

    pub fn deriver_mut(&mut self) -> Option<&mut (dyn Deriver + 'static)> {
        self.deriver.as_deref_mut()
    }

    /// Runs `f` as a transaction that may call the deriver directly (e.g. `derive_all`).
    pub fn transact_with_deriver<R>(&mut self, meta: TxMeta, f: impl FnOnce(&mut Tx<'_>, Option<&mut (dyn Deriver + 'static)>) -> Result<R, Error>) -> Result<(R, ChangeSet), Error> {
        let mut d = self.deriver.take();
        let r = self.transact(meta, |tx| f(tx, d.as_deref_mut()));
        self.deriver = d;
        r
    }

    pub fn on_commit(&mut self, f: impl FnMut(&Commit<'_>) + 'static) {
        self.observers.push(Box::new(f));
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|e| e.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    /// Runs `f` as one transaction. If `f` returns an error, every op it applied is rolled back
    /// and the document is unchanged.
    pub fn transact<R>(&mut self, meta: TxMeta, f: impl FnOnce(&mut Tx<'_>) -> Result<R, Error>) -> Result<(R, ChangeSet), Error> {
        if self.open.is_some() {
            return Err(open_error());
        }
        let mut tx = Tx { doc: &mut self.doc, ids: &mut self.ids, ops: Vec::new(), inverses: Vec::new(), changes: ChangeSet::default() };
        let value = match f(&mut tx) {
            Ok(v) => v,
            Err(e) => {
                tx.rollback();
                return Err(e);
            }
        };
        if let Some(d) = self.deriver.as_deref_mut()
            && !tx.changes.is_empty()
        {
            let changes = tx.changes.clone();
            if let Err(e) = d.derive(&mut tx, &changes) {
                tx.rollback();
                return Err(e);
            }
        }
        let Tx { ops, inverses, changes, .. } = tx;
        self.record(meta, ops, inverses, &changes);
        Ok((value, changes))
    }

    /// Notifies observers and pushes an undo step (nothing when `ops` is empty).
    fn record(&mut self, meta: TxMeta, ops: Vec<Op>, inverses: Vec<Op>, changes: &ChangeSet) {
        if ops.is_empty() {
            return;
        }
        self.notify(meta.origin, &meta.label, &ops, changes);
        self.redo.clear();
        match self.undo.back_mut() {
            Some(last) if meta.coalesce.is_some() && last.coalesce == meta.coalesce => {
                last.ops.extend(ops);
                last.inverses.extend(inverses);
            }
            _ => {
                self.undo.push_back(Entry { label: meta.label, coalesce: meta.coalesce, ops, inverses });
                if self.undo.len() > self.max_undo {
                    self.undo.pop_front();
                }
            }
        }
    }

    // ---- open transactions --------------------------------------------------------------------

    /// Opens a transaction that stays open across [`Store::in_open`] calls until
    /// [`Store::commit`] or [`Store::rollback`]. Meanwhile `transact`, `undo` and `redo` fail.
    pub fn begin(&mut self, meta: TxMeta) -> Result<(), Error> {
        if self.open.is_some() {
            return Err(open_error());
        }
        self.open = Some(Open { meta, ops: Vec::new(), inverses: Vec::new(), changes: ChangeSet::default(), underived: ChangeSet::default() });
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Changes the open transaction's label and coalescing key, decided only once its edits
    /// are known (IME composition becoming a typing step).
    pub fn set_open_meta(&mut self, meta: TxMeta) {
        if let Some(o) = &mut self.open {
            o.meta = meta;
        }
    }

    /// Runs `f` inside the open transaction. If `f` fails, only its own ops are rolled back and
    /// the transaction stays open. Returns `f`'s value and the changes it made.
    pub fn in_open<R>(&mut self, f: impl FnOnce(&mut Tx<'_>) -> Result<R, Error>) -> Result<(R, ChangeSet), Error> {
        let Some(open) = self.open.as_mut() else { return Err(not_open()) };
        let mut tx = Tx { doc: &mut self.doc, ids: &mut self.ids, ops: Vec::new(), inverses: Vec::new(), changes: ChangeSet::default() };
        let value = match f(&mut tx) {
            Ok(v) => v,
            Err(e) => {
                tx.rollback();
                return Err(e);
            }
        };
        let Tx { ops, inverses, changes, .. } = tx;
        open.ops.extend(ops);
        open.inverses.extend(inverses);
        open.changes.extend(&changes);
        open.underived.extend(&changes);
        Ok((value, changes))
    }

    /// Runs the deriver over the open transaction's changes so far (e.g. layout, before reading
    /// computed positions). Returns the changes the deriver made.
    pub fn derive_open(&mut self) -> Result<ChangeSet, Error> {
        let Some(open) = self.open.as_mut() else { return Err(not_open()) };
        let Some(d) = self.deriver.as_deref_mut() else { return Ok(ChangeSet::default()) };
        if open.underived.is_empty() {
            return Ok(ChangeSet::default());
        }
        let underived = std::mem::take(&mut open.underived);
        let mut tx = Tx { doc: &mut self.doc, ids: &mut self.ids, ops: Vec::new(), inverses: Vec::new(), changes: ChangeSet::default() };
        if let Err(e) = d.derive(&mut tx, &underived) {
            tx.rollback();
            open.underived = underived;
            return Err(e);
        }
        let Tx { ops, inverses, changes, .. } = tx;
        open.ops.extend(ops);
        open.inverses.extend(inverses);
        open.changes.extend(&changes);
        Ok(changes)
    }

    /// Commits the open transaction as one undo step. Returns all of its changes and, separately,
    /// those the final derive made. If that derive fails, everything is rolled back and the error
    /// comes back with the rollback's changes (boxed: the pair is large and rare).
    pub fn commit(&mut self) -> Result<(ChangeSet, ChangeSet), Box<(Error, ChangeSet)>> {
        let derived = match self.derive_open() {
            Ok(d) => d,
            Err(e) => return Err(Box::new((e, self.rollback()))),
        };
        let Some(Open { meta, ops, inverses, changes, .. }) = self.open.take() else { return Err(Box::new((not_open(), ChangeSet::default()))) };
        self.record(meta, ops, inverses, &changes);
        Ok((changes, derived))
    }

    /// Undoes everything the open transaction applied and closes it. Returns the changes of the
    /// undo, so views can resync the touched nodes.
    pub fn rollback(&mut self) -> ChangeSet {
        let Some(open) = self.open.take() else { return ChangeSet::default() };
        let mut changes = ChangeSet::default();
        for inv in open.inverses.into_iter().rev() {
            match self.doc.apply(inv) {
                Ok((_, c)) => changes.push(c),
                Err(e) => debug_assert!(false, "rollback failed: {e:?}"),
            }
        }
        changes
    }

    /// Undoes the last transaction. Returns `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Result<Option<ChangeSet>, Error> {
        if self.open.is_some() {
            return Err(open_error());
        }
        let Some(entry) = self.undo.pop_back() else { return Ok(None) };
        let ops: Vec<Op> = entry.inverses.iter().rev().cloned().collect();
        match self.replay(Origin::Undo, &entry.label, ops) {
            Ok(changes) => {
                self.redo.push(entry);
                Ok(Some(changes))
            }
            Err(e) => {
                self.undo.push_back(entry);
                Err(e)
            }
        }
    }

    pub fn redo(&mut self) -> Result<Option<ChangeSet>, Error> {
        if self.open.is_some() {
            return Err(open_error());
        }
        let Some(entry) = self.redo.pop() else { return Ok(None) };
        match self.replay(Origin::Redo, &entry.label, entry.ops.clone()) {
            Ok(changes) => {
                self.undo.push_back(entry);
                Ok(Some(changes))
            }
            Err(e) => {
                self.redo.push(entry);
                Err(e)
            }
        }
    }

    /// Applies `ops` atomically without touching the undo stacks.
    fn replay(&mut self, origin: Origin, label: &str, ops: Vec<Op>) -> Result<ChangeSet, Error> {
        let mut tx = Tx { doc: &mut self.doc, ids: &mut self.ids, ops: Vec::new(), inverses: Vec::new(), changes: ChangeSet::default() };
        for op in ops {
            if let Err(e) = tx.apply(op) {
                tx.rollback();
                return Err(e.into());
            }
        }
        let Tx { ops, mut changes, .. } = tx;
        if let Some(d) = self.deriver.as_deref_mut() {
            let derived = d.observe(&mut self.doc, &changes);
            changes.extend(&derived);
        }
        self.notify(origin, label, &ops, &changes);
        Ok(changes)
    }

    fn notify(&mut self, origin: Origin, label: &str, ops: &[Op], changes: &ChangeSet) {
        self.seq += 1;
        if self.observers.is_empty() {
            return;
        }
        let commit = Commit { seq: self.seq, origin, label, ops, changes };
        for o in &mut self.observers {
            o(&commit);
        }
    }
}
