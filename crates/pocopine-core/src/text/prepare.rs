//! `prepare()` — runs analysis + measurement once so [`crate::text::layout`]
//! can stay pure arithmetic.

use crate::text::analysis::{normalize_whitespace, segment, SegmentKind};
use crate::text::measure::Measurer;
use unicode_segmentation::UnicodeSegmentation;

/// CSS `font` shorthand, e.g. `"600 16px system-ui"`. Forwarded
/// verbatim to `canvas.measureText()`.
#[derive(Debug, Clone)]
pub struct Font(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteSpace {
    /// Collapse whitespace runs and trim edges — CSS default.
    #[default]
    Normal,
}

#[derive(Debug, Clone, Default)]
pub struct PrepareOptions {
    pub white_space: WhiteSpace,
    /// Extra CSS letter-spacing in px, applied between rendered
    /// graphemes. `0.0` disables the spacing code path. Reserved
    /// for future use — v1 line breaker ignores this.
    pub letter_spacing: f64,
}

pub(crate) struct PreparedSegment {
    pub(crate) kind: SegmentKind,
    pub(crate) text: String,
    pub(crate) width: f64,
    /// Per-grapheme widths, present only for `Text` segments with
    /// more than one grapheme. Used by [`super::line_break`] when a
    /// word overflows on its own and has to fall back to
    /// character-level placement.
    pub(crate) grapheme_widths: Option<Vec<f64>>,
}

pub struct PreparedText {
    pub(crate) font: Font,
    #[allow(dead_code)]
    pub(crate) options: PrepareOptions,
    pub(crate) segments: Vec<PreparedSegment>,
    pub(crate) soft_hyphen_width: f64,
    pub(crate) normalized: String,
}

impl PreparedText {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
    pub fn normalized(&self) -> &str {
        &self.normalized
    }
    pub fn font(&self) -> &Font {
        &self.font
    }
}

pub fn prepare<M: Measurer>(
    text: &str,
    font: &Font,
    options: PrepareOptions,
    measurer: &M,
) -> PreparedText {
    let normalized = match options.white_space {
        WhiteSpace::Normal => normalize_whitespace(text),
    };
    let raw = segment(&normalized);

    let mut segments = Vec::with_capacity(raw.len());
    for rs in raw {
        let (width, grapheme_widths) = match rs.kind {
            SegmentKind::Text => {
                let total = measurer.measure(font, &rs.text);
                let has_multiple = rs.text.graphemes(true).nth(1).is_some();
                let widths = if has_multiple {
                    Some(
                        rs.text
                            .graphemes(true)
                            .map(|g| measurer.measure(font, g))
                            .collect(),
                    )
                } else {
                    None
                };
                (total, widths)
            }
            SegmentKind::Space => (measurer.measure(font, " "), None),
            SegmentKind::ZeroWidthBreak => (0.0, None),
            SegmentKind::SoftHyphen => (0.0, None),
        };
        segments.push(PreparedSegment {
            kind: rs.kind,
            text: rs.text,
            width,
            grapheme_widths,
        });
    }

    let soft_hyphen_width = measurer.measure(font, "-");

    PreparedText {
        font: font.clone(),
        options,
        segments,
        soft_hyphen_width,
        normalized,
    }
}
