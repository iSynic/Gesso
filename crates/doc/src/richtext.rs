//! Span-preserving edits of rich text: insert, delete and restyle byte ranges of a text node's
//! spans. Offsets are byte offsets into the spans' joined text and must fall on character
//! boundaries (they are clamped down to one otherwise). Results are normalised: neighbours with
//! the same style merge, empty spans go, and at least one span is kept so an emptied text keeps
//! its style for what is typed next.

use std::ops::Range;

use crate::document::DocError;
use crate::id::NodeId;
use crate::model::TextSpan;
use crate::node::NodeKind;
use crate::props::PropValue;
use crate::store::{Error, Tx};

/// The spans' text, joined.
pub fn plain(spans: &[TextSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Length in bytes of the spans' joined text.
pub fn len(spans: &[TextSpan]) -> usize {
    spans.iter().map(|s| s.text.len()).sum()
}

/// The largest character boundary of the joined text at or below `at`.
pub fn floor_boundary(spans: &[TextSpan], at: usize) -> usize {
    let mut start = 0;
    for s in spans {
        let end = start + s.text.len();
        if at < end {
            let mut i = at - start;
            while !s.text.is_char_boundary(i) {
                i -= 1;
            }
            return start + i;
        }
        start = end;
    }
    start
}

/// The style of the span, with no text.
pub fn style_of(s: &TextSpan) -> TextSpan {
    TextSpan { text: String::new(), ..s.clone() }
}

/// Two spans have the same style (everything but the text matches).
pub fn same_style(a: &TextSpan, b: &TextSpan) -> bool {
    a.font_family == b.font_family
        && a.font_size == b.font_size
        && a.font_weight == b.font_weight
        && a.italic == b.italic
        && a.decoration == b.decoration
        && a.color == b.color
        && a.link == b.link
}

/// The style text typed at `at` takes: that of the character before it, else of the first span.
pub fn style_at(spans: &[TextSpan], at: usize) -> TextSpan {
    let mut start = 0;
    for s in spans {
        let end = start + s.text.len();
        if at > start && at <= end {
            return style_of(s);
        }
        start = end;
    }
    spans.first().map(style_of).unwrap_or_default()
}

/// Merges neighbours of the same style and drops empty spans, keeping one if all are empty.
pub fn normalize(spans: Vec<TextSpan>) -> Vec<TextSpan> {
    let first = spans.first().map(style_of);
    let mut out: Vec<TextSpan> = Vec::with_capacity(spans.len());
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if same_style(last, &s) => last.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    if out.is_empty() {
        out.push(first.unwrap_or_default());
    }
    out
}

/// Splits the spans so that `at` falls between two of them; returns the index of the span
/// starting at `at` (`spans.len()` at the end).
fn split_at(spans: &mut Vec<TextSpan>, at: usize) -> usize {
    let mut start = 0;
    for i in 0..spans.len() {
        let end = start + spans[i].text.len();
        if at == start {
            return i;
        }
        if at < end {
            let tail = spans[i].text.split_off(at - start);
            let mut rest = style_of(&spans[i]);
            rest.text = tail;
            spans.insert(i + 1, rest);
            return i + 1;
        }
        start = end;
    }
    spans.len()
}

/// Inserts `text` at `at`, in `style` (its `text` is ignored) or else the style at `at`.
pub fn insert(spans: &[TextSpan], at: usize, text: &str, style: Option<&TextSpan>) -> Vec<TextSpan> {
    let at = floor_boundary(spans, at);
    let mut piece = style.map(style_of).unwrap_or_else(|| style_at(spans, at));
    piece.text = text.to_string();
    let mut out = spans.to_vec();
    let i = split_at(&mut out, at);
    out.insert(i, piece);
    normalize(out)
}

/// Deletes the byte range `range`.
pub fn delete(spans: &[TextSpan], range: Range<usize>) -> Vec<TextSpan> {
    let (a, b) = (floor_boundary(spans, range.start), floor_boundary(spans, range.end));
    let (a, b) = (a.min(b), a.max(b));
    let mut out = spans.to_vec();
    if a == b {
        return normalize(out);
    }
    let first = out.first().map(style_of);
    let i = split_at(&mut out, a);
    let j = split_at(&mut out, b);
    out.drain(i..j);
    // Deleting everything keeps the style of what was deleted first, for typing on.
    if out.iter().all(|s| s.text.is_empty()) {
        let keep = spans.iter().find(|s| !s.text.is_empty()).map(style_of).or(first);
        return vec![keep.unwrap_or_default()];
    }
    normalize(out)
}

/// Replaces `range` with `text`, typed in the style of the range's first character (or the
/// style at the range when it is empty), or in `style` when given.
pub fn replace(spans: &[TextSpan], range: Range<usize>, text: &str, style: Option<&TextSpan>) -> Vec<TextSpan> {
    let a = floor_boundary(spans, range.start.min(range.end));
    let style = style.map(style_of).unwrap_or_else(|| if range.start == range.end { style_at(spans, a) } else { style_at(spans, next_boundary(spans, a)) });
    let out = delete(spans, range);
    if text.is_empty() {
        return out;
    }
    insert(&out, a, text, Some(&style))
}

fn next_boundary(spans: &[TextSpan], at: usize) -> usize {
    let n = len(spans);
    let mut i = at + 1;
    while i < n && floor_boundary(spans, i) != i {
        i += 1;
    }
    i.min(n)
}

/// Applies `f` to the style of every character in `range`, splitting spans at its ends.
pub fn restyle(spans: &[TextSpan], range: Range<usize>, f: impl Fn(&mut TextSpan)) -> Vec<TextSpan> {
    let (a, b) = (floor_boundary(spans, range.start), floor_boundary(spans, range.end));
    let (a, b) = (a.min(b), a.max(b));
    let mut out = spans.to_vec();
    if a == b {
        return normalize(out);
    }
    let i = split_at(&mut out, a);
    let j = split_at(&mut out, b);
    out[i..j].iter_mut().for_each(f);
    normalize(out)
}

/// The spans covering `range`, in order (partial spans cut to the range).
pub fn slice(spans: &[TextSpan], range: Range<usize>) -> Vec<TextSpan> {
    let (a, b) = (floor_boundary(spans, range.start), floor_boundary(spans, range.end));
    let mut out = spans.to_vec();
    let j = split_at(&mut out, b.max(a));
    out.truncate(j);
    let i = split_at(&mut out, a.min(b));
    out.drain(..i);
    out
}

impl Tx<'_> {
    /// Replaces a text node's spans (normalised), keeping its other text properties. On a text
    /// inside an instance this writes an override, like [`Tx::update`].
    pub fn set_spans(&mut self, id: NodeId, spans: Vec<TextSpan>) -> Result<(), Error> {
        let n = self.doc.get(id).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        let NodeKind::Text { style, align, vertical_align, auto_resize, .. } = &n.kind else {
            return Err(Error::Input(format!("{id} is not a text node")));
        };
        let kind = NodeKind::Text { spans: normalize(spans), style: style.clone(), align: *align, vertical_align: *vertical_align, auto_resize: *auto_resize };
        self.update(id, PropValue::Kind(kind))
    }

    /// Edits a text node's spans with `f` (see [`Tx::set_spans`]).
    pub fn edit_spans(&mut self, id: NodeId, f: impl FnOnce(&[TextSpan]) -> Vec<TextSpan>) -> Result<(), Error> {
        let n = self.doc.get(id).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        let NodeKind::Text { spans, .. } = &n.kind else {
            return Err(Error::Input(format!("{id} is not a text node")));
        };
        let new = f(spans);
        self.set_spans(id, new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(t: &str, w: Option<u16>) -> TextSpan {
        TextSpan { text: t.into(), font_weight: w, ..Default::default() }
    }

    #[test]
    fn insert_inherits_left_style() {
        let s = vec![span("ab", Some(700)), span("cd", None)];
        let r = insert(&s, 2, "X", None);
        assert_eq!(r, vec![span("abX", Some(700)), span("cd", None)]);
        let r = insert(&s, 0, "X", None);
        assert_eq!(r, vec![span("Xab", Some(700)), span("cd", None)]);
    }

    #[test]
    fn delete_merges_and_keeps_style() {
        let s = vec![span("ab", None), span("cd", Some(700)), span("ef", None)];
        assert_eq!(delete(&s, 2..4), vec![span("abef", None)]);
        assert_eq!(delete(&s, 0..6), vec![span("", None)]);
        assert_eq!(delete(&s, 1..5), vec![span("af", None)]);
    }

    #[test]
    fn restyle_splits() {
        let s = vec![span("abcdef", None)];
        let r = restyle(&s, 2..4, |t| t.font_weight = Some(700));
        assert_eq!(r, vec![span("ab", None), span("cd", Some(700)), span("ef", None)]);
        let back = restyle(&r, 0..6, |t| t.font_weight = None);
        assert_eq!(back, s);
    }

    #[test]
    fn multibyte_boundaries() {
        let s = vec![span("aé日b", None)];
        // Offset 2 is inside "é"; it is clamped to 1.
        assert_eq!(floor_boundary(&s, 2), 1);
        assert_eq!(delete(&s, 1..6), vec![span("ab", None)]);
        assert_eq!(slice(&s, 1..6), vec![span("é日", None)]);
        assert_eq!(replace(&s, 3..6, "x", None), vec![span("aéxb", None)]);
    }
}
