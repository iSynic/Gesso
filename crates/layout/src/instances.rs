//! Instance expansion: an instance shows its component's content with the overrides applied,
//! bindings resolved in its mode context, and constraints applied at the instance's size. The
//! content is built here as [`VirtualSpec`]s and stored by the document as virtual nodes under
//! the instance; auto layout and text sizing inside it are then done by the ordinary layout
//! passes, which see virtual nodes like any others.

use std::collections::BTreeMap;

use doc::model::Sizing;
use doc::{ChangeSet, Document, Id, Node, NodeId, NodeKey, NodeKind, PropKey, PropValue, VirtualSpec};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::constraints::constrain;
use crate::semantics::auto_layout;

/// Nested instances deeper than this show nothing (component cycles are rejected on edit, so
/// this only bounds pathological nesting).
const MAX_DEPTH: u32 = 16;

/// A virtual id: a hash of the scope (the instance, or a nested instance's virtual id) and the
/// source node. Deterministic, so the same content keeps its ids across rebuilds and sessions.
pub fn virtual_id(scope: NodeId, source: NodeId) -> NodeId {
    fn h(a: u128, b: u128, seed: u64) -> u64 {
        let mut x = seed;
        for w in [a as u64, (a >> 64) as u64, b as u64, (b >> 64) as u64] {
            x = (x ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            x ^= x >> 29;
        }
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
    Id(((h(scope.0, source.0, 0x5157_A11E) as u128) << 64) | h(scope.0, source.0, 0x00DD_BA11) as u128)
}

fn real(doc: &Document, id: NodeId) -> Option<NodeKey> {
    doc.key(id).filter(|k| !doc.is_virtual(*k))
}

fn is_instance(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Instance { .. })
}

/// The real instances whose content `changes` may affect (`None`: all of them). `parents` maps
/// deleted and moved nodes to their former parents.
pub fn dirty(doc: &Document, changes: Option<&ChangeSet>, parents: &FxHashMap<NodeId, NodeId>) -> Vec<NodeKey> {
    let all = || doc.iter().filter(|(_, n)| n.virt.is_none() && is_instance(n)).map(|(k, _)| k).collect();
    let Some(ch) = changes else { return all() };
    if ch.variables {
        return all();
    }
    let mut out: FxHashSet<NodeKey> = FxHashSet::default();
    // Nodes whose enclosing components' instances are dirty.
    let mut stack: Vec<NodeKey> = Vec::new();
    let subtree = |k: NodeKey, out: &mut FxHashSet<NodeKey>, stack: &mut Vec<NodeKey>| {
        for d in doc.descendants(k) {
            if doc.node(d).is_some_and(|n| n.virt.is_none() && is_instance(n)) && out.insert(d) {
                stack.push(d);
            }
        }
    };
    for id in ch.inserted.iter().chain(&ch.moved) {
        if let Some(k) = real(doc, *id) {
            // Moved instances may have changed mode context.
            subtree(k, &mut out, &mut stack);
            stack.push(k);
        }
    }
    for id in ch.deleted.iter().chain(&ch.moved) {
        if let Some(p) = parents.get(id).and_then(|p| real(doc, *p)) {
            stack.push(p);
        }
    }
    for (id, prop) in &ch.updated {
        let Some(k) = real(doc, *id) else { continue };
        if *prop == PropKey::Modes {
            subtree(k, &mut out, &mut stack);
        } else if doc.node(k).is_some_and(is_instance) && out.insert(k) {
            stack.push(k);
        }
        stack.push(k);
    }
    // Content of a component changed: its instances change, and so do the components that
    // contain those instances.
    let mut seen: FxHashSet<NodeKey> = FxHashSet::default();
    while let Some(k) = stack.pop() {
        if !seen.insert(k) {
            continue;
        }
        for c in std::iter::once(k).chain(doc.ancestors(k)) {
            let Some(n) = doc.node(c) else { continue };
            if matches!(n.kind, NodeKind::Component { .. }) {
                for i in doc.instances_of(n.id) {
                    if out.insert(i) {
                        stack.push(i);
                    }
                }
            }
        }
    }
    let mut v: Vec<NodeKey> = out.into_iter().filter(|k| doc.node(*k).is_some()).collect();
    v.sort_unstable();
    v
}

/// Mode choices in force at `key` from its ancestors (not `key` itself), nearest winning.
pub fn inherited_modes(doc: &Document, key: NodeKey) -> BTreeMap<Id, Id> {
    let chain: Vec<NodeKey> = doc.ancestors(key).collect();
    let mut modes = BTreeMap::new();
    for k in chain.into_iter().rev() {
        if let Some(n) = doc.node(k) {
            modes.extend(n.props.modes.iter().map(|(a, b)| (*a, *b)));
        }
    }
    modes
}

/// Writes each bound property's value for `modes` into `node`. Bindings that don't resolve or
/// don't fit stay dormant.
pub fn apply_bindings(doc: &Document, node: &mut Node, modes: &BTreeMap<Id, Id>) {
    if node.props.bindings.is_empty() {
        return;
    }
    let bindings: Vec<(String, Id)> = node.props.bindings.iter().map(|(p, v)| (p.clone(), *v)).collect();
    for (path, var) in bindings {
        let Ok(value) = doc.variables().resolve(var, modes) else { continue };
        if let Some(v) = doc::bind::apply(node, &path, &value) {
            node.set(v);
        }
    }
}

/// Overrides in force for one component expansion, lowest priority first.
type Overrides<'a> = Vec<(&'a [Id], &'a PropValue)>;

/// Props that place a component root inside its instance; the instance decides them.
const ROOT_FIXED: &[PropKey] = &[
    PropKey::X,
    PropKey::Y,
    PropKey::Rotation,
    PropKey::FlipX,
    PropKey::FlipY,
    PropKey::Width,
    PropKey::Height,
    PropKey::SizingH,
    PropKey::SizingV,
    PropKey::Constraints,
    PropKey::LayoutChild,
];

/// Sets an override on `n`. A component's binding of the same property gives way, so the
/// override is what shows (a `kind` override covers the bindings into type properties, such as
/// `style/color`).
fn set_override(n: &mut Node, v: &PropValue) {
    n.set(v.clone());
    let key = v.key();
    if key == PropKey::Bindings || n.props.bindings.is_empty() {
        return;
    }
    let name = key.as_str();
    n.props.bindings.retain(|path, _| {
        let root = path.split('/').next().unwrap_or_default();
        if key == PropKey::Kind { PropKey::parse(root).is_some() } else { root != name }
    });
}

struct Builder<'a> {
    doc: &'a Document,
    out: Vec<VirtualSpec>,
}

/// The content of the real instance `key`, depth-first.
pub fn specs(doc: &Document, key: NodeKey) -> Vec<VirtualSpec> {
    let Some(inst) = doc.node(key).filter(|n| n.virt.is_none()) else { return Vec::new() };
    let NodeKind::Instance { component, overrides } = &inst.kind else { return Vec::new() };
    let mut b = Builder { doc, out: Vec::new() };
    let ovr: Overrides<'_> = overrides.iter().map(|o| (o.target.as_slice(), &o.value)).collect();
    let mut modes = inherited_modes(doc, key);
    b.content(inst.id, *component, &ovr, &mut modes, &inst.props.modes, (inst.props.width, inst.props.height), 0);
    b.out
}

impl Builder<'_> {
    /// Expands `component` under `parent` (the instance or a nested instance's virtual node),
    /// whose ids are scoped by `parent`. `own` are the instance's own mode choices, which win
    /// over the component root's.
    #[allow(clippy::too_many_arguments)]
    fn content(&mut self, parent: NodeId, component: NodeId, ovr: &Overrides<'_>, modes: &mut BTreeMap<Id, Id>, own: &BTreeMap<Id, Id>, size: (f64, f64), depth: u32) {
        let doc = self.doc;
        let Some(comp) = real(doc, component).and_then(|k| doc.node(k)) else { return };
        let NodeKind::Component { auto_layout, .. } = &comp.kind else { return };
        if depth >= MAX_DEPTH {
            return;
        }
        let mut root = Node {
            id: virtual_id(parent, comp.id),
            parent: None,
            children: Vec::new(),
            name: comp.name.clone(),
            // Instances clip their content to the root's shape.
            kind: NodeKind::Frame { auto_layout: auto_layout.clone(), clip: true },
            props: comp.props.clone(),
            virt: None,
        };
        for (path, v) in ovr {
            if path.is_empty() && !ROOT_FIXED.contains(&v.key()) {
                set_override(&mut root, v);
            }
        }
        let p = &mut root.props;
        (p.x, p.y, p.rotation, p.flip_x, p.flip_y) = (0.0, 0.0, 0.0, false, false);
        (p.width, p.height) = size;
        (p.sizing_h, p.sizing_v) = (Sizing::Fixed, Sizing::Fixed);
        p.constraints = Default::default();
        p.layout_child = Default::default();
        let saved = (!root.props.modes.is_empty() || !own.is_empty()).then(|| modes.clone());
        modes.extend(root.props.modes.iter().map(|(a, b)| (*a, *b)));
        modes.extend(own.iter().map(|(a, b)| (*a, *b)));
        apply_bindings(doc, &mut root, modes);
        let rid = root.id;
        let resized = auto_layout.is_none().then_some(((comp.props.width, comp.props.height), size));
        self.push(root, parent, comp.id);
        for c in comp.children.iter().filter_map(|k| doc.node(*k)).filter(|n| n.virt.is_none()) {
            self.node(c, parent, rid, ovr, modes, resized, depth);
        }
        if let Some(saved) = saved {
            *modes = saved;
        }
    }

    /// One component node and its subtree. `resized` is the parent's (source, shown) size when
    /// the parent applies constraints.
    #[allow(clippy::too_many_arguments)]
    fn node(&mut self, src: &Node, scope: NodeId, parent: NodeId, ovr: &Overrides<'_>, modes: &mut BTreeMap<Id, Id>, resized: Option<((f64, f64), (f64, f64))>, depth: u32) {
        let doc = self.doc;
        let mut n = Node { id: virtual_id(scope, src.id), parent: None, children: Vec::new(), name: src.name.clone(), kind: src.kind.clone(), props: src.props.clone(), virt: None };
        if let NodeKind::Component { auto_layout, .. } = &n.kind {
            n.kind = NodeKind::Frame { auto_layout: auto_layout.clone(), clip: false };
        }
        if let Some((old, new)) = resized.filter(|(o, n)| o != n) {
            let p = &mut n.props;
            [p.x, p.y, p.width, p.height] = constrain(p.constraints, [p.x, p.y, p.width, p.height], old, new);
        }
        // Overrides addressed to this node, and those passing through it into a nested
        // instance (their path minus this step). A nested instance's look is its content
        // root's, so overrides addressed to it reach that root too.
        let mut inner: Overrides<'_> = Vec::new();
        let instance = matches!(src.kind, NodeKind::Instance { .. });
        for (path, v) in ovr {
            match path.split_first() {
                Some((first, rest)) if *first == src.id => {
                    if rest.is_empty() {
                        set_override(&mut n, v);
                    }
                    if !rest.is_empty() || instance {
                        inner.push((rest, *v));
                    }
                }
                _ => {}
            }
        }
        let saved = (!n.props.modes.is_empty()).then(|| modes.clone());
        modes.extend(n.props.modes.iter().map(|(a, b)| (*a, *b)));
        apply_bindings(doc, &mut n, modes);
        let id = n.id;
        let size = (n.props.width, n.props.height);
        let constrains = matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. }) && auto_layout(&n.kind).is_none();
        let nested = match &n.kind {
            NodeKind::Instance { component, overrides } => Some((*component, overrides.clone(), n.props.modes.clone())),
            _ => None,
        };
        self.push(n, parent, src.id);
        if let Some((component, own, own_modes)) = nested {
            // The nested instance's own overrides first, then the outer ones, which win.
            let mut all: Vec<(Vec<Id>, PropValue)> = own.into_iter().map(|o| (o.target, o.value)).collect();
            all.extend(inner.iter().map(|(p, v)| (p.to_vec(), (*v).clone())));
            let all: Overrides<'_> = all.iter().map(|(p, v)| (p.as_slice(), v)).collect();
            self.content(id, component, &all, modes, &own_modes, size, depth + 1);
        } else {
            // A path step names any node inside the component, so children see the same list.
            let resized = constrains.then_some(((src.props.width, src.props.height), size));
            for c in src.children.iter().filter_map(|k| doc.node(*k)).filter(|n| n.virt.is_none()) {
                self.node(c, scope, id, ovr, modes, resized, depth);
            }
        }
        if let Some(saved) = saved {
            *modes = saved;
        }
    }

    fn push(&mut self, n: Node, parent: NodeId, source: NodeId) {
        self.out.push(VirtualSpec { id: n.id, parent, name: n.name, kind: n.kind, props: n.props, source });
    }
}
