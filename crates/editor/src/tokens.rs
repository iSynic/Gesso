//! Variables (design tokens) for the panels: which fields can bind and to what type, binding
//! and unbinding, choosing modes, and editing collections, modes and variables. Every change is
//! one undoable transaction.

use std::collections::BTreeMap;

use doc::variables::{Collection, Mode, Variable, VariableType, VariableValue, Variables};
use doc::{Document, Id, NodeId, NodeKind, PropValue, TxMeta};

use crate::session::{Error, Session};

/// The binding path and variable type for a properties-panel field (`inspect` paths use `.`,
/// bindings use `/`), or `None` if the field can't bind.
pub fn bindable(field_path: &str) -> Option<(String, VariableType)> {
    let ty = match field_path {
        "opacity" | "autoLayout.gap" | "autoLayout.crossGap" | "style.fontSize" | "style.letterSpacing" | "style.lineHeight" => VariableType::Number,
        p if p.starts_with("radius.") || p.starts_with("autoLayout.padding.") => VariableType::Number,
        "style.color" => VariableType::Color,
        "text" | "style.fontFamily" => VariableType::String,
        "visible" => VariableType::Boolean,
        _ => return None,
    };
    let path = if field_path.starts_with("radius.") { "radius".to_string() } else { field_path.replace('.', "/") };
    Some((path, ty))
}

/// `(id, "Collection/name")` of every variable of type `ty`.
pub fn variables_of(v: &Variables, ty: VariableType) -> Vec<(Id, String)> {
    v.collections.iter().flat_map(|c| c.variables.iter().filter(move |x| x.ty == ty).map(move |x| (x.id, format!("{}/{}", c.name, x.name)))).collect()
}

/// A variable's display name, `Collection/name`.
pub fn variable_name(v: &Variables, id: Id) -> String {
    v.find(id).map_or_else(|| id.to_string(), |(c, x)| format!("{}/{}", c.name, x.name))
}

/// The variable `path` is bound to on the first of `ids` that has a binding there.
pub fn bound(doc: &Document, ids: &[NodeId], path: &str) -> Option<Id> {
    ids.iter().filter_map(|id| doc.get(*id)).find_map(|n| n.props.bindings.get(path).copied())
}

/// Binds `path` to `var` on every node in `ids` (`None` unbinds). The bound value is written by
/// the engine in the same step.
pub fn bind(s: &mut Session, ids: &[NodeId], path: &str, var: Option<Id>) -> Result<(), Error> {
    let doc = s.doc();
    let updates: Vec<(NodeId, BTreeMap<String, Id>)> = ids
        .iter()
        .filter_map(|id| doc.get(*id))
        .map(|n| {
            let mut b = n.props.bindings.clone();
            match var {
                Some(v) => b.insert(path.to_string(), v),
                None => b.remove(path),
            };
            (n.id, b)
        })
        .collect();
    let label = if var.is_some() { "Bind variable" } else { "Unbind variable" };
    s.transact(TxMeta::user(label), |tx| {
        for (id, b) in updates {
            tx.update(id, PropValue::Bindings(b))?;
        }
        Ok(())
    })
}

/// Whether a node can choose modes.
pub fn takes_modes(doc: &Document, id: NodeId) -> bool {
    doc.get(id).is_some_and(|n| n.kind.takes_modes())
}

/// Sets (or with `None` clears) the mode `ids` use for `collection`.
pub fn set_mode(s: &mut Session, ids: &[NodeId], collection: Id, mode: Option<Id>) -> Result<(), Error> {
    let doc = s.doc();
    let updates: Vec<(NodeId, BTreeMap<Id, Id>)> = ids
        .iter()
        .filter_map(|id| doc.get(*id))
        .filter(|n| n.kind.takes_modes())
        .map(|n| {
            let mut m = n.props.modes.clone();
            match mode {
                Some(x) => m.insert(collection, x),
                None => m.remove(&collection),
            };
            (n.id, m)
        })
        .collect();
    s.transact(TxMeta::user("Set mode"), |tx| {
        for (id, m) in updates {
            tx.update(id, PropValue::Modes(m))?;
        }
        Ok(())
    })
}

/// Applies `edit` to a copy of the variables and stores the result, after removing bindings
/// and mode choices that would point at deleted variables, modes or collections.
fn edit_variables(s: &mut Session, label: &str, edit: impl FnOnce(&mut Variables, &mut dyn FnMut() -> Id) -> Result<(), String>) -> Result<(), Error> {
    s.transact(TxMeta::user(label), |tx| {
        let mut v = tx.doc().variables().clone();
        edit(&mut v, &mut || tx.new_id()).map_err(doc::Error::Input)?;
        // Detach nodes from what no longer exists.
        let vars: std::collections::BTreeSet<Id> = v.collections.iter().flat_map(|c| c.variables.iter().map(|x| x.id)).collect();
        let modes: std::collections::BTreeSet<(Id, Id)> = v.collections.iter().flat_map(|c| c.modes.iter().map(move |m| (c.id, m.id))).collect();
        let doc = tx.doc();
        let mut fixes: Vec<(NodeId, PropValue)> = Vec::new();
        for (_, n) in doc.iter().filter(|(_, n)| n.virt.is_none()) {
            if n.props.bindings.values().any(|x| !vars.contains(x)) {
                fixes.push((n.id, PropValue::Bindings(n.props.bindings.iter().filter(|(_, x)| vars.contains(x)).map(|(p, x)| (p.clone(), *x)).collect())));
            }
            if n.props.modes.iter().any(|(c, m)| !modes.contains(&(*c, *m))) {
                fixes.push((n.id, PropValue::Modes(n.props.modes.iter().filter(|(c, m)| modes.contains(&(**c, **m))).map(|(c, m)| (*c, *m)).collect())));
            }
        }
        for (id, value) in fixes {
            tx.update_raw(id, value)?;
        }
        tx.set_variables(v)
    })
}

fn collection_mut(v: &mut Variables, id: Id) -> Result<&mut Collection, String> {
    v.collections.iter_mut().find(|c| c.id == id).ok_or_else(|| format!("no collection {id}"))
}

/// Adds a collection with one mode, "Default"; returns nothing (select it by name).
pub fn add_collection(s: &mut Session, name: &str) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Add collection", |v, new_id| {
        let (id, mode) = (new_id(), new_id());
        v.collections.push(Collection { id, name, modes: vec![Mode { id: mode, name: "Default".into() }], default_mode: mode, variables: Vec::new() });
        Ok(())
    })
}

pub fn rename_collection(s: &mut Session, id: Id, name: &str) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Rename collection", |v, _| {
        collection_mut(v, id)?.name = name;
        Ok(())
    })
}

pub fn delete_collection(s: &mut Session, id: Id) -> Result<(), Error> {
    edit_variables(s, "Delete collection", |v, _| {
        v.collections.retain(|c| c.id != id);
        Ok(())
    })
}

/// Adds a mode to a collection, starting with the default mode's values.
pub fn add_mode(s: &mut Session, collection: Id, name: &str) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Add mode", |v, new_id| {
        let c = collection_mut(v, collection)?;
        let id = new_id();
        let default = c.default_mode;
        c.modes.push(Mode { id, name });
        for var in &mut c.variables {
            if let Some(x) = var.values.get(&default).cloned() {
                var.values.insert(id, x);
            }
        }
        Ok(())
    })
}

pub fn rename_mode(s: &mut Session, collection: Id, mode: Id, name: &str) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Rename mode", |v, _| {
        let c = collection_mut(v, collection)?;
        c.modes.iter_mut().find(|m| m.id == mode).ok_or("no such mode")?.name = name;
        Ok(())
    })
}

/// Deletes a mode (not the collection's last one); the default moves to the first remaining.
pub fn delete_mode(s: &mut Session, collection: Id, mode: Id) -> Result<(), Error> {
    edit_variables(s, "Delete mode", |v, _| {
        let c = collection_mut(v, collection)?;
        if c.modes.len() <= 1 {
            return Err("a collection needs at least one mode".into());
        }
        c.modes.retain(|m| m.id != mode);
        for var in &mut c.variables {
            var.values.remove(&mode);
        }
        if c.default_mode == mode {
            c.default_mode = c.modes[0].id;
        }
        Ok(())
    })
}

/// A starting value for a new variable of type `ty`.
pub fn default_value(ty: VariableType) -> VariableValue {
    match ty {
        VariableType::Color => VariableValue::String("#000000".into()),
        VariableType::Number => VariableValue::Number(0.0),
        VariableType::String => VariableValue::String(String::new()),
        VariableType::Boolean => VariableValue::Boolean(false),
    }
}

pub fn add_variable(s: &mut Session, collection: Id, name: &str, ty: VariableType) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Add variable", |v, new_id| {
        let c = collection_mut(v, collection)?;
        let id = new_id();
        let values = c.modes.iter().map(|m| (m.id, default_value(ty))).collect();
        c.variables.push(Variable { id, name, ty, values });
        Ok(())
    })
}

pub fn rename_variable(s: &mut Session, var: Id, name: &str) -> Result<(), Error> {
    let name = name.to_string();
    edit_variables(s, "Rename variable", |v, _| {
        let x = v.collections.iter_mut().flat_map(|c| c.variables.iter_mut()).find(|x| x.id == var).ok_or("no such variable")?;
        x.name = name;
        Ok(())
    })
}

/// Deletes a variable; nodes bound to it keep their current values, unbound.
pub fn delete_variable(s: &mut Session, var: Id) -> Result<(), Error> {
    edit_variables(s, "Delete variable", |v, _| {
        for c in &mut v.collections {
            c.variables.retain(|x| x.id != var);
        }
        Ok(())
    })
}

/// Sets one variable's value for one mode. Drags pass a coalesce key.
pub fn set_value(s: &mut Session, var: Id, mode: Id, value: VariableValue, coalesce: Option<&str>) -> Result<(), Error> {
    let mut v = s.doc().variables().clone();
    let x = v.collections.iter_mut().flat_map(|c| c.variables.iter_mut()).find(|x| x.id == var).ok_or_else(|| Error::Other("no such variable".into()))?;
    x.values.insert(mode, value);
    let mut meta = TxMeta::user("Set variable");
    if let Some(k) = coalesce {
        meta = meta.coalesce(k);
    }
    s.transact(meta, |tx| tx.set_variables(v))
}

/// The real instance, or the instance a virtual node is shown in.
pub fn owning_instance(doc: &Document, id: NodeId) -> Option<NodeId> {
    let n = doc.get(id)?;
    match (n.virt, &n.kind) {
        (Some(v), _) => Some(v.instance),
        (None, NodeKind::Instance { .. }) => Some(id),
        _ => None,
    }
}
