//! Arbitrary call sequences through the script API: the input is JSON `[[name, [args…]], …]`,
//! replayed by a script that calls each function (catching errors) and then throws or not. The
//! host must never panic, the document must stay valid, a failed script must change nothing,
//! and undoing a successful one must restore the document.
#![no_main]
use libfuzzer_sys::fuzz_target;
use script::{Engine, Execute, Options};

const FIXTURE: &[u8] = include_bytes!("../../crates/format/tests/fixtures/all-kinds.canvas/document.json");

fuzz_target!(|data: &[u8]| {
    let Ok(calls) = serde_json::from_slice::<Vec<(String, Vec<serde_json::Value>)>>(data) else { return };
    let Ok(doc) = format::from_json(FIXTURE) else { return };
    let Ok(mut session) = editor::Session::new(doc, None, text::FontDb::bundled_only()) else { return };
    let before = format::to_json(session.doc());
    let fail = calls.len() % 2 == 1;
    let code = format!(
        "for (const [n, a] of {}) {{ try {{ const f = globalThis[n] ?? doc[n]; if (typeof f === 'function') f(...a); }} catch (e) {{}} }}\n{}",
        serde_json::to_string(&calls).unwrap_or_default(),
        if fail { "throw new Error('rollback')" } else { "null" }
    );
    let opts = Options { timeout: std::time::Duration::from_millis(200), memory_limit: 16 << 20, ..Options::default() };
    let r = Engine::new().execute(&mut session, &Execute { code: Some(code), ..Default::default() }, &opts);
    session.doc().check_invariants().unwrap();
    match r {
        Err(_) => assert_eq!(format::to_json(session.doc()), before),
        Ok(_) => {
            if session.store().can_undo() {
                session.undo().unwrap();
                assert_eq!(format::to_json(session.doc()), before);
            }
        }
    }
});
