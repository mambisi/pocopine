//! Render template-validation errors as pre-formatted
//! `annotate-snippets` blocks that rustc prints verbatim.
//!
//! Why we render ourselves:
//!
//! Stable proc-macros can't construct spans that point inside
//! external files (`proc_macro_span` is unstable). Rustc's
//! arrow on a `syn::Error` / `compile_error!` attached to our
//! assertion lands on the `#[component]` attribute — not at
//! line 14 of the `.poco`. So we stop trying to drive rustc's
//! arrow and render the snippet ourselves, then emit it as
//! the error message. Rustc prints the string verbatim; IDEs
//! parse `--> path:line:col` linkify regardless of who produced
//! it.
//!
//! See RFC 049 §4.6 and RFC 050 §4.6 for the design rationale.

use std::ops::Range;

use annotate_snippets::{Level, Renderer, Snippet};

/// Render one `.poco` template error as a rustc-shaped snippet.
///
/// Parameters:
///
/// * `poco_src` — the raw `.poco` source text.
/// * `file_path` — path shown in the `-->` line; typically
///   relative to the consumer crate's manifest dir.
/// * `byte_range` — byte offsets inside `poco_src` the snippet
///   should caret at. Must be non-empty; callers filter on
///   non-zero ranges per RFC 050's byte-range contract.
/// * `title` — one-line headline shown alongside the `error:`
///   prefix.
/// * `label` — short caption under the caret.
///
/// The returned string already contains all newlines and
/// spacing rustc expects; embed it directly in a
/// `syn::Error::new(span, rendered)` or a `compile_error!`
/// body.
pub(crate) fn render_template_error(
    poco_src: &str,
    file_path: &str,
    byte_range: Range<usize>,
    title: &str,
    label: &str,
) -> String {
    let message = Level::Error.title(title).snippet(
        Snippet::source(poco_src)
            .origin(file_path)
            .fold(true)
            .annotation(Level::Error.span(byte_range).label(label)),
    );
    format!("{}", Renderer::styled().render(message))
}

/// Render an error whose byte range is unknown or zero — falls
/// back to a file-level message without a source snippet.
/// Used when a `ParseError` surfaces without a range (e.g.
/// inherited from html5ever without position info).
pub(crate) fn render_fileless_error(
    file_path: &str,
    title: &str,
    note: &str,
) -> String {
    let footer = format!("in {file_path}: {note}");
    let message = Level::Error
        .title(title)
        .footer(Level::Note.title(&footer));
    format!("{}", Renderer::styled().render(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_produces_file_line_col_header() {
        let src = "<div>a</div>\n<div>b</div>";
        // Second root at byte 13 (the `<` of the second div).
        let rendered = render_template_error(
            src,
            "test.poco",
            13..25,
            "has more than one root element",
            "additional root — drops at runtime",
        );
        assert!(
            rendered.contains("test.poco"),
            "rendered output should include file path, got:\n{rendered}"
        );
        assert!(
            rendered.contains("has more than one root element"),
            "rendered output should include title, got:\n{rendered}"
        );
        assert!(
            rendered.contains("2"),
            "rendered output should include line 2 in gutter, got:\n{rendered}"
        );
    }

    #[test]
    fn fileless_render_includes_file_and_note() {
        let rendered = render_fileless_error(
            "test.poco",
            "malformed template",
            "unclosed tag",
        );
        assert!(rendered.contains("test.poco"));
        assert!(rendered.contains("unclosed tag"));
    }
}
