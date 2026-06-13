//! Text segmentation — simplified port of pretext's analysis pass.
//!
//! Normalizes whitespace (collapses `[ \t\n\r\f]+` runs to a single
//! `' '` and trims edges) then walks graphemes to emit a flat
//! segment list. Four kinds exist in v1:
//!
//! - [`SegmentKind::Text`] — everything renderable.
//! - [`SegmentKind::Space`] — a single U+0020 (spaces are always
//!   single after normalization).
//! - [`SegmentKind::ZeroWidthBreak`] — U+200B.
//! - [`SegmentKind::SoftHyphen`] — U+00AD.
//!
//! CJK/kinsoku/URL-run merging/emoji correction from pretext's
//! analysis.ts are intentionally omitted — the data layout leaves
//! room to add them without changing the public surface.

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Text,
    Space,
    ZeroWidthBreak,
    SoftHyphen,
}

#[derive(Debug, Clone)]
pub(crate) struct RawSegment {
    pub(crate) kind: SegmentKind,
    pub(crate) text: String,
}

const SHY: char = '\u{00AD}';
const ZWSP: char = '\u{200B}';

pub(crate) fn normalize_whitespace(text: &str) -> String {
    if !text
        .chars()
        .any(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C'))
    {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C') {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn special_kind(c: char) -> Option<SegmentKind> {
    match c {
        ' ' => Some(SegmentKind::Space),
        ZWSP => Some(SegmentKind::ZeroWidthBreak),
        SHY => Some(SegmentKind::SoftHyphen),
        _ => None,
    }
}

pub(crate) fn segment(text: &str) -> Vec<RawSegment> {
    let mut out: Vec<RawSegment> = Vec::new();
    let mut buf = String::new();
    for g in text.graphemes(true) {
        // Only treat a grapheme as a break marker when it is
        // exactly one of the single-char markers. Combining
        // sequences that happen to start with a space are rare and
        // would break text runs incorrectly if we split eagerly.
        if g.chars().count() == 1
            && let Some(kind) = special_kind(g.chars().next().unwrap()) {
                if !buf.is_empty() {
                    out.push(RawSegment {
                        kind: SegmentKind::Text,
                        text: std::mem::take(&mut buf),
                    });
                }
                out.push(RawSegment {
                    kind,
                    text: g.to_string(),
                });
                continue;
            }
        buf.push_str(g);
    }
    if !buf.is_empty() {
        out.push(RawSegment {
            kind: SegmentKind::Text,
            text: buf,
        });
    }
    out
}
