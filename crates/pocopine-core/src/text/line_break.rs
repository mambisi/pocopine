//! Cursor-walking line breaker.
//!
//! Port of the shape of pretext's `walkPreparedLinesRaw` simplified
//! to the v1 segment kinds ([`SegmentKind::Text`] / `Space` /
//! `ZeroWidthBreak` / `SoftHyphen`). The walker holds one
//! "pending break" — the most recent position we can retreat to
//! when the next segment overflows — and falls back to per-grapheme
//! placement when a single Text segment is itself wider than the
//! line.

use crate::text::analysis::SegmentKind;
use crate::text::prepare::PreparedText;

/// Position inside a prepared text, used as both line start and
/// line end. `grapheme_index == 0` means aligned with the start of
/// `segments[segment_index]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutCursor {
    pub segment_index: u32,
    pub grapheme_index: u32,
}

impl LayoutCursor {
    pub const fn start() -> Self {
        Self {
            segment_index: 0,
            grapheme_index: 0,
        }
    }
}

/// Subpixel slack absorbing canvas measurement jitter. Matches
/// pretext's `lineFitEpsilon` defaults.
const FIT_EPSILON: f64 = 0.05;

fn breaks_after(k: SegmentKind) -> bool {
    matches!(
        k,
        SegmentKind::Space | SegmentKind::ZeroWidthBreak | SegmentKind::SoftHyphen
    )
}

fn consumes_at_line_start(k: SegmentKind) -> bool {
    matches!(
        k,
        SegmentKind::Space | SegmentKind::ZeroWidthBreak | SegmentKind::SoftHyphen
    )
}

/// Internal per-line record. Public [`crate::text::layout::LayoutLine`]
/// is materialized from this + the prepared text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InternalLine {
    pub(crate) start: LayoutCursor,
    pub(crate) end: LayoutCursor,
    pub(crate) width: f64,
    /// True when the line was broken at a mid-word soft hyphen and
    /// the consumer should render a visible "-" at the end.
    pub(crate) soft_hyphen_break: bool,
}

pub(crate) fn walk<F: FnMut(InternalLine)>(
    prepared: &PreparedText,
    max_width: f64,
    mut visit: F,
) -> u32 {
    let segs = &prepared.segments;
    if segs.is_empty() {
        return 0;
    }
    let fit_limit = max_width.max(0.0) + FIT_EPSILON;
    let hyphen_w = prepared.soft_hyphen_width;

    let mut i: usize = 0;
    let mut line_count: u32 = 0;
    let mut has_content = false;
    let mut line_w: f64 = 0.0;
    let mut line_start = LayoutCursor::start();
    let mut line_end = LayoutCursor::start();
    // `-1` = none; otherwise the segment index where the line
    // would end if we committed the retreat.
    let mut pending_seg: i64 = -1;
    let mut pending_width: f64 = 0.0;
    let mut pending_soft_hyphen: bool = false;

    while i < segs.len() {
        if !has_content {
            while i < segs.len() && consumes_at_line_start(segs[i].kind) {
                i += 1;
            }
            if i >= segs.len() {
                break;
            }
            line_start = LayoutCursor {
                segment_index: i as u32,
                grapheme_index: 0,
            };
            line_end = line_start;
        }

        let seg = &segs[i];
        let w = seg.width;

        if !has_content {
            if matches!(seg.kind, SegmentKind::Text) && w > fit_limit {
                if let Some(gw) = &seg.grapheme_widths {
                    // Per-grapheme fallback for an oversize word.
                    let seg_idx = i as u32;
                    let mut acc = 0.0_f64;
                    let mut g_start: u32 = 0;
                    let mut g: u32 = 0;
                    while (g as usize) < gw.len() {
                        let step = gw[g as usize];
                        if acc + step > fit_limit && g > g_start {
                            line_count += 1;
                            visit(InternalLine {
                                start: LayoutCursor {
                                    segment_index: seg_idx,
                                    grapheme_index: g_start,
                                },
                                end: LayoutCursor {
                                    segment_index: seg_idx,
                                    grapheme_index: g,
                                },
                                width: acc,
                                soft_hyphen_break: false,
                            });
                            g_start = g;
                            acc = 0.0;
                        }
                        acc += step;
                        g += 1;
                    }
                    // Remainder continues as current-line content.
                    line_start = LayoutCursor {
                        segment_index: seg_idx,
                        grapheme_index: g_start,
                    };
                    line_end = LayoutCursor {
                        segment_index: seg_idx + 1,
                        grapheme_index: 0,
                    };
                    line_w = acc;
                    has_content = acc > 0.0;
                    pending_seg = -1;
                    pending_soft_hyphen = false;
                    i += 1;
                    continue;
                }
            }
            line_w = w;
            line_end = LayoutCursor {
                segment_index: (i + 1) as u32,
                grapheme_index: 0,
            };
            has_content = true;
            if breaks_after(seg.kind) {
                pending_seg = (i + 1) as i64;
                let collapsed = if matches!(seg.kind, SegmentKind::Space) {
                    w
                } else {
                    0.0
                };
                pending_width = line_w - collapsed;
                pending_soft_hyphen = matches!(seg.kind, SegmentKind::SoftHyphen);
                if pending_soft_hyphen {
                    pending_width = line_w + hyphen_w - collapsed;
                }
            }
            i += 1;
            continue;
        }

        // Soft-hyphen marker mid-line: record a break opportunity.
        if matches!(seg.kind, SegmentKind::SoftHyphen) {
            pending_seg = (i + 1) as i64;
            pending_width = line_w + hyphen_w;
            pending_soft_hyphen = true;
            i += 1;
            continue;
        }

        let new_w = line_w + w;
        if new_w > fit_limit {
            if breaks_after(seg.kind) {
                // The overflowing segment is itself a break.
                line_count += 1;
                visit(InternalLine {
                    start: line_start,
                    end: line_end,
                    width: line_w,
                    soft_hyphen_break: false,
                });
                has_content = false;
                line_w = 0.0;
                pending_seg = -1;
                pending_soft_hyphen = false;
                i += 1;
                continue;
            }
            if pending_seg >= 0 && pending_width <= fit_limit {
                line_count += 1;
                visit(InternalLine {
                    start: line_start,
                    end: LayoutCursor {
                        segment_index: pending_seg as u32,
                        grapheme_index: 0,
                    },
                    width: pending_width,
                    soft_hyphen_break: pending_soft_hyphen,
                });
                i = pending_seg as usize;
                has_content = false;
                line_w = 0.0;
                pending_seg = -1;
                pending_soft_hyphen = false;
                continue;
            }
            // No retreat — emit the line-so-far and retry this
            // segment at line start.
            line_count += 1;
            visit(InternalLine {
                start: line_start,
                end: line_end,
                width: line_w,
                soft_hyphen_break: false,
            });
            has_content = false;
            line_w = 0.0;
            pending_seg = -1;
            pending_soft_hyphen = false;
            continue;
        }

        line_w = new_w;
        line_end = LayoutCursor {
            segment_index: (i + 1) as u32,
            grapheme_index: 0,
        };
        if breaks_after(seg.kind) {
            pending_seg = (i + 1) as i64;
            let collapsed = if matches!(seg.kind, SegmentKind::Space) {
                w
            } else {
                0.0
            };
            pending_width = line_w - collapsed;
            pending_soft_hyphen = false;
        }
        i += 1;
    }

    if has_content {
        line_count += 1;
        visit(InternalLine {
            start: line_start,
            end: line_end,
            width: line_w,
            soft_hyphen_break: false,
        });
    }
    line_count
}
