//! RFC 049 — `uses = [...]` list parsing for consumer
//! `#[component]` invocations.
//!
//! Consumer components opt into slot-contract validation by
//! listing the component types they reference in their own
//! `.poco` template. The macro parses the list into a local
//! `tag → TypePath` table that the consumer-side template scan
//! (task #14) uses to resolve child tags.
//!
//! Forms accepted:
//!
//! * **Bare ident** — `PineContextMenuItem`. The macro derives
//!   the tag string from the ident via the same kebab-casing
//!   rule `#[component]` uses by default
//!   (`PineContextMenuItem` → `"pine-context-menu-item"`).
//! * **Explicit pair** — `(OddlyNamed, "my-custom-tag")`.
//!   Needed when the component declared a custom `name = "..."`
//!   that doesn't match the default kebab-case rule.
//!
//! Mixing both in one `uses = [...]` list is allowed.
//!
//! Duplicate tag mappings are rejected at macro expansion time
//! per RFC 049 §4.7 — two entries resolving to the same tag
//! string would make slot checks nondeterministic.

use syn::{Expr, ExprLit, ExprPath, Lit, LitStr, Path, spanned::Spanned};

/// One entry in a `uses = [...]` list, before tag-string
/// resolution.
#[derive(Debug, Clone)]
pub(crate) enum UsesEntry {
    /// `PineContextMenuItem` — tag derived by default kebab.
    Bare(Path),
    /// `(OddlyNamed, "my-custom-tag")` — explicit tag override.
    Explicit(Path, LitStr),
}

/// A resolved `tag-string → TypePath` table, local to one
/// consumer `#[component]`.
///
/// Construction validates uniqueness: two entries mapping to
/// the same tag string are a hard compile error (per RFC 049
/// §4.7 step 3).
#[derive(Debug, Clone, Default)]
pub(crate) struct UsesTable {
    /// `(tag, type_path)` pairs in source order.
    pub entries: Vec<(String, Path)>,
}

impl UsesTable {
    /// Look up a tag string in the table. Returns the matching
    /// Rust type path, if any.
    #[allow(dead_code)] // consumed by task #14
    pub fn lookup(&self, tag: &str) -> Option<&Path> {
        self.entries
            .iter()
            .find_map(|(t, p)| (t == tag).then_some(p))
    }
}

/// RFC 060 Tier 3 — parse the `extends = [...]` value of a
/// bundle's `#[component]` attribute. Bundles re-export a fixed
/// set of component types; the list is plain type paths only —
/// no kebab strings, no tag overrides, no resolution table.
/// Caller validates non-empty + mutual exclusion with
/// the `template` argument (a path or a `poco!` body).
pub(crate) fn parse_extends_array(value: Expr) -> syn::Result<Vec<Path>> {
    let Expr::Array(arr) = value else {
        return Err(syn::Error::new(
            value.span(),
            "`extends` expects an array literal — `extends = [PineDialogRoot, PineDialogTrigger]`",
        ));
    };
    let mut out = Vec::with_capacity(arr.elems.len());
    for elem in arr.elems {
        match elem {
            Expr::Path(ExprPath { path, .. }) => out.push(path),
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "`extends` entries must be plain type paths — \
                     `extends = [TypeA, TypeB]`",
                ));
            }
        }
    }
    Ok(out)
}

/// Parse a `uses = [...]` value — specifically the `[...]`
/// part — from an `Expr::Array`. Returns the raw entries; use
/// [`resolve_uses`] to turn them into a `UsesTable`.
pub(crate) fn parse_uses_array(value: Expr) -> syn::Result<Vec<UsesEntry>> {
    let Expr::Array(arr) = value else {
        return Err(syn::Error::new(
            value.span(),
            "`uses` expects an array literal — `uses = [PineFoo, (Named, \"tag\")]`",
        ));
    };
    let mut out = Vec::with_capacity(arr.elems.len());
    for elem in arr.elems {
        match elem {
            Expr::Path(ExprPath { path, .. }) => {
                out.push(UsesEntry::Bare(path));
            }
            Expr::Tuple(tup) => {
                if tup.elems.len() != 2 {
                    return Err(syn::Error::new(
                        tup.span(),
                        "`uses` tuple entries must be `(TypePath, \"tag-string\")` — exactly two elements",
                    ));
                }
                let mut iter = tup.elems.into_iter();
                let ty_expr = iter.next().expect("len checked above");
                let tag_expr = iter.next().expect("len checked above");
                let ty = match ty_expr {
                    Expr::Path(ExprPath { path, .. }) => path,
                    other => {
                        return Err(syn::Error::new(
                            other.span(),
                            "first tuple element must be a type path",
                        ));
                    }
                };
                let tag = match tag_expr {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => s,
                    other => {
                        return Err(syn::Error::new(
                            other.span(),
                            "second tuple element must be a string literal",
                        ));
                    }
                };
                out.push(UsesEntry::Explicit(ty, tag));
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "`uses` entries must be a type path (e.g. `PineFoo`) \
                     or a tuple `(TypePath, \"tag-string\")`",
                ));
            }
        }
    }
    Ok(out)
}

/// Resolve raw `UsesEntry` items into a `UsesTable`, applying
/// the default kebab rule to bare entries and validating
/// uniqueness.
pub(crate) fn resolve_uses(entries: Vec<UsesEntry>) -> syn::Result<UsesTable> {
    let mut table = UsesTable::default();
    for entry in entries {
        let (tag, type_path, anchor_span) = match entry {
            UsesEntry::Bare(path) => {
                let ident = path
                    .segments
                    .last()
                    .ok_or_else(|| {
                        syn::Error::new(
                            path.span(),
                            "bare `uses` entry must be a non-empty type path",
                        )
                    })?
                    .ident
                    .to_string();
                let tag = crate::kebab_case(&ident);
                let span = path.span();
                (tag, path, span)
            }
            UsesEntry::Explicit(path, lit) => {
                let tag = lit.value();
                if tag.is_empty() {
                    return Err(syn::Error::new(
                        lit.span(),
                        "`uses` tag override must not be empty",
                    ));
                }
                let span = lit.span();
                (tag, path, span)
            }
        };

        if let Some(existing_pos) = table.entries.iter().position(|(t, _)| t == &tag) {
            let _ = existing_pos;
            return Err(syn::Error::new(
                anchor_span,
                format!(
                    "duplicate `uses` entry for tag `{tag}` — \
                     slot checks would be nondeterministic"
                ),
            ));
        }
        table.entries.push((tag, type_path));
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn parse_expr(src: proc_macro2::TokenStream) -> Expr {
        syn::parse2(src).expect("expected parseable Expr")
    }

    #[test]
    fn bare_entries_use_default_kebab() {
        let expr = parse_expr(quote! { [PineContextMenuItem, PineFoo] });
        let entries = parse_uses_array(expr).unwrap();
        let table = resolve_uses(entries).unwrap();
        assert_eq!(table.entries.len(), 2);
        assert!(table.lookup("pine-context-menu-item").is_some());
        assert!(table.lookup("pine-foo").is_some());
    }

    #[test]
    fn explicit_tuple_overrides_tag() {
        let expr = parse_expr(quote! { [(OddlyNamed, "my-custom-tag")] });
        let entries = parse_uses_array(expr).unwrap();
        let table = resolve_uses(entries).unwrap();
        assert!(table.lookup("my-custom-tag").is_some());
        // The kebab-derived tag for `OddlyNamed` is NOT in the
        // table — the explicit override is what landed.
        assert!(table.lookup("oddly-named").is_none());
    }

    #[test]
    fn mixed_bare_and_explicit() {
        let expr = parse_expr(quote! {
            [PineFoo, (OddlyNamed, "my-custom-tag"), PineBar]
        });
        let entries = parse_uses_array(expr).unwrap();
        let table = resolve_uses(entries).unwrap();
        assert_eq!(table.entries.len(), 3);
        assert!(table.lookup("pine-foo").is_some());
        assert!(table.lookup("my-custom-tag").is_some());
        assert!(table.lookup("pine-bar").is_some());
    }

    #[test]
    fn duplicate_tag_is_rejected() {
        // Both bare entries resolve to "pine-foo".
        let expr = parse_expr(quote! { [PineFoo, (Different, "pine-foo")] });
        let entries = parse_uses_array(expr).unwrap();
        let err = resolve_uses(entries);
        assert!(err.is_err(), "duplicate tag must be rejected");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("duplicate"),
            "expected 'duplicate' in error: {msg}"
        );
    }

    #[test]
    fn empty_explicit_tag_rejected() {
        let expr = parse_expr(quote! { [(Foo, "")] });
        let entries = parse_uses_array(expr).unwrap();
        let err = resolve_uses(entries);
        assert!(err.is_err(), "empty tag must be rejected");
    }

    #[test]
    fn non_array_value_rejected() {
        let expr = parse_expr(quote! { 42 });
        let err = parse_uses_array(expr);
        assert!(err.is_err());
    }

    #[test]
    fn bad_tuple_shape_rejected() {
        let expr = parse_expr(quote! { [(OnlyOneElement,)] });
        let entries = parse_uses_array(expr);
        assert!(entries.is_err(), "1-element tuple must be rejected");

        let expr = parse_expr(quote! { [(A, B, C)] });
        let entries = parse_uses_array(expr);
        assert!(entries.is_err(), "3-element tuple must be rejected");
    }

    #[test]
    fn non_string_tag_rejected() {
        let expr = parse_expr(quote! { [(Foo, 42)] });
        let err = parse_uses_array(expr);
        assert!(err.is_err(), "non-string tag must be rejected");
    }

    #[test]
    fn preserves_source_order() {
        let expr = parse_expr(quote! { [PineZeta, PineAlpha, PineMu] });
        let entries = parse_uses_array(expr).unwrap();
        let table = resolve_uses(entries).unwrap();
        let tags: Vec<_> = table.entries.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(tags, vec!["pine-zeta", "pine-alpha", "pine-mu"]);
    }
}
