//! Arbitrary bytes as document.json: must return Ok or Err, never panic; anything that loads
//! must satisfy the document invariants and re-save to a file that loads to the same data.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = format::from_json(data) {
        doc.check_invariants().unwrap();
        let text = format::to_json(&doc);
        let again = format::from_json(text.as_bytes()).unwrap();
        assert_eq!(again.to_data(), doc.to_data());
    }
});
