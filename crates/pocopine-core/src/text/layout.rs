//! Public layout entry points.
//!
//! Wraps [`crate::text::line_break::walk`] into the two result
//! shapes consumers actually want: a cheap summary ([`layout`]) and
//! a materialized line list ([`layout_with_lines`]).

use crate::text::analysis::SegmentKind;
use crate::text::line_break::{walk, InternalLine, LayoutCursor};
use crate::text::prepare::PreparedText;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy)]
pub struct LayoutResult {
    pub line_count: u32,
    pub height: f64,
    pub max_line_width: f64,
}

#[derive(Debug, Clone)]
pub struct LayoutLine {
    /// Reconstructed line text. Visible soft-hyphen breaks append a
    /// rendered `"-"` so consumers can set `.textContent` directly.
    pub text: String,
    pub width: f64,
    pub start: LayoutCursor,
    pub end: LayoutCursor,
    /// True when the line was cut at a soft-hyphen — `text` ends
    /// with a visible hyphen the author didn't type.
    pub soft_hyphen_break: bool,
}

pub fn layout(prepared: &PreparedText, max_width: f64, line_height: f64) -> LayoutResult {
    if prepared.is_empty() {
        return LayoutResult {
            line_count: 0,
            height: 0.0,
            max_line_width: 0.0,
        };
    }
    let mut max_w = 0.0_f64;
    let count = walk(prepared, max_width, |line: InternalLine| {
        if line.width > max_w {
            max_w = line.width;
        }
    });
    LayoutResult {
        line_count: count,
        height: (count as f64) * line_height,
        max_line_width: max_w,
    }
}

pub fn layout_with_lines(
    prepared: &PreparedText,
    max_width: f64,
    line_height: f64,
) -> (LayoutResult, Vec<LayoutLine>) {
    if prepared.is_empty() {
        return (
            LayoutResult {
                line_count: 0,
                height: 0.0,
                max_line_width: 0.0,
            },
            Vec::new(),
        );
    }
    let mut max_w = 0.0_f64;
    let mut lines: Vec<LayoutLine> = Vec::new();
    let count = walk(prepared, max_width, |line| {
        if line.width > max_w {
            max_w = line.width;
        }
        let mut text = slice_line_text(prepared, line.start, line.end);
        // Trailing normalized spaces collapse at end of line.
        while text.ends_with(' ') {
            text.pop();
        }
        if line.soft_hyphen_break {
            text.push('-');
        }
        lines.push(LayoutLine {
            text,
            width: line.width,
            start: line.start,
            end: line.end,
            soft_hyphen_break: line.soft_hyphen_break,
        });
    });
    (
        LayoutResult {
            line_count: count,
            height: (count as f64) * line_height,
            max_line_width: max_w,
        },
        lines,
    )
}

fn slice_line_text(prepared: &PreparedText, start: LayoutCursor, end: LayoutCursor) -> String {
    let mut out = String::new();
    let si = start.segment_index as usize;
    let ei = end.segment_index as usize;
    let sg = start.grapheme_index as usize;
    let eg = end.grapheme_index as usize;
    let mut idx = si;
    while idx <= ei && idx < prepared.segments.len() {
        let seg = &prepared.segments[idx];
        let is_first = idx == si;
        let is_last = idx == ei;
        let bounded_end = if is_last { eg } else { usize::MAX };

        // Skip markers that do not render text.
        let skip = matches!(
            seg.kind,
            SegmentKind::ZeroWidthBreak | SegmentKind::SoftHyphen
        );

        if !skip {
            if is_first && is_last {
                if bounded_end == 0 {
                    break;
                }
                append_range(&seg.text, sg, bounded_end, &mut out);
            } else if is_first {
                append_range(&seg.text, sg, usize::MAX, &mut out);
            } else if is_last {
                if bounded_end == 0 {
                    break;
                }
                append_range(&seg.text, 0, bounded_end, &mut out);
            } else {
                out.push_str(&seg.text);
            }
        }
        if is_last {
            break;
        }
        idx += 1;
    }
    out
}

fn append_range(segment_text: &str, start_g: usize, end_g: usize, out: &mut String) {
    if end_g == 0 || start_g >= end_g {
        return;
    }
    for (i, g) in segment_text.graphemes(true).enumerate() {
        if i < start_g {
            continue;
        }
        if i >= end_g {
            break;
        }
        out.push_str(g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::measure::Measurer;
    use crate::text::prepare::{prepare, Font, PrepareOptions};

    struct Mock;
    impl Measurer for Mock {
        fn measure(&self, _font: &Font, text: &str) -> f64 {
            // 10px per char keeps numbers round.
            text.chars().count() as f64 * 10.0
        }
    }

    fn font() -> Font {
        Font("16px system-ui".into())
    }

    #[test]
    fn empty_text_produces_no_lines() {
        let p = prepare("", &font(), PrepareOptions::default(), &Mock);
        let r = layout(&p, 100.0, 20.0);
        assert_eq!(r.line_count, 0);
        assert_eq!(r.height, 0.0);
    }

    #[test]
    fn short_text_fits_one_line() {
        let p = prepare("hello", &font(), PrepareOptions::default(), &Mock);
        let r = layout(&p, 100.0, 20.0);
        assert_eq!(r.line_count, 1);
        assert_eq!(r.height, 20.0);
    }

    #[test]
    fn wraps_at_space_when_next_word_overflows() {
        // "hello world" -> hello=50, space=10, world=50 = 110 total.
        // max_width=60 fits "hello" only.
        let p = prepare("hello world", &font(), PrepareOptions::default(), &Mock);
        let (r, lines) = layout_with_lines(&p, 60.0, 20.0);
        assert_eq!(r.line_count, 2);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, "world");
    }

    #[test]
    fn collapses_and_trims_whitespace() {
        let p = prepare(
            "  hello   world  ",
            &font(),
            PrepareOptions::default(),
            &Mock,
        );
        assert_eq!(p.normalized(), "hello world");
    }

    #[test]
    fn zero_width_space_is_a_break_opportunity() {
        // "abcd\u{200B}efgh" with ZWSP between halves.
        let p = prepare(
            "abcd\u{200B}efgh",
            &font(),
            PrepareOptions::default(),
            &Mock,
        );
        let (r, lines) = layout_with_lines(&p, 45.0, 20.0);
        assert_eq!(r.line_count, 2);
        assert_eq!(lines[0].text, "abcd");
        assert_eq!(lines[1].text, "efgh");
    }

    #[test]
    fn soft_hyphen_inserts_visible_dash() {
        // "longword" with SHY between "long" and "word", narrow
        // width that fits "long" + "-" but not the full word.
        let p = prepare(
            "long\u{00AD}word",
            &font(),
            PrepareOptions::default(),
            &Mock,
        );
        // long=40, hyphen(display)=10, so line 1 fits "long-" at 50px.
        // word=40 on line 2.
        let (r, lines) = layout_with_lines(&p, 55.0, 20.0);
        assert_eq!(r.line_count, 2);
        assert!(lines[0].soft_hyphen_break);
        assert!(lines[0].text.ends_with('-'));
        assert_eq!(lines[1].text, "word");
    }

    #[test]
    fn oversize_word_breaks_at_grapheme_boundaries() {
        // 8 chars at 10px each = 80 total; max_width=25 -> 3 lines
        // of 2-3 chars.
        let p = prepare("abcdefgh", &font(), PrepareOptions::default(), &Mock);
        let (r, lines) = layout_with_lines(&p, 25.0, 20.0);
        assert!(r.line_count >= 3);
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined, "abcdefgh");
    }

    #[test]
    fn exact_boundary_fits_on_one_line() {
        // "abcde" = 50px, max_width = 50.0 — must fit.
        let p = prepare("abcde", &font(), PrepareOptions::default(), &Mock);
        let r = layout(&p, 50.0, 20.0);
        assert_eq!(r.line_count, 1);
    }
}
