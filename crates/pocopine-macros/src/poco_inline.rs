//! RFC-116 — the `poco!` inline-template macro.
//!
//! The body is bare HTML tokens. The macro never *interprets* them: it
//! recovers the verbatim source text spanned by the body and hands that to
//! the same `pocopine-template-parser` that reads `.poco` files, so an inline
//! template and a file template are the same string by the time anything
//! downstream sees them.
//!
//! The one transformation applied is **quoted-text unquoting** (RFC-116
//! "Quoted text"). Rust's lexer rejects a lot of ordinary prose — `don't`,
//! `©`, `—`, emoji — and it does so *before* any proc macro runs, so the
//! escape hatch has to be something that lexes: a string literal, which is a
//! single opaque token. A literal in text position is decoded and
//! HTML-escaped into the template as static text.
//!
//! ```ignore
//! poco! { <p>"Don't stop — © 2026"</p> }   //  →  <p>Don't stop — © 2026</p>
//! ```

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Lit, LitStr};

/// Verbatim body source plus the anchor span diagnostics should point at.
pub(crate) struct RecoveredTemplate {
    pub(crate) source: String,
    /// Anchor for `syn::Error`. Points at the body, so template errors
    /// squiggle the HTML rather than the macro name.
    pub(crate) span: Span,
    /// Set when span provenance was unavailable and `source` is a lossy
    /// token re-print (rust-analyzer speculative expansion). Callers skip
    /// validation and emit a warning instead of hard-failing a build that
    /// rustc itself would accept.
    pub(crate) lossy: bool,
}

/// Recover the verbatim source of a `poco!` body.
///
/// `Err` carries a ready-to-emit `compile_error!`.
pub(crate) fn recover(input: TokenStream) -> Result<RecoveredTemplate, syn::Error> {
    let tokens: Vec<proc_macro::TokenTree> = input.into_iter().collect();

    let (first, last) = match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => (first, last),
        _ => {
            return Err(syn::Error::new(
                Span::call_site(),
                "poco!: empty template — the macro body must contain a template, \
                 e.g. `poco! { <div>…</div> }`",
            ));
        }
    };

    let anchor = Span::from(first.span());

    // Tier 1: real cargo builds. `Span::join` + `source_text` hand back the
    // author's bytes exactly — whitespace, indentation, `{{ }}` and all.
    let joined = first.span().join(last.span());
    let Some(source) = joined.and_then(|span| span.source_text()) else {
        // Tier 2: rust-analyzer runs speculative expansions with no span
        // provenance. Keep the expansion buildable with a lossy re-print;
        // a real build always takes tier 1, so this never ships.
        let lossy = tokens
            .into_iter()
            .map(|token| token.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        return Ok(RecoveredTemplate {
            source: lossy,
            span: anchor,
            lossy: true,
        });
    };

    let base = joined
        .map(|span| span.byte_range().start)
        .expect("join succeeded above");

    Ok(RecoveredTemplate {
        source: unquote_text_literals(&tokens, &source, base),
        span: anchor,
        lossy: false,
    })
}

/// Replace string literals sitting in **text** position with their decoded,
/// HTML-escaped contents.
///
/// Positional rule, evaluated on tokens rather than parsed HTML:
///
///   * a literal preceded by `=` is an attribute value (`class="card"`) and
///     is left exactly as written;
///   * literals nested inside a group are `{{ }}` interpolation and belong to
///     `pocopine-expr`, so this walk never descends;
///   * anything else at the top level is text.
fn unquote_text_literals(tokens: &[proc_macro::TokenTree], source: &str, base: usize) -> String {
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut prev_is_eq = false;

    for token in tokens {
        match token {
            proc_macro::TokenTree::Literal(literal) => {
                if !prev_is_eq {
                    // `syn` owns literal decoding: escapes, raw strings, and
                    // unicode escapes all resolve here rather than in a
                    // hand-rolled unescaper.
                    let parsed: Result<Lit, _> = syn::parse_str(&literal.to_string());
                    if let Ok(Lit::Str(text)) = parsed {
                        let range = literal.span().byte_range();
                        if range.start >= base && range.end <= base + source.len() {
                            edits.push((
                                (range.start - base)..(range.end - base),
                                html_escape_text(&text.value()),
                            ));
                        }
                    }
                }
                prev_is_eq = false;
            }
            proc_macro::TokenTree::Punct(punct) => {
                prev_is_eq = punct.as_char() == '=';
            }
            // Groups are `{{ }}` interpolation — expression territory.
            _ => prev_is_eq = false,
        }
    }

    if edits.is_empty() {
        return source.to_string();
    }

    // Apply back-to-front so earlier ranges stay valid.
    let mut out = source.to_string();
    for (range, replacement) in edits.into_iter().rev() {
        if out.is_char_boundary(range.start) && out.is_char_boundary(range.end) {
            out.replace_range(range, &replacement);
        }
    }
    out
}

/// Escape a decoded literal for text position. Quotes are deliberately left
/// alone: they are legal in text and escaping them would surprise authors who
/// quoted a run precisely to avoid thinking about entities.
fn html_escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// Body of the `#[proc_macro] poco` entry point.
pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let recovered = match recover(input) {
        Ok(recovered) => recovered,
        Err(error) => return error.to_compile_error().into(),
    };

    let warnings = match validate(&recovered, "poco!") {
        Ok(warnings) => warnings,
        Err(tokens) => return tokens,
    };

    let literal = proc_macro2::Literal::string(&recovered.source);
    quote! {
        {
            #warnings
            ::pocopine::__private::PocoTemplate::__new(#literal)
        }
    }
    .into()
}

/// Parse the recovered source with the `.poco` parser and render failures as
/// `annotate-snippets` blocks, matching file-template diagnostics.
///
/// Standalone `poco!` deliberately does **not** enforce the single-root rule:
/// the value may be a fragment, and root shape is the consuming component's
/// contract.
pub(crate) fn validate(
    recovered: &RecoveredTemplate,
    display_name: &str,
) -> Result<TokenStream2, TokenStream> {
    if recovered.lossy {
        return Ok(crate::build_warning_tokens(
            "pocopine: template validation was skipped because macro span \
             provenance was unavailable (speculative expansion). A normal \
             cargo build validates this template.",
        ));
    }

    let display_path = format!("<inline template in {display_name}>");
    match crate::template_parser::parse_strict(&recovered.source, &display_path) {
        Ok(_) => Ok(TokenStream2::new()),
        Err(parse_errors) => {
            let mut blocks: Vec<String> = Vec::new();
            for error in parse_errors {
                let in_range = error.byte_range.end > error.byte_range.start
                    && error.byte_range.end <= recovered.source.len();
                blocks.push(if in_range {
                    crate::diagnostics::render_template_error(
                        &recovered.source,
                        &display_path,
                        error.byte_range,
                        &format!("pocopine: invalid inline template — {}", error.message),
                        &error.message,
                    )
                } else {
                    crate::diagnostics::render_fileless_error(
                        &display_path,
                        "pocopine: invalid inline template",
                        &error.message,
                    )
                });
            }
            let rendered = blocks.join("\n\n");
            if crate::is_lenient_mode() {
                Ok(crate::build_warning_tokens(&rendered))
            } else {
                Err(syn::Error::new(recovered.span, rendered)
                    .to_compile_error()
                    .into())
            }
        }
    }
}

/// Synthesize the `LitStr` the `#[component]` pipeline consumes, carrying the
/// body span so downstream diagnostics land on the HTML.
pub(crate) fn as_component_literal(recovered: &RecoveredTemplate) -> LitStr {
    LitStr::new(&recovered.source, recovered.span)
}
