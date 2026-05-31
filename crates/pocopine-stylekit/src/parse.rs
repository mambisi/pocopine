//! Class-string parser for the Tailwind-shaped grammar (RFC 092 D3).
//!
//! Grammar (informal):
//!
//! ```text
//! class    := variant* base
//! variant  := IDENT ':'                      // hover:  md:  focus-visible:
//!           | "data-[" .. "]" ':'            // data-[state=on]:
//!           | "aria-[" .. "]" ':'
//! base     := IDENT                           // flex, items-center
//!           | IDENT '-[' .. ']'              // w-[12px], text-[13px], grid-cols-[…]
//! ```
//!
//! The parser is purely syntactic: it splits variants from the base and
//! peels an arbitrary `[…]` value. Whether the base names a real
//! utility family, and whether the arbitrary value is type-correct, is
//! the [`crate::registry`]'s job.

use crate::diagnostics::Diagnostic;

/// A leading variant, e.g. `hover`, `md`, or `data-[state=on]`. Stored
/// verbatim (without the trailing `:`); the registry maps it to a
/// selector/at-rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant(pub String);

/// A parsed class string: ordered variants + base utility, with the
/// arbitrary `[…]` value peeled off when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedClass {
    pub variants: Vec<Variant>,
    /// Base utility name without any arbitrary value, e.g. `w`, `flex`,
    /// `items-center`, `grid-cols`.
    pub base: String,
    /// Inner text of an arbitrary `[…]` value, if any (e.g. `12px`).
    pub arbitrary: Option<String>,
}

/// Split a class string into its variants and base, peeling an
/// arbitrary value. Bracket contents are treated opaquely so `:` and
/// `-` inside `[…]` (e.g. `grid-cols-[minmax(0,1fr)_80px]`) don't
/// confuse variant/base splitting.
pub fn parse_class(input: &str) -> Result<ParsedClass, Diagnostic> {
    if input.is_empty() {
        return Err(Diagnostic::error("empty class"));
    }

    let mut variants = Vec::new();
    let mut rest = input;

    // Peel `variant:` prefixes, but never split on a `:` inside `[…]`.
    while let Some(colon) = top_level_colon(rest) {
        let (head, tail) = rest.split_at(colon);
        variants.push(Variant(head.to_string()));
        rest = &tail[1..]; // skip the ':'
    }

    if rest.is_empty() {
        return Err(Diagnostic::error(format!(
            "class `{input}` has a trailing variant but no utility"
        )));
    }

    // Peel a trailing arbitrary value: either `[…]` (explicit) or the
    // Tailwind v4 `(--var)` CSS-variable shorthand. `bg-(--brand)` is
    // sugar for `bg-[var(--brand)]`; `text-(length:--x)` keeps the type
    // hint as `[length:var(--x)]`.
    let (mut base, mut arbitrary) =
        if let Some(open) = rest.find('[').filter(|_| rest.ends_with(']')) {
            let inner = &rest[open + 1..rest.len() - 1];
            // Drop the connecting '-' before '[' from the base, if any.
            let base = rest[..open].strip_suffix('-').unwrap_or(&rest[..open]);
            (base.to_string(), Some(inner.to_string()))
        } else if let Some(open) = rest.rfind('(').filter(|_| rest.ends_with(')')) {
            let inner = &rest[open + 1..rest.len() - 1];
            match var_shorthand(inner) {
                Some(value) => {
                    let base = rest[..open].strip_suffix('-').unwrap_or(&rest[..open]);
                    (base.to_string(), Some(value))
                }
                // Not a `(--var)` form — leave it for the registry to reject.
                None => (rest.to_string(), None),
            }
        } else {
            (rest.to_string(), None)
        };

    // `color/[alpha]` / `color/(--a)` — the bracket is the *opacity
    // modifier's* value, not a base arbitrary, so the peel above left a
    // trailing `/`. Fold it back to `color/alpha` for the colour resolver.
    if base.ends_with('/') {
        if let Some(a) = arbitrary.take() {
            base.push_str(&a);
        }
    }

    Ok(ParsedClass {
        variants,
        base,
        arbitrary,
    })
}

/// Expand a `(…)` CSS-variable shorthand to the equivalent arbitrary
/// value, or `None` if it isn't one. `--brand` → `var(--brand)`. A typed
/// shorthand (`length:--x`) keeps only the variable — Stylekit routes
/// arbitrary values by the base utility, not the type hint, so emitting
/// the hint would produce invalid CSS.
fn var_shorthand(inner: &str) -> Option<String> {
    let name = inner.rsplit(':').next().unwrap_or(inner);
    let var = name.strip_prefix("--").filter(|r| !r.is_empty())?;
    Some(format!("var(--{var})"))
}

/// Find the first `:` that is not nested inside `[…]` or `(…)` (so the
/// `:` in a `data-[state=on]:` value or a `(length:--x)` shorthand
/// doesn't get mistaken for a variant separator).
fn top_level_colon(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedClass {
        parse_class(s).unwrap()
    }

    #[test]
    fn plain_utility() {
        let c = p("items-center");
        assert!(c.variants.is_empty());
        assert_eq!(c.base, "items-center");
        assert_eq!(c.arbitrary, None);
    }

    #[test]
    fn variant_chain() {
        let c = p("hover:focus-visible:bg-surface");
        assert_eq!(
            c.variants,
            vec![Variant("hover".into()), Variant("focus-visible".into())]
        );
        assert_eq!(c.base, "bg-surface");
    }

    #[test]
    fn arbitrary_value() {
        let c = p("text-[13px]");
        assert_eq!(c.base, "text");
        assert_eq!(c.arbitrary.as_deref(), Some("13px"));
    }

    #[test]
    fn arbitrary_with_inner_punctuation() {
        let c = p("grid-cols-[minmax(0,1fr)_80px]");
        assert_eq!(c.base, "grid-cols");
        assert_eq!(c.arbitrary.as_deref(), Some("minmax(0,1fr)_80px"));
    }

    #[test]
    fn data_variant_then_arbitrary() {
        let c = p("data-[state=on]:bg-accent");
        assert_eq!(c.variants, vec![Variant("data-[state=on]".into())]);
        assert_eq!(c.base, "bg-accent");
    }

    #[test]
    fn css_variable_shorthand() {
        // `bg-(--brand)` ≡ `bg-[var(--brand)]`.
        let c = p("bg-(--brand)");
        assert_eq!(c.base, "bg");
        assert_eq!(c.arbitrary.as_deref(), Some("var(--brand)"));
        // Typed shorthand keeps only the variable.
        let c = p("text-(length:--size)");
        assert_eq!(c.base, "text");
        assert_eq!(c.arbitrary.as_deref(), Some("var(--size)"));
        // Works under a variant.
        let c = p("hover:w-(--w)");
        assert_eq!(c.variants, vec![Variant("hover".into())]);
        assert_eq!(c.base, "w");
        assert_eq!(c.arbitrary.as_deref(), Some("var(--w)"));
        // A `(…)` that isn't a var shorthand is not treated as arbitrary.
        let c = p("foo-(bar)");
        assert_eq!(c.base, "foo-(bar)");
        assert_eq!(c.arbitrary, None);
    }
}
