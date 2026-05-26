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
    /// RFC 084 — typed slot props. When `Some(T)`, the macro
    /// validates the compound's `<slot :LHS=...>` publications
    /// against `T`'s `#[prop]` field set, and the caller's
    /// `pp-let` binding is typed as `T`. `T` must
    /// `#[derive(Props)]`.
    pub props: Option<Path>,
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
    let mut props: Option<Path> = None;

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
            SlotArg::Props(path) => {
                if props.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "#[slot]: `props = T` may appear at most once per slot declaration",
                    ));
                }
                props = Some(path);
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
        props,
        span,
    })
}

/// One argument inside `#[slot(...)]` — `default`,
/// `name = "..."`, `accepts = [...]`, `only = [...]`, or
/// `props = TypeName` (RFC 084).
enum SlotArg {
    Default,
    Name(LitStr),
    Accepts(Vec<Path>),
    Only(Vec<Path>),
    Props(Path),
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
            "props" => {
                input.parse::<Token![=]>()?;
                let path: Path = input.parse()?;
                Ok(SlotArg::Props(path))
            }
            other => Err(syn::Error::new(
                ident.span(),
                format!(
                    "#[slot]: unknown argument `{other}` — expected \
                     `default`, `name = \"...\"`, `accepts = [...]`, \
                     `only = [...]`, or `props = TypeName`"
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

// ── RFC 084 — typed slot props validation ────────────────────────

/// Output of [`emit_slot_props_validation`].
///
/// `tokens` are spliced into the `#[component]` macro's output
/// as `const _: () = { … }` blocks. `template_edits` are
/// byte-level inserts applied to the template source BEFORE
/// `compile_template_static` runs — used by iterated mode to
/// inject `:VAR="VAR"` publications onto `<slot>` elements that
/// sit inside a `pp-for`, so the runtime's existing publication
/// path picks them up without a runtime-side change.
pub(crate) struct SlotValidationEmit {
    pub tokens: TokenStream,
    pub template_edits: Vec<TemplateEdit>,
}

/// Byte-level insert into the template source. Mirrors the
/// shape of [`crate::for_plan::TemplateStamp`] so the application
/// strategy stays consistent across macro passes.
pub(crate) struct TemplateEdit {
    pub insert_at: usize,
    pub text: String,
}

/// `pp-for` context flowing down through the AST walk. Captured
/// when a `pp-for="X in EXPR"` is parsed; consulted at each
/// `<slot>` to decide iterated-mode dispatch.
#[derive(Clone)]
struct ForContext {
    iter_var: String,
    /// Bare field-path RHS of `pp-for="X in <RHS>"`. v1 only
    /// supports a single identifier (a struct field). Anything
    /// else stays `None` and forces iterated-mode-needing slots
    /// to emit a "use static mode" compile error.
    iter_field: Option<String>,
}

fn extract_for_context(el: &crate::template_parser::Element) -> Option<ForContext> {
    let raw = el.attrs.iter().find_map(|(k, v)| {
        if k == "pp-for" {
            Some(v.as_str())
        } else {
            None
        }
    })?;
    // Shape: `X in EXPR`. Match the literal ` in ` separator.
    let (var, expr) = raw.split_once(" in ")?;
    let iter_var = var.trim().to_string();
    let expr = expr.trim();
    // v1 — only bare-ident RHS qualifies for iterated-mode
    // type inference. `pp-for="row in self.rows.iter()"`-style
    // expressions get `iter_field = None` and the slot is
    // forced into static mode (with an actionable error).
    let iter_field =
        if !expr.is_empty() && expr.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            Some(expr.to_string())
        } else {
            None
        };
    Some(ForContext {
        iter_var,
        iter_field,
    })
}

/// Per-slot resolution: which mode, and the enclosing `pp-for`
/// context (if any). Populated by the AST walk.
struct SlotMatch<'a> {
    el: &'a crate::template_parser::Element,
    for_ctx: Option<ForContext>,
}

/// Emit `const _: () = { … }` blocks that validate every typed
/// slot's publication shape AND collect byte-level template
/// edits for iterated-mode auto-publication. Per RFC 084.
///
/// Mode resolution per `<slot>` element:
///
/// * Any `:LHS=…` attr → **static mode** (publications must
///   cover every `#[prop]` field on `T`).
/// * No `:LHS=…` AND a `pp-for` ancestor with a bare-ident RHS
///   → **iterated mode** (auto-publish the iteration variable;
///   emit a Rust type assertion verifying the iter item
///   matches `T`).
/// * No `:LHS=…` AND no usable `pp-for` ancestor → **static
///   mode with empty publication** (errors if `T` has any
///   `#[prop]` fields).
pub(crate) fn emit_slot_props_validation(
    ast: &crate::template_parser::TemplateAst,
    slots: &[SlotDecl],
    struct_ident: &Ident,
) -> SlotValidationEmit {
    let mut out = TokenStream::new();
    let mut template_edits = Vec::new();

    for decl in slots {
        let Some(props_ty) = decl.props.as_ref() else {
            continue;
        };
        // T: Props boundary — fires a clear "the type is not
        // Props" error at the slot's props arg if the user passes
        // a non-Props type.
        let props_ty_str = quote!(#props_ty).to_string();
        out.extend(quote! {
            const _: fn() = || {
                fn __pocopine_assert_props<__T: ::pocopine::__private::Props>() {}
                __pocopine_assert_props::<#props_ty>();
            };
        });

        let target_name: String = match &decl.name {
            SlotName::Default => "default".into(),
            SlotName::Named(s) => s.clone(),
        };

        // Scan every element in the template AST for matching
        // `<slot>` elements, threading the nearest enclosing
        // `pp-for` context as we descend. A `<slot>` with no
        // `name` attribute is the default slot.
        let mut found: Vec<SlotMatch<'_>> = Vec::new();
        for root in &ast.roots {
            collect_slot_matches(root, &target_name, None, &mut found);
        }

        if found.is_empty() {
            // The decl mentions a slot the template never
            // exposes — fail fast with a clear directive
            // message. This isn't strictly typed-props-specific
            // but it's the right place to surface it: the
            // author opted into a typed slot, so they should
            // also have a matching `<slot>` in the template.
            let msg = format!(
                "pocopine: `#[slot(name = \"{target_name}\", props = {props_ty_str})]` declared on this component but no matching `<slot{name_attr}>` element appears in the template",
                name_attr = match &decl.name {
                    SlotName::Default => "".to_string(),
                    SlotName::Named(s) => format!(" name=\"{s}\""),
                },
            );
            out.extend(quote! { ::core::compile_error!(#msg); });
            continue;
        }

        for SlotMatch { el, for_ctx } in &found {
            let has_publications = el.attrs.iter().any(|(k, _)| k.starts_with(':'));
            if !has_publications && for_ctx.is_some() {
                emit_iterated_mode(
                    &target_name,
                    props_ty,
                    &props_ty_str,
                    el,
                    for_ctx.as_ref().unwrap(),
                    struct_ident,
                    &mut out,
                    &mut template_edits,
                );
            } else {
                emit_static_publication_validation(
                    &target_name,
                    props_ty,
                    &props_ty_str,
                    el,
                    &mut out,
                );
            }
        }
    }

    SlotValidationEmit {
        tokens: out,
        template_edits,
    }
}

/// Recursive walk with `pp-for` ancestor tracking. Records every
/// matching `<slot>` paired with the nearest enclosing
/// `pp-for`'s context (or `None` if none).
fn collect_slot_matches<'a>(
    node: &'a crate::template_parser::Node,
    target_name: &str,
    enclosing_for: Option<ForContext>,
    out: &mut Vec<SlotMatch<'a>>,
) {
    let crate::template_parser::Node::Element(el) = node else {
        return;
    };
    // If THIS element has pp-for, its context applies to its
    // descendants (replacing the enclosing one, if any).
    let descend_ctx = extract_for_context(el).or_else(|| enclosing_for.clone());

    if el.tag == "slot" {
        let name_attr = el.attrs.iter().find(|(k, _)| k == "name");
        let this_name = name_attr.map(|(_, v)| v.as_str()).unwrap_or("default");
        if this_name == target_name {
            out.push(SlotMatch {
                el,
                for_ctx: enclosing_for.clone(),
            });
        }
    }
    for child in &el.children {
        collect_slot_matches(child, target_name, descend_ctx.clone(), out);
    }
}

/// Emit the iterated-mode artifacts for one `<slot>` element:
/// the Rust type assertion that the iter item matches `T`, plus
/// a template edit that injects `:VAR="VAR"` into the slot's
/// opening tag so the runtime sees the auto-publication.
fn emit_iterated_mode(
    target_name: &str,
    props_ty: &Path,
    props_ty_str: &str,
    el: &crate::template_parser::Element,
    for_ctx: &ForContext,
    struct_ident: &Ident,
    out: &mut TokenStream,
    template_edits: &mut Vec<TemplateEdit>,
) {
    // No usable iteration source → emit a directive error and
    // skip the type assertion / edit. The author should either
    // simplify the `pp-for` expression or write `<slot :LHS=…>`
    // attributes explicitly (static mode).
    let Some(iter_field) = for_ctx.iter_field.as_deref() else {
        let msg = format!(
            "pocopine: slot `{target_name}` sits inside a `pp-for` whose iteration source isn't a bare field path — iterated-mode auto-publish needs `pp-for=\"X in field_name\"`. Either simplify the pp-for expression, or write the publications explicitly on the `<slot>` element (static mode)."
        );
        out.extend(quote! { ::core::compile_error!(#msg); });
        return;
    };

    let iter_var = &for_ctx.iter_var;
    let iter_var_ident = match syn::parse_str::<syn::Ident>(iter_var) {
        Ok(i) => i,
        Err(_) => {
            // Defensive: pp-for parsed an iter var that isn't a
            // Rust ident. Skip — the directive registry should
            // already have flagged this elsewhere.
            return;
        }
    };
    let iter_field_ident = match syn::parse_str::<syn::Ident>(iter_field) {
        Ok(i) => i,
        Err(_) => return,
    };

    // Rust type assertion: walking `this.<iter_field>` must
    // yield items of type `&T`. Lives in a `const _: fn(&Self)`
    // closure so the typechecker sees it at `cargo check` time
    // without running anything at runtime.
    let mismatch_note = format!(
        "the iteration source `{iter_field}` for slot `{target_name}` must yield items of type `{props_ty_str}` (matching `#[slot(name = \"{target_name}\", props = {props_ty_str})]`)"
    );
    // Type assertion lives at module-item position (alongside
    // the emitted struct), so `Self` isn't in scope — name the
    // struct explicitly via `#struct_ident`.
    out.extend(quote! {
        const _: fn(&#struct_ident) = |__poco_this: &#struct_ident| {
            // Force-resolve the iter item's type via a `match`
            // on `IntoIterator`. The `unreachable!()` branch is
            // never executed (the assertion is in const-fn
            // closure position, never called) — Rust still
            // typechecks the branch and reports the mismatch
            // with this `let` binding's `&#props_ty` annotation.
            let _: &#props_ty = match (&__poco_this.#iter_field_ident).into_iter().next() {
                ::core::option::Option::Some(__poco_item) => __poco_item,
                ::core::option::Option::None => {
                    let _ = #mismatch_note;
                    ::core::unreachable!();
                }
            };
            let _ = stringify!(#iter_var_ident);
        };
    });

    // Template edit: inject ` :VAR="VAR"` just before the
    // closing `>` of the slot's opening tag. The runtime's
    // existing `materialize_slot` path (mount.rs:920-928)
    // picks it up identically to an explicit static-mode
    // publication.
    //
    // `opening_tag_range.end` points just past `>`; insert at
    // `end - 1` so we land before the angle bracket. Skip if
    // the range is the placeholder `0..0` (foster-parented
    // element with no real source position).
    if el.opening_tag_range.end > el.opening_tag_range.start {
        let insert_at = el.opening_tag_range.end.saturating_sub(1);
        template_edits.push(TemplateEdit {
            insert_at,
            text: format!(" :{iter_var}=\"{iter_var}\""),
        });
    }
}

/// Emit per-publication-key existence checks plus a coverage
/// (set-equality) check for one `<slot>` element in static mode.
fn emit_static_publication_validation(
    target_name: &str,
    props_ty: &Path,
    props_ty_str: &str,
    el: &crate::template_parser::Element,
    out: &mut TokenStream,
) {
    let publications: Vec<(String, String)> = el
        .attrs
        .iter()
        .filter_map(|(k, _v)| k.strip_prefix(':').map(|lhs| (lhs.to_string(), _v.clone())))
        .collect();

    // Per-publication existence: each LHS must be a `#[prop]`
    // field on the props type. Quotes the offending key so the
    // error message is actionable.
    for (lhs, _rhs) in &publications {
        let msg = format!(
            "pocopine: slot `{target_name}` publishes `{lhs}` which isn't a `#[prop]` field on `{props_ty_str}`. Add `#[prop] pub {lhs}: …` to the props struct or remove the `:{lhs}=…` publication from the `<slot>` element."
        );
        let lhs_lit = lhs.as_str();
        out.extend(quote! {
            const _: () = {
                if !::pocopine::__private::str_slice_contains_const(
                    <#props_ty>::__POC_PROP_LEAVES,
                    #lhs_lit,
                ) {
                    ::core::panic!(#msg);
                }
            };
        });
    }

    // Coverage: every `#[prop]` field on T must be published.
    // Const-fn set-equality (both directions) gives one diagnostic
    // if anything is missing; combined with the per-key checks
    // above, the author sees per-extra-key errors AND a generic
    // "missing fields" error pointing at the props struct.
    let lhs_lits: Vec<&str> = publications.iter().map(|(k, _)| k.as_str()).collect();
    let coverage_msg = format!(
        "pocopine: slot `{target_name}` publication doesn't cover every `#[prop]` field declared on `{props_ty_str}`. Inspect the struct's `#[prop]` fields and add a `:field=…` publication on the `<slot>` element for each."
    );
    out.extend(quote! {
        const _: () = {
            const __POCO_PUB: &[&str] = &[#(#lhs_lits),*];
            if !::pocopine::__private::str_slice_set_eq_const(
                <#props_ty>::__POC_PROP_LEAVES,
                __POCO_PUB,
            ) {
                ::core::panic!(#coverage_msg);
            }
        };
    });
}

/// Apply byte-level inserts to a template source. Same shape as
/// [`crate::for_plan::apply_stamps`] — sort by descending
/// `insert_at` then splice each text in, so earlier byte offsets
/// stay valid as edits accumulate.
pub(crate) fn apply_template_edits(source: &str, edits: &[TemplateEdit]) -> String {
    if edits.is_empty() {
        return source.to_string();
    }
    let mut sorted: Vec<&TemplateEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.insert_at));
    let mut out = source.to_string();
    for edit in sorted {
        if edit.insert_at > out.len() {
            continue;
        }
        // Defensive: only insert at a char boundary. Templates
        // are typically ASCII at HTML-syntax positions, but
        // safer to skip than to panic.
        if !out.is_char_boundary(edit.insert_at) {
            continue;
        }
        out.insert_str(edit.insert_at, &edit.text);
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
            props: None,
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
            props: None,
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
            props: None,
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

    // ── RFC 084 — typed slot props parser tests ─────────────────

    #[test]
    fn slot_props_parses_as_path() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(name = "header", props = UploadHeaderProps)]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots.len(), 1);
        let props = slots[0].props.as_ref().expect("props should be Some");
        let rendered = quote::quote!(#props).to_string();
        assert_eq!(rendered.replace(' ', ""), "UploadHeaderProps");
    }

    #[test]
    fn slot_props_accepts_module_qualified_path() {
        // `props = some::module::Type` should round-trip.
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default, props = crate::upload::UploadItemProps)]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        let props = slots[0].props.as_ref().expect("props should be Some");
        let rendered = quote::quote!(#props).to_string();
        assert!(
            rendered.contains("UploadItemProps") && rendered.contains("crate"),
            "props path should preserve qualifier: {rendered}"
        );
    }

    #[test]
    fn slot_props_default_is_none() {
        let mut st = parse_struct_with_attr(quote! {
            #[slot(default)]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert!(slots[0].props.is_none());
    }

    #[test]
    fn slot_props_coexists_with_accepts_or_only() {
        // RFC 084 doesn't change the accepts/only contract; both
        // can sit alongside `props = T`.
        let mut st = parse_struct_with_attr(quote! {
            #[slot(name = "row", only = [UploadFile], props = UploadItemProps)]
            struct Foo;
        });
        let slots = parse_and_strip_slots(&mut st.attrs).unwrap();
        assert_eq!(slots[0].mode, SlotMode::Only);
        assert_eq!(slots[0].accepts.len(), 1);
        assert!(slots[0].props.is_some());
    }

    #[test]
    fn slot_props_duplicate_rejected() {
        let attr_tokens: syn::ItemStruct = parse_struct_with_attr(quote! {
            #[slot(name = "row", props = A, props = B)]
            struct Foo;
        });
        let err = parse_slot_for_test(&attr_tokens.attrs[0]).unwrap_err();
        assert!(
            err.to_string().contains("at most once"),
            "duplicate `props =` should error: {}",
            err
        );
    }

    // ── RFC 084 Phase 2 — iterated-mode tests ──────────────────

    fn ast(src: &str) -> crate::template_parser::TemplateAst {
        crate::template_parser::parse_strict(src, "test.poco")
            .expect("template should parse cleanly")
    }

    fn props_path(s: &str) -> Path {
        syn::parse_str(s).expect("path parses")
    }

    fn iter_decl(name: &str, props: &str) -> SlotDecl {
        SlotDecl {
            name: SlotName::Named(name.to_string()),
            mode: SlotMode::Accepts,
            accepts: Vec::new(),
            props: Some(props_path(props)),
            span: proc_macro2::Span::call_site(),
        }
    }

    #[test]
    fn iterated_slot_emits_template_edit() {
        let src = r#"<root>
<ul>
  <li pp-for="file in files">
    <slot name="row"></slot>
  </li>
</ul>
</root>"#;
        let slots = vec![iter_decl("row", "UploadFile")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        assert_eq!(
            emit.template_edits.len(),
            1,
            "expected one auto-publish edit, got {:?}",
            emit.template_edits.len()
        );
        let edit = &emit.template_edits[0];
        assert!(
            edit.text.contains(":file=\"file\""),
            "edit should inject :file=\"file\": {:?}",
            edit.text
        );
        // The type-assertion closure should also appear in tokens.
        let s = emit.tokens.to_string();
        assert!(
            s.contains("__poco_this"),
            "type assertion missing from tokens: {s}"
        );
    }

    #[test]
    fn iterated_slot_skips_static_set_equality_check() {
        // Iterated mode auto-publishes a single key (the iter
        // var). Static-mode's set-equality check would error
        // ("publication doesn't cover every #[prop] field"); the
        // iterated path must NOT emit that check.
        let src = r#"<root>
<li pp-for="file in files">
  <slot name="row"></slot>
</li>
</root>"#;
        let slots = vec![iter_decl("row", "UploadFile")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        let s = emit.tokens.to_string();
        assert!(
            !s.contains("str_slice_set_eq_const"),
            "iterated slot must not emit static-mode set-equality check: {s}"
        );
    }

    #[test]
    fn static_slot_emits_no_template_edit() {
        // Static mode (any `:LHS=` present) must NOT trigger an
        // auto-publish template edit.
        let src = r#"<root>
<div>
  <slot name="header" :queue_size="queue_size"></slot>
</div>
</root>"#;
        let slots = vec![iter_decl("header", "HeaderProps")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        assert!(
            emit.template_edits.is_empty(),
            "static slot should yield no template edits: {:?}",
            emit.template_edits.len()
        );
    }

    #[test]
    fn slot_with_publications_inside_pp_for_is_static() {
        // §5.4 rule: presence of any `:LHS=` forces static mode
        // even when inside a `pp-for`. The macro must NOT
        // auto-publish (no template edit), AND must validate
        // the explicit publication against the props type.
        let src = r#"<root>
<li pp-for="file in files">
  <slot name="row" :file="file"></slot>
</li>
</root>"#;
        let slots = vec![iter_decl("row", "UploadFile")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        assert!(
            emit.template_edits.is_empty(),
            "static-mode (has :LHS=) must not emit an auto-publish edit"
        );
        let s = emit.tokens.to_string();
        assert!(
            s.contains("str_slice_set_eq_const"),
            "static-mode should emit set-equality coverage check: {s}"
        );
    }

    #[test]
    fn iterated_slot_outside_pp_for_falls_back_to_static() {
        // No pp-for ancestor AND no `:LHS=` → static-empty.
        // The coverage check fires because UploadFile has props.
        // (We can't run the const-eval check at test time but we
        // can confirm the tokens take the static path — no
        // template edit.)
        let src = r#"<root>
<slot name="row"></slot>
</root>"#;
        let slots = vec![iter_decl("row", "UploadFile")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        assert!(
            emit.template_edits.is_empty(),
            "no pp-for ancestor → no auto-publish edit"
        );
    }

    #[test]
    fn pp_for_complex_expression_errors_with_directive_message() {
        // §5.6 / §7 — `pp-for` over anything other than a bare
        // ident falls back to "use static mode" with an
        // actionable error.
        let src = r#"<root>
<li pp-for="file in user.files">
  <slot name="row"></slot>
</li>
</root>"#;
        let slots = vec![iter_decl("row", "UploadFile")];
        let emit = emit_slot_props_validation(
            &ast(src),
            &slots,
            &syn::Ident::new("TestHost", proc_macro2::Span::call_site()),
        );
        let s = emit.tokens.to_string();
        assert!(
            s.contains("compile_error") && s.contains("bare field path"),
            "complex pp-for expression should emit a directive compile_error: {s}"
        );
        assert!(
            emit.template_edits.is_empty(),
            "no edit when iterated-mode bails"
        );
    }

    #[test]
    fn apply_template_edits_inserts_in_reverse_order() {
        // Two edits at different positions should both land
        // intact without shifting each other.
        let src = "<slot></slot><slot></slot>";
        let edits = vec![
            TemplateEdit {
                insert_at: 5, // before first `>`
                text: " :a=\"a\"".to_string(),
            },
            TemplateEdit {
                insert_at: 18, // before second `>`
                text: " :b=\"b\"".to_string(),
            },
        ];
        let out = apply_template_edits(src, &edits);
        assert_eq!(out, r#"<slot :a="a"></slot><slot :b="b"></slot>"#);
    }
}
