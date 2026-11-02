//! Editing commands: delete, duplicate, group/ungroup, z-order, components, and the keyboard map.

use doc::{NodeId, NodeKind, PropValue, TxMeta};
use kurbo::Vec2;
use serde_json::json;

use crate::components::{self, real_items};
use crate::session::{Error, Session};
use crate::tools::Tool;
use crate::transform::{is_flow_child, top_items};
use crate::xform::Placement;
use crate::select;

/// Everything a key or menu can ask for. The shell handles the ones that need the OS (files,
/// clipboard); the rest run through [`execute`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Undo,
    Redo,
    Delete,
    Duplicate,
    SelectAll,
    Group,
    Ungroup,
    BringForward,
    SendBackward,
    BringToFront,
    SendToBack,
    ZoomToFit,
    ZoomToSelection,
    ZoomTo100,
    /// Zooms about the viewport's centre to this zoom (1.0 is 100 %).
    ZoomTo(f64),
    ZoomIn,
    ZoomOut,
    Escape,
    Nudge(Vec2),
    Tool(Tool),
    ToggleSnapPixels,
    CreateComponent,
    DetachInstance,
    ResetOverrides,
    GoToMainComponent,
    /// Tab: the next sibling.
    SelectNext,
    /// Shift+Tab: the previous sibling.
    SelectPrevious,
    /// Enter: the selection's children.
    SelectChildren,
    /// Shift+Enter: the selection's parent.
    SelectParent,
    // Shell commands.
    Copy,
    Cut,
    Paste,
    New,
    Open,
    Save,
    SaveAs,
    /// Export the selection (or the page) to a file.
    Export,
    /// Place an image or SVG file.
    Place,
    /// Copy the selection as a PNG bitmap.
    CopyAsPng,
    /// Copy the selection as SVG text.
    CopyAsSvg,
}

/// Keys the editor understands, independent of the windowing library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Delete,
    Backspace,
    Escape,
    Enter,
    Tab,
    Home,
    End,
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyMods {
    /// Ctrl, or Cmd on macOS.
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// The keyboard map.
pub fn shortcut(key: Key, m: KeyMods) -> Option<Command> {
    let step = if m.shift { 10.0 } else { 1.0 };
    Some(match (key, m.ctrl, m.shift) {
        (Key::Left, false, _) => Command::Nudge(Vec2::new(-step, 0.0)),
        (Key::Right, false, _) => Command::Nudge(Vec2::new(step, 0.0)),
        (Key::Up, false, _) => Command::Nudge(Vec2::new(0.0, -step)),
        (Key::Down, false, _) => Command::Nudge(Vec2::new(0.0, step)),
        (Key::Delete | Key::Backspace, false, _) => Command::Delete,
        (Key::Escape, _, _) => Command::Escape,
        (Key::Tab, false, shift) if !m.alt => if shift { Command::SelectPrevious } else { Command::SelectNext },
        (Key::Enter, false, shift) if !m.alt => if shift { Command::SelectParent } else { Command::SelectChildren },
        (Key::Char(c), true, false) if m.alt => match c.to_ascii_lowercase() {
            'k' => Command::CreateComponent,
            'b' => Command::DetachInstance,
            _ => return None,
        },
        (Key::Char(c), true, shift) => match (c.to_ascii_lowercase(), shift) {
            ('z', false) => Command::Undo,
            ('z', true) | ('y', false) => Command::Redo,
            ('d', false) => Command::Duplicate,
            ('a', false) => Command::SelectAll,
            ('g', false) => Command::Group,
            ('g', true) => Command::Ungroup,
            (']', false) => Command::BringForward,
            ('[', false) => Command::SendBackward,
            (']', true) | ('}', _) => Command::BringToFront,
            ('[', true) | ('{', _) => Command::SendToBack,
            ('0', false) => Command::ZoomTo100,
            ('=' | '+', _) => Command::ZoomIn,
            ('-', false) => Command::ZoomOut,
            ('c', false) => Command::Copy,
            ('x', false) => Command::Cut,
            ('v', false) => Command::Paste,
            ('n', false) => Command::New,
            ('o', false) => Command::Open,
            ('s', false) => Command::Save,
            ('s', true) => Command::SaveAs,
            ('e', true) => Command::Export,
            ('k', true) => Command::Place,
            ('c', true) => Command::CopyAsPng,
            ('\'', false) => Command::ToggleSnapPixels,
            _ => return None,
        },
        (Key::Char(c), false, shift) if !m.alt => match (c.to_ascii_lowercase(), shift) {
            ('1' | '!', true) => Command::ZoomToFit,
            ('2' | '@', true) => Command::ZoomToSelection,
            ('v', false) => Command::Tool(Tool::Move),
            ('h', false) => Command::Tool(Tool::Hand),
            ('f', false) => Command::Tool(Tool::Frame),
            ('r', false) => Command::Tool(Tool::Rectangle),
            ('o', false) => Command::Tool(Tool::Ellipse),
            ('l', false) => Command::Tool(Tool::Line),
            ('t', false) => Command::Tool(Tool::Text),
            ('p', false) => Command::Tool(Tool::Pen),
            ('+' | '=', _) => Command::ZoomIn,
            ('-', false) => Command::ZoomOut,
            _ => return None,
        },
        _ => return None,
    })
}

/// Runs an editor command. Returns `false` for the shell's commands (files, clipboard) and for
/// tool changes, which the caller's controller handles.
pub fn execute(s: &mut Session, cmd: Command) -> Result<bool, Error> {
    match cmd {
        Command::Undo => {
            s.undo()?;
        }
        Command::Redo => {
            s.redo()?;
        }
        Command::Delete => delete(s)?,
        Command::Duplicate => duplicate(s)?,
        Command::SelectAll => s.select_all(),
        Command::Group => group(s)?,
        Command::Ungroup => ungroup(s)?,
        Command::BringForward => restack(s, Restack::Forward)?,
        Command::SendBackward => restack(s, Restack::Backward)?,
        Command::BringToFront => restack(s, Restack::Front)?,
        Command::SendToBack => restack(s, Restack::Back)?,
        Command::ZoomToFit => s.zoom_to_fit(),
        Command::ZoomToSelection => s.zoom_to_selection(),
        Command::ZoomTo100 => zoom_centred(s, 1.0 / s.zoom()),
        Command::ZoomTo(z) => zoom_centred(s, z / s.zoom()),
        Command::ZoomIn => zoom_centred(s, 2.0),
        Command::ZoomOut => zoom_centred(s, 0.5),
        Command::Escape => s.escape(),
        Command::Nudge(d) => crate::transform::nudge(s, d)?,
        Command::ToggleSnapPixels => s.snap_to_pixels = !s.snap_to_pixels,
        Command::CreateComponent => components::create_component(s)?,
        Command::DetachInstance => components::detach(s)?,
        Command::ResetOverrides => components::reset_overrides(s)?,
        Command::GoToMainComponent => components::go_to_main(s)?,
        Command::SelectNext => s.select_sibling(true),
        Command::SelectPrevious => s.select_sibling(false),
        Command::SelectChildren => s.select_children(),
        Command::SelectParent => s.select_parent(),
        Command::Tool(_) | Command::Copy | Command::Cut | Command::Paste | Command::New | Command::Open | Command::Save | Command::SaveAs | Command::Export | Command::Place | Command::CopyAsPng | Command::CopyAsSvg => return Ok(false),
    }
    Ok(true)
}

fn zoom_centred(s: &mut Session, factor: f64) {
    let c = (s.viewport / 2.0).to_point();
    s.camera.zoom_about(c, factor);
}

pub fn delete(s: &mut Session) -> Result<(), Error> {
    let ids = real_items(s);
    if ids.is_empty() {
        return Ok(());
    }
    s.transact(TxMeta::user("Delete"), |tx| {
        for &id in &ids {
            tx.delete(id)?;
        }
        Ok(())
    })?;
    s.clear_selection();
    Ok(())
}

/// Copies the selection just above each original, offset by 10 px (in place inside auto layout).
pub fn duplicate(s: &mut Session) -> Result<(), Error> {
    let ids = real_items(s);
    if ids.is_empty() {
        return Ok(());
    }
    let copies = s.transact(TxMeta::user("Duplicate"), |tx| {
        let mut out = Vec::new();
        for &id in &ids {
            let d = tx.doc();
            let Some(k) = d.key(id) else { continue };
            let parent = select::parent_of(d, id);
            let at = d.index_of(k).map(|i| i + 1);
            let flow = is_flow_child(d, id);
            let (x, y) = d.get(id).map(|n| (n.props.x, n.props.y)).unwrap_or_default();
            let c = tx.copy(id, parent, at, None)?;
            if !flow {
                tx.update(c, PropValue::X(x + 10.0))?;
                tx.update(c, PropValue::Y(y + 10.0))?;
            }
            out.push(c);
        }
        Ok(out)
    })?;
    s.select(copies);
    Ok(())
}

/// Wraps the selection in a group, in the parent of the first selected node, at the topmost
/// selected node's position in the stack. Positions are kept.
pub fn group(s: &mut Session) -> Result<(), Error> {
    let doc = s.doc();
    let mut ids = real_items(s);
    let Some(&first) = ids.first() else { return Ok(()) };
    let Some(parent) = select::parent_of(doc, first) else { return Ok(()) };
    ids.retain(|&id| select::parent_of(doc, id) == Some(parent));
    // Stacking order, bottom first.
    ids.sort_by_key(|&id| doc.key(id).and_then(|k| doc.index_of(k)));
    let top_index = ids.last().and_then(|&id| doc.key(id)).and_then(|k| doc.index_of(k)).unwrap_or(0);
    let (gx, gy) = ids.iter().filter_map(|&id| doc.get(id)).fold((f64::INFINITY, f64::INFINITY), |(x, y), n| (x.min(n.props.x), y.min(n.props.y)));
    let g = s.transact(TxMeta::user("Group"), |tx| {
        let g = tx.insert_json(Some(parent), Some(top_index + 1), json!({"type": "group", "name": "Group", "x": gx, "y": gy}))?;
        for &id in &ids {
            let (x, y) = tx.doc().get(id).map(|n| (n.props.x, n.props.y)).unwrap_or_default();
            tx.move_node(id, Some(g), None)?;
            tx.update(id, PropValue::X(x - gx))?;
            tx.update(id, PropValue::Y(y - gy))?;
        }
        Ok(g)
    })?;
    s.select(vec![g]);
    Ok(())
}

/// Replaces each selected group by its children, keeping their placement on the canvas.
pub fn ungroup(s: &mut Session) -> Result<(), Error> {
    let doc = s.doc();
    let groups: Vec<NodeId> = s.selection().iter().copied().filter(|&id| doc.get(id).is_some_and(|n| matches!(n.kind, NodeKind::Group {}))).collect();
    if groups.is_empty() {
        return Ok(());
    }
    let freed = s.transact(TxMeta::user("Ungroup"), |tx| {
        let mut freed = Vec::new();
        for &g in &groups {
            let d = tx.doc();
            let Some(gk) = d.key(g) else { continue };
            let Some(gn) = d.node(gk) else { continue };
            let gl = Placement::of(gn).local();
            let parent = select::parent_of(d, g);
            let at = d.index_of(gk).unwrap_or(0);
            let kids: Vec<(NodeId, Placement)> = gn.children.iter().filter_map(|&k| d.node(k)).map(|n| (n.id, Placement::of(n))).collect();
            for (i, (id, p)) in kids.iter().enumerate() {
                tx.move_node(*id, parent, Some(at + 1 + i))?;
                let q = Placement::from_affine(gl * p.local(), p.w, p.h, p);
                for u in q.diff(p) {
                    tx.update(*id, u)?;
                }
                freed.push(*id);
            }
            tx.delete(g)?;
        }
        Ok(freed)
    })?;
    s.select(freed);
    Ok(())
}

#[derive(Clone, Copy)]
enum Restack {
    Forward,
    Backward,
    Front,
    Back,
}

fn restack(s: &mut Session, how: Restack) -> Result<(), Error> {
    let doc = s.doc();
    let mut ids = real_items(s);
    ids.sort_by_key(|&id| doc.key(id).and_then(|k| doc.index_of(k)));
    // Move the topmost first when raising, so neighbours don't swap with each other.
    if matches!(how, Restack::Forward | Restack::Front) {
        ids.reverse();
    }
    let label = match how {
        Restack::Forward => "Bring forward",
        Restack::Backward => "Send backward",
        Restack::Front => "Bring to front",
        Restack::Back => "Send to back",
    };
    s.transact(TxMeta::user(label), |tx| {
        for (n, &id) in ids.iter().enumerate() {
            let d = tx.doc();
            let Some(k) = d.key(id) else { continue };
            let (Some(i), Some(parent)) = (d.index_of(k), select::parent_of(d, id)) else { continue };
            let len = d.get(parent).map_or(0, |p| p.children.len());
            let j = match how {
                Restack::Forward => (i + 1).min(len - 1 - n),
                Restack::Backward => i.saturating_sub(1).max(n),
                Restack::Front => len - 1 - n,
                Restack::Back => n,
            };
            if j != i {
                tx.move_node(id, Some(parent), Some(j))?;
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Moves `ids` (in order) into `parent` just before what is at `slot` among its other
/// children, keeping each node's place on the canvas (the layers panel's drag and drop).
/// Ignored when `parent` is one of `ids` or inside one, or can't hold children.
pub fn move_layers(s: &mut Session, ids: &[NodeId], parent: NodeId, slot: usize) -> Result<(), Error> {
    let doc = s.doc();
    let Some(pk) = doc.key(parent) else { return Ok(()) };
    if !doc.node(pk).is_some_and(|n| n.kind.is_container()) {
        return Ok(());
    }
    let ids = top_items(doc, ids);
    let cycle = ids.iter().any(|&id| id == parent || doc.key(id).is_some_and(|k| doc.is_descendant_of(pk, k)));
    if ids.is_empty() || cycle {
        return Ok(());
    }
    let index = &s.scene.index;
    let target = index.world(parent).unwrap_or(kurbo::Affine::IDENTITY);
    let placed: Vec<(NodeId, Placement)> = ids
        .iter()
        .filter_map(|&id| {
            let n = doc.get(id)?;
            let p = Placement::of(n);
            let w = index.world(id)?;
            Some((id, Placement::from_affine(target.inverse() * w, p.w, p.h, &p)))
        })
        .collect();
    s.transact(TxMeta::user("Move layers"), |tx| {
        crate::transform::apply_reorder(tx, parent, &ids, slot)?;
        for (id, p) in &placed {
            let now = Placement::of(tx.doc().get(*id).ok_or(doc::Error::Input("node went away".into()))?);
            for u in p.diff(&now) {
                tx.update(*id, u)?;
            }
        }
        Ok(())
    })?;
    s.select(ids);
    Ok(())
}
