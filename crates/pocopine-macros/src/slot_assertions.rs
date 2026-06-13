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

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::Path;

use crate::slot::SlotName;
use crate::template_parser::{Element, Node, TemplateAst};
use crate::uses::UsesTable;
use crate::HTML5_ELEMENTS;

/// Walk `ast` alongside `uses` and emit slot-contract
/// assertions for every typed-parent / direct-child pair.
///
/// Returns an empty `TokenStream` when `uses` is empty or the
/// template contains no recognised typed-parent tags.
pub(crate) fn emit_slot_assertions(ast: &TemplateAst, uses: &UsesTable) -> TokenStream {
    if uses.entries.is_empty() {
        return TokenStream::new();
    }

    let mut out = TokenStream::new();

    // Reference every `uses` entry so imports land as used even
    // when a leaf primitive (e.g. `PineInput`) appears in the
    // list but isn't involved in any emitted assertion. Wrapping
    // the references in `const _: fn() = || { ... };` keeps this
    // compile-time only with zero runtime footprint.
    let uses_refs = uses.entries.iter().map(|(_, path)| {
        quote! {
            let _: ::core::marker::PhantomData<#path> = ::core::marker::PhantomData;
        }
    });
    out.extend(quote! {
        #[allow(unused_variables, dead_code, non_snake_case)]
        const _: fn() = || {
            #(#uses_refs)*
        };
    });

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
        if child_el.tag == "template"
            && let Some(slot_name) = pp_slot_name(child_el) {
                let site = SlotName::Named(slot_name);
                for slotted in &child_el.children {
                    if let Node::Element(slotted_el) = slotted {
                        emit_one_assertion(parent_path, &site, slotted_el, uses, out);
                    }
                }
                continue;
            }

        // Default slot — any non-template-wrapped direct child.
        emit_one_assertion(parent_path, &SlotName::Default, child_el, uses, out);
    }
}

fn emit_one_assertion(
    parent_path: &Path,
    slot_name: &SlotName,
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

    // Emit `<ParentType>::__pocopine_assert_<slot>_slot::<ChildType>()`.
    // Routing through the inherent method (emitted by
    // `slot::emit_slot_traits`) means consumers only need the
    // parent struct in scope — the marker trait doesn't have
    // to be imported separately.
    let assert_method = slot_name.assert_method_ident();
    let assertion = quote! {
        #[allow(unused_variables, dead_code, non_snake_case)]
        const _: fn() = || {
            <#parent_path>::#assert_method::<#child_path>();
        };
    };
    out.extend(assertion);
}

/// RFC 060 Tier 2 — emit `compile_error!` for every
/// custom-element tag in `ast` not covered by `uses`. A custom
/// element is any tag containing `-` that isn't an HTML5 native.
/// Each unique unknown tag fires once (further occurrences in
/// the same template are deduplicated).
///
/// Returns an empty `TokenStream` when every custom tag
/// resolves. Validation only runs when the consumer declares
/// `uses = [...]`; components without `uses` continue to
/// compile unchecked (the brief's incremental rollout).
pub(crate) fn emit_unknown_tag_diagnostics(ast: &TemplateAst, uses: &UsesTable) -> TokenStream {
    let mut out = TokenStream::new();
    let mut seen: HashSet<String> = HashSet::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk_for_unknown_tags(el, uses, &mut out, &mut seen);
        }
    }
    out
}

fn walk_for_unknown_tags(
    el: &Element,
    uses: &UsesTable,
    out: &mut TokenStream,
    seen: &mut HashSet<String>,
) {
    if !el.synthetic
        && is_custom_tag(&el.tag)
        && uses.lookup(&el.tag).is_none()
        && seen.insert(el.tag.clone())
    {
        let hint = pascal_case(&el.tag);
        let msg = format!(
            "tag `<{tag}>` is not declared in this component's `uses` list\n  \
             help: add `uses = [{hint}]` to the #[component(...)] attribute,\n  \
                   or re-export via a bundle (e.g. `uses = [pine::Dialog]`).",
            tag = el.tag,
        );
        out.extend(quote! { ::core::compile_error!(#msg); });
    }
    for child in &el.children {
        if let Node::Element(child_el) = child {
            walk_for_unknown_tags(child_el, uses, out, seen);
        }
    }
}

/// `true` for any hyphenated tag that isn't an HTML5 native —
/// the Custom Elements spec reserves hyphenation for custom
/// elements, and `HTML5_ELEMENTS` is the canonical native list.
fn is_custom_tag(tag: &str) -> bool {
    tag.contains('-') && HTML5_ELEMENTS.binary_search(&tag).is_err()
}

fn pascal_case(kebab: &str) -> String {
    kebab
        .split('-')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
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
            s.contains("__pocopine_assert_default_slot"),
            "expected inherent-method call on parent, got:\n{s}"
        );
        assert!(
            s.contains("PineFoo") && s.contains("PineItem"),
            "expected both parent and child types in assertion, got:\n{s}"
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
        let uses = table(vec![bare("PineFoo"), bare("PineItem"), bare("PineTitle")]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("__pocopine_assert_header_slot"),
            "expected named-slot method, got:\n{s}"
        );
        assert!(
            s.contains("__pocopine_assert_default_slot"),
            "expected default-slot method for unwrapped child, got:\n{s}"
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
        let uses = table(vec![bare("PineOuter"), bare("PineInner"), bare("PineItem")]);
        let tokens = emit_slot_assertions(&ast, &uses);
        let s = tokens.to_string();
        // Two assertion blocks — both via the default-slot
        // method, one on the outer parent, one on the inner.
        let method_count = s.matches("__pocopine_assert_default_slot").count();
        assert_eq!(method_count, 2, "expected 2 method calls, got:\n{s}");
        assert!(s.contains("PineOuter"));
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

    // ── RFC 060 Tier 2 — unknown-tag diagnostics ──────────

    #[test]
    fn unknown_tag_emits_compile_error_with_help() {
        let src = r#"<pine-foo><pine-bar></pine-bar></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![bare("PineFoo")]);
        let tokens = emit_unknown_tag_diagnostics(&ast, &uses);
        let s = tokens.to_string();
        assert!(
            s.contains("compile_error"),
            "expected compile_error! macro call, got:\n{s}"
        );
        assert!(s.contains("pine-bar"), "expected offending tag in message");
        assert!(
            s.contains("PineBar"),
            "expected pascal-cased type hint, got:\n{s}"
        );
        assert!(
            s.contains("uses ="),
            "expected `uses = [...]` help text, got:\n{s}"
        );
    }

    #[test]
    fn known_tags_produce_no_diagnostics() {
        let src = r#"<pine-foo><pine-bar></pine-bar></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![bare("PineFoo"), bare("PineBar")]);
        let tokens = emit_unknown_tag_diagnostics(&ast, &uses);
        assert!(
            tokens.is_empty(),
            "every tag is in `uses`, expected empty diagnostics"
        );
    }

    #[test]
    fn html5_native_tags_are_not_flagged() {
        // `<div>` and `<span>` are HTML5 native — never flagged
        // even when no `uses` entry exists for them.
        let src = r#"<div><span><pine-foo></pine-foo></span></div>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![bare("PineFoo")]);
        let tokens = emit_unknown_tag_diagnostics(&ast, &uses);
        assert!(
            tokens.is_empty(),
            "HTML5 natives must not trigger validation, got:\n{tokens}"
        );
    }

    #[test]
    fn duplicate_unknown_tag_is_reported_once() {
        let src = r#"<pine-foo><pine-bar></pine-bar><pine-bar></pine-bar></pine-foo>"#;
        let (ast, _errors) = parse(src, "test.poco");
        let uses = table(vec![bare("PineFoo")]);
        let tokens = emit_unknown_tag_diagnostics(&ast, &uses);
        let s = tokens.to_string();
        let occurrences = s.matches("pine-bar").count();
        // Single message — the `pine-bar` substring appears once
        // in the format string. (If we ever change the message to
        // include the tag twice, bump this expectation.)
        assert_eq!(
            occurrences, 1,
            "the same unknown tag should fire one compile_error, got:\n{s}"
        );
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
            "type path should preserve crate::pine prefix, got:\n{s}"
        );
        assert!(s.contains("MyFoo"));
        assert!(s.contains("__pocopine_assert_default_slot"));
    }
}
