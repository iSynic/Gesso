//! Property test: any sequence of API calls (some failing, some caught) is one undo step that
//! restores the document exactly, and a script that fails changes nothing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use editor::Session;
use proptest::prelude::*;
use script::{Engine, Execute, Options};

#[derive(Debug, Clone)]
enum Call {
    Insert { parent: usize, ty: usize, w: u16, auto: bool },
    Update { target: usize, prop: usize, v: u16 },
    Move { target: usize, parent: usize, index: u8 },
    Copy { target: usize, parent: usize },
    Remove { target: usize },
    Read { target: usize },
}

fn call() -> impl Strategy<Value = Call> {
    prop_oneof![
        4 => (0..8usize, 0..4usize, 1..400u16, any::<bool>()).prop_map(|(parent, ty, w, auto)| Call::Insert { parent, ty, w, auto }),
        3 => (0..8usize, 0..5usize, 0..300u16).prop_map(|(target, prop, v)| Call::Update { target, prop, v }),
        2 => (0..8usize, 0..8usize, 0..4u8).prop_map(|(target, parent, index)| Call::Move { target, parent, index }),
        1 => (0..8usize, 0..8usize).prop_map(|(target, parent)| Call::Copy { target, parent }),
        1 => (0..8usize).prop_map(|target| Call::Remove { target }),
        1 => (0..8usize).prop_map(|target| Call::Read { target }),
    ]
}

/// Each call is wrapped in try/catch so invalid ones (moving into a rectangle, say) are skipped;
/// `ids` holds every id created so far (index 0 = the page).
fn program(calls: &[Call], fail: bool) -> String {
    let mut js = String::from("const ids = [page];\nconst pick = (i) => ids[i % ids.length];\n");
    for c in calls {
        let line = match c {
            Call::Insert { parent, ty, w, auto } => {
                let ty = ["frame", "rectangle", "text", "ellipse"][*ty];
                let extra = match (ty, auto) {
                    ("frame", true) => ", autoLayout: { direction: 'vertical', gap: 4, padding: 8 }, sizingV: 'hug'".to_string(),
                    ("text", _) => ", text: 'Label', sizingH: 'fill'".to_string(),
                    _ => String::new(),
                };
                format!("ids.push(insert(pick({parent}), {{ type: '{ty}', width: {w}, height: {}{extra} }}));", w / 2 + 1)
            }
            Call::Update { target, prop, v } => {
                let p = match prop {
                    0 => format!("{{ x: {v} }}"),
                    1 => format!("{{ width: {} }}", v + 1),
                    2 => format!("{{ opacity: {} }}", f64::from(*v % 100) / 100.0),
                    3 => format!("{{ name: 'n{v}', rotation: {} }}", v % 90),
                    _ => "{ fills: ['#336699'] }".to_string(),
                };
                format!("update(pick({target}), {p});")
            }
            Call::Move { target, parent, index } => format!("move(pick({target}), pick({parent}), {index});"),
            Call::Copy { target, parent } => format!("ids.push(copy(pick({target}), pick({parent})));"),
            Call::Remove { target } => format!("remove(pick({target}));"),
            Call::Read { target } => format!("get(pick({target}), {{ depth: 1 }});"),
        };
        js.push_str(&format!("try {{ {line} }} catch (e) {{}}\n"));
    }
    if fail {
        js.push_str("throw new Error('abort');\n");
    }
    js
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), ..ProptestConfig::default() })]

    #[test]
    fn scripts_are_atomic_and_undo_exactly(calls in prop::collection::vec(call(), 1..30), fail in any::<bool>()) {
        let mut s = Session::untitled(text::FontDb::bundled_only()).unwrap();
        let mut e = Engine::new();
        let before = format::to_json(s.doc());
        let undo_before = s.store().undo_label().map(str::to_string);
        let r = e.execute(&mut s, &Execute { code: Some(program(&calls, fail)), ..Default::default() }, &Options { timeout: std::time::Duration::from_secs(10), ..Options::default() });
        prop_assert!(s.doc().check_invariants().is_ok());
        match r {
            Err(err) => {
                prop_assert!(fail, "unexpected failure: {err}");
                prop_assert_eq!(format::to_json(s.doc()), before);
                prop_assert_eq!(s.store().undo_label().map(str::to_string), undo_before);
            }
            Ok(o) => {
                prop_assert!(!fail);
                if o.undo_label.is_some() {
                    s.undo().unwrap();
                }
                prop_assert_eq!(format::to_json(s.doc()), before);
            }
        }
        // The scene and index agree with a fresh build afterwards.
        let fresh = render::Scene::new(s.doc(), s.page(), text::FontDb::bundled_only(), std::sync::Arc::new(render::NoAssets));
        prop_assert_eq!(s.scene.index.page_bounds(s.page()), fresh.index.page_bounds(s.page()));
    }
}
