//! RFC 049 — `#[slot]` helper attribute parsing and marker-trait
//! emission.
//!
//! `#[slot]` is an **inert helper attribute** on the `#[component]`
//! struct. `#[component]` parses it off the struct (same pattern
//! `#[prop]` already uses per RFC 031), strips it from the emitted
//! struct, and emits one concrete marker trait plus one blanket
//! impl per entry in `accepts` / `only` as a module-level sibling
//! of the struct.
//!
//! Forms accepted:
//!
//! * `#[slot(default, accepts = [A, B])]` — loose slot contract.
//!   Items in the list are allowed; HTML wrappers (`<div>` etc.)
//!   pass silently. Unknown custom elements also pass.
//! * `#[slot(default, only = [A, B])]` — strict slot contract.
//!   Every direct child element must be one of the listed
//!   accepted component tags. HTML wrappers are rejected by the
//!   consumer-side scan.
//! * `#[slot(name = "footer", accepts = [A])]` — same as above,
//!   but for a named slot.
//! * `#[slot(default)]` with no `accepts`/`only` — declares the
//!   slot exists for future typed-yield use. Emits no trait;
//!   RFC 049's consumer-side scan skips these.
//!
//! See RFC 049 §4.1 / §4.2 for the spec.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Attribute, Ident, LitStr, Path, Token,
};

/// One `#[slot(...)]` declaration on the parent struct.
#[derive(Debug, Clone)]
pub(crate) struct SlotDecl {
    /// Which slot this constrains — the default (unnamed) slot
    /// or a named one (`name = "footer"`).
    pub name: SlotName,
    /// Strictness mode — loose or strict.
    pub mode: SlotMode,
    /// Accepted child component types.
    pub accepts: Vec<Path>,
    /// Span of the `#[slot(...)]` attribute for diagnostics.
    /// Currently unused at emit time; kept so future diagnostic
    /// paths can anchor errors at the declaration.
    #[allow(dead_code)]
    pub span: proc_macro2::Span,
}

#[derive(Debug, Clone)]
pub(crate) enum SlotName {
    Default,
    Named(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotMode {
    /// Loose — only listed components are checked, others pass.
    Accepts,
    /// Strict — every direct child must match a listed
    /// component; HTML wrappers / unknown tags are rejected.
    Only,
}

impl SlotName {
    /// PascalCase suffix used for the emitted marker trait
    /// name. `SlotName::Default` → `"Default"`;
    /// `SlotName::Named("footer")` → `"Footer"`.
    pub(crate) fn trait_suffix(&self) -> String {
        match self {
            SlotName::Default => "Default".to_string(),
            SlotName::Named(s) => pascal_case(s),
        }
    }

    /// Inherent-method ident used for the consumer-side
    /// assertion call. Shape is
    /// `__pocopine_assert_<snake_name>_slot`. Consumers call
    /// it via `<Struct>::<ident>::<ChildType>()`.
    pub(crate) fn assert_method_ident(&self) -> Ident {
        let name = match self {
            SlotName::Default => "default".to_string(),
            SlotName::Named(s) => snake_case(s),
        };
        format_ident!("__pocopine_assert_{}_slot", name)
    }

    /// Human-readable slot name for diagnostic messages.
    fn display(&self) -> String {
        match self {
            SlotName::Default => "default".to_string(),
            SlotName::Named(s) => s.clone(),
        }
    }
}

/// Strip all `#[slot(...)]` attributes from `attrs`, parsing each
/// into a [`SlotDecl`]. Mutates `attrs` in place — the remaining
/// attributes are re-emitted on the struct as normal.
pub(crate) fn parse_and_strip_slots(attrs: &mut Vec<Attribute>) -> syn::Result<Vec<SlotDecl>> {
    let mut decls = Vec::new();
    let mut i = 0;
    while i < attrs.len() {
        if attrs[i].path().is_ident("slot") {
            let attr = attrs.remove(i);
            decls.push(parse_slot_attr(&attr)?);
        } else {
            i += 1;
        }
    }
    Ok(decls)
}

fn parse_slot_attr(attr: &Attribute) -> syn::Result<SlotDecl> {
    let span = attr.span_ident_or_body();

    // Expected shape: `#[slot(selector, mode = [types, ...])]`
    // where `selector` is `default` or `name = "..."`, and
    // `mode` is `accepts` or `only`. Either mode may be absent
    // (declares the slot with no constraint).
    let mut name: Option<SlotName> = None;
    let mut mode: Option<SlotMode> = None;
    let mut accepts: Vec<Path> = Vec::new();

    // Parse the parenthesised body. `Meta`-list parsing is
    // comma-separated; first entry is the selector.
    let nested = attr.parse_args_with(Punctuated::<SlotArg, Token![,]>::parse_terminated)?;

    for arg in nested {
        match arg {
            SlotArg::Default => {
                if name.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "#[slot]: multiple slot selectors — use either `default` or `name = \"...\"`, not both",
                    ));
                }
                name = Some(SlotName::Default);
            }
            SlotArg::Name(lit) => {
                if name.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "#[slot]: multiple slot selectors — use either `default` or `name = \"...\"`, not both",
                    ));
                }
                if lit.value().is_empty() {
                    return Err(syn::Error::new(
                        lit.span(),
                        "#[slot]: `name = \"\"` is not a valid slot name",
                    ));
                }
                name = Some(SlotName::Named(lit.value()));
            }
            SlotArg::Accepts(paths) => {
                if mode.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "#[slot]: use either `accepts = [...]` or `only = [...]`, not both",
                    ));
                }
                mode = Some(SlotMode::Accepts);
                accepts = paths;
            }
            SlotArg::Only(paths) => {
                if mode.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "#[slot]: use either `accepts = [...]` or `only = [...]`, not both",
                    ));
                }
                mode = Some(SlotMode::Only);
                accepts = paths;
            }
        }
    }

    let name = name.ok_or_else(|| {
        syn::Error::new(
            span,
            "#[slot]: missing slot selector — write `#[slot(default, ...)]` or `#[slot(name = \"...\", ...)]`",
        )
    })?;

    Ok(SlotDecl {
        name,
        // Slot declared without accepts/only = metadata-only
        // (RFC 049 §4.1 row 4). Default to Accepts with empty
        // list; downstream sees no trait emitted and the
        // consumer-side scan does nothing.
        mode: mode.unwrap_or(SlotMode::Accepts),
        accepts,
        span,
    })
}

/// One argument inside `#[slot(...)]` — `default`,
/// `name = "..."`, `accepts = [...]`, or `only = [...]`.
enum SlotArg {
    Default,
    Name(LitStr),
    Accepts(Vec<Path>),
    Only(Vec<Path>),
}

impl Parse for SlotArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident: Ident = input.parse()?;
        let ident_name = ident.to_string();
        match ident_name.as_str() {
            "default" => Ok(SlotArg::Default),
            "name" => {
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                Ok(SlotArg::Name(lit))
            }
            "accepts" | "only" => {
                input.parse::<Token![=]>()?;
                let content;
                syn::bracketed!(content in input);
                let paths = Punctuated::<Path, Token![,]>::parse_terminated(&content)?;
                let paths: Vec<Path> = paths.into_iter().collect();
                if matches!(ident_name.as_str(), "accepts") {
                    Ok(SlotArg::Accepts(paths))
                } else {
                    Ok(SlotArg::Only(paths))
                }
            }
            other => Err(syn::Error::new(
                ident.span(),
                format!(
                    "#[slot]: unknown argument `{other}` — expected \
                     `default`, `name = \"...\"`, `accepts = [...]`, or `only = [...]`"
                ),
            )),
        }
    }
}

/// Emit marker traits + blanket impls + inherent assertion
/// method for each of this component's slot declarations.
///
/// The inherent method (`__pocopine_assert_<slot>_slot::<T>()`)
/// is the path consumers use — it routes the trait bound
/// through the struct so importing `#struct_ident` is enough;
/// the trait itself doesn't need to be in scope at the call
/// site. Per RFC 049 §5.2.
///
/// Slots with no `accepts`/`only` list produce no trait (the slot
/// is metadata for future typed-yield use; RFC 049's consumer
/// scan ignores them).
pub(crate) fn emit_slot_traits(
    struct_ident: &Ident,
    component_tag: &str,
    slots: &[SlotDecl],
) -> TokenStream {
    let mut out = TokenStream::new();

    // RFC 060 Tier 2 — emit a permissive fallback
    // `__pocopine_assert_default_slot::<__T>()` on every
    // component that doesn't declare a strict default slot, so
    // RFC 049's consumer-side scan can fire on parent / child
    // pairs where the parent is a leaf primitive (e.g.
    // `<pine-dialog-trigger><pine-button>...`) that never
    // opted into typed-slot validation. Strict defaults (when
    // `#[slot(default, only = [...])]` is declared with a
    // non-empty list) still emit their own bounded method
    // below and shadow the fallback.
    let has_strict_default = slots
        .iter()
        .any(|s| matches!(s.name, SlotName::Default) && !s.accepts.is_empty());
    if !has_strict_default {
        out.extend(quote! {
            impl #struct_ident {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                pub fn __pocopine_assert_default_slot<__T>() {}
            }
        });
    }

    for slot in slots {
        if slot.accepts.is_empty() {
            // Slot declared for future typed-yield use; no
            // compile-time child constraint means no trait.
            continue;
        }
        let trait_ident = format_ident!("{}{}Child", struct_ident, slot.name.trait_suffix(),);
        let assert_method_ident = slot.name.assert_method_ident();
        let slot_display = slot.name.display();
        let mode_suffix = match slot.mode {
            SlotMode::Accepts => "accepts",
            SlotMode::Only => "only",
        };
        let on_unimplemented_message = format!(
            "`{{Self}}` is not an accepted child of `<{component_tag}>`'s {slot_display} slot"
        );
        let on_unimplemented_note = format!(
            "allowed children are declared on `{struct_ident}` via `#[slot({slot_selector}, {mode_suffix}=[...])]`",
            slot_selector = match &slot.name {
                SlotName::Default => "default".to_string(),
                SlotName::Named(s) => format!("name = \"{s}\""),
            },
        );
        let accepts = &slot.accepts;

        // Mode split per RFC 049 §4.1 / §4.3:
        //
        // `only` — strict rejection. Trait has specific impls
        // only; non-listed types fail the trait bound and the
        // consumer-side assertion triggers the on_unimplemented
        // diagnostic.
        //
        // `accepts` — declarative, non-rejecting. Trait still
        // exists (for future typed-yield use per §4.4) but
        // carries a blanket impl so any T satisfies the bound.
        // HTML wrappers pass silently (RFC 049 §4.3 rule 2);
        // other pocopine components that happen to appear as
        // direct children — e.g. `<pine-calendar-root>` inside
        // `<pine-popover-content>` — also pass, treated as
        // author content. Nothing gets rejected; the listed
        // types are documentation of pocopine's semantic parts.
        let impls = match slot.mode {
            SlotMode::Only => quote! {
                #(
                    impl #trait_ident for #accepts {}
                )*
            },
            SlotMode::Accepts => quote! {
                // Blanket impl — `accepts` is declarative-only.
                // Listed types in #accepts are pocopine's own
                // semantic parts; they're still "implementors"
                // via the blanket and don't need individual
                // impls (which would conflict with the blanket).
                impl<__T> #trait_ident for __T {}
            },
        };

        out.extend(quote! {
            #[diagnostic::on_unimplemented(
                message = #on_unimplemented_message,
                note = #on_unimplemented_note,
            )]
            #[allow(non_camel_case_types)]
            pub trait #trait_ident {}

            #impls

            // RFC 049 §5.2 — inherent method that carries the
            // trait bound. Consumers call
            // `<#struct_ident>::#assert_method_ident::<ChildType>()`
            // to enforce the slot contract; they don't have to
            // import `#trait_ident` to do so.
            impl #struct_ident {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                pub fn #assert_method_ident<__T: #trait_ident>() {}
            }
        });
    }
    out
}

fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '-' || c == '_' || c == ' ' {
            capitalize_next = true;
            continue;
        }
        if capitalize_next {
            out.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `footer` → `footer`, `my-slot` → `my_slot`, `MySlot` →
/// `m_y_slot`. Used for the inherent-method ident so named
/// slots emit valid Rust identifiers.
fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c == '-' || c == ' ' {
            out.push('_');
        } else if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Shim trait — lets us span-at either the attribute's
/// `#[slot]` ident or the body tokens, whichever is available.
trait AttrSpanHelpers {
    fn span_ident_or_body(&self) -> proc_macro2::Span;
}

impl AttrSpanHelpers for Attribute {
    fn span_ident_or_body(&self) -> proc_macro2::Span {
        use syn::spanned::Spanned;
        self.span()
    }
}

// Re-export for tests; kept as a free function so the test
// module can call it without grabbing Path imports.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn parse_slot_for_test(attr: &Attribute) -> syn::Result<SlotDecl> {
    parse_slot_attr(attr)
}

#[allow(unused_imports)]
use syn::spanned::Spanned;

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn parse_struct_with_attr(src: proc_macro2::TokenStream) -> syn::ItemStruct {
        syn::parse2(src).expect("expected parseable struct")
    }

    #[test]
    fn slot_default_accepts_parses() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default, accepts = [ItemA, ItemB])]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots.len(), 1);
        assert!(matches!(slots[0].name, SlotName::Default));
        assert_eq!(slots[0].mode, SlotMode::Accepts);
        assert_eq!(slots[0].accepts.len(), 2);
        // #[slot] stripped from the struct.
        assert!(st.attrs.is_empty());
    }

    #[test]
    fn slot_named_only_parses() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(name = "footer", only = [Title, Subtitle])]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots.len(), 1);
        if let SlotName::Named(n) = &slots[0].name {
            assert_eq!(n, "footer");
        } else {
            panic!("expected named slot");
        }
        assert_eq!(slots[0].mode, SlotMode::Only);
    }

    #[test]
    fn slot_default_bare_no_trait_expected() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default)]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots.len(), 1);
        assert!(slots[0].accepts.is_empty());
        let out = emit_slot_traits(
            &syn::Ident::new("Foo", proc_macro2::Span::call_site()),
            "foo",
            &slots,
        );
        // No `pub trait` in the output — empty accepts means no
        // compile-time constraint.
        assert!(!out.to_string().contains("pub trait"));
    }

    #[test]
    fn both_accepts_and_only_is_rejected() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default, accepts = [A], only = [B])]
            struct Foo;
        });
        let err = parse_and_strip_slots(&mut st.attrs);
        assert!(err.is_err(), "mixing accepts + only must fail");
    }

    #[test]
    fn missing_selector_is_rejected() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(accepts = [A])]
            struct Foo;
        });
        let err = parse_and_strip_slots(&mut st.attrs);
        assert!(err.is_err(), "missing default/name selector must fail");
    }

    #[test]
    fn unknown_argument_is_rejected() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default, lol = [A])]
            struct Foo;
        });
        let err = parse_and_strip_slots(&mut st.attrs);
        assert!(err.is_err(), "unknown argument must fail");
    }

    #[test]
    fn multiple_slots_parse() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default, accepts = [A])]
            #[slot(name = "footer", only = [B])]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots.len(), 2);
        assert!(matches!(slots[0].name, SlotName::Default));
        assert!(matches!(&slots[1].name, SlotName::Named(n) if n == "footer"));
    }

    #[test]
    fn emit_only_slot_produces_named_trait_with_specific_impls() {
        // `only` mode — strict rejection. Trait + specific
        // impls per accepted type.
        let slots = vec![SlotDecl {
            name: SlotName::Default,
            mode: SlotMode::Only,
            accepts: vec![
                syn::parse_str::<Path>("PineItem").unwrap(),
                syn::parse_str::<Path>("PineSeparator").unwrap(),
            ],
            span: proc_macro2::Span::call_site(),
        }];
        let out = emit_slot_traits(
            &syn::Ident::new("PineContextMenuContent", proc_macro2::Span::call_site()),
            "pine-context-menu-content",
            &slots,
        );
        let s = out.to_string();
        assert!(s.contains("pub trait PineContextMenuContentDefaultChild"));
        assert!(s.contains("impl PineContextMenuContentDefaultChild for PineItem"));
        assert!(s.contains("impl PineContextMenuContentDefaultChild for PineSeparator"));
        // `only` mode should NOT emit a blanket impl.
        assert!(!s.contains("impl < __T >"));
    }

    #[test]
    fn emit_accepts_slot_emits_blanket_impl() {
        // `accepts` mode — declarative, non-rejecting. Trait
        // exists + has a blanket impl so any type satisfies
        // the bound. Specific impls are omitted (would conflict
        // with the blanket per Rust's coherence rules).
        let slots = vec![SlotDecl {
            name: SlotName::Default,
            mode: SlotMode::Accepts,
            accepts: vec![
                syn::parse_str::<Path>("PineDialogTitle").unwrap(),
                syn::parse_str::<Path>("PineDialogDescription").unwrap(),
            ],
            span: proc_macro2::Span::call_site(),
        }];
        let out = emit_slot_traits(
            &syn::Ident::new("PineDialogContent", proc_macro2::Span::call_site()),
            "pine-dialog-content",
            &slots,
        );
        let s = out.to_string();
        assert!(s.contains("pub trait PineDialogContentDefaultChild"));
        // Blanket impl shape: `impl<__T> Trait for __T {}`.
        assert!(
            s.contains("impl < __T >") && s.contains("for __T"),
            "expected blanket impl, got:\n{s}"
        );
        // No per-type impls in accepts mode.
        assert!(!s.contains("for PineDialogTitle"));
    }

    #[test]
    fn emit_named_slot_produces_pascalcased_trait() {
        let slots = vec![SlotDecl {
            name: SlotName::Named("footer".to_string()),
            mode: SlotMode::Only,
            accepts: vec![syn::parse_str::<Path>("PineTitle").unwrap()],
            span: proc_macro2::Span::call_site(),
        }];
        let out = emit_slot_traits(
            &syn::Ident::new("Foo", proc_macro2::Span::call_site()),
            "foo",
            &slots,
        );
        assert!(out.to_string().contains("pub trait FooFooterChild"));
    }

    #[test]
    fn pascal_case_handles_kebab_names() {
        assert_eq!(pascal_case("footer"), "Footer");
        assert_eq!(pascal_case("my-named-slot"), "MyNamedSlot");
        assert_eq!(pascal_case("other_name"), "OtherName");
    }
}
