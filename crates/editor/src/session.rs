//! One open document: the store with its layout deriver, the render scene and index for the
//! current page, the camera, selection and scope. Every edit goes through [`Session::transact`]
//! (or undo/redo), which brings the scene, index and layers model up to date.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use doc::model::{AssetId, AssetMeta};
use doc::{ChangeSet, Document, NodeId, NodeKind, Op, Store, Tx, TxMeta};
use format::Bundle;
use kurbo::{Point, Rect, Vec2};
use layout::LayoutDeriver;
use render::{AssetSource, Scene};
use rustc_hash::FxHashMap;
use serde_json::json;
use text::{FontDb, TextEngine};

use crate::camera::Camera;
use crate::layers::Layers;
use crate::select;

/// An SVG ready to insert (from [`Session::import_svg`]).
pub struct SvgImport {
    pub node: serde_json::Value,
    pub assets: Vec<(AssetId, AssetMeta)>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Doc(#[from] doc::Error),
    #[error(transparent)]
    Format(#[from] format::FormatError),
    #[error("{0}")]
    Other(String),
}

/// Image bytes not yet written to a bundle (an untitled document's), by id.
#[derive(Default)]
struct Pending(RwLock<FxHashMap<AssetId, Arc<Vec<u8>>>>);

/// A session's images: those added since the last Save As, then the bundle's.
struct Assets {
    bundle: Option<Bundle>,
    pending: Arc<Pending>,
}

impl AssetSource for Assets {
    fn read(&self, id: &AssetId, meta: &AssetMeta) -> Option<Vec<u8>> {
        if let Some(b) = self.pending.0.read().ok().and_then(|m| m.get(id).cloned()) {
            return Some(b.to_vec());
        }
        self.bundle.as_ref().and_then(|b| b.read_asset(id, meta).ok())
    }
}

/// How a click changes the selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClickMods {
    /// Toggle the picked node (Shift).
    pub toggle: bool,
    /// Pick the deepest node (Ctrl).
    pub deep: bool,
}

pub struct Session {
    store: Store,
    pub scene: Scene,
    page: NodeId,
    pub camera: Camera,
    cameras: FxHashMap<NodeId, Camera>,
    /// Viewport size in logical pixels.
    pub viewport: Vec2,
    /// Interface points per canvas unit: the shell's interface scale. The camera works in
    /// interface points, so this keeps the interface's size out of the canvas's.
    pub view_scale: f64,
    selection: Vec<NodeId>,
    /// The container whose children clicks select; `None` = page level.
    scope: Option<NodeId>,
    pub layers: Layers,
    /// Snap to neighbours' edges, centres and spacing.
    pub snap_to_objects: bool,
    /// Round to whole pixels when nothing else snaps.
    pub snap_to_pixels: bool,
    fonts: FontDb,
    assets: Arc<dyn AssetSource>,
    pending: Arc<Pending>,
    image_waker: Option<render::Waker>,
    bundle: Option<Bundle>,
    dirty: bool,
    /// Bumped on every change to the document.
    revision: u64,
    /// The ops of every commit (edits, undo, redo) since the last [`Session::take_journal`],
    /// once [`Session::enable_journal`] was called (autosave).
    journal: Option<Arc<Mutex<Vec<Op>>>>,
    /// Changes of the open transaction (see [`Session::begin`]) the scene hasn't seen yet.
    tx_unsynced: ChangeSet,
    /// Whether the scene has seen part of the open transaction.
    tx_synced: bool,
}

/// A document with one empty page.
pub fn untitled_doc() -> Result<Document, Error> {
    let mut s = Store::new(Document::new());
    s.transact(TxMeta::user("New document"), |tx| tx.insert_json(None, None, json!({"type": "page", "name": "Page 1"})))?;
    Ok(s.into_doc())
}

impl Session {
    /// A session over `doc`, laid out and indexed. A document without pages gets one.
    pub fn new(mut doc: Document, bundle: Option<Bundle>, fonts: FontDb) -> Result<Self, Error> {
        if doc.pages().is_empty() {
            let mut s = Store::new(doc);
            s.transact(TxMeta::user("Add page"), |tx| tx.insert_json(None, None, json!({"type": "page", "name": "Page 1"})))?;
            doc = s.into_doc();
        }
        let page = doc.pages().first().and_then(|&k| doc.node(k)).map(|n| n.id).ok_or_else(|| Error::Other("document has no page".into()))?;
        let mut deriver = LayoutDeriver::new(TextEngine::new(fonts.clone()));
        deriver.prepare(&doc);
        let mut store = Store::new(doc);
        store.set_deriver(Box::new(deriver));
        let pending = Arc::new(Pending::default());
        let assets: Arc<dyn AssetSource> = Arc::new(Assets { bundle: bundle.clone(), pending: pending.clone() });
        let scene = Scene::new(store.doc(), page, fonts.clone(), assets.clone());
        Ok(Session {
            store,
            scene,
            page,
            camera: Camera::default(),
            cameras: FxHashMap::default(),
            viewport: Vec2::new(1280.0, 800.0),
            view_scale: 1.0,
            selection: Vec::new(),
            scope: None,
            layers: Layers::new(page),
            snap_to_objects: true,
            snap_to_pixels: true,
            fonts,
            assets,
            pending,
            image_waker: None,
            bundle,
            dirty: false,
            revision: 0,
            journal: None,
            tx_unsynced: ChangeSet::default(),
            tx_synced: false,
        })
    }

    /// Starts recording committed ops for [`Session::take_journal`]. Idempotent.
    pub fn enable_journal(&mut self) {
        if self.journal.is_some() {
            return;
        }
        let j = Arc::new(Mutex::new(Vec::new()));
        let sink = j.clone();
        self.store.on_commit(move |c| {
            if let Ok(mut v) = sink.lock() {
                v.extend_from_slice(c.ops);
            }
        });
        self.journal = Some(j);
    }

    /// The ops committed since the last call (empty unless journaling is on).
    pub fn take_journal(&mut self) -> Vec<Op> {
        self.journal.as_ref().and_then(|j| j.lock().ok().map(|mut v| std::mem::take(&mut *v))).unwrap_or_default()
    }

    pub fn untitled(fonts: FontDb) -> Result<Self, Error> {
        Self::new(untitled_doc()?, None, fonts)
    }

    /// Opens a bundle folder.
    pub fn open(path: impl Into<PathBuf>, fonts: FontDb) -> Result<Self, Error> {
        let opened = Bundle::open(path)?;
        Self::new(opened.doc, Some(opened.bundle), fonts)
    }

    pub fn doc(&self) -> &Document {
        self.store.doc()
    }

    /// Expands the layers tree down to `id`.
    pub fn reveal_layer(&mut self, id: NodeId) {
        self.layers.reveal(self.store.doc(), id);
    }

    /// The layers panel's rows with the document they index.
    pub fn layer_rows(&mut self) -> (&Document, &[crate::layers::Row]) {
        let doc = self.store.doc();
        (doc, self.layers.rows(doc))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The scene and the document together (rendering needs both).
    pub fn scene_and_doc(&mut self) -> (&mut Scene, &Document) {
        (&mut self.scene, self.store.doc())
    }

    pub fn page(&self) -> NodeId {
        self.page
    }

    pub fn path(&self) -> Option<&Path> {
        self.bundle.as_ref().map(Bundle::root)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the document changed without an edit (a restored recovery snapshot).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.revision += 1;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn fonts(&self) -> &FontDb {
        &self.fonts
    }

    // ---- edits --------------------------------------------------------------------------------

    /// Runs one transaction and brings the scene, index and layers up to date.
    pub fn transact<R>(&mut self, meta: TxMeta, f: impl FnOnce(&mut Tx<'_>) -> Result<R, doc::Error>) -> Result<R, Error> {
        let (r, changes) = self.store.transact(meta, f)?;
        self.after(&changes);
        Ok(r)
    }

    // ---- open transactions (scripts) ---------------------------------------------------------

    /// Opens a transaction that stays open across [`Session::in_tx`] calls, for a script that
    /// calls back into the session. It ends with [`Session::commit`] (one undo step) or
    /// [`Session::rollback`].
    pub fn begin(&mut self, meta: TxMeta) -> Result<(), Error> {
        self.store.begin(meta)?;
        self.tx_unsynced = ChangeSet::default();
        self.tx_synced = false;
        Ok(())
    }

    /// Whether a transaction opened with [`Session::begin`] is still open.
    pub fn tx_open(&self) -> bool {
        self.store.is_open()
    }

    /// Relabels the open transaction (and its coalescing key) before it is committed.
    pub fn set_tx_meta(&mut self, meta: TxMeta) {
        self.store.set_open_meta(meta);
    }

    /// Runs `f` in the open transaction. A failing `f` leaves nothing behind and the transaction
    /// stays open.
    pub fn in_tx<R>(&mut self, f: impl FnOnce(&mut Tx<'_>) -> Result<R, doc::Error>) -> Result<R, Error> {
        let (r, changes) = self.store.in_open(f)?;
        self.tx_unsynced.extend(&changes);
        Ok(r)
    }

    /// Lays out the open transaction's edits so far and brings the scene and index up to date,
    /// so reads (bounds, hit-tests, screenshots) see them.
    pub fn sync_tx(&mut self) -> Result<(), Error> {
        let derived = self.store.derive_open()?;
        self.tx_unsynced.extend(&derived);
        if !self.tx_unsynced.is_empty() {
            let changes = std::mem::take(&mut self.tx_unsynced);
            self.scene.apply(self.store.doc(), &changes);
            self.tx_synced = true;
        }
        Ok(())
    }

    /// Commits the open transaction as one undo step. On a failed final layout everything is
    /// rolled back and the error returned.
    pub fn commit(&mut self) -> Result<ChangeSet, Error> {
        match self.store.commit() {
            Ok((all, derived)) => {
                let mut scene = std::mem::take(&mut self.tx_unsynced);
                scene.extend(&derived);
                self.tx_synced = false;
                self.after_parts(&scene, &all);
                Ok(all)
            }
            Err(failed) => {
                let (e, undone) = *failed;
                self.resync_after_rollback(&undone);
                Err(e.into())
            }
        }
    }

    /// Undoes everything the open transaction did and closes it.
    pub fn rollback(&mut self) {
        let undone = self.store.rollback();
        self.resync_after_rollback(&undone);
    }

    fn resync_after_rollback(&mut self, undone: &ChangeSet) {
        self.tx_unsynced = ChangeSet::default();
        if std::mem::take(&mut self.tx_synced) {
            // The scene saw some of the edits; resync every node the rollback touched.
            self.scene.apply(self.store.doc(), undone);
        }
    }

    /// Ids come from a seeded sequence until [`Session::random_ids`] (for reproducible scripts).
    pub fn seed_ids(&mut self, seed: u64) {
        self.store.set_ids(doc::IdGen::seeded(seed));
    }

    pub fn random_ids(&mut self) {
        self.store.set_ids(doc::IdGen::Random);
    }

    /// Undoes the last step; returns its label.
    pub fn undo(&mut self) -> Result<Option<String>, Error> {
        let label = self.store.undo_label().map(str::to_string);
        match self.store.undo()? {
            Some(ch) => {
                self.after(&ch);
                Ok(label)
            }
            None => Ok(None),
        }
    }

    pub fn redo(&mut self) -> Result<Option<String>, Error> {
        let label = self.store.redo_label().map(str::to_string);
        match self.store.redo()? {
            Some(ch) => {
                self.after(&ch);
                Ok(label)
            }
            None => Ok(None),
        }
    }

    fn after(&mut self, changes: &ChangeSet) {
        self.after_parts(changes, changes);
    }

    /// Like `after`, when the scene has already seen part of the changes (an open transaction
    /// synced for reads): `scene` is what it hasn't seen, `all` is everything.
    fn after_parts(&mut self, scene: &ChangeSet, all: &ChangeSet) {
        if all.is_empty() {
            return;
        }
        let doc = self.store.doc();
        if doc.get(self.page).is_none() {
            // The page itself went away (undo of "add page"): fall back to the first page.
            if let Some(p) = doc.pages().first().and_then(|&k| doc.node(k)).map(|n| n.id) {
                self.switch_scene(p);
            }
        } else {
            self.scene.apply(doc, scene);
            self.layers.apply(all);
        }
        let doc = self.store.doc();
        let page = self.page;
        self.selection.retain(|&id| select::page_of(doc, id) == Some(page) && id != page);
        if self.scope.is_some_and(|s| select::page_of(doc, s) != Some(page)) {
            self.scope = None;
        }
        self.dirty = true;
        self.revision += 1;
    }

    // ---- pages --------------------------------------------------------------------------------

    pub fn pages(&self) -> Vec<NodeId> {
        let doc = self.doc();
        doc.pages().iter().filter_map(|&k| doc.node(k)).map(|n| n.id).collect()
    }

    /// Shows another page, keeping each page's camera.
    pub fn set_page(&mut self, page: NodeId) {
        if page == self.page || !self.doc().get(page).is_some_and(|n| matches!(n.kind, NodeKind::Page { .. })) {
            return;
        }
        self.switch_scene(page);
    }

    fn switch_scene(&mut self, page: NodeId) {
        self.cameras.insert(self.page, self.camera);
        self.page = page;
        self.scene = Scene::new(self.store.doc(), page, self.fonts.clone(), self.assets.clone());
        self.scene.set_image_waker(self.image_waker.clone());
        self.layers.set_page(page);
        self.selection.clear();
        self.scope = None;
        match self.cameras.get(&page) {
            Some(c) => self.camera = *c,
            None => self.zoom_to_fit(),
        }
    }

    /// The display lists for one frame of the current page. `camera` maps world to device
    /// pixels; `budget` caps fragment re-encoding (stale lists are drawn past it).
    pub fn frame(&mut self, camera: kurbo::Affine, width: u32, height: u32, budget: Option<std::time::Duration>) -> render::Frame {
        self.scene.frame(self.store.doc(), camera, width, height, budget)
    }

    // ---- camera -------------------------------------------------------------------------------

    /// Fits the whole page (zoom at most 100 %).
    pub fn zoom_to_fit(&mut self) {
        match self.scene.page_bounds(self.store.doc()) {
            Some(r) => self.camera = Camera::fit(r, self.viewport, 48.0, 1.0 / self.view_scale),
            None => {
                let zoom = 1.0 / self.view_scale;
                self.camera = Camera { zoom, origin: Point::new(-self.viewport.x / (2.0 * zoom), -self.viewport.y / (2.0 * zoom)) };
            }
        }
    }

    /// Fits the selection (zoom at most 400 %).
    pub fn zoom_to_selection(&mut self) {
        if let Some(r) = self.selection_bounds() {
            self.camera = Camera::fit(r, self.viewport, 64.0, 4.0 / self.view_scale);
        }
    }

    /// The zoom users see: 1.0 shows a canvas unit at one unscaled interface point.
    pub fn zoom(&self) -> f64 {
        self.camera.zoom * self.view_scale
    }

    /// Changes the interface scale; the canvas keeps its size on screen and its top-left corner.
    pub fn set_view_scale(&mut self, scale: f64) {
        let k = self.view_scale / scale;
        self.view_scale = scale;
        self.camera.zoom *= k;
        for c in self.cameras.values_mut() {
            c.zoom *= k;
        }
    }

    /// The world rectangle on screen.
    pub fn view(&self) -> Rect {
        self.camera.view(self.viewport)
    }

    /// Edge tolerance for hit-testing at the current zoom, in world units.
    pub fn tolerance(&self) -> f64 {
        select::tolerance(self.camera.zoom)
    }

    // ---- selection ----------------------------------------------------------------------------

    pub fn selection(&self) -> &[NodeId] {
        &self.selection
    }

    pub fn scope(&self) -> Option<NodeId> {
        self.scope
    }

    /// Replaces the selection; the scope follows the first node, so its siblings stay one click
    /// away.
    pub fn select(&mut self, ids: Vec<NodeId>) {
        let doc = self.store.doc();
        let page = self.page;
        let mut ids: Vec<NodeId> = ids.into_iter().filter(|&id| id != page && select::page_of(doc, id) == Some(page)).collect();
        ids.dedup();
        self.scope = ids.first().and_then(|&id| select::scope_for(doc, id));
        self.selection = ids;
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.scope = None;
    }

    pub fn toggle(&mut self, id: NodeId) {
        if let Some(i) = self.selection.iter().position(|&x| x == id) {
            self.selection.remove(i);
        } else {
            if self.selection.is_empty() {
                self.scope = select::scope_for(self.store.doc(), id);
            }
            self.selection.push(id);
        }
    }

    /// The node a click at `world` would select (for hover).
    pub fn pick(&self, world: Point, deep: bool) -> Option<NodeId> {
        select::pick(self.store.doc(), &self.scene.index, self.page, self.scope, world, self.tolerance(), deep)
    }

    /// Topmost node under the pointer whatever the scope (for hover outlines and Ctrl).
    pub fn hit(&self, world: Point) -> Option<NodeId> {
        select::hit_all(self.store.doc(), &self.scene.index, self.page, world, self.tolerance()).first().copied()
    }

    /// A click on the canvas. Returns the picked node.
    pub fn click(&mut self, world: Point, mods: ClickMods) -> Option<NodeId> {
        let picked = self.pick(world, mods.deep);
        match (picked, mods.toggle) {
            (Some(id), true) => self.toggle(id),
            (Some(id), false) => self.select(vec![id]),
            (None, true) => {}
            (None, false) => self.clear_selection(),
        }
        picked
    }

    /// A double-click: enters the selected container under the pointer. Returns the node now
    /// selected, or `None` when nothing changed (the caller may then edit text, in Stage 7).
    pub fn double_click(&mut self, world: Point) -> Option<NodeId> {
        let doc = self.store.doc();
        let container = self.selection.iter().copied().find(|&id| doc.get(id).is_some_and(select::is_container))?;
        let child = select::drill(doc, &self.scene.index, self.page, container, world, self.tolerance())?;
        self.selection = vec![child];
        self.scope = Some(select::inner(doc, container));
        Some(child)
    }

    /// Esc: selects the parent of the selection, or clears it at page level.
    pub fn escape(&mut self) {
        let doc = self.store.doc();
        let parent = self.selection.first().and_then(|&id| select::parent_of(doc, id)).map(|p| select::selectable(doc, p));
        match parent {
            Some(p) if p != self.page => self.select(vec![p]),
            _ => self.clear_selection(),
        }
    }

    /// Selectable children of `id` (an instance's are its content's), in stacking order.
    fn children_of(&self, id: NodeId) -> Vec<NodeId> {
        let doc = self.store.doc();
        let inner = select::inner(doc, id);
        let Some(k) = doc.key(inner) else { return Vec::new() };
        doc.node(k)
            .map(|n| n.children.iter().filter(|&&c| !select::is_locked(doc, c)).filter_map(|&c| doc.node(c)).filter(|c| c.props.visible).map(|c| c.id).collect())
            .unwrap_or_default()
    }

    /// Tab (Shift+Tab): selects the next (previous) sibling of the first selected node,
    /// wrapping around, or the page's first (last) child when nothing is selected.
    pub fn select_sibling(&mut self, forward: bool) {
        let doc = self.store.doc();
        let Some(&first) = self.selection.first() else {
            let kids = self.children_of(self.page);
            if let Some(&id) = if forward { kids.first() } else { kids.last() } {
                self.select(vec![id]);
            }
            return;
        };
        let parent = select::parent_of(doc, first).unwrap_or(self.page);
        let kids = self.children_of(parent);
        let Some(i) = kids.iter().position(|&c| c == first) else { return };
        let n = kids.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        self.select(vec![kids[j]]);
    }

    /// Enter: selects the children of the selected containers.
    pub fn select_children(&mut self) {
        let kids: Vec<NodeId> = self.selection.clone().into_iter().flat_map(|id| self.children_of(id)).collect();
        if !kids.is_empty() {
            self.select(kids);
        }
    }

    /// Shift+Enter: selects the parent of the selection (never the page).
    pub fn select_parent(&mut self) {
        let doc = self.store.doc();
        let parent = self.selection.first().and_then(|&id| select::parent_of(doc, id)).map(|p| select::selectable(doc, p));
        if let Some(p) = parent.filter(|&p| p != self.page) {
            self.select(vec![p]);
        }
    }

    /// Selects what a marquee from `start` to `end` (world) covers. `add` keeps the current
    /// selection (Shift).
    pub fn marquee(&mut self, start: Point, end: Point, add: bool) -> Vec<NodeId> {
        let doc = self.store.doc();
        let container = select::marquee_container(doc, &self.scene.index, self.page, self.scope, start);
        let found = select::marquee(doc, &self.scene.index, container, select::drag_rect(start, end));
        let mut ids = if add { self.selection.clone() } else { Vec::new() };
        for id in &found {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        self.selection = ids;
        self.scope = if container == self.page || select::is_top_level_frame(doc, container) { None } else { Some(container) };
        found
    }

    /// Selects every child of the current container (Ctrl+A).
    pub fn select_all(&mut self) {
        let doc = self.store.doc();
        let container = self.scope.or_else(|| self.selection.first().and_then(|&id| select::parent_of(doc, id))).unwrap_or(self.page);
        let ids: Vec<NodeId> = doc.get(container).map(|c| c.children.iter().filter_map(|&k| doc.node(k)).filter(|n| !n.props.locked && n.props.visible).map(|n| n.id).collect()).unwrap_or_default();
        self.selection = ids;
        self.scope = if container == self.page || select::is_top_level_frame(doc, container) { None } else { Some(container) };
    }

    /// World bounds of the selection.
    pub fn selection_bounds(&self) -> Option<Rect> {
        select::union_bounds(&self.scene.index, &self.selection)
    }

    /// Decodes images in the background from now on (see [`Scene::set_image_waker`]), for
    /// interactive use; headless sessions decode before drawing.
    pub fn set_image_waker(&mut self, wake: Option<render::Waker>) {
        self.scene.set_image_waker(wake.clone());
        self.image_waker = wake;
    }

    // ---- files --------------------------------------------------------------------------------

    /// Stores image bytes (in the bundle, or in memory until the document is saved) and returns
    /// the id and metadata to register with `Tx::put_asset`. Identical bytes are stored once.
    pub fn add_asset(&mut self, bytes: Vec<u8>) -> Result<(AssetId, AssetMeta), Error> {
        match &self.bundle {
            Some(b) => Ok(b.add_asset(&bytes)?),
            None => {
                let (id, meta) = format::identify_asset(&bytes)?;
                if let Ok(mut m) = self.pending.0.write() {
                    m.entry(id.clone()).or_insert_with(|| Arc::new(bytes));
                }
                Ok((id, meta))
            }
        }
    }

    /// Imports SVG source as a frame named `name` (see `export::import`): stores its images and
    /// returns the node for `Tx::insert_json` (register the assets with `Tx::put_asset` first)
    /// and what could not be imported exactly.
    pub fn import_svg(&mut self, source: &str, name: &str) -> Result<SvgImport, Error> {
        let imp = export::import::import_svg(source, name, &[]).map_err(|e| Error::Other(e.to_string()))?;
        let assets = imp.images.into_iter().map(|b| self.add_asset(b)).collect::<Result<_, _>>()?;
        Ok(SvgImport { node: imp.node, assets, warnings: imp.warnings })
    }

    /// Saves to the session's bundle. Errors if it has none (use [`Session::save_as`]).
    pub fn save(&mut self) -> Result<(), Error> {
        let b = self.bundle.as_ref().ok_or_else(|| Error::Other("document has no file yet".into()))?;
        b.save(self.store.doc())?;
        self.dirty = false;
        Ok(())
    }

    /// Saves to a new bundle folder, copying the assets over, and makes it the session's file.
    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> Result<(), Error> {
        let to = Bundle::create(path)?;
        if let Some(from) = &self.bundle
            && from.root() != to.root()
        {
            for (id, meta) in self.store.doc().assets() {
                if let Ok(bytes) = from.read_asset(id, meta) {
                    let dst = to.asset_path(id, meta);
                    std::fs::write(&dst, bytes).map_err(|e| format::FormatError::Io { path: dst.display().to_string(), source: e })?;
                }
            }
        }
        // Images added to an untitled document go into the bundle now.
        let pending: Vec<(AssetId, Arc<Vec<u8>>)> = self.pending.0.read().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        for (_, bytes) in &pending {
            to.add_asset(bytes)?;
        }
        if let Ok(mut m) = self.pending.0.write() {
            m.clear();
        }
        to.save(self.store.doc())?;
        self.assets = Arc::new(Assets { bundle: Some(to.clone()), pending: self.pending.clone() });
        self.bundle = Some(to);
        // Images already decoded stay cached; new reads come from the new bundle.
        self.scene = Scene::new(self.store.doc(), self.page, self.fonts.clone(), self.assets.clone());
        self.scene.set_image_waker(self.image_waker.clone());
        self.dirty = false;
        Ok(())
    }
}
