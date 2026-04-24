//! RFC 049 — consumer-side template scan and slot-contract
//! assertion emission.
//!
//! The consumer's `#[component]` macro runs this after parsing
//! its `.poco` template to a [`TemplateAst`]. For every parent-
//! tag usage (tag resolvable against the consumer's `uses`
//! list), the scan emits one trait-bound assertion per direct
//! child that's also in `uses`. rustc's trait-solver does the
//! actual rejection work — when the child's type doesn't impl
//! the parent's slot-contract marker trait, the unimplemented-
//! trait error (customised via `#[diagnostic::on_unimplemented]`
//! on the parent's trait emission — see `slot.rs`) fires at the
//! assertion call site.
//!
//! ### What v1 does
//!
//! - Emits `const _: fn() = || { fn assert_child<T: ParentDefaultChild>() {}
//!   assert_child::<ChildType>(); };` for each (parent, child)
//!   pair where both tags resolve via the `uses` list.
//! - Skips HTML tags and unknown custom tags silently (per RFC
//!   049 §4.3 rules 2 and 3).
//! - Recurses through the whole template tree — nested typed
//!   parents get their own assertion blocks.
//! - Handles named slots via `<template pp-slot="NAME">` wrapper
//!   detection per RFC 049 §4.5.
//!
//! ### What v1 does NOT do yet
//!
//! - **HTML-wrapper rejection in `only` mode.** RFC 049 §4.3
//!   rule 4 says strict slots should reject `<div>` / `<span>`
//!   direct children. That requires a cross-crate signal from
//!   parent to consumer (whether the slot was declared with
//!   `accepts` vs `only`). Open for a follow-up RFC; tracked in
//!   §8 open questions.
//! - **`.poco`-line-anchored error rendering via `annotate-
//!   snippets`.** The trait-based rejection points at the
//!   consumer's `#[component]` attribute, not at the offending
//!   `.poco` line. RFC 049 §4.6 plans layered rendering once
//!   the trait half is proven. This commit establishes the
//!   trait half.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Path;

use crate::template_parser::{Element, Node, TemplateAst};
use crate::uses::UsesTable;

/// Walk `ast` alongside `uses` and emit slot-contract
/// assertions for every typed-parent / direct-child pair.
///
/// Returns an empty `TokenStream` when `uses` is empty or the
/// template contains no recognised typed-parent tags.
pub(crate) fn emit_slot_assertions(
    ast: &TemplateAst,
    uses: &UsesTable,
) -> TokenStream {
    if uses.entries.is_empty() {
        return TokenStream::new();
    }

    let mut out = TokenStream::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, uses, &mut out);
        }
    }
    out
}

/// Recursively visit an element, checking each direct child
/// against the parent's slot contract (if the parent is a
/// typed compound in `uses`) and then recursing into every
/// child to keep nested parents in scope.
fn walk(el: &Element, uses: &UsesTable, out: &mut TokenStream) {
    if el.synthetic {
        // Synthetic elements (html5ever auto-inserts like
        // `<tbody>`) have no authored meaning — skip them but
        // still recurse through their children, since the
        // authored descendants live there.
        for child in &el.children {
            if let Node::Element(child_el) = child {
                walk(child_el, uses, out);
            }
        }
        return;
    }

    // Is this element a typed compound the consumer knows about?
    if let Some(parent_path) = uses.lookup(&el.tag) {
        emit_assertions_for_parent(el, parent_path, uses, out);
    }

    // Recurse regardless so nested typed parents get their own
    // assertion blocks.
    for child in &el.children {
        if let Node::Element(child_el) = child {
            walk(child_el, uses, out);
        }
    }
}

/// For a parent element resolved via `uses`, emit one
/// assertion per direct child whose tag is also in `uses`.
/// Handles `<template pp-slot="NAME">` wrappers per RFC 049 §4.5.
fn emit_assertions_for_parent(
    parent: &Element,
    parent_path: &Path,
    uses: &UsesTable,
    out: &mut TokenStream,
) {
    for child in &parent.children {
        let Node::Element(child_el) = child else {
            continue;
        };
        if child_el.synthetic {
            continue;
        }

        // Named-slot wrapper: `<template pp-slot="name">
        // <ActualChild/> </template>` — the actual slotted
        // children are this template's element children. RFC
        // 011 syntax.
        if child_el.tag == "template" {
            if let Some(slot_name) = pp_slot_name(child_el) {
                for slotted in &child_el.children {
                    if let Node::Element(slotted_el) = slotted {
                        emit_one_assertion(
                            parent_path,
                            &SlotSite::Named(slot_name.clone()),
                            slotted_el,
                            uses,
                            out,
                        );
                    }
                }
                continue;
            }
        }

        // Default slot — any non-template-wrapped direct child.
        emit_one_assertion(
            parent_path,
            &SlotSite::Default,
            child_el,
            uses,
            out,
        );
    }
}

enum SlotSite {
    Default,
    Named(String),
}

impl SlotSite {
    /// PascalCase suffix used in the parent's marker-trait
    /// name. Matches the naming scheme `slot::emit_slot_traits`
    /// uses (`<StructIdent><SlotName>Child`).
    fn trait_suffix(&self) -> String {
        match self {
            SlotSite::Default => "Default".to_string(),
            SlotSite::Named(s) => pascal_case(s),
        }
    }
}

fn emit_one_assertion(
    parent_path: &Path,
    site: &SlotSite,
    child_el: &Element,
    uses: &UsesTable,
    out: &mut TokenStream,
) {
    // Only assert when the child tag resolves in `uses` — we
    // need a concrete Rust type to hand the trait solver.
    // Unknown tags (plain HTML, external custom elements not in
    // `uses`) are silently skipped per RFC 049 §4.3 rules 2-3.
    let Some(child_path) = uses.lookup(&child_el.tag) else {
        return;
    };

    let trait_path = build_trait_path(parent_path, site);
    let assertion = quote! {
        #[allow(unused_variables, dead_code, non_snake_case)]
        const _: fn() = || {
            fn __pocopine_assert_child<__T: #trait_path>() {}
            __pocopine_assert_child::<#child_path>();
        };
    };
    out.extend(assertion);
}

/// Build the marker-trait path for a parent/slot pair. Keeps
/// the parent's module path intact; rewrites just the last
/// ident (`Parent`) into `Parent<SlotSuffix>Child`.
fn build_trait_path(parent_path: &Path, site: &SlotSite) -> Path {
    let mut trait_path = parent_path.clone();
    let last = trait_path
        .segments
        .last_mut()
        .expect("uses entry must be a non-empty path");
    let new_ident = format_ident!(
        "{}{}Child",
        last.ident,
        site.trait_suffix(),
    );
    last.ident = new_ident;
    // Drop any generic arguments the caller attached — marker
    // traits we emit are generics-free.
    last.arguments = syn::PathArguments::None;
    trait_path
}

/// Extract `pp-slot="NAME"` value from an element's
/// attributes. Returns `None` if the attribute is missing or
/// has an empty value.
fn pp_slot_name(el: &Element) -> Option<String> {
    for (name, value) in &el.attrs {
        if name == "pp-slot" {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut cap_next = true;
    for c in s.chars() {
        if c == '-' || c == '_' || c == ' ' {
            cap_next = true;
            continue;
        }
        if cap_next {
            out.extend(c.to_uppercase());
            cap_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template_parser::parse;
    use crate::uses::{resolve_uses, UsesEntry};

    fn table(entries: Vec<UsesEntry>) -> UsesTable {
        resolve_uses(entries).unwrap()
    }

    fn bare(path: &str) -> UsesEntry {
        UsesEntry::Bare(syn::parse_str(path).unwrap())
    }

    #[test]
    fn emits_assertion_for_known_child_of_known_parent() {
        let src = r#"<pine-foo><pine-item></pine-item></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![bare("PineFoo"), bare("PineItem")]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("PineFooDefaultChild"),
            "expected parent's DefaultChild trait bound, got:\n{s}"
        );
        assert!(
            s.contains("PineItem"),
            "expected child type in assertion, got:\n{s}"
        );
    }

    #[test]
    fn skips_unknown_child_tags() {
        let src = r#"<pine-foo><pine-random></pine-random></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        // Only PineFoo is in uses — PineRandom is unknown.
        let uses = table(vec![bare("PineFoo")]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            !s.contains("assert_child"),
            "unknown child should emit no assertion, got:\n{s}"
        );
    }

    #[test]
    fn skips_html_children() {
        let src = r#"<pine-foo><div><pine-item></pine-item></div></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        // uses lists pine-foo and pine-item but not div.
        let uses = table(vec![bare("PineFoo"), bare("PineItem")]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        // The `<div>` is the direct child — not in uses, so no
        // assertion against it. `<pine-item>` inside the div is
        // NOT a direct child of <pine-foo>, so it doesn't fire
        // either. This is the "skip silently" behaviour of
        // loose mode (RFC 049 §4.3 rules 2-3).
        assert!(
            !s.contains("assert_child"),
            "wrapped HTML should produce no assertion in v1, got:\n{s}"
        );
    }

    #[test]
    fn named_slot_uses_named_trait() {
        let src = r#"<pine-foo>
            <template pp-slot="header">
                <pine-title></pine-title>
            </template>
            <pine-item></pine-item>
        </pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![
            bare("PineFoo"),
            bare("PineItem"),
            bare("PineTitle"),
        ]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("PineFooHeaderChild"),
            "expected named-slot trait, got:\n{s}"
        );
        assert!(
            s.contains("PineFooDefaultChild"),
            "expected default-slot trait for unwrapped child, got:\n{s}"
        );
        assert!(s.contains("PineTitle"));
        assert!(s.contains("PineItem"));
    }

    #[test]
    fn nested_typed_parents_both_get_assertions() {
        let src = r#"<pine-outer>
            <pine-inner>
                <pine-item></pine-item>
            </pine-inner>
        </pine-outer>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![
            bare("PineOuter"),
            bare("PineInner"),
            bare("PineItem"),
        ]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("PineOuterDefaultChild"),
            "outer parent missing"
        );
        assert!(
            s.contains("PineInnerDefaultChild"),
            "inner parent missing"
        );
        assert!(s.contains("PineInner"));
        assert!(s.contains("PineItem"));
    }

    #[test]
    fn empty_uses_produces_no_assertions() {
        let src = r#"<pine-foo><pine-item></pine-item></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = UsesTable::default();
        let tokens = emit_slot_assertions(&ast, &uses);
        assert!(tokens.is_empty());
    }

    #[test]
    fn trait_path_preserves_parent_module_path() {
        // When uses entry is a multi-segment path
        // (e.g. `pine::context_menu::PineContextMenuContent`),
        // the emitted trait lives at the same module path with
        // just the last ident rewritten.
        let src = r#"<my-foo><my-bar></my-bar></my-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![
            UsesEntry::Explicit(
                syn::parse_str("crate::pine::MyFoo").unwrap(),
                syn::parse_str::<syn::LitStr>("\"my-foo\"").unwrap(),
            ),
            UsesEntry::Explicit(
                syn::parse_str("crate::pine::MyBar").unwrap(),
                syn::parse_str::<syn::LitStr>("\"my-bar\"").unwrap(),
            ),
        ]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("crate") && s.contains("pine"),
            "trait path should preserve crate::pine prefix, got:\n{s}"
        );
        assert!(s.contains("MyFooDefaultChild"));
    }
}
