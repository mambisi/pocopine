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

/// Plain-text variant for diagnostics embedded inside generated
/// `compile_error!` tokens. Rustc applies its own styling around the outer
/// error; embedding ANSI sequences would make snapshot output and non-TTY
/// logs display escape bytes instead of readable source context.
pub(crate) fn render_template_error_plain(
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
    let rendered = strip_ansi(&format!("{}", Renderer::styled().render(message)));
    rendered
        .strip_prefix("error: ")
        .unwrap_or(&rendered)
        .to_string()
}

/// Render a diagnostic whose primary failure lives inside a structural
/// template body. The owning `<template ...>` is marked as context and the
/// offending descendant as the error. `fold(true)` keeps both ends visible
/// while collapsing the middle of a large or visually dense body.
pub(crate) fn render_template_error_plain_with_context(
    poco_src: &str,
    file_path: &str,
    byte_range: Range<usize>,
    title: &str,
    label: &str,
    context_range: Range<usize>,
    context_label: &str,
) -> String {
    // Keep the primary error annotation first: annotate-snippets uses
    // insertion order for the `--> file:line:column` header, while still
    // rendering source lines in their natural order. The header therefore
    // remains anchored to the offending child rather than the owner template.
    let message = Level::Error.title(title).snippet(
        Snippet::source(poco_src)
            .origin(file_path)
            .fold(true)
            .annotation(Level::Error.span(byte_range).label(label))
            .annotation(Level::Info.span(context_range).label(context_label)),
    );

    let rendered = strip_ansi(&format!("{}", Renderer::styled().render(message)));
    rendered
        .strip_prefix("error: ")
        .unwrap_or(&rendered)
        .to_string()
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

/// Render an error whose byte range is unknown or zero — falls
/// back to a file-level message without a source snippet.
/// Used when a `ParseError` surfaces without a range (e.g.
/// inherited from html5ever without position info).
pub(crate) fn render_fileless_error(file_path: &str, title: &str, note: &str) -> String {
    let footer = format!("in {file_path}: {note}");
    let message = Level::Error.title(title).footer(Level::Note.title(&footer));
    format!("{}", Renderer::styled().render(message))
}

/// Render a `.poco` template error like [`render_template_error`]
/// but with optional `help:` and `note:` footers (rustc-shaped).
///
/// Used when the diagnostic wants to teach the author a fix
/// (e.g. "move the condition onto the concrete row element with
/// `pp-show`") in addition to pointing at the offending source
/// span.
pub(crate) fn render_template_error_with_footers(
    poco_src: &str,
    file_path: &str,
    byte_range: Range<usize>,
    title: &str,
    label: &str,
    help: Option<&str>,
    note: Option<&str>,
) -> String {
    let mut message = Level::Error.title(title).snippet(
        Snippet::source(poco_src)
            .origin(file_path)
            .fold(true)
            .annotation(Level::Error.span(byte_range).label(label)),
    );
    if let Some(h) = help {
        message = message.footer(Level::Help.title(h));
    }
    if let Some(n) = note {
        message = message.footer(Level::Note.title(n));
    }
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
        let rendered = render_fileless_error("test.poco", "malformed template", "unclosed tag");
        assert!(rendered.contains("test.poco"));
        assert!(rendered.contains("unclosed tag"));
    }

    #[test]
    fn compile_error_render_keeps_source_coordinates_without_ansi() {
        let rendered = render_template_error_plain(
            "<div>one</div>\n<div>two</div>",
            "nested.poco",
            15..18,
            "invalid nested branch",
            "second root",
        );
        assert!(rendered.contains("nested.poco:2:1"), "{rendered}");
        assert!(rendered.contains("second root"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
    }

    #[test]
    fn compile_error_render_reports_character_column_after_utf8() {
        let rendered = render_template_error_plain(
            "é <b>x</b>",
            "utf8.poco",
            3..6,
            "invalid nested branch",
            "second root",
        );
        assert!(rendered.contains("utf8.poco:1:3"), "{rendered}");
    }

    #[test]
    fn structural_error_shows_owner_then_offending_child() {
        let source = r#"<div>
  <template pp-if="open">
    <section>
      <template pp-match="state">
        <template pp-case="Ready">
          <b>first</b>
          <b>second</b>
        </template>
      </template>
    </section>
  </template>
</div>
"#;
        let context_start = source.find("<template pp-case").unwrap();
        let context_end = context_start + "<template pp-case=\"Ready\">".len();
        let primary_start = source.find("<b>second").unwrap();
        let rendered = render_template_error_plain_with_context(
            source,
            "nested.poco",
            primary_start..primary_start + 3,
            "invalid nested branch",
            "second root",
            context_start..context_end,
            "`pp-case` template body starts here",
        );

        assert!(rendered.contains("nested.poco:7:11"), "{rendered}");
        assert!(
            rendered.contains("5 |         <template pp-case"),
            "{rendered}"
        );
        assert!(rendered.contains("template body starts here"), "{rendered}");
        assert!(rendered.contains("<b>second</b>"), "{rendered}");
    }

    #[test]
    fn structural_error_folds_a_large_first_child() {
        let source = r#"<template pp-case="Ready">
  <section>
    <div>one</div>
    <div>two</div>
    <div>three</div>
    <div>four</div>
    <div>five</div>
    <div>six</div>
  </section>
  <b>second</b>
</template>
"#;
        let context_start = source.find("<template pp-case").unwrap();
        let context_end = context_start + "<template pp-case=\"Ready\">".len();
        let primary_start = source.find("<b>second").unwrap();
        let rendered = render_template_error_plain_with_context(
            source,
            "nested.poco",
            primary_start..primary_start + 3,
            "invalid nested branch",
            "second root",
            context_start..context_end,
            "`pp-case` template body starts here",
        );

        assert!(rendered.contains("nested.poco:10:3"), "{rendered}");
        assert!(rendered.contains("<template pp-case"), "{rendered}");
        assert!(rendered.contains("<b>second</b>"), "{rendered}");
        assert!(rendered.contains("..."), "{rendered}");
        assert!(!rendered.contains("<div>three</div>"), "{rendered}");
    }
}
