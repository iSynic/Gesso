//! Property tests for span-preserving text edits: random inserts, deletes and restyles keep the
//! style of every character they don't touch, typing coalesced into one undo step undoes
//! exactly, and undoing every step restores the document.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::TextSpan;
use doc::richtext as text;
use doc::*;
use proptest::prelude::*;
use serde_json::json;

#[derive(Debug, Clone)]
enum Edit {
    Insert { at: u16, text: String },
    Delete { a: u16, b: u16 },
    Bold { a: u16, b: u16, on: bool },
    Size { a: u16, b: u16, size: u8 },
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        3 => (any::<u16>(), "[a-z é日\n]{1,6}").prop_map(|(at, text)| Edit::Insert { at, text }),
        2 => (any::<u16>(), any::<u16>()).prop_map(|(a, b)| Edit::Delete { a, b }),
        2 => (any::<u16>(), any::<u16>(), any::<bool>()).prop_map(|(a, b, on)| Edit::Bold { a, b, on }),
        1 => (any::<u16>(), any::<u16>(), 8u8..40).prop_map(|(a, b, size)| Edit::Size { a, b, size }),
    ]
}

/// Each character with its style, for comparing styles across edits.
fn styled_chars(spans: &[TextSpan]) -> Vec<(char, TextSpan)> {
    spans.iter().flat_map(|s| s.text.chars().map(move |c| (c, text::style_of(s)))).collect()
}

fn at(spans: &[TextSpan], v: u16) -> usize {
    let n = text::len(spans);
    text::floor_boundary(spans, if n == 0 { 0 } else { v as usize % (n + 1) })
}

fn char_index(spans: &[TextSpan], byte: usize) -> usize {
    text::plain(spans)[..byte].chars().count()
}

fn apply(spans: &[TextSpan], e: &Edit) -> Vec<TextSpan> {
    match e {
        Edit::Insert { at: p, text: t } => text::insert(spans, at(spans, *p), t, None),
        Edit::Delete { a, b } => {
            let (a, b) = (at(spans, *a), at(spans, *b));
            text::delete(spans, a.min(b)..a.max(b))
        }
        Edit::Bold { a, b, on } => {
            let (a, b) = (at(spans, *a), at(spans, *b));
            text::restyle(spans, a.min(b)..a.max(b), |s| s.font_weight = on.then_some(700))
        }
        Edit::Size { a, b, size } => {
            let (a, b) = (at(spans, *a), at(spans, *b));
            text::restyle(spans, a.min(b)..a.max(b), |s| s.font_size = Some(f64::from(*size)))
        }
    }
}

/// Checks that characters outside an edit keep their style and order.
fn check_preserved(before: &[TextSpan], after: &[TextSpan], e: &Edit) -> Result<(), TestCaseError> {
    let (b, a) = (styled_chars(before), styled_chars(after));
    match e {
        Edit::Insert { at: p, text: t } => {
            let i = char_index(before, at(before, *p));
            let n = t.chars().count();
            prop_assert_eq!(&a[..i], &b[..i]);
            prop_assert_eq!(&a[i + n..], &b[i..]);
            let inserted: String = a[i..i + n].iter().map(|(c, _)| *c).collect();
            prop_assert_eq!(&inserted, t);
        }
        Edit::Delete { a: x, b: y } => {
            let (x, y) = (at(before, *x), at(before, *y));
            let (i, j) = (char_index(before, x.min(y)), char_index(before, x.max(y)));
            prop_assert_eq!(&a[..i], &b[..i]);
            prop_assert_eq!(&a[i..], &b[j..]);
        }
        Edit::Bold { a: x, b: y, .. } | Edit::Size { a: x, b: y, .. } => {
            let (x, y) = (at(before, *x), at(before, *y));
            let (i, j) = (char_index(before, x.min(y)), char_index(before, x.max(y)));
            prop_assert_eq!(a.len(), b.len());
            prop_assert_eq!(&a[..i], &b[..i]);
            prop_assert_eq!(&a[j..], &b[j..]);
            for k in i..j {
                prop_assert_eq!(a[k].0, b[k].0);
            }
        }
    }
    // Normalised: no empty spans (unless the only one), no equal neighbours.
    prop_assert!(!after.is_empty());
    prop_assert!(after.len() == 1 || after.iter().all(|s| !s.text.is_empty()));
    prop_assert!(after.windows(2).all(|w| !text::same_style(&w[0], &w[1])));
    Ok(())
}

fn spans_of(s: &Store, id: NodeId) -> Vec<TextSpan> {
    match &s.doc().get(id).unwrap().kind {
        NodeKind::Text { spans, .. } => spans.clone(),
        _ => panic!("not text"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn span_edits_preserve_styles_and_undo_exactly(edits in prop::collection::vec(edit(), 1..30), coalesce in any::<bool>()) {
        let mut s = Store::default().with_ids(IdGen::seeded(7));
        let (page, _) = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap();
        let (id, _) = s.transact(TxMeta::user("text"), |tx| tx.insert_json(Some(page), None, json!({
            "type": "text", "spans": [{"text": "Hello "}, {"text": "bold", "fontWeight": 700}, {"text": " world"}]
        }))).unwrap();
        let start = s.doc().to_data();
        let mut states = vec![start.clone()];
        for e in &edits {
            let before = spans_of(&s, id);
            let expected = apply(&before, e);
            let meta = if coalesce { TxMeta::user("Type").coalesce("typing") } else { TxMeta::user("Edit text") };
            s.transact(meta, |tx| tx.edit_spans(id, |sp| apply(sp, e))).unwrap();
            let after = spans_of(&s, id);
            prop_assert_eq!(&after, &expected);
            check_preserved(&before, &after, e)?;
            states.push(s.doc().to_data());
        }
        if coalesce {
            // All edits are one undo step.
            prop_assert!(s.undo().unwrap().is_some());
            prop_assert_eq!(&s.doc().to_data(), &start);
        } else {
            for expected in states.iter().rev().skip(1) {
                prop_assert!(s.undo().unwrap().is_some());
                prop_assert_eq!(&s.doc().to_data(), expected);
            }
        }
    }
}
