//! Predefined typography input rules. Mirrors
//! `prosemirror-inputrules/src/rules.ts`.
//!
//! These rules are pure character substitutions — em-dash, ellipsis,
//! and the four smart-quote variants (open/close × single/double).
//! Use them via [`crate::extensions::SmartTypographyExtension`] (which
//! contributes them as a bundle) or pick individual rules a la carte.
//!
//! Every rule's `in_code_mark` policy is `Disallow` so they don't
//! fire inside an inline `<code>...</code>` span — preserving
//! literal punctuation in code is the documented contract upstream.

use regex::Regex;

use super::rule::{InCodePolicy, InputRule};

/// `--` → `—` (em-dash).
pub fn em_dash() -> InputRule {
    InputRule::replacement(Regex::new(r"--$").unwrap(), "—").in_code_mark(InCodePolicy::Disallow)
}

/// `...` → `…` (ellipsis).
pub fn ellipsis() -> InputRule {
    InputRule::replacement(Regex::new(r"\.\.\.$").unwrap(), "…")
        .in_code_mark(InCodePolicy::Disallow)
}

/// Opening `"` after start-of-line / whitespace / paren-style →
/// `“`. Mirrors upstream's `openDoubleQuote`. Rust's regex doesn't
/// need (or accept) escaped braces / brackets / parens inside a
/// character class, so the upstream `\{\[\(\<` becomes a bare
/// `{[(<` here.
pub fn open_double_quote() -> InputRule {
    InputRule::replacement(
        Regex::new("(?:^|[\\s{\\[(<'\"\u{2018}\u{201C}])(\")$").unwrap(),
        "\u{201C}", // “
    )
    .in_code_mark(InCodePolicy::Disallow)
}

/// Closing `"` everywhere else → `”`. Pattern is intentionally a
/// catch-all so any unmatched `"` becomes a closing quote when it
/// can't match the opening pattern. Mirrors upstream's
/// `closeDoubleQuote`.
pub fn close_double_quote() -> InputRule {
    InputRule::replacement(Regex::new(r#""$"#).unwrap(), "\u{201D}")
        .in_code_mark(InCodePolicy::Disallow)
}

/// Opening `'` → `‘`. Mirrors `openSingleQuote`.
pub fn open_single_quote() -> InputRule {
    InputRule::replacement(
        Regex::new("(?:^|[\\s{\\[(<'\u{2018}\u{201C}])(')$").unwrap(),
        "\u{2018}", // ‘
    )
    .in_code_mark(InCodePolicy::Disallow)
}

/// Closing `'` → `’`. Mirrors `closeSingleQuote`.
pub fn close_single_quote() -> InputRule {
    InputRule::replacement(Regex::new(r"'$").unwrap(), "\u{2019}")
        .in_code_mark(InCodePolicy::Disallow)
}

/// Bundle of smart-quote rules in PM's canonical order: opens before
/// closes (the opens have more specific patterns so they're checked
/// first; closes are the catch-all fallback). Mirrors upstream's
/// `smartQuotes` array.
pub fn smart_quotes() -> [InputRule; 4] {
    [
        open_double_quote(),
        close_double_quote(),
        open_single_quote(),
        close_single_quote(),
    ]
}
