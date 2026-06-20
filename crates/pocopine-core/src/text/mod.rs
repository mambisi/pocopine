//! Text layout engine — a Rust port of pretext's prepare/layout model.
//!
//! The engine has no rendering. It takes text + a font + an options
//! bag and returns line counts, widths, heights, and cursor-addressed
//! per-line slices so callers can drive DOM / canvas / SVG rendering
//! themselves. Width measurement is delegated to the [`Measurer`]
//! trait; [`CanvasMeasurer`] is the default in-browser impl and
//! forwards to Canvas 2D `measureText()` via a small JS shim.
//!
//! Two-phase model, matching pretext:
//!
//! 1. [`prepare`] runs analysis + measurement once and caches
//!    everything in an opaque [`PreparedText`].
//! 2. [`layout`] / [`layout_with_lines`] are pure arithmetic — cheap
//!    to re-run on every resize.
//!
//! ```ignore
//! use pocopine_core::text::{prepare, layout, CanvasMeasurer, Font, PrepareOptions};
//!
//! let measurer = CanvasMeasurer;
//! let font = Font("16px system-ui".into());
//! let prepared = prepare("Hello world", &font, PrepareOptions::default(), &measurer);
//! let result = layout(&prepared, 80.0, 20.0);
//! ```
//!
//! Scope is pragmatic LTR: grapheme-correct segmentation, soft
//! hyphens, zero-width breaks, collapsible whitespace. CJK word-break,
//! kinsoku, bidi, emoji correction, and `pre-wrap` hard-breaks are
//! deferred.

pub mod analysis;
pub mod layout;
pub mod line_break;
pub mod measure;
pub mod measure_js;
pub mod prepare;

pub use layout::{LayoutLine, LayoutResult, layout, layout_with_lines};
pub use line_break::LayoutCursor;
pub use measure::{CanvasMeasurer, Measurer};
pub use prepare::{Font, PrepareOptions, PreparedText, WhiteSpace, prepare};

/// Canonical number→text used by every text-rendering path
/// (pp-text, interpolation, row plans, the typed text lane, and
/// the mutation-channel interpreter's `String(v)`): JS `String()`
/// semantics, so 1e21 → "1e+21", Infinity → "Infinity", and the
/// channel/direct paths can never disagree on a number's text.
/// RFC-099 Phase 1 — one pure-Rust implementation
/// ([`crate::js_number::to_js_string`]) on both targets, so the wasm
/// client and the SSR host renderer produce byte-identical text.
pub(crate) fn js_number_string(n: f64) -> String {
    crate::js_number::to_js_string(n)
}
