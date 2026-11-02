//! Arbitrary JavaScript source under tight limits: never a panic or a hang, and the document
//! stays valid whether the script succeeds or not.
#![no_main]
use libfuzzer_sys::fuzz_target;
use script::{Engine, Execute, Options};

fuzz_target!(|data: &[u8]| {
    let Ok(code) = std::str::from_utf8(data) else { return };
    let Ok(mut session) = editor::Session::untitled(text::FontDb::bundled_only()) else { return };
    let opts = Options { timeout: std::time::Duration::from_millis(20), memory_limit: 8 << 20, ..Options::default() };
    let _ = Engine::new().execute(&mut session, &Execute { code: Some(code.into()), ..Default::default() }, &opts);
    session.doc().check_invariants().unwrap();
});
