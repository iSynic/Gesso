//! Variable bindings: a node binds a property path (`opacity`, `fills/0/color`, `style/color`,
//! `text`) to a variable, and the bound property shows the variable's value for the node's
//! modes. This module parses paths and computes the update that makes a node show a value.

use serde_json::Value;

use crate::document::Node;
use crate::model::{Color, PaintKind, Radius, TextSpan};
use crate::node::NodeKind;
use crate::props::{PropKey, PropValue};
use crate::variables::Resolved;

/// Kind-specific fields a path may start with (the JSON names in the node format).
const KIND_FIELDS: &[&str] = &["style", "spans", "autoLayout", "clip", "arc", "sides", "starInnerRatio", "align", "verticalAlign", "background", "description", "fit"];

/// Whether `path` starts at a property of the node format. Deeper steps are checked when the
/// binding is applied, since they depend on the node (a fill index may not exist yet).
pub fn known_path(path: &str) -> bool {
    let top = path.split('/').next().unwrap_or("");
    top == "text" || KIND_FIELDS.contains(&top) || PropKey::parse(top).is_some_and(|k| !matches!(k, PropKey::Kind | PropKey::Bindings | PropKey::Modes))
}

/// The update that makes `node` show `value` at `path`, or `None` when it already does or the
/// path doesn't fit this node (a missing fill, a type mismatch); such bindings stay dormant.
pub fn apply(node: &Node, path: &str, value: &Resolved) -> Option<PropValue> {
    let mut steps = path.split('/');
    let top = steps.next()?;
    let rest: Vec<&str> = steps.collect();
    // Fast paths for the common bindings; everything else goes through the JSON form.
    match (top, rest.as_slice(), value) {
        ("opacity", [], Resolved::Number(n)) => return (node.props.opacity != *n).then_some(PropValue::Opacity(*n)),
        ("radius", [], Resolved::Number(n)) => {
            let r = Radius::Uniform(*n);
            return (node.props.radius != r).then_some(PropValue::Radius(r));
        }
        ("fills", [i, "color"], Resolved::Color(c)) => {
            let mut fills = node.props.fills.clone();
            return set_solid(fills.get_mut(i.parse::<usize>().ok()?).map(|p| &mut p.kind)?, *c).then_some(PropValue::Fills(std::mem::take(&mut fills)));
        }
        ("strokes", [i, "color"], Resolved::Color(c)) => {
            let mut strokes = node.props.strokes.clone();
            return set_solid(strokes.get_mut(i.parse::<usize>().ok()?).map(|s| &mut s.paint.kind)?, *c).then_some(PropValue::Strokes(strokes));
        }
        ("text", [], Resolved::String(s)) => return set_text(node, s),
        ("name", [], Resolved::String(s)) => return (node.name != *s).then(|| PropValue::Name(s.clone())),
        _ => {}
    }
    let json = to_json(value);
    if let Some(key) = PropKey::parse(top).filter(|k| !matches!(k, PropKey::Name | PropKey::Kind | PropKey::Bindings | PropKey::Modes)) {
        let old = node.get(key);
        let mut v = serde_json::to_value(&old).ok()?.get_mut("value")?.take();
        set_pointer(&mut v, &rest, json)?;
        let new = PropValue::from_json(key, v).ok()?;
        return (new != old).then_some(new);
    }
    if KIND_FIELDS.contains(&top) {
        let mut v = serde_json::to_value(&node.kind).ok()?;
        let mut all = vec![top];
        all.extend(&rest);
        set_pointer(&mut v, &all, json)?;
        let kind: NodeKind = serde_json::from_value(v).ok()?;
        return (kind != node.kind).then_some(PropValue::Kind(kind));
    }
    None
}

fn to_json(value: &Resolved) -> Value {
    match value {
        Resolved::Color(c) => serde_json::to_value(c).unwrap_or_default(),
        Resolved::Number(n) => Value::from(*n),
        Resolved::String(s) => Value::from(s.as_str()),
        Resolved::Boolean(b) => Value::from(*b),
    }
}

/// Replaces the value at `steps` (which must exist; `[]` is the whole value). `None` if a step
/// is missing.
fn set_pointer(v: &mut Value, steps: &[&str], new: Value) -> Option<()> {
    let mut cur = v;
    for s in steps {
        cur = match cur {
            Value::Object(o) => o.get_mut(*s)?,
            Value::Array(a) => a.get_mut(s.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    *cur = new;
    Some(())
}

/// Sets a solid paint's colour; returns whether it changed (gradients and images don't bind).
fn set_solid(kind: &mut PaintKind, c: Color) -> bool {
    match kind {
        PaintKind::Solid { color } if *color != c => {
            *color = c;
            true
        }
        _ => false,
    }
}

/// A text node's content: the first span's style carries over to the new text.
fn set_text(node: &Node, s: &str) -> Option<PropValue> {
    let NodeKind::Text { spans, .. } = &node.kind else { return None };
    if spans.iter().map(|sp| sp.text.as_str()).collect::<String>() == s {
        return None;
    }
    let mut kind = node.kind.clone();
    if let NodeKind::Text { spans, .. } = &mut kind {
        let first = spans.first().cloned().unwrap_or_default();
        *spans = vec![TextSpan { text: s.to_string(), ..first }];
    }
    Some(PropValue::Kind(kind))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::model::Paint;
    use crate::props::CommonProps;
    use crate::{Id, model::TextStyle};

    fn node(kind: NodeKind, props: CommonProps) -> Node {
        Node { id: Id(1), parent: None, children: Vec::new(), name: "n".into(), kind, props, virt: None }
    }

    #[test]
    fn paths() {
        assert!(known_path("fills/0/color"));
        assert!(known_path("style/color"));
        assert!(known_path("text"));
        assert!(known_path("autoLayout/gap"));
        assert!(!known_path("colour"));
        assert!(!known_path("bindings/x"));
    }

    #[test]
    fn applies_and_skips() {
        let red = Color::rgb(255, 0, 0);
        let n = node(NodeKind::Rectangle {}, CommonProps { fills: vec![Paint::solid(Color::WHITE)], ..Default::default() });
        let Some(PropValue::Fills(f)) = apply(&n, "fills/0/color", &Resolved::Color(red)) else { panic!("fill") };
        assert_eq!(f[0].kind, PaintKind::Solid { color: red });
        assert_eq!(apply(&n, "fills/3/color", &Resolved::Color(red)), None, "missing fill stays dormant");
        assert_eq!(apply(&n, "opacity", &Resolved::Number(1.0)), None, "unchanged");
        assert_eq!(apply(&n, "minWidth", &Resolved::Number(40.0)), Some(PropValue::MinWidth(Some(40.0))));
        assert_eq!(apply(&n, "opacity", &Resolved::String("x".into())), None, "type mismatch");
        let t = node(NodeKind::Text { spans: vec![TextSpan { text: "a".into(), ..Default::default() }], style: TextStyle::default(), align: Default::default(), vertical_align: Default::default(), auto_resize: Default::default() }, CommonProps::default());
        let Some(PropValue::Kind(NodeKind::Text { style, .. })) = apply(&t, "style/color", &Resolved::Color(red)) else { panic!("style") };
        assert_eq!(style.color, red);
        let Some(PropValue::Kind(NodeKind::Text { spans, .. })) = apply(&t, "text", &Resolved::String("Hello".into())) else { panic!("text") };
        assert_eq!(spans[0].text, "Hello");
    }
}
