//! Framework-owned diagnostics with source spans (RFC 092 D5).
//!
//! Unknown utilities, wrong arbitrary-value types, and unknown tokens
//! are errors carrying a [`Span`] back into the originating `.poco`
//! file; class conflicts are warnings. Suggestions ("did you mean
//! `bg-surface`?") use edit distance over the known vocabulary.

use strsim::levenshtein;

/// Byte range into a source file, plus the file id it belongs to.
///
/// `file` indexes into the caller's file table (the extractor owns the
/// path↔id mapping). `Span::UNKNOWN` is used before extraction wiring
/// lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const UNKNOWN: Span = Span {
        file: u32::MAX,
        start: 0,
        end: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A single Stylekit diagnostic. Rendered through the shared
/// `pocopine-cli` diagnostic sink alongside Pocopine diagnostics.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Optional "did you mean …" follow-up.
    pub help: Option<String>,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            help: None,
            span: Span::UNKNOWN,
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            help: None,
            span: Span::UNKNOWN,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Attach the originating span (set late, once the class's location
    /// is known).
    pub fn at(mut self, span: Span) -> Self {
        self.span = span;
        self
    }
}

/// Pick the closest candidate to `input` within a small edit distance,
/// for "did you mean" help. Returns `None` when nothing is close enough.
pub fn suggest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    // Cap proportional to length so short typos match but unrelated
    // names don't get a spurious suggestion.
    let max = (input.len() / 3).max(2);
    candidates
        .into_iter()
        .map(|c| (levenshtein(input, c), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_nearest_token() {
        let known = ["bg-surface", "bg-surface-2", "bg-ink-10"];
        assert_eq!(suggest("bg-surafce", known), Some("bg-surface"));
    }

    #[test]
    fn no_suggestion_when_far() {
        let known = ["flex", "grid", "block"];
        assert_eq!(suggest("background-attachment-fixed", known), None);
    }
}
