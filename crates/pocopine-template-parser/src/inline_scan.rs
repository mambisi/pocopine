//! RFC-116 — find `poco!` template bodies inside Rust sources.
//!
//! Inline templates would otherwise be invisible to every tool that walks
//! `.poco` files: Stylekit's utility-class extraction and `pocopine lsp`
//! diagnostics both scan the filesystem for templates. This module gives them
//! one shared way to locate the bodies embedded in `.rs` sources, so an
//! inline template is as discoverable as a file one.
//!
//! The scan runs on **tokens**, never on a regex over source. That is what
//! makes it trustworthy: a `poco!` inside a comment, or the text `"poco!"`
//! inside a string literal, is not a macro invocation and is skipped for
//! free — a textual search would report both.

use std::ops::Range;

/// One `poco!` body found in a Rust source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTemplate {
    /// Byte range of the body **between** the macro's delimiters, into the
    /// scanned source. Template byte `i` is source byte `body_range.start + i`,
    /// which is what lets diagnostics point at the real `.rs` position.
    pub body_range: Range<usize>,
}

impl InlineTemplate {
    /// The body text, borrowed from the source that produced this hit.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        source
            .get(self.body_range.clone())
            .expect("body_range comes from a span into this source")
    }
}

/// Every `poco!` body in `rust_source`, in source order.
///
/// Unlexable input yields an empty list rather than an error: rustc owns
/// reporting broken Rust, and a formatter or language server should not
/// produce a second, worse diagnostic for the same problem.
pub fn scan_inline_templates(rust_source: &str) -> Vec<InlineTemplate> {
    let Ok(stream) = rust_source.parse::<proc_macro2::TokenStream>() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    collect(stream, &mut found);
    found.sort_by_key(|hit| hit.body_range.start);
    found
}

/// Walk the token tree looking for the `poco` `!` `⟨group⟩` triple.
///
/// Only the trailing three tokens are inspected, so a path-qualified
/// `pocopine::poco! { … }` matches on the same code path as a bare `poco!`.
/// The walk descends into every group so bodies inside `#[component(...)]`
/// attribute args, nested modules and function bodies are all reachable.
fn collect(stream: proc_macro2::TokenStream, out: &mut Vec<InlineTemplate>) {
    use proc_macro2::TokenTree;

    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut index = 0;
    while index < tokens.len() {
        let is_poco = matches!(&tokens[index], TokenTree::Ident(ident) if ident == "poco");
        let is_bang =
            matches!(tokens.get(index + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!');

        if is_poco
            && is_bang
            && let Some(TokenTree::Group(group)) = tokens.get(index + 2)
        {
            if let Some(range) = inner_range(group) {
                out.push(InlineTemplate { body_range: range });
            }
            // The body is template markup, not Rust: do not descend.
            index += 3;
            continue;
        }

        if let TokenTree::Group(group) = &tokens[index] {
            collect(group.stream(), out);
        }
        index += 1;
    }
}

/// Blank everything in `rust_source` that is not inside a `poco!` body,
/// returning `None` when the file holds no inline template.
///
/// This is how a Rust file becomes something the `.poco` toolchain can read.
/// Masking rather than slicing is deliberate: **line numbers and columns are
/// preserved**, so a tool can run its existing template passes over the result
/// and report positions that land on the real `.rs` file. Because only body
/// text survives, ordinary Rust — string literals especially — can never be
/// mistaken for template content.
///
/// Blanking is per character and sized in **UTF-16 code units**, which is the
/// unit LSP columns are counted in. Padding by UTF-8 bytes instead would widen
/// every non-ASCII character (`é` is two bytes but one column) and shift the
/// reported column of any template later on that line.
pub fn mask_non_template_text(rust_source: &str) -> Option<String> {
    let hits = scan_inline_templates(rust_source);
    if hits.is_empty() {
        return None;
    }

    let mut masked = String::with_capacity(rust_source.len());
    let mut cursor = 0;
    // `scan_inline_templates` returns hits in source order.
    for hit in hits {
        blank_into(&mut masked, &rust_source[cursor..hit.body_range.start]);
        masked.push_str(&rust_source[hit.body_range.clone()]);
        cursor = hit.body_range.end;
    }
    blank_into(&mut masked, &rust_source[cursor..]);
    Some(masked)
}

/// Append `text` as blanks: newlines survive so line numbers hold, and every
/// other character becomes as many spaces as it occupies UTF-16 code units so
/// columns hold too.
fn blank_into(out: &mut String, text: &str) {
    for character in text.chars() {
        if character == '\n' {
            out.push('\n');
        } else {
            for _ in 0..character.len_utf16() {
                out.push(' ');
            }
        }
    }
}

/// Byte range strictly inside a group's delimiters.
///
/// `Group::span()` covers the delimiters too, so trim one byte from each end.
/// Both delimiters are ASCII, which keeps that arithmetic on char boundaries.
fn inner_range(group: &proc_macro2::Group) -> Option<Range<usize>> {
    let outer = group.span().byte_range();
    if outer.end < outer.start + 2 {
        return None;
    }
    Some((outer.start + 1)..(outer.end - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bodies(source: &str) -> Vec<String> {
        scan_inline_templates(source)
            .iter()
            .map(|hit| hit.text(source).to_string())
            .collect()
    }

    #[test]
    fn finds_a_body_in_expression_position() {
        let source = r#"fn main() { let t = poco! { <div>x</div> }; }"#;
        assert_eq!(bodies(source), vec![" <div>x</div> "]);
    }

    #[test]
    fn finds_a_body_in_component_attribute_position() {
        let source = r#"
#[component(name = "card", template = poco! { <div class="card"></div> })]
struct Card;
"#;
        assert_eq!(bodies(source), vec![r#" <div class="card"></div> "#]);
    }

    #[test]
    fn finds_a_path_qualified_invocation() {
        let source = r#"const T: PocoTemplate = pocopine::poco! { <p>hi</p> };"#;
        assert_eq!(bodies(source), vec![" <p>hi</p> "]);
    }

    #[test]
    fn byte_range_maps_back_onto_the_source() {
        let source = r#"fn main() { let t = poco! { <b>x</b> }; }"#;
        let hit = &scan_inline_templates(source)[0];
        // The offset is usable directly as a file position.
        assert_eq!(&source[hit.body_range.clone()], " <b>x</b> ");
        assert!(source[..hit.body_range.start].ends_with('{'));
    }

    #[test]
    fn finds_every_body_in_source_order() {
        let source = r#"
fn a() { let _ = poco! { <i>1</i> }; }
mod inner { pub fn b() { let _ = poco! { <i>2</i> }; } }
"#;
        assert_eq!(bodies(source), vec![" <i>1</i> ", " <i>2</i> "]);
    }

    #[test]
    fn ignores_commented_out_and_stringified_invocations() {
        // The reason this scan is token-based: neither of these is a macro
        // invocation, and a textual search would report both.
        let source = r#"
// let _ = poco! { <div>commented</div> };
fn a() { let _ = "poco! { <div>quoted</div> }"; }
"#;
        assert!(scan_inline_templates(source).is_empty());
    }

    #[test]
    fn unlexable_source_yields_nothing_rather_than_erroring() {
        // rustc owns reporting this; a second diagnostic would be noise.
        assert!(scan_inline_templates("fn main() { let x = \" }").is_empty());
    }

    #[test]
    fn other_macros_are_not_mistaken_for_templates() {
        let source = r#"fn main() { println!("{}", 1); vec![1, 2]; }"#;
        assert!(scan_inline_templates(source).is_empty());
    }

    #[test]
    fn masking_returns_none_without_a_template() {
        assert!(mask_non_template_text(r#"fn main() { println!("hi"); }"#).is_none());
    }

    #[test]
    fn masking_keeps_only_template_text() {
        let source = r#"#[component(template = poco! { <div class="p-4"></div> })] struct C;"#;
        let masked = mask_non_template_text(source).expect("one body");

        assert!(masked.contains(r#"<div class="p-4"></div>"#));
        assert!(!masked.contains("struct"));
        assert!(!masked.contains("component"));
    }

    /// `(line, UTF-16 column)` of `needle` — the coordinates LSP reports in.
    fn line_and_column(text: &str, needle: &str) -> (usize, usize) {
        let at = text.find(needle).expect("needle present");
        let line = text[..at].matches('\n').count();
        let line_start = text[..at].rfind('\n').map(|nl| nl + 1).unwrap_or(0);
        (line, text[line_start..at].encode_utf16().count())
    }

    #[test]
    fn masking_preserves_lines_and_columns() {
        // The whole point: a position in the masked text is a position in the
        // real file, so tools need no coordinate arithmetic.
        let source = "// héllo ünicode\nfn a() { let _ = poco! { <b>x</b> }; }\n";
        let masked = mask_non_template_text(source).expect("one body");

        assert_eq!(masked.lines().count(), source.lines().count());
        assert_eq!(
            line_and_column(&masked, "<b>"),
            line_and_column(source, "<b>")
        );
    }

    #[test]
    fn multibyte_rust_before_a_template_does_not_shift_its_column() {
        // `é` is two UTF-8 bytes but one UTF-16 column. Padding by bytes would
        // push the template a column right of where it is, and every
        // diagnostic inside it along with it.
        let source = r#"const N: &str = "héllo"; const T: X = poco! { <b>x</b> };"#;
        let masked = mask_non_template_text(source).expect("one body");

        assert_eq!(
            line_and_column(&masked, "<b>"),
            line_and_column(source, "<b>")
        );
    }

    #[test]
    fn astral_characters_keep_their_two_column_width() {
        // An emoji is a single `char` but two UTF-16 code units, so blanking
        // it as one space would shift the other way.
        let source = r#"const N: &str = "🎉"; const T: X = poco! { <b>x</b> };"#;
        let masked = mask_non_template_text(source).expect("one body");

        assert_eq!(
            line_and_column(&masked, "<b>"),
            line_and_column(source, "<b>")
        );
    }

    #[test]
    fn masking_covers_every_body_in_the_file() {
        let source =
            "fn a() { let _ = poco! { <i>1</i> }; }\nfn b() { let _ = poco! { <i>2</i> }; }";
        let masked = mask_non_template_text(source).expect("two bodies");

        assert!(masked.contains("<i>1</i>"));
        assert!(masked.contains("<i>2</i>"));
        assert!(!masked.contains("fn"));
    }
}
