//! Arbitrary JSON op lists applied to the all-kinds fixture in one transaction: either the whole
//! transaction applies (invariants hold, undo restores the original) or nothing changes.
#![no_main]
use doc::{Op, Store, TxMeta};
use libfuzzer_sys::fuzz_target;

const FIXTURE: &[u8] = include_bytes!("../../crates/format/tests/fixtures/all-kinds.canvas/document.json");

fuzz_target!(|data: &[u8]| {
    let Ok(ops) = serde_json::from_slice::<Vec<Op>>(data) else { return };
    let mut store = Store::new(format::from_json(FIXTURE).unwrap());
    let before = store.doc().to_data();
    let result = store.transact(TxMeta::user("fuzz"), |tx| {
        for op in ops {
            tx.apply(op)?;
        }
        Ok(())
    });
    store.doc().check_invariants().unwrap();
    match result {
        Err(_) => assert_eq!(store.doc().to_data(), before),
        Ok(_) => {
            if store.undo().unwrap().is_some() {
                assert_eq!(store.doc().to_data(), before);
            }
        }
    }
});
