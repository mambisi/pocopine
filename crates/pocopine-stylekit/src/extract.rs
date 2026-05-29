//! `.poco` class extraction (RFC 092 D5).
//!
//! Extraction runs on the **Pocopine parser AST**, not a text scan, so
//! it can tell a real `class="…"` from a string literal in a comment
//! and understand component class props. v1 recognizes:
//!
//! - static `class="…"` attributes,
//! - component class props Pocopine treats as class-like,
//! - static class maps in supported binding forms,
//! - `pp-bind:class` only when the class set is statically discoverable.
//!
//! Opaque dynamic construction yields a [`Diagnostic`] pointing at a
//! migration path to a static class map — never a silent miss.
//!
//! The AST-walking implementation is wired under the phased plan once
//! this crate depends on the Pocopine parser; this module currently
//! defines the data contract every front-end consumes.

use crate::diagnostics::Span;

/// One class token discovered in a source file, with its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedClass {
    /// Verbatim class literal, e.g. `hover:bg-surface` or `text-[13px]`.
    pub value: String,
    /// Where it appears, for diagnostics.
    pub span: Span,
}

impl UsedClass {
    pub fn new(value: impl Into<String>, span: Span) -> Self {
        Self {
            value: value.into(),
            span,
        }
    }
}

/// Extract used classes from a parsed `.poco` document.
///
/// The seam every front-end (build/dev) calls. The implementation walks
/// the Pocopine AST; until that dependency is wired it returns empty so
/// the pipeline composes end-to-end.
pub trait Extractor {
    /// Pull every statically-discoverable class from one source file,
    /// reporting opaque dynamic construction as diagnostics.
    fn extract(&self, file: u32, source: &str) -> Vec<UsedClass>;
}
