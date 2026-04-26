//! RFC-058 Phase 2 — macro-time whole-template plan compilation.
//!
//! Walks the parsed `TemplateAst` (produced by RFC-050's
//! `template_parser::parse_strict`) and emits two artefacts the
//! macro then bakes into the component's registration:
//!
//! 1. A `&'static StaticTemplatePlan` literal describing every
//!    plan-eligible directive in the template — `pp-text`,
//!    `pp-html`, `pp-show`, `pp-bind:<attr>`, `pp-on:<event>`,
//!    `pp-ref`, deferred `pp-init`. Indexed by `node_path:
//!    &'static [u16]` over the cloned-template DOM, matching
//!    the convention RFC-054 row plans already use.
//! 2. A "cleaned HTML" string — the template re-serialised
//!    with the classified attributes stripped, plus the
//!    `data-pp-text-managed` marker stamped where `pp-text`
//!    was removed (so `interp::scan_children` can still
//!    distinguish planned text from `{...}` interpolation
//!    sites — see RFC-058 §5.4).
//!
//! v1 envelope per RFC-057 §6 (deferred to RFC-058 §6.2):
//!
//! * Eligible: native HTML elements only — every directive
//!   on or under a non-HTML5 tag is whole-subtree
//!   walker-owned (council pass 3 amendment).
//! * Eligible directives: `pp-text`, `pp-html`, `pp-show`,
//!   `pp-bind:<arg>` / `:<arg>`, `pp-on:<event>` / `@event`
//!   when every modifier is in the supported set, `pp-ref`,
//!   `pp-init` (deferred).
//! * Whole-subtree boundaries (walker-owned, classifier
//!   skips the subtree): `pp-for`, `pp-if`, `pp-teleport`,
//!   `<slot>`, every non-HTML5 tag.
//! * `pp-model` and `pp-route` are explicitly deferred (§7
//!   follow-ups).
//! * Listener modifier set: `prevent`, `stop`, `self`, `once`,
//!   `window`, `document`, `outside`, `capture`, key
//!   modifiers, `debounce` + numeric-ms pair.
//!
//! Every other attribute survives unchanged on the rewritten
//! HTML — it's the runtime walker's job to handle them as
//! today (attribute-preserved fallback, RFC-057 §8.1).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::template_parser::{Element, Node, TemplateAst};

/// Result of analysing one component's template.
pub(crate) struct EmittedTemplatePlan {
    /// `Some(quoted &'static StaticTemplatePlan)` when at least
    /// one plan entry was emitted; `None` when the template has
    /// nothing eligible (every directive is walker-owned or the
    /// template has no directives at all). The macro emits
    /// `register_template_plan` only when this is `Some`.
    pub plan_tokens: Option<TokenStream>,
    /// HTML the macro should pass to `register_template` instead
    /// of the raw `.poco` source. Classified attributes are
    /// stripped; `data-pp-text-managed` is stamped where
    /// `pp-text` was removed. `None` when the analysis emitted
    /// no entries — the caller falls back to the original
    /// source bytes.
    pub cleaned_html: Option<String>,
    /// RFC-058 Phase 3.5b — `fn` items the macro should emit
    /// inside the parent's `register()` body so the
    /// `StaticChildMount.slots` literal in `plan_tokens` can
    /// reference them by name. Empty token stream when the
    /// classifier didn't lift any slot content into a
    /// fragment.
    pub slot_fragment_fns: TokenStream,
    /// RFC-058 Phase 4.1d — `fn` items the macro should emit
    /// inside the parent's `register()` body so the
    /// `StaticIfPlan.body` literal in `plan_tokens` can
    /// reference them by ident. Empty token stream when the
    /// classifier didn't lift any pp-if body into a fragment.
    pub if_body_fns: TokenStream,
}

/// Walk the template AST, classify every directive, return the
/// emitted plan tokens + cleaned HTML. Behaviour-preserving
/// when nothing is eligible — `EmittedTemplatePlan { None,
/// None }` and the caller behaves as if this analysis didn't
/// run.
///
/// `row_plan_assignments` carries `(template_node_path,
/// plan_id)` pairs from the row-plan analyzer (RFC-058 §6.2
/// layering). When non-empty, the cleaned-HTML serializer
/// stamps `data-pp-row-plan="<id>"` onto each `<template
/// pp-for>` opening tag the row-plan analyzer claimed, so the
/// row-plan registry lookup still finds its target after the
/// template-plan rewrite. Empty slice = no row plans (or
/// row-plan analyser hadn't run) — same behaviour as
/// pre-§6.2 layering.
pub(crate) fn analyze_template_plan(
    ast: &TemplateAst,
    row_plan_assignments: &[(Vec<u16>, u32)],
) -> EmittedTemplatePlan {
    let mut ctx = AnalysisCtx {
        row_plan_assignments: row_plan_assignments.to_vec(),
        ..AnalysisCtx::default()
    };
    let mut path: Vec<u16> = Vec::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, &mut ctx, &mut path);
        }
    }
    if !ctx.has_any_entry() && row_plan_assignments.is_empty() {
        return EmittedTemplatePlan {
            plan_tokens: None,
            cleaned_html: None,
            slot_fragment_fns: TokenStream::new(),
            if_body_fns: TokenStream::new(),
        };
    }
    let cleaned_html = serialize_cleaned(&ast.roots, &ctx);
    let slot_fragment_fns = ctx.emit_slot_fragment_fns();
    let if_body_fns = ctx.emit_if_body_fns();
    // When the only "entry" is a row-plan stamp (template has no
    // plan-eligible directive on its own), still emit cleaned HTML
    // so the row-plan attribute is baked in — but skip the
    // `register_template_plan` call by leaving plan_tokens None.
    let plan_tokens = if ctx.has_any_entry() {
        Some(ctx.emit_plan_tokens())
    } else {
        None
    };
    EmittedTemplatePlan {
        plan_tokens,
        cleaned_html: Some(cleaned_html),
        slot_fragment_fns,
        if_body_fns,
    }
}

#[derive(Default)]
struct AnalysisCtx {
    bindings: Vec<BindingLite>,
    listeners: Vec<ListenerLite>,
    inits: Vec<InitLite>,
    refs: Vec<RefLite>,
    /// RFC-058 Phase 3.3 — child-component mount sites the
    /// classifier discovered (every non-HTML5 tag inside the
    /// plan-eligible portion of the template, excluding
    /// `<slot>` / `<template>`-block wrappers / pp-for /
    /// pp-if / pp-teleport subtrees). The runtime applier
    /// invokes [`crate::walker::mount_child_component`] for
    /// each before the walker recurses, and the walker's
    /// `__pp_mounted` guard makes the discovery a no-op for
    /// any tag the plan already mounted.
    child_mounts: Vec<ChildMountLite>,
    /// RFC-058 Phase 3.5b — `(fn_ident, slot_html)` pairs the
    /// macro should emit as `fn` items inside the parent's
    /// generated `register()` body. Each fragment's `fn` body
    /// is `::pocopine::__private::stamp_static_html(ctx.host,
    /// "<slot_html>")`. Indexed by `ChildMountLite.slot_fragments`
    /// — the same ident appears in both places so the
    /// `StaticChildMount.slots` literal references the function
    /// the macro emits below it.
    slot_fragment_emissions: Vec<(syn::Ident, String)>,
    /// RFC-058 Phase 4.1d — `pp-if` body fragments the macro
    /// should emit as `fn` items inside the parent's
    /// `register()` body. Each fragment's body builds a
    /// `StaticTemplatePlan` const for the body's
    /// bindings/listeners/refs, then calls `stamp_if_body`
    /// against the parent scope to materialise + install at
    /// mount time. Indexed by `IfPlanLite.body_fn_ident` —
    /// the same ident appears in both places so the
    /// `StaticIfPlan.body` literal references the function
    /// the macro emits below it.
    if_body_emissions: Vec<IfBodyEmission>,
    /// RFC-058 Phase 4.1b — `pp-if` controller sites the
    /// classifier lifted out of the runtime walker's
    /// directive-dispatch path. Each entry pins a
    /// `<template>`'s `node_path` + the truthy expression
    /// source; the runtime applier resolves the template,
    /// parses the expression, and calls
    /// [`crate::directives::if_::install`].
    if_plans: Vec<IfPlanLite>,
    /// RFC-058 Phase 4.2 — `pp-for` controller sites the
    /// classifier lifted out of the runtime walker's
    /// directive-dispatch path. The classifier parses
    /// `pp-for="<item> in <items>"` at compile time + reads
    /// `pp-key` / `pp-stagger` siblings into the entry; the
    /// applier hands the pre-resolved pieces to
    /// [`crate::directives::for_::install`].
    for_plans: Vec<ForPlanLite>,
    /// RFC-058 Phase 4.3 — `pp-teleport` controller sites
    /// without a co-occurring `pp-if`. Each entry pins the
    /// `<template>`'s `node_path` + the literal target
    /// selector; the runtime applier resolves the target and
    /// calls [`crate::directives::teleport::install`].
    teleport_plans: Vec<TeleportPlanLite>,
    /// Set of (node_path, attr_name) entries the cleaned-HTML
    /// serializer should drop. Lookup is O(scan) per attribute
    /// — fine at typical template sizes.
    stripped: Vec<StrippedAttr>,
    /// Node paths where the macro removed `pp-text` and the
    /// serializer should stamp `data-pp-text-managed`.
    text_managed_paths: Vec<Vec<u16>>,
    /// RFC-058 §6.2 — `(template_node_path, plan_id)` pairs
    /// from the row-plan analyser. The cleaned-HTML serializer
    /// stamps `data-pp-row-plan="<id>"` onto each pp-for
    /// `<template>` opening tag whose path matches an entry,
    /// so the runtime row-plan registry lookup still finds its
    /// target after the template-plan rewrite.
    row_plan_assignments: Vec<(Vec<u16>, u32)>,
}

struct StrippedAttr {
    node_path: Vec<u16>,
    name: String,
}

struct BindingLite {
    node_path: Vec<u16>,
    kind: BindingKindLite,
    expr_src: String,
}

enum BindingKindLite {
    Text,
    Html,
    /// `pp-bind:<arg>` / `:<arg>`. Static `&'static str` arg
    /// because `BindingKind::Bind` carries it as such on the
    /// runtime side.
    Bind {
        arg: String,
    },
    Show,
}

struct ListenerLite {
    node_path: Vec<u16>,
    event: String,
    modifiers: Vec<String>,
    expr_src: String,
}

struct InitLite {
    node_path: Vec<u16>,
    expr_src: String,
}

struct RefLite {
    node_path: Vec<u16>,
    name: String,
}

struct IfBodyEmission {
    ident: syn::Ident,
    html: String,
    bindings: Vec<BindingLite>,
    listeners: Vec<ListenerLite>,
    refs: Vec<RefLite>,
}

struct IfPlanLite {
    template_node_path: Vec<u16>,
    expr_src: String,
    /// RFC-058 Phase 4.1d — `Some` when the body subtree was
    /// lift-eligible and the macro emitted a body fragment fn
    /// the `StaticIfPlan` literal should reference. `None`
    /// when the body falls outside the v1 envelope (nested
    /// custom tag, `<slot>`, nested controller, `pp-init`,
    /// etc.) — the runtime installer falls back to the legacy
    /// `clone_template_body` + `walker::walk` path.
    body_fn_ident: Option<syn::Ident>,
}

struct ForPlanLite {
    template_node_path: Vec<u16>,
    item_name: String,
    items_expr: String,
    key_expr: Option<String>,
    stagger_ms: u32,
    /// RFC-058 Phase 4.2c — `Some` when the row body subtree
    /// was lift-eligible AND no RFC-054 row plan claimed the
    /// same site. The macro emits a body fragment fn the
    /// `StaticForPlan.body` literal references.
    body_fn_ident: Option<syn::Ident>,
}

struct TeleportPlanLite {
    template_node_path: Vec<u16>,
    selector: String,
}

struct ChildMountLite {
    node_path: Vec<u16>,
    tag: String,
    /// `(slot_name, generated_fn_ident)` pairs for every
    /// statically-eligible slot the macro lifted into a
    /// fragment function. Empty when the parent left no
    /// children inside the custom tag, or when the children
    /// contained anything walker-only (any `pp-*` / `@` /
    /// `:` directive, any non-HTML5 tag, any `<slot>` / pp-let
    /// / pp-for / pp-if / pp-teleport descendant).
    ///
    /// The idents match entries in
    /// [`AnalysisCtx::slot_fragment_emissions`] so the
    /// `StaticSlotFragment.fragment` literal in the plan
    /// references the same `fn` the macro emits below.
    slot_fragments: Vec<(String, syn::Ident)>,
}

impl AnalysisCtx {
    fn has_any_entry(&self) -> bool {
        !self.bindings.is_empty()
            || !self.listeners.is_empty()
            || !self.inits.is_empty()
            || !self.refs.is_empty()
            || !self.child_mounts.is_empty()
            || !self.if_plans.is_empty()
            || !self.for_plans.is_empty()
            || !self.teleport_plans.is_empty()
    }

    fn is_stripped(&self, node_path: &[u16], attr_name: &str) -> bool {
        self.stripped
            .iter()
            .any(|s| s.node_path == node_path && s.name == attr_name)
    }

    fn is_text_managed(&self, node_path: &[u16]) -> bool {
        self.text_managed_paths
            .iter()
            .any(|p| p.as_slice() == node_path)
    }

    /// Returns `Some(plan_id)` when the row-plan analyser
    /// assigned a row plan to the `<template pp-for>` at
    /// `node_path`. The cleaned-HTML serializer uses this to
    /// stamp `data-pp-row-plan="<id>"` so the runtime row-plan
    /// registry finds its target after the rewrite.
    fn row_plan_id(&self, node_path: &[u16]) -> Option<u32> {
        self.row_plan_assignments
            .iter()
            .find(|(p, _)| p.as_slice() == node_path)
            .map(|(_, id)| *id)
    }

    /// RFC-058 Phase 4.1d — generate one
    /// `fn <ident>(scope_id, proxy) -> Option<Element> { … }`
    /// item per `pp-if` body fragment the analyser lifted.
    /// The body builds a `StaticTemplatePlan` const for the
    /// body's bindings/listeners/refs (no inits, no nested
    /// controllers, no slots — v1 envelope guarantees that),
    /// then calls `stamp_if_body` to materialise + install
    /// against the parent scope.
    fn emit_if_body_fns(&self) -> TokenStream {
        let items = self.if_body_emissions.iter().map(|emission| {
            let ident = &emission.ident;
            let html_lit = proc_macro2::Literal::string(&emission.html);
            let bindings_tokens = emission.bindings.iter().map(emit_binding);
            let listeners_tokens = emission.listeners.iter().map(emit_listener);
            let refs_tokens = emission.refs.iter().map(emit_ref);
            quote! {
                fn #ident(
                    scope_id: ::pocopine::ScopeId,
                    proxy: &::pocopine::__private::JsValue,
                ) -> ::core::option::Option<::pocopine::__private::web_sys::Element> {
                    const PLAN: ::pocopine::__private::StaticTemplatePlan =
                        ::pocopine::__private::StaticTemplatePlan {
                            bindings: &[ #(#bindings_tokens),* ],
                            listeners: &[ #(#listeners_tokens),* ],
                            inits: &[],
                            refs: &[ #(#refs_tokens),* ],
                            child_mounts: &[],
                            if_plans: &[],
                            for_plans: &[],
                            teleport_plans: &[],
                        };
                    ::pocopine::__private::stamp_if_body(
                        #html_lit,
                        &PLAN,
                        scope_id,
                        proxy,
                    )
                }
            }
        });
        quote! { #(#items)* }
    }

    /// Generate one `fn <ident>(ctx: SlotMountCtx) { … }` item
    /// per emitted slot fragment. The macro splices the
    /// returned `TokenStream` into the parent's `register()`
    /// body so the fragment idents are in scope when the
    /// `StaticTemplatePlan` literal references them. Empty
    /// stream when no fragment was emitted — same as today's
    /// pre-Phase-3.5b behaviour.
    fn emit_slot_fragment_fns(&self) -> TokenStream {
        let items = self.slot_fragment_emissions.iter().map(|(ident, html)| {
            let html_lit = proc_macro2::Literal::string(html);
            quote! {
                fn #ident(ctx: ::pocopine::__private::SlotMountCtx<'_>) {
                    ::pocopine::__private::stamp_static_html(ctx.host, #html_lit);
                }
            }
        });
        quote! { #(#items)* }
    }

    fn emit_plan_tokens(&self) -> TokenStream {
        let bindings_tokens = self.bindings.iter().map(emit_binding);
        let listeners_tokens = self.listeners.iter().map(emit_listener);
        let inits_tokens = self.inits.iter().map(emit_init);
        let refs_tokens = self.refs.iter().map(emit_ref);
        let child_mounts_tokens = self.child_mounts.iter().map(emit_child_mount);
        let if_plans_tokens = self.if_plans.iter().map(emit_if_plan);
        let for_plans_tokens = self.for_plans.iter().map(emit_for_plan);
        let teleport_plans_tokens = self.teleport_plans.iter().map(emit_teleport_plan);
        quote! {
            ::pocopine::__private::StaticTemplatePlan {
                bindings: &[ #(#bindings_tokens),* ],
                listeners: &[ #(#listeners_tokens),* ],
                inits: &[ #(#inits_tokens),* ],
                refs: &[ #(#refs_tokens),* ],
                child_mounts: &[ #(#child_mounts_tokens),* ],
                if_plans: &[ #(#if_plans_tokens),* ],
                for_plans: &[ #(#for_plans_tokens),* ],
                teleport_plans: &[ #(#teleport_plans_tokens),* ],
            }
        }
    }
}

fn emit_node_path(path: &[u16]) -> TokenStream {
    let elems = path.iter().map(|i| {
        let lit = proc_macro2::Literal::u16_unsuffixed(*i);
        quote! { #lit }
    });
    quote! { &[ #(#elems),* ] }
}

fn emit_binding(b: &BindingLite) -> TokenStream {
    let path = emit_node_path(&b.node_path);
    let expr = proc_macro2::Literal::string(&b.expr_src);
    let kind = match &b.kind {
        BindingKindLite::Text => quote! { ::pocopine::__private::BindingKind::Text },
        BindingKindLite::Html => quote! { ::pocopine::__private::BindingKind::Html },
        BindingKindLite::Show => quote! { ::pocopine::__private::BindingKind::Show },
        BindingKindLite::Bind { arg } => {
            let arg_lit = proc_macro2::Literal::string(arg);
            quote! { ::pocopine::__private::BindingKind::Bind { arg: #arg_lit } }
        }
    };
    quote! {
        ::pocopine::__private::StaticBinding {
            node_path: #path,
            kind: #kind,
            expr_src: #expr,
        }
    }
}

fn emit_listener(l: &ListenerLite) -> TokenStream {
    let path = emit_node_path(&l.node_path);
    let event_lit = proc_macro2::Literal::string(&l.event);
    let expr_lit = proc_macro2::Literal::string(&l.expr_src);
    let modifier_tokens = l.modifiers.iter().map(|m| {
        let lit = proc_macro2::Literal::string(m);
        quote! { #lit }
    });
    quote! {
        ::pocopine::__private::StaticListener {
            node_path: #path,
            event: #event_lit,
            modifiers: &[ #(#modifier_tokens),* ],
            expr_src: #expr_lit,
        }
    }
}

fn emit_init(i: &InitLite) -> TokenStream {
    let path = emit_node_path(&i.node_path);
    let expr = proc_macro2::Literal::string(&i.expr_src);
    quote! {
        ::pocopine::__private::StaticInit {
            node_path: #path,
            expr_src: #expr,
        }
    }
}

fn emit_ref(r: &RefLite) -> TokenStream {
    let path = emit_node_path(&r.node_path);
    let name = proc_macro2::Literal::string(&r.name);
    quote! {
        ::pocopine::__private::StaticRef {
            node_path: #path,
            name: #name,
        }
    }
}

fn emit_if_plan(ip: &IfPlanLite) -> TokenStream {
    let path = emit_node_path(&ip.template_node_path);
    let expr = proc_macro2::Literal::string(&ip.expr_src);
    // RFC-058 Phase 4.1d-c will populate `body` with a
    // macro-emitted `IfBodyFn` when the body subtree qualifies
    // for fragment lifting; v1 ships `None` so every site
    // routes through the legacy `clone_template_body` +
    // `walker::walk` path the runtime applier already drives.
    let body_tokens = match &ip.body_fn_ident {
        Some(ident) => quote! { ::core::option::Option::Some(#ident) },
        None => quote! { ::core::option::Option::None },
    };
    quote! {
        ::pocopine::__private::StaticIfPlan {
            template_node_path: #path,
            expr_src: #expr,
            body: #body_tokens,
        }
    }
}

fn emit_teleport_plan(tp: &TeleportPlanLite) -> TokenStream {
    let path = emit_node_path(&tp.template_node_path);
    let sel = proc_macro2::Literal::string(&tp.selector);
    quote! {
        ::pocopine::__private::StaticTeleportPlan {
            template_node_path: #path,
            selector: #sel,
        }
    }
}

fn emit_for_plan(fp: &ForPlanLite) -> TokenStream {
    let path = emit_node_path(&fp.template_node_path);
    let item_lit = proc_macro2::Literal::string(&fp.item_name);
    let items_lit = proc_macro2::Literal::string(&fp.items_expr);
    let key_tokens = match fp.key_expr.as_deref() {
        Some(k) => {
            let lit = proc_macro2::Literal::string(k);
            quote! { ::core::option::Option::Some(#lit) }
        }
        None => quote! { ::core::option::Option::None },
    };
    let stagger = proc_macro2::Literal::u32_unsuffixed(fp.stagger_ms);
    let body_tokens = match &fp.body_fn_ident {
        Some(ident) => quote! { ::core::option::Option::Some(#ident) },
        None => quote! { ::core::option::Option::None },
    };
    quote! {
        ::pocopine::__private::StaticForPlan {
            template_node_path: #path,
            item_name: #item_lit,
            items_expr: #items_lit,
            key_expr: #key_tokens,
            stagger_ms: #stagger,
            body: #body_tokens,
        }
    }
}

fn emit_child_mount(c: &ChildMountLite) -> TokenStream {
    let path = emit_node_path(&c.node_path);
    let tag = proc_macro2::Literal::string(&c.tag);
    let slot_tokens = c.slot_fragments.iter().map(|(name, ident)| {
        let name_lit = proc_macro2::Literal::string(name);
        quote! {
            ::pocopine::__private::StaticSlotFragment {
                name: #name_lit,
                fragment: #ident,
            }
        }
    });
    quote! {
        ::pocopine::__private::StaticChildMount {
            node_path: #path,
            tag: #tag,
            slots: &[ #(#slot_tokens),* ],
        }
    }
}

// ─── walk + classification ───────────────────────────────────────

fn walk(el: &Element, ctx: &mut AnalysisCtx, path: &mut Vec<u16>) {
    if el.synthetic {
        // Synthetic elements (html5ever auto-inserted) confuse
        // the path-indexing model since the runtime walks
        // authored structure. Skip them entirely — every
        // directive on or under a synthetic node falls back to
        // the walker.
        return;
    }

    // RFC-058 Phase 4.2 — `pp-for` on a `<template>` host
    // graduates into a `StaticForPlan` entry. Same eligibility
    // shape as Phase 4.1's pp-if: must be on `<template>`,
    // parseable `<item> in <items>`, no co-occurring
    // `pp-teleport` (defer that combo to the walker — the
    // applier doesn't capture teleport targets in v1). The
    // `data-pp-row-plan` attribute the §6.2 layering bakes
    // into the cleaned HTML stays alongside the strip so the
    // RFC-054 row-plan registry still resolves keyed lists.
    if let Some(for_attr) = pp_for_value(el) {
        if el.tag == "template" && !el.attrs.iter().any(|(n, _)| n == "pp-teleport") {
            if let Some((item_name, items_expr)) = parse_pp_for(&for_attr) {
                let key_expr = el
                    .attrs
                    .iter()
                    .find(|(n, _)| n == "pp-key")
                    .map(|(_, v)| v.clone())
                    .filter(|s| !s.trim().is_empty());
                let stagger_ms = el
                    .attrs
                    .iter()
                    .find(|(n, _)| n == "pp-stagger")
                    .and_then(|(_, v)| v.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                // RFC-058 Phase 4.2c — try to lift the row body
                // into a fragment fn. Skip when an RFC-054 row
                // plan claims this site (the row-plan fast path
                // is strictly better than per-row
                // `apply_static_plan` — proxy elision, no
                // per-row effect creation, etc.).
                let row_plan_claims_site = ctx
                    .row_plan_assignments
                    .iter()
                    .any(|(p, _)| p.as_slice() == path.as_slice());
                let body_fn_ident = if row_plan_claims_site {
                    None
                } else {
                    analyze_lift_body(el).map(|(html, body_ctx)| {
                        let ident = format_ident!("__poc_for_body_{}", ctx.if_body_emissions.len());
                        ctx.if_body_emissions.push(IfBodyEmission {
                            ident: ident.clone(),
                            html,
                            bindings: body_ctx.bindings,
                            listeners: body_ctx.listeners,
                            refs: body_ctx.refs,
                        });
                        ident
                    })
                };
                ctx.for_plans.push(ForPlanLite {
                    template_node_path: path.clone(),
                    item_name,
                    items_expr,
                    key_expr,
                    stagger_ms,
                    body_fn_ident,
                });
                ctx.stripped.push(StrippedAttr {
                    node_path: path.clone(),
                    name: "pp-for".to_string(),
                });
                ctx.stripped.push(StrippedAttr {
                    node_path: path.clone(),
                    name: "pp-key".to_string(),
                });
                ctx.stripped.push(StrippedAttr {
                    node_path: path.clone(),
                    name: "pp-stagger".to_string(),
                });
                // Don't recurse — the row body is owned by the
                // RFC-054 row plan (when present) or the walker
                // (when absent). Either way the template-plan
                // classifier doesn't follow `<template>` content.
                return;
            }
        }
        // Ineligible (wrong host, has pp-teleport, or expr
        // doesn't parse) — fall through to block-boundary skip
        // so today's walker dispatch handles it.
        return;
    }

    // RFC-058 Phase 4.3 — `pp-teleport` on a `<template>` host
    // graduates into a `StaticTeleportPlan` entry. Eligibility:
    // host is `<template>`, no co-occurring `pp-if` (pp-if owns
    // the mount cycle in that combo and reads pp-teleport
    // itself), no co-occurring `pp-for` (pp-for graduated above
    // and shouldn't be paired with pp-teleport on the same
    // element — degenerate case, leave on walker).
    if let Some(selector) = pp_teleport_value(el) {
        let has_if = el.attrs.iter().any(|(n, _)| n == "pp-if");
        let has_for = el.attrs.iter().any(|(n, _)| n == "pp-for");
        if el.tag == "template" && !has_if && !has_for && !selector.trim().is_empty() {
            ctx.teleport_plans.push(TeleportPlanLite {
                template_node_path: path.clone(),
                selector,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.clone(),
                name: "pp-teleport".to_string(),
            });
            return;
        }
        // Ineligible (wrong host, has pp-if/pp-for, empty
        // selector) — leave the walker to dispatch.
        return;
    }

    // Whole-element block boundary: `<slot>`. Every directive
    // on or under it stays on the walker.
    if is_block_boundary(el) {
        return;
    }

    // RFC-058 Phase 4.1b — `pp-if` on a `<template>` host
    // graduates into a `StaticIfPlan` entry. The applier
    // resolves the template + parses the expression at compile
    // time; the macro strips the `pp-if` attribute from the
    // cleaned HTML so the runtime walker's directive-dispatch
    // path doesn't double-install the effect. The template
    // body lives in `<template>.content` (a separate
    // `DocumentFragment` that doesn't appear in `el.children`),
    // so body content stays on the walker — exactly like
    // today's clone+walk path. Phase 4.1c+ will lift the body
    // into a fragment function.
    //
    // Co-occurrence with `pp-teleport` falls back to the
    // walker for v1 — the templates_plan applier doesn't
    // capture the teleport target, so leaving the
    // pp-if attribute on the cleaned HTML keeps today's
    // walker pipeline running.
    if let Some(if_expr) = pp_if_value(el) {
        if el.tag == "template"
            && !el.attrs.iter().any(|(n, _)| n == "pp-teleport")
            && pocopine_expr::parse(&if_expr).is_ok()
        {
            // RFC-058 Phase 4.1d — try to lift the body
            // subtree into a fragment fn the runtime installer
            // invokes instead of `clone_template_body` +
            // `walker::walk`. v1 envelope is narrow (HTML5
            // natives + plan-eligible directives only); when
            // the body falls outside, `body_fn_ident` stays
            // `None` and the legacy clone+walk path runs.
            let body_fn_ident = analyze_lift_body(el).map(|(html, body_ctx)| {
                let ident = format_ident!("__poc_if_body_{}", ctx.if_body_emissions.len());
                ctx.if_body_emissions.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    bindings: body_ctx.bindings,
                    listeners: body_ctx.listeners,
                    refs: body_ctx.refs,
                });
                ident
            });
            ctx.if_plans.push(IfPlanLite {
                template_node_path: path.clone(),
                expr_src: if_expr,
                body_fn_ident,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.clone(),
                name: "pp-if".to_string(),
            });
            return;
        }
        // Ineligible (wrong host, has pp-teleport, or expr
        // doesn't parse) — fall through to walker as today.
        return;
    }

    // Whole-subtree boundary: non-HTML5 tags (per council pass 3
    // amendment to RFC-058 §6.2). The element's own attributes
    // and descendants stay walker-owned (slot content is the
    // common case there), but RFC-058 Phase 3 captures the
    // mount site itself: the runtime applier calls
    // [`crate::walker::mount_child_component`] before the
    // walker's recursive descent reaches the tag, and the
    // walker's `__pp_mounted` guard turns the discovery into a
    // no-op afterwards.
    if !is_html5_native(&el.tag) {
        // RFC-058 Phase 3.5b — if the custom tag's children are
        // entirely static (no `pp-*` / `@` / `:` attrs anywhere
        // in the subtree, no nested non-HTML5 tags, no `<slot>`
        // element), lift them into a fragment function. The
        // parent passes that fragment via the static-plan
        // child-mount entry; the runtime walker's
        // `materialize_slot` invokes it instead of running the
        // legacy capture/replay path. Anything dynamic stays on
        // the walker — slot content with directives needs the
        // parent-proxy machinery this v1 doesn't ship yet.
        let mut slot_fragments: Vec<(String, syn::Ident)> = Vec::new();
        if !el.children.is_empty() && slot_subtree_is_static(&el.children) {
            let html = serialize_slot_children(&el.children);
            let ident = format_ident!("__poc_slot_frag_{}", ctx.slot_fragment_emissions.len());
            ctx.slot_fragment_emissions.push((ident.clone(), html));
            slot_fragments.push(("default".to_string(), ident));
        }
        ctx.child_mounts.push(ChildMountLite {
            node_path: path.clone(),
            tag: el.tag.clone(),
            slot_fragments,
        });
        return;
    }

    // Classify every attribute on this element.
    let mut listener_unsupported_modifier = false;
    let mut had_text = false;
    for (name, value) in &el.attrs {
        match classify_attr(name, value, path, ctx, &mut listener_unsupported_modifier) {
            ClassifyOutcome::Stripped { is_text } => {
                if is_text {
                    had_text = true;
                }
            }
            ClassifyOutcome::Preserved => {}
        }
    }
    if had_text {
        ctx.text_managed_paths.push(path.clone());
    }
    let _ = listener_unsupported_modifier; // already handled per-attr

    // Recurse into element children. Path indices are over
    // *element* children only — text / comments don't shift the
    // index (matches `Element.children` in JS DOM and the
    // for_plan walker's convention).
    for (i, child) in el.children.iter().enumerate() {
        if let Node::Element(child_el) = child {
            let idx = el
                .children
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            walk(child_el, ctx, path);
            path.pop();
        }
    }
}

fn is_html5_native(tag: &str) -> bool {
    crate::HTML5_ELEMENTS.binary_search(&tag).is_ok()
}

fn is_block_boundary(el: &Element) -> bool {
    el.tag == "slot"
}

fn pp_if_value(el: &Element) -> Option<String> {
    el.attrs
        .iter()
        .find(|(n, _)| n == "pp-if")
        .map(|(_, v)| v.clone())
}

fn pp_for_value(el: &Element) -> Option<String> {
    el.attrs
        .iter()
        .find(|(n, _)| n == "pp-for")
        .map(|(_, v)| v.clone())
}

fn pp_teleport_value(el: &Element) -> Option<String> {
    el.attrs
        .iter()
        .find(|(n, _)| n == "pp-teleport")
        .map(|(_, v)| v.clone())
}

/// Mirror of `crate::directives::for_::parse_expr` so the
/// classifier can pre-validate `pp-for="<item> in <items>"`.
fn parse_pp_for(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let (lhs, rhs) = s.split_once(" in ")?;
    let ident = lhs.trim();
    let items = rhs.trim();
    if ident.is_empty() || items.is_empty() {
        return None;
    }
    if !ident.chars().all(|c| c.is_alphanumeric() || c == '_')
        || ident.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((ident.to_string(), items.to_string()))
}

enum ClassifyOutcome {
    Stripped { is_text: bool },
    Preserved,
}

fn classify_attr(
    name: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
    listener_unsupported_modifier: &mut bool,
) -> ClassifyOutcome {
    // RFC-020 listener shorthand: `@event[.mod]`.
    if let Some(rest) = name.strip_prefix('@') {
        return classify_listener(rest, value, path, ctx, listener_unsupported_modifier);
    }
    // RFC-020 bind shorthand: `:<arg>` (NOT `::` which is
    // illegal in HTML and therefore not a real attribute name).
    if let Some(arg) = name.strip_prefix(':') {
        if arg.is_empty() {
            return ClassifyOutcome::Preserved;
        }
        return classify_bind(arg, value, path, ctx);
    }
    if let Some(rest) = name.strip_prefix("pp-") {
        // pp-text="<expr>"
        if rest == "text" {
            if pocopine_expr::parse(value).is_err() {
                return ClassifyOutcome::Preserved;
            }
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Text,
                expr_src: value.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped { is_text: true };
        }
        // pp-html="<expr>"
        if rest == "html" {
            if pocopine_expr::parse(value).is_err() {
                return ClassifyOutcome::Preserved;
            }
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Html,
                expr_src: value.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped { is_text: false };
        }
        // pp-show="<expr>"
        if rest == "show" {
            if pocopine_expr::parse(value).is_err() {
                return ClassifyOutcome::Preserved;
            }
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Show,
                expr_src: value.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped { is_text: false };
        }
        // pp-bind:<arg>="<expr>"
        if let Some(arg) = rest.strip_prefix("bind:") {
            return classify_bind_full(name, arg, value, path, ctx);
        }
        // pp-on:<event>[.<mod>]="<expr>"
        if let Some(rest) = rest.strip_prefix("on:") {
            return classify_listener(rest, value, path, ctx, listener_unsupported_modifier);
        }
        // pp-ref="<name>"
        if rest == "ref" {
            // `pp-ref` value is a static name, not an expression
            // — empty / whitespace is an author bug; let the
            // walker surface it.
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return ClassifyOutcome::Preserved;
            }
            ctx.refs.push(RefLite {
                node_path: path.to_vec(),
                name: trimmed.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped { is_text: false };
        }
        // pp-init="<expr>"
        if rest == "init" {
            if pocopine_expr::parse(value).is_err() {
                return ClassifyOutcome::Preserved;
            }
            ctx.inits.push(InitLite {
                node_path: path.to_vec(),
                expr_src: value.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped { is_text: false };
        }
        // Every other pp-* attribute (pp-data, pp-cloak,
        // pp-model, pp-route, pp-anchor, pp-resize,
        // pp-intersect, pp-roving, pp-flip, pp-transition:*,
        // pp-stagger, etc.) — preserved on the cleaned HTML
        // and handled by the runtime walker as today.
        return ClassifyOutcome::Preserved;
    }
    // Plain HTML attribute — preserved.
    ClassifyOutcome::Preserved
}

fn classify_bind(arg: &str, value: &str, path: &[u16], ctx: &mut AnalysisCtx) -> ClassifyOutcome {
    // `:<arg>` shorthand. Modifier suffix (`.<mod>`) is
    // currently unsupported in the planned envelope; preserve
    // the whole binding in that case.
    if arg.contains('.') {
        return ClassifyOutcome::Preserved;
    }
    classify_bind_inner(&format!(":{arg}"), arg, value, path, ctx)
}

fn classify_bind_full(
    full_name: &str,
    arg: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
) -> ClassifyOutcome {
    if arg.contains('.') {
        return ClassifyOutcome::Preserved;
    }
    classify_bind_inner(full_name, arg, value, path, ctx)
}

fn classify_bind_inner(
    attr_to_strip: &str,
    arg: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
) -> ClassifyOutcome {
    if arg.is_empty() {
        return ClassifyOutcome::Preserved;
    }
    if pocopine_expr::parse(value).is_err() {
        return ClassifyOutcome::Preserved;
    }
    ctx.bindings.push(BindingLite {
        node_path: path.to_vec(),
        kind: BindingKindLite::Bind {
            arg: arg.to_string(),
        },
        expr_src: value.to_string(),
    });
    ctx.stripped.push(StrippedAttr {
        node_path: path.to_vec(),
        name: attr_to_strip.to_string(),
    });
    ClassifyOutcome::Stripped { is_text: false }
}

fn classify_listener(
    rest: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
    unsupported: &mut bool,
) -> ClassifyOutcome {
    // `rest` is `event[.mod1.mod2…]`.
    let mut parts = rest.split('.');
    let event = match parts.next() {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => return ClassifyOutcome::Preserved,
    };
    let modifiers: Vec<String> = parts.map(|m| m.to_string()).collect();
    if !modifiers.iter().all(|m| is_supported_modifier(m)) {
        // RFC-057 §6.1 — unsupported modifier means the whole
        // listener stays attribute-preserved so the runtime
        // walker picks it up unchanged.
        *unsupported = true;
        return ClassifyOutcome::Preserved;
    }
    if pocopine_expr::parse(value).is_err() {
        return ClassifyOutcome::Preserved;
    }
    let stripped_name = if rest.starts_with(|_: char| false) {
        rest.to_string()
    } else {
        // We don't know whether the source attribute was
        // `@event...` or `pp-on:event...` — caller passes the
        // post-prefix `rest`. The full attribute name is
        // reconstructed from the caller's match arm, but we
        // need both shapes — stash both forms; the serializer
        // checks both.
        rest.to_string()
    };
    ctx.listeners.push(ListenerLite {
        node_path: path.to_vec(),
        event,
        modifiers,
        expr_src: value.to_string(),
    });
    // Strip both possible source spellings — the cleaned-HTML
    // serializer drops whichever the author wrote.
    ctx.stripped.push(StrippedAttr {
        node_path: path.to_vec(),
        name: format!("@{stripped_name}"),
    });
    ctx.stripped.push(StrippedAttr {
        node_path: path.to_vec(),
        name: format!("pp-on:{stripped_name}"),
    });
    ClassifyOutcome::Stripped { is_text: false }
}

fn is_supported_modifier(m: &str) -> bool {
    matches!(
        m,
        "prevent" | "stop" | "self" | "once" | "window" | "document" | "outside" | "capture"
    ) || is_key_modifier(m)
        || is_debounce_modifier(m)
        || is_debounce_ms(m)
}

fn is_key_modifier(m: &str) -> bool {
    matches!(
        m,
        "ctrl"
            | "shift"
            | "alt"
            | "meta"
            | "enter"
            | "escape"
            | "tab"
            | "space"
            | "arrow-up"
            | "arrow-down"
            | "arrow-left"
            | "arrow-right"
    ) || m.len() == 1
}

fn is_debounce_modifier(m: &str) -> bool {
    m == "debounce"
}

fn is_debounce_ms(m: &str) -> bool {
    m.parse::<u32>().is_ok()
}

// ─── pp-if body lift eligibility + analysis ──────────────────────

/// RFC-058 Phase 4.1d v1 envelope — `true` when every node in
/// the `pp-if` body subtree is safe to install via the Phase
/// 1 helpers + `apply_static_plan`. Anything outside this
/// envelope falls back to the legacy `clone_template_body` +
/// `walker::walk` path the controller already drives.
///
/// Excludes:
///   * non-HTML5 tags (would need recursive child-mount
///     emission inside a body fragment — Phase 4.1d v2);
///   * `<slot>` elements (would need slot capture/replay
///     hooks inside a body fragment — Phase 3.5c+);
///   * nested controllers (`pp-for` / `pp-if` / `pp-teleport`)
///     — same scope-acrobatics they already have at template
///     level, but inside a fragment hosted on the parent
///     scope. Defer to v2;
///   * `pp-init` (the deferred-fire ordering depends on the
///     walker's post-order drain — fragment paths don't
///     trigger it);
///   * `pp-data` / `pp-model` / `pp-route` (component scope
///     boundaries — the body fragment installs against the
///     enclosing scope only).
fn if_body_subtree_is_eligible(el: &Element) -> bool {
    if el.synthetic {
        for child in &el.children {
            if let Node::Element(child_el) = child {
                if !if_body_subtree_is_eligible(child_el) {
                    return false;
                }
            }
        }
        return true;
    }
    if el.tag == "slot" {
        return false;
    }
    if !is_html5_native(&el.tag) {
        return false;
    }
    for (name, _) in &el.attrs {
        if name == "pp-for"
            || name == "pp-if"
            || name == "pp-teleport"
            || name == "pp-init"
            || name == "pp-data"
            || name == "pp-model"
            || name.starts_with("pp-model:")
            || name == "pp-route"
        {
            return false;
        }
    }
    for child in &el.children {
        if let Node::Element(child_el) = child {
            if !if_body_subtree_is_eligible(child_el) {
                return false;
            }
        }
    }
    true
}

/// Analyse a `<template>` element's body subtree for fragment
/// lifting (shared between `pp-if` Phase 4.1d and `pp-for`
/// Phase 4.2c — both use the same v1 envelope and per-mount
/// install pattern). Returns `Some` when the body's element
/// children reduce to a single root + the subtree passes
/// `if_body_subtree_is_eligible`. The cleaned HTML +
/// collected bindings/listeners/refs flow into the macro's
/// `if_body_emissions` slot for later `fn` emission.
fn analyze_lift_body(template_el: &Element) -> Option<(String, AnalysisCtx)> {
    // Single element child — same constraint pp-if::install
    // already enforces at runtime via `clone_template_body`.
    let mut elements: Vec<&Element> = Vec::new();
    for child in &template_el.children {
        if let Node::Element(child_el) = child {
            elements.push(child_el);
        }
    }
    if elements.len() != 1 {
        return None;
    }
    let root_el = elements[0];
    if !if_body_subtree_is_eligible(root_el) {
        return None;
    }
    let mut ctx = AnalysisCtx::default();
    let mut path: Vec<u16> = Vec::new();
    walk(root_el, &mut ctx, &mut path);
    // Defensive: the eligibility check excluded everything
    // that would emit non-binding/listener/ref entries, but
    // re-check before ratifying — a stale envelope mismatch
    // would produce a body fragment with entries `apply_static_plan`
    // can't honor against the body's static plan shape.
    if !ctx.child_mounts.is_empty()
        || !ctx.if_plans.is_empty()
        || !ctx.for_plans.is_empty()
        || !ctx.teleport_plans.is_empty()
        || !ctx.inits.is_empty()
        || !ctx.slot_fragment_emissions.is_empty()
    {
        return None;
    }
    let mut html = String::new();
    let mut sp: Vec<u16> = Vec::new();
    emit_element(root_el, &ctx, &mut html, &mut sp);
    Some((html, ctx))
}

// ─── static slot eligibility + serialization ─────────────────────

/// RFC-058 Phase 3.5b — `true` when every node in the subtree
/// can be stamped via the static-HTML fragment path with no
/// behavioural change against today's walker capture/replay.
///
/// Eligibility (intentionally narrow):
///   * every element is HTML5 native (no nested custom tags —
///     defer recursive child-component fragment lifting to a
///     follow-up that grows the fragment ABI);
///   * no `<slot>` element (would change semantics on
///     materialisation);
///   * no attribute starts with `pp-`, `@`, or `:` (any
///     directive needs the parent-proxy machinery this v1
///     doesn't ship — Phase 3.5c+ teaches the fragment ABI
///     about parent-scope binding install).
fn slot_subtree_is_static(nodes: &[Node]) -> bool {
    for node in nodes {
        if !slot_node_is_static(node) {
            return false;
        }
    }
    true
}

fn slot_node_is_static(node: &Node) -> bool {
    match node {
        Node::Element(el) => {
            if el.synthetic {
                // Walk through synthetic html5ever wrappers — they
                // don't appear in authored markup so they can't
                // make a subtree dynamic on their own.
                return slot_subtree_is_static(&el.children);
            }
            if el.tag == "slot" {
                return false;
            }
            if !is_html5_native(&el.tag) {
                return false;
            }
            for (name, _) in &el.attrs {
                if name.starts_with("pp-") || name.starts_with('@') || name.starts_with(':') {
                    return false;
                }
            }
            slot_subtree_is_static(&el.children)
        }
        Node::Text(_, _) | Node::Comment(_, _) => true,
    }
}

/// Re-serialise a static slot subtree to HTML. Reuses the same
/// emit-element walker the cleaned-HTML serializer uses, with
/// an empty `AnalysisCtx` so nothing gets stripped or
/// `data-pp-text-managed`-stamped — slot content qualified by
/// `slot_subtree_is_static` has no plan-eligible attributes by
/// construction, so the empty context is correct.
fn serialize_slot_children(nodes: &[Node]) -> String {
    let empty_ctx = AnalysisCtx::default();
    let mut out = String::new();
    let mut path: Vec<u16> = Vec::new();
    for node in nodes {
        emit_node(node, &empty_ctx, &mut out, &mut path);
    }
    out
}

// ─── cleaned HTML serializer ─────────────────────────────────────

fn serialize_cleaned(roots: &[Node], ctx: &AnalysisCtx) -> String {
    let mut out = String::new();
    let mut path: Vec<u16> = Vec::new();
    for node in roots {
        emit_node(node, ctx, &mut out, &mut path);
    }
    out
}

fn emit_node(node: &Node, ctx: &AnalysisCtx, out: &mut String, path: &mut Vec<u16>) {
    match node {
        Node::Element(el) => emit_element(el, ctx, out, path),
        Node::Text(text, _) => out.push_str(&escape_text(text)),
        Node::Comment(text, _) => {
            out.push_str("<!--");
            out.push_str(text);
            out.push_str("-->");
        }
    }
}

fn emit_element(el: &Element, ctx: &AnalysisCtx, out: &mut String, path: &mut Vec<u16>) {
    if el.synthetic {
        // Don't emit synthetic elements as wrappers — emit only
        // their children. The runtime walker doesn't see them
        // either (they're inserted by html5ever's tree builder
        // post-parse).
        for (i, child) in el.children.iter().enumerate() {
            if matches!(child, Node::Element(_)) {
                let idx = el
                    .children
                    .iter()
                    .take(i)
                    .filter(|n| matches!(n, Node::Element(_)))
                    .count() as u16;
                path.push(idx);
                emit_node(child, ctx, out, path);
                path.pop();
            } else {
                emit_node(child, ctx, out, path);
            }
        }
        return;
    }

    out.push('<');
    out.push_str(&el.tag);
    for (name, value) in &el.attrs {
        if ctx.is_stripped(path, name) {
            continue;
        }
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&escape_attr(value));
        out.push('"');
    }
    if ctx.is_text_managed(path) {
        out.push_str(" data-pp-text-managed=\"\"");
    }
    // RFC-058 §6.2 layering — stamp `data-pp-row-plan="<id>"`
    // when the row-plan analyser claimed this `<template
    // pp-for>` site. Replaces the byte-position rewrite
    // `for_plan::apply_stamps` would have done on the raw
    // source — when the template-plan classifier owns the
    // serialization, it owns this stamp too.
    if let Some(plan_id) = ctx.row_plan_id(path) {
        out.push_str(&format!(" data-pp-row-plan=\"{plan_id}\""));
    }
    if is_void_element(&el.tag) {
        out.push_str(" />");
        return;
    }
    out.push('>');
    for (i, child) in el.children.iter().enumerate() {
        if let Node::Element(_) = child {
            let idx = el
                .children
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            emit_node(child, ctx, out, path);
            path.pop();
        } else {
            emit_node(child, ctx, out, path);
        }
    }
    out.push_str("</");
    out.push_str(&el.tag);
    out.push('>');
}

fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}
