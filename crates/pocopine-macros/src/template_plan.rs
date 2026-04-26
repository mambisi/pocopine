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
    let mut emissions = Emissions::default();
    let mut path: Vec<u16> = Vec::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, &mut ctx, &mut emissions, &mut path);
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
    let slot_fragment_fns = emit_slot_fragment_fns(&emissions);
    let if_body_fns = emit_if_body_fns(&emissions);
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

/// Per-subtree analysis state. Each lifted body / slot subtree
/// runs `walk()` against its own `AnalysisCtx`; the entries
/// it accumulates (bindings, listeners, refs, child_mounts,
/// nested controller plans, stripped attrs, text-managed
/// paths) belong to that subtree's per-fragment plan only.
///
/// Shared state (fragment fn emissions + ID counters) lives
/// on `Emissions` so nested lifts can register fragment fns
/// into the same top-level register() body.
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
    /// RFC-058 Phase 3.5e — `<slot>` outlets discovered inside
    /// compiled component templates. The runtime applier
    /// materialises these explicitly instead of relying on the
    /// recursive walker to discover `<slot>` elements.
    slot_outlets: Vec<SlotOutletLite>,
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
    /// True when this template/fragment still contains
    /// framework-owned attributes that the emitted plan does not
    /// install. Fragment mounting uses this as the guard between
    /// compiled post-order finalization and temporary walker
    /// fallback.
    requires_walker: bool,
}

/// Shared across the whole top-level analysis — fragment fn
/// emissions for slot fragments (Phase 3.5b/3.5c) and pp-if
/// body fragments (Phase 4.1d). Lifted bodies and slot
/// subtrees share the same emissions queue so every emitted
/// fn lives at the top of the parent's `register()` body and
/// nested fragments can reference each other by ident.
///
/// The counters bump monotonically across nested lifts so
/// each emission gets a unique `__poc_*_<n>` ident even when
/// allocations interleave.
#[derive(Default)]
struct Emissions {
    /// RFC-058 Phase 3.5b + 3.5c — slot fragment emissions.
    slot_fragments: Vec<SlotFragmentEmission>,
    /// RFC-058 Phase 4.1d — `pp-if` / `pp-for` / `pp-teleport`
    /// body fragments. All three controller body lifts share
    /// this queue (the fn signature is identical and they're
    /// all emitted the same way).
    if_bodies: Vec<IfBodyEmission>,
    /// Monotonic counter for `__poc_slot_frag_<n>` ident
    /// allocation. Bumped on every slot fragment emission so
    /// nested lifts get unique idents even when they allocate
    /// out-of-order with the outer push.
    next_slot_frag_id: usize,
    /// Monotonic counter for `__poc_*_body_<n>` ident
    /// allocation (pp-if / pp-for / pp-teleport bodies share
    /// the same counter).
    next_if_body_id: usize,
}

impl Emissions {
    fn alloc_slot_frag_ident(&mut self, prefix: &str) -> syn::Ident {
        let id = self.next_slot_frag_id;
        self.next_slot_frag_id += 1;
        format_ident!("__poc_{}_{}", prefix, id)
    }

    fn alloc_if_body_ident(&mut self, prefix: &str) -> syn::Ident {
        let id = self.next_if_body_id;
        self.next_if_body_id += 1;
        format_ident!("__poc_{}_{}", prefix, id)
    }
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

/// One emitted slot fragment fn. Static slots stamp HTML
/// only; dynamic slots carry inline plan entries that install
/// bindings/listeners/refs against the parent scope (RFC-058
/// Phase 3.5c).
///
/// `Dynamic` is intentionally fatter than `Static` — it owns a
/// full `AnalysisCtx` for the slot subtree so the per-fragment
/// plan literal can include nested child_mounts / controllers
/// (Phase 3.5d). At macro-expansion frequencies the size
/// difference is irrelevant; suppress the clippy lint that
/// would otherwise force a `Box`.
#[allow(clippy::large_enum_variant)]
enum SlotFragmentEmission {
    Static {
        ident: syn::Ident,
        html: String,
    },
    /// Dynamic slot — carries an `AnalysisCtx` so the per-
    /// fragment `StaticTemplatePlan` literal includes any
    /// child_mounts / nested controllers the recursive
    /// classifier accumulated for the slot's subtree.
    Dynamic {
        ident: syn::Ident,
        html: String,
        plan: AnalysisCtx,
    },
}

impl SlotFragmentEmission {
    fn ident(&self) -> &syn::Ident {
        match self {
            SlotFragmentEmission::Static { ident, .. } => ident,
            SlotFragmentEmission::Dynamic { ident, .. } => ident,
        }
    }
}

/// Macro-emitted `pp-if` / `pp-for` / `pp-teleport` body
/// fragment. Carries the body's full `AnalysisCtx` so the
/// per-fragment plan literal includes any child_mounts +
/// nested controllers the recursive lift accumulated.
struct IfBodyEmission {
    ident: syn::Ident,
    html: String,
    plan: AnalysisCtx,
}

struct IfPlanLite {
    template_node_path: Vec<u16>,
    expr_src: String,
    teleport_selector: Option<String>,
    /// RFC-058 Phase 4.1d — `Some` when the body subtree was
    /// lift-eligible and the macro emitted a body fragment fn
    /// the `StaticIfPlan` literal should reference. `None`
    /// when the body falls outside the v1 envelope (`<slot>`,
    /// `pp-data`, native `pp-model`, `pp-route`, etc.) — the
    /// runtime installer falls back to the legacy
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
    /// RFC-058 Phase 4.3c — `Some` when the teleport body
    /// subtree was lift-eligible. The macro emits a body
    /// fragment fn the `StaticTeleportPlan.body` literal
    /// references.
    body_fn_ident: Option<syn::Ident>,
}

struct SlotOutletLite {
    node_path: Vec<u16>,
    name: String,
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
    host_bindings: Vec<ChildHostBindingLite>,
    host_listeners: Vec<ChildHostListenerLite>,
    host_models: Vec<ChildHostModelLite>,
}

struct ChildHostBindingLite {
    arg: String,
    expr_src: String,
}

struct ChildHostListenerLite {
    event: String,
    modifiers: Vec<String>,
    expr_src: String,
}

struct ChildHostModelLite {
    arg: Option<String>,
    modifiers: Vec<String>,
    expr_src: String,
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
            || !self.slot_outlets.is_empty()
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

    fn emit_plan_tokens(&self) -> TokenStream {
        emit_static_template_plan_literal(self)
    }
}

/// Render the inline `StaticTemplatePlan` literal for a fully
/// populated `AnalysisCtx`. Reused by the per-template emitter
/// and by the per-fragment emitters (`pp-if` body, `pp-for`
/// row body, `pp-teleport` body, dynamic slot fragments) so
/// every plan literal has the same shape — including
/// child_mounts / if_plans / for_plans / teleport_plans for
/// recursive lifting (Phase 3.5d).
fn emit_static_template_plan_literal(ctx: &AnalysisCtx) -> TokenStream {
    let bindings_tokens = ctx.bindings.iter().map(emit_binding);
    let listeners_tokens = ctx.listeners.iter().map(emit_listener);
    let inits_tokens = ctx.inits.iter().map(emit_init);
    let refs_tokens = ctx.refs.iter().map(emit_ref);
    let child_mounts_tokens = ctx.child_mounts.iter().map(emit_child_mount);
    let if_plans_tokens = ctx.if_plans.iter().map(emit_if_plan);
    let for_plans_tokens = ctx.for_plans.iter().map(emit_for_plan);
    let teleport_plans_tokens = ctx.teleport_plans.iter().map(emit_teleport_plan);
    let slot_outlets_tokens = ctx.slot_outlets.iter().map(emit_slot_outlet);
    let requires_walker = ctx.requires_walker;
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
            slot_outlets: &[ #(#slot_outlets_tokens),* ],
            requires_walker: #requires_walker,
        }
    }
}

/// Generate `fn` items for every accumulated `pp-if` /
/// `pp-for` / `pp-teleport` body fragment. Body fragments
/// share the same shape: `(scope_id, &proxy) ->
/// Option<Element>` that stamps cleaned HTML and applies a
/// per-fragment `StaticTemplatePlan` against the passed
/// scope. Recursive lifting (Phase 3.5d) populates the
/// per-fragment plan's `child_mounts` / `if_plans` /
/// `for_plans` / `teleport_plans` for nested custom tags +
/// nested controllers inside the body.
fn emit_if_body_fns(emissions: &Emissions) -> TokenStream {
    let items = emissions.if_bodies.iter().map(|emission| {
        let ident = &emission.ident;
        let html_lit = proc_macro2::Literal::string(&emission.html);
        let plan_literal = emit_static_template_plan_literal(&emission.plan);
        quote! {
            fn #ident(
                scope_id: ::pocopine::ScopeId,
                proxy: &::pocopine::__private::JsValue,
            ) -> ::core::option::Option<::pocopine::__private::web_sys::Element> {
                const PLAN: ::pocopine::__private::StaticTemplatePlan = #plan_literal;
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

/// Generate `fn` items for every accumulated slot fragment.
/// Static slots stamp HTML only; dynamic slots stamp HTML +
/// apply a per-fragment plan against the parent scope. Phase
/// 3.5d's recursive lifting populates the dynamic plan's
/// `child_mounts` for nested custom tags inside slot content.
fn emit_slot_fragment_fns(emissions: &Emissions) -> TokenStream {
    let items = emissions
        .slot_fragments
        .iter()
        .map(|emission| match emission {
            SlotFragmentEmission::Static { ident, html } => {
                let html_lit = proc_macro2::Literal::string(html);
                quote! {
                    fn #ident(ctx: ::pocopine::__private::SlotMountCtx<'_>) {
                        ::pocopine::__private::stamp_static_html(ctx.host, #html_lit);
                    }
                }
            }
            SlotFragmentEmission::Dynamic { ident, html, plan } => {
                let html_lit = proc_macro2::Literal::string(html);
                let plan_literal = emit_static_template_plan_literal(plan);
                quote! {
                    fn #ident(ctx: ::pocopine::__private::SlotMountCtx<'_>) {
                        const PLAN: ::pocopine::__private::StaticTemplatePlan = #plan_literal;
                        ::pocopine::__private::stamp_dynamic_slot(
                            ctx.host,
                            #html_lit,
                            &PLAN,
                            ctx.parent_scope_id,
                            ctx.parent_proxy,
                            ctx.child_scope_id,
                        );
                    }
                }
            }
        });
    quote! { #(#items)* }
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
    let teleport_selector_tokens = match ip.teleport_selector.as_deref() {
        Some(selector) => {
            let selector = proc_macro2::Literal::string(selector);
            quote! { ::core::option::Option::Some(#selector) }
        }
        None => quote! { ::core::option::Option::None },
    };
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
            teleport_selector: #teleport_selector_tokens,
            body: #body_tokens,
        }
    }
}

fn emit_teleport_plan(tp: &TeleportPlanLite) -> TokenStream {
    let path = emit_node_path(&tp.template_node_path);
    let sel = proc_macro2::Literal::string(&tp.selector);
    let body_tokens = match &tp.body_fn_ident {
        Some(ident) => quote! { ::core::option::Option::Some(#ident) },
        None => quote! { ::core::option::Option::None },
    };
    quote! {
        ::pocopine::__private::StaticTeleportPlan {
            template_node_path: #path,
            selector: #sel,
            body: #body_tokens,
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

fn emit_slot_outlet(s: &SlotOutletLite) -> TokenStream {
    let path = emit_node_path(&s.node_path);
    let name = proc_macro2::Literal::string(&s.name);
    quote! {
        ::pocopine::__private::StaticSlotOutlet {
            node_path: #path,
            name: #name,
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
    let binding_tokens = c.host_bindings.iter().map(|b| {
        let arg = proc_macro2::Literal::string(&b.arg);
        let expr = proc_macro2::Literal::string(&b.expr_src);
        quote! {
            ::pocopine::__private::StaticChildHostBinding {
                arg: #arg,
                expr_src: #expr,
            }
        }
    });
    let listener_tokens = c.host_listeners.iter().map(|l| {
        let event = proc_macro2::Literal::string(&l.event);
        let expr = proc_macro2::Literal::string(&l.expr_src);
        let modifiers = l.modifiers.iter().map(|m| {
            let m = proc_macro2::Literal::string(m);
            quote! { #m }
        });
        quote! {
            ::pocopine::__private::StaticChildHostListener {
                event: #event,
                modifiers: &[ #(#modifiers),* ],
                expr_src: #expr,
            }
        }
    });
    let model_tokens = c.host_models.iter().map(|m| {
        let expr = proc_macro2::Literal::string(&m.expr_src);
        let arg_tokens = match m.arg.as_deref() {
            Some(arg) => {
                let arg = proc_macro2::Literal::string(arg);
                quote! { ::core::option::Option::Some(#arg) }
            }
            None => quote! { ::core::option::Option::None },
        };
        let modifiers = m.modifiers.iter().map(|modifier| {
            let modifier = proc_macro2::Literal::string(modifier);
            quote! { #modifier }
        });
        quote! {
            ::pocopine::__private::StaticChildHostModel {
                arg: #arg_tokens,
                modifiers: &[ #(#modifiers),* ],
                expr_src: #expr,
            }
        }
    });
    quote! {
        ::pocopine::__private::StaticChildMount {
            node_path: #path,
            tag: #tag,
            slots: &[ #(#slot_tokens),* ],
            bindings: &[ #(#binding_tokens),* ],
            listeners: &[ #(#listener_tokens),* ],
            models: &[ #(#model_tokens),* ],
        }
    }
}

// ─── walk + classification ───────────────────────────────────────

fn walk(el: &Element, ctx: &mut AnalysisCtx, emissions: &mut Emissions, path: &mut Vec<u16>) {
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
                    analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                        let ident = emissions.alloc_if_body_ident("for_body");
                        emissions.if_bodies.push(IfBodyEmission {
                            ident: ident.clone(),
                            html,
                            plan: body_ctx,
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
        if has_if && !has_for {
            // The pp-if classifier below owns the combined
            // `pp-if` + `pp-teleport` site. It records the
            // selector on StaticIfPlan and strips both source
            // attrs so runtime discovery cannot double-install
            // either controller.
        } else if el.tag == "template" && !has_if && !has_for && !selector.trim().is_empty() {
            // RFC-058 Phase 4.3c — try to lift the teleport
            // body into a fragment fn (same v1 envelope as
            // pp-if / pp-for body lifting).
            let body_fn_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                let ident = emissions.alloc_if_body_ident("teleport_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                });
                ident
            });
            ctx.teleport_plans.push(TeleportPlanLite {
                template_node_path: path.clone(),
                selector,
                body_fn_ident,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.clone(),
                name: "pp-teleport".to_string(),
            });
            return;
        }
        // Ineligible (wrong host, has pp-if/pp-for, empty
        // selector) — leave the walker to dispatch.
        if !has_if {
            return;
        }
    }

    // RFC-058 Phase 3.5e — `<slot>` outlets graduate into the
    // template plan. The cleaned HTML keeps the actual element
    // and attributes; the applier materialises the outlet after
    // all other path-based entries resolve.
    if el.tag == "slot" {
        let name = el
            .attrs
            .iter()
            .find(|(n, _)| n == "name")
            .map(|(_, v)| v.clone())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "default".to_string());
        ctx.slot_outlets.push(SlotOutletLite {
            node_path: path.clone(),
            name,
        });
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
    if let Some(if_expr) = pp_if_value(el) {
        let teleport_selector = el
            .attrs
            .iter()
            .find(|(n, _)| n == "pp-teleport")
            .map(|(_, v)| v.clone())
            .filter(|s| !s.trim().is_empty());
        if el.tag == "template" && pocopine_expr::parse(&if_expr).is_ok() {
            // RFC-058 Phase 4.1d — try to lift the body
            // subtree into a fragment fn the runtime installer
            // invokes instead of `clone_template_body` +
            // `walker::walk`. v1 envelope is narrow (HTML5
            // natives + plan-eligible directives only); when
            // the body falls outside, `body_fn_ident` stays
            // `None` and the legacy clone+walk path runs.
            let body_fn_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                let ident = emissions.alloc_if_body_ident("if_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                });
                ident
            });
            ctx.if_plans.push(IfPlanLite {
                template_node_path: path.clone(),
                expr_src: if_expr,
                teleport_selector: teleport_selector.clone(),
                body_fn_ident,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.clone(),
                name: "pp-if".to_string(),
            });
            if teleport_selector.is_some() {
                ctx.stripped.push(StrippedAttr {
                    node_path: path.clone(),
                    name: "pp-teleport".to_string(),
                });
            }
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
    if !is_plan_native(&el.tag) {
        let mut host_bindings = Vec::new();
        let mut host_listeners = Vec::new();
        let mut host_models = Vec::new();
        let has_pp_as = el.attrs.iter().any(|(name, _)| name == "pp-as");
        if has_pp_as {
            if el
                .attrs
                .iter()
                .any(|(name, _)| name != "pp-as" && is_framework_attr(name))
            {
                ctx.requires_walker = true;
            }
        } else {
            for (name, value) in &el.attrs {
                match classify_child_host_attr(name, value) {
                    ChildHostAttrOutcome::Binding(binding) => {
                        ctx.stripped.push(StrippedAttr {
                            node_path: path.clone(),
                            name: name.clone(),
                        });
                        host_bindings.push(binding);
                    }
                    ChildHostAttrOutcome::Listener(listener) => {
                        ctx.stripped.push(StrippedAttr {
                            node_path: path.clone(),
                            name: name.clone(),
                        });
                        host_listeners.push(listener);
                    }
                    ChildHostAttrOutcome::Model(model) => {
                        ctx.stripped.push(StrippedAttr {
                            node_path: path.clone(),
                            name: name.clone(),
                        });
                        host_models.push(model);
                    }
                    ChildHostAttrOutcome::Preserved => {
                        if is_framework_attr(name) {
                            ctx.requires_walker = true;
                        }
                    }
                }
            }
        }
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
        if !el.children.is_empty() {
            if let Some(emission) = analyze_slot_subtree(&el.children, emissions) {
                slot_fragments.push(("default".to_string(), emission.ident().clone()));
                emissions.slot_fragments.push(emission);
            }
        }
        ctx.child_mounts.push(ChildMountLite {
            node_path: path.clone(),
            tag: el.tag.clone(),
            slot_fragments,
            host_bindings,
            host_listeners,
            host_models,
        });
        return;
    }

    // Classify every attribute on this element.
    if el.attrs.iter().any(|(name, _)| name == "pp-as") && el.tag != "root" {
        ctx.requires_walker = true;
        return;
    }
    let mut listener_unsupported_modifier = false;
    let mut had_text = false;
    for (name, value) in &el.attrs {
        if el.tag == "root" && name == "pp-as" {
            continue;
        }
        match classify_attr(name, value, path, ctx, &mut listener_unsupported_modifier) {
            ClassifyOutcome::Stripped { is_text } => {
                if is_text {
                    had_text = true;
                }
            }
            ClassifyOutcome::Preserved => {
                if is_framework_attr(name) {
                    ctx.requires_walker = true;
                }
            }
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
            walk(child_el, ctx, emissions, path);
            path.pop();
        }
    }
}

fn is_html5_native(tag: &str) -> bool {
    crate::HTML5_ELEMENTS.binary_search(&tag).is_ok()
}

fn is_plan_native(tag: &str) -> bool {
    tag == "root" || is_html5_native(tag)
}

fn is_framework_attr(name: &str) -> bool {
    name.starts_with("pp-") || name.starts_with('@') || name.starts_with(':')
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

enum ChildHostAttrOutcome {
    Binding(ChildHostBindingLite),
    Listener(ChildHostListenerLite),
    Model(ChildHostModelLite),
    Preserved,
}

fn classify_child_host_attr(name: &str, value: &str) -> ChildHostAttrOutcome {
    if let Some(rest) = name.strip_prefix('@') {
        return classify_child_host_listener(rest, value);
    }
    if let Some(arg) = name.strip_prefix(':') {
        if arg.is_empty() || arg.contains('.') || pocopine_expr::parse(value).is_err() {
            return ChildHostAttrOutcome::Preserved;
        }
        return ChildHostAttrOutcome::Binding(ChildHostBindingLite {
            arg: arg.to_string(),
            expr_src: value.to_string(),
        });
    }
    let Some(rest) = name.strip_prefix("pp-") else {
        return ChildHostAttrOutcome::Preserved;
    };
    if let Some(arg) = rest.strip_prefix("bind:") {
        if arg.is_empty() || arg.contains('.') || pocopine_expr::parse(value).is_err() {
            return ChildHostAttrOutcome::Preserved;
        }
        return ChildHostAttrOutcome::Binding(ChildHostBindingLite {
            arg: arg.to_string(),
            expr_src: value.to_string(),
        });
    }
    if let Some(rest) = rest.strip_prefix("on:") {
        return classify_child_host_listener(rest, value);
    }
    if rest == "model" || rest.starts_with("model.") || rest.starts_with("model:") {
        let (arg, modifiers) = parse_child_host_model(rest);
        if value.trim().is_empty() {
            return ChildHostAttrOutcome::Preserved;
        }
        return ChildHostAttrOutcome::Model(ChildHostModelLite {
            arg,
            modifiers,
            expr_src: value.to_string(),
        });
    }
    ChildHostAttrOutcome::Preserved
}

fn classify_child_host_listener(rest: &str, value: &str) -> ChildHostAttrOutcome {
    let mut parts = rest.split('.');
    let Some(event) = parts.next().filter(|s| !s.is_empty()) else {
        return ChildHostAttrOutcome::Preserved;
    };
    let modifiers: Vec<String> = parts.map(str::to_string).collect();
    if !modifiers.iter().all(|m| is_supported_modifier(m)) || pocopine_expr::parse(value).is_err() {
        return ChildHostAttrOutcome::Preserved;
    }
    ChildHostAttrOutcome::Listener(ChildHostListenerLite {
        event: event.to_string(),
        modifiers,
        expr_src: value.to_string(),
    })
}

fn parse_child_host_model(rest: &str) -> (Option<String>, Vec<String>) {
    let body = rest.strip_prefix("model").unwrap_or(rest);
    if let Some(after_colon) = body.strip_prefix(':') {
        let mut parts = after_colon.split('.');
        let arg = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let modifiers = parts.map(str::to_string).collect();
        return (arg, modifiers);
    }
    if let Some(after_dot) = body.strip_prefix('.') {
        let modifiers = after_dot
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        return (None, modifiers);
    }
    (None, Vec::new())
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
///   * `<slot>` elements (would need slot capture/replay
///     hooks inside a body fragment — Phase 3.5c+);
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
    // Phase 3.5d expansion: non-HTML5 tags are allowed here.
    // `walk()` emits child_mount entries for them into the
    // body fragment's own static plan, and the runtime fallback
    // walk over the cleaned fragment binds any preserved
    // directives inside the mounted child template.
    let is_custom = !is_plan_native(&el.tag);
    for (name, _) in &el.attrs {
        if name == "pp-data" || name == "pp-route" {
            return false;
        }
        if !is_custom && (name == "pp-model" || name.starts_with("pp-model:")) {
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
/// lifting (shared between `pp-if` Phase 4.1d, `pp-for` Phase
/// 4.2c, and `pp-teleport` Phase 4.3c). Returns `Some` when
/// the body's element children reduce to a single root + the
/// subtree passes `if_body_subtree_is_eligible`.
///
/// Phase 3.5d expansion: nested custom tags + nested
/// controllers (`pp-if` / `pp-for` / `pp-teleport`) inside the
/// body are eligible. Walks into a fresh `AnalysisCtx` so
/// per-subtree state stays isolated; nested fragment fns get
/// pushed into the shared `Emissions` queue so every emission
/// lands at the top of the parent's `register()` body.
fn analyze_lift_body(
    template_el: &Element,
    emissions: &mut Emissions,
) -> Option<(String, AnalysisCtx)> {
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
    walk(root_el, &mut ctx, emissions, &mut path);
    let mut html = String::new();
    let mut sp: Vec<u16> = Vec::new();
    emit_element(root_el, &ctx, &mut html, &mut sp);
    Some((html, ctx))
}

// ─── slot subtree eligibility + emission ─────────────────────────

/// RFC-058 Phase 3.5b + 3.5c — analyse a `<custom-tag>`'s
/// children for slot fragment lifting. Returns `None` when
/// anything in the subtree falls outside the v1 envelope (`<slot>`,
/// `pp-data` / `pp-model` / `pp-route`). Otherwise returns
/// `Some(SlotFragmentEmission)` — `Static` when the subtree
/// has no plan-eligible directive (3.5b path), `Dynamic` when
/// it does (3.5c path: stamps cleaned HTML + applies a
/// per-fragment static plan against the parent scope).
///
/// Multi-root subtrees are fine — the macro emits one fragment
/// fn per slot site, not per element. The runtime
/// `stamp_dynamic_slot` helper wraps the children in a
/// temporary `<div>` so `apply_static_plan` can resolve
/// `node_path`s against a single element root.
fn analyze_slot_subtree(nodes: &[Node], emissions: &mut Emissions) -> Option<SlotFragmentEmission> {
    if !slot_subtree_is_lift_eligible(nodes) {
        return None;
    }
    let mut ctx = AnalysisCtx::default();
    let mut path: Vec<u16> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if let Node::Element(el) = node {
            let idx = nodes
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            walk(el, &mut ctx, emissions, &mut path);
            path.pop();
        }
    }
    let html = serialize_slot_children_with(nodes, &ctx);
    let is_dynamic = !ctx.bindings.is_empty()
        || !ctx.listeners.is_empty()
        || !ctx.refs.is_empty()
        || !ctx.child_mounts.is_empty()
        || !ctx.if_plans.is_empty()
        || !ctx.for_plans.is_empty()
        || !ctx.teleport_plans.is_empty();
    let ident = emissions.alloc_slot_frag_ident("slot_frag");
    if is_dynamic {
        Some(SlotFragmentEmission::Dynamic {
            ident,
            html,
            plan: ctx,
        })
    } else {
        Some(SlotFragmentEmission::Static { ident, html })
    }
}

/// Same shape as `slot_subtree_is_static` but allows
/// plan-eligible directives (`pp-text`, `pp-bind:*`, `@event`,
/// `pp-on:event`, `pp-show`, `pp-html`, `pp-ref`). Rejects
/// what the dynamic-slot v1 envelope can't handle yet:
/// non-HTML5 tags (would need recursive child mounts inside a
/// fragment — Phase 3.5d), `<slot>` (nested designation —
/// Phase 3.5e), nested controllers (`pp-for` / `pp-if` /
/// `pp-teleport` — same scope semantics they already have at
/// template level but inside a parent-scope fragment), and
/// the directives whose semantics the fragment can't honour
/// (`pp-data` / `pp-model` / `pp-route` are component scope
/// boundaries).
fn slot_subtree_is_lift_eligible(nodes: &[Node]) -> bool {
    nodes.iter().all(slot_node_is_lift_eligible)
}

fn slot_node_is_lift_eligible(node: &Node) -> bool {
    match node {
        Node::Element(el) => {
            if el.synthetic {
                return slot_subtree_is_lift_eligible(&el.children);
            }
            if el.tag == "slot" {
                return false;
            }
            // Phase 3.5d expansion: non-HTML5 tags = nested
            // child mounts are now allowed. The walker
            // handles them via `analyze_slot_subtree` recursion
            // (which lands at the same `walk()` path that
            // emits child_mount entries + nested slot
            // fragments into the shared `Emissions` queue).
            let is_custom = !is_plan_native(&el.tag);
            for (name, _) in &el.attrs {
                if name == "pp-data" || name == "pp-route" {
                    return false;
                }
                if !is_custom && (name == "pp-model" || name.starts_with("pp-model:")) {
                    return false;
                }
            }
            slot_subtree_is_lift_eligible(&el.children)
        }
        Node::Text(_, _) | Node::Comment(_, _) => true,
    }
}

/// Re-serialise slot children using a populated `AnalysisCtx`
/// so attribute strips + `data-pp-text-managed` markers land
/// alongside the directives the analyzer collected.
fn serialize_slot_children_with(nodes: &[Node], ctx: &AnalysisCtx) -> String {
    let mut out = String::new();
    let mut path: Vec<u16> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if matches!(node, Node::Element(_)) {
            let idx = nodes
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            emit_node(node, ctx, &mut out, &mut path);
            path.pop();
        } else {
            emit_node(node, ctx, &mut out, &mut path);
        }
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
