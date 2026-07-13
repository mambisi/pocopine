//! RFC-058 Phase 2 — macro-time whole-template plan compilation.
//!
//! Walks the parsed `TemplateAst` (produced by RFC-050's
//! `template_parser::parse_strict`) and emits two artefacts the
//! macro then bakes into the component's registration:
//!
//! 1. A `&'static StaticTemplatePlan` literal describing every
//!    plan-eligible directive in the template — `pp-text`,
//!    `pp-html`, `pp-show`, `pp-bind:<attr>`, `pp-on:<event>`,
//!    `pp-ref`. Indexed by `node_path:
//!    &'static [u16]` over the cloned-template DOM, matching
//!    the convention RFC-054 row plans already use.
//! 2. A "cleaned HTML" string — the template re-serialised
//!    with the classified attributes stripped.
//!
//! The original v1 envelope from RFC-057/058 has since expanded to cover
//! structural bodies, child components, slots, models, and selected opaque
//! directives. One post-walker rule is now fundamental:
//!
//! * Eligible: native HTML elements only — every directive
//!   on or under a non-HTML5 tag is whole-subtree
//!   mount-owned (council pass 3 amendment).
//! * Eligible directives: `pp-text`, `pp-html`, `pp-show`,
//!   `pp-bind:<arg>` / `:<arg>`, `pp-on:<event>` / `@event`
//!   when every modifier is in the supported set, `pp-ref`.
//! * Whole-subtree boundaries (mount-owned, classifier
//!   skips the subtree): `pp-for`, `pp-if`, `pp-teleport`,
//!   `<slot>`, every non-HTML5 tag.
//! * `pp-model` and `pp-route` are explicitly deferred (§7
//!   follow-ups).
//! * Listener modifier set: `prevent`, `stop`, `self`, `once`,
//!   `window`, `document`, `outside`, `capture`, key
//!   modifiers, `debounce` + numeric-ms pair.
//!
//! Preserving an uncompiled framework directive in HTML is **not** a runtime
//! fallback: the generic walker is gone. Such syntax is inert, so analysis
//! failures must surface as diagnostics instead of silently degrading.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::template_parser::{Element, Node, TemplateAst};

/// Result of analysing one component's template.
pub(crate) struct EmittedTemplatePlan {
    /// `Some(quoted &'static StaticTemplatePlan)` when at least
    /// one plan entry was emitted; `None` when the template has
    /// nothing eligible (normally a fully static template). The macro emits
    /// `register_template_plan` only when this is `Some`.
    pub plan_tokens: Option<TokenStream>,
    /// HTML the macro should pass to `register_template` instead
    /// of the raw `.poco` source. Classified attributes are
    /// stripped; `data-pp-text-managed` is stamped where
    /// `pp-text` was removed. `None` when the analysis emitted
    /// no entries — the caller uses the original source bytes. A planner path
    /// resolution failure also takes that lane, but emits a build warning.
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
    /// RFC 062 — unrolled component mount body. Component
    /// mounts use this generated body directly; the generic
    /// plan applier remains for lifted fragment internals only.
    pub specialized_mount_body: Option<TokenStream>,
    /// Compile-time diagnostics collected at any analysis depth. Kept
    /// separate from fragment functions so diagnostics-only templates still
    /// emit errors even when no `StaticTemplatePlan` is generated.
    pub diagnostics: TokenStream,
    /// RFC 081 — every `pp-ref="name"` collected from the
    /// template, dedup'd in template-order. The consuming
    /// macro emits a `<ComponentName>Refs` struct with one
    /// `fn <name>(&self) -> RefAccessor` per entry so handlers
    /// can write `refs.body()` instead of
    /// `refs::get_component::<T>("body")`.
    pub ref_names: Vec<String>,
    /// Build warnings the consuming macro should surface via the
    /// `#[deprecated]`-const trick (RFC 045 §9.4) — non-fatal
    /// authoring notes such as the JS-equality desugar.
    pub warnings: Vec<String>,
    /// RFC-113 — element-child path from the rendered template root to the
    /// unique unconditional native element marked `pp-owned-content`.
    /// `None` means the template declares no outlet or outlet validation
    /// emitted a compile diagnostic.
    pub owned_content_outlet_path: Option<Vec<u16>>,
}

#[derive(Clone)]
struct PlanDiagnostic {
    message: String,
    byte_range: Option<std::ops::Range<usize>>,
    context: Option<PlanDiagnosticContext>,
}

#[derive(Clone)]
struct PlanDiagnosticContext {
    label: String,
    byte_range: std::ops::Range<usize>,
}

#[derive(Default)]
struct PlanDiagnostics(Vec<PlanDiagnostic>);

impl PlanDiagnostics {
    fn push(&mut self, message: impl Into<String>) {
        self.0.push(PlanDiagnostic {
            message: message.into(),
            byte_range: None,
            context: None,
        });
    }

    fn push_at(&mut self, message: impl Into<String>, byte_range: std::ops::Range<usize>) {
        self.0.push(PlanDiagnostic {
            message: message.into(),
            byte_range: Some(byte_range),
            context: None,
        });
    }

    fn push_at_with_context(
        &mut self,
        message: impl Into<String>,
        byte_range: std::ops::Range<usize>,
        context_label: impl Into<String>,
        context_range: std::ops::Range<usize>,
    ) {
        self.0.push(PlanDiagnostic {
            message: message.into(),
            byte_range: Some(byte_range),
            context: Some(PlanDiagnosticContext {
                label: context_label.into(),
                byte_range: context_range,
            }),
        });
    }

    fn extend(&mut self, diagnostics: impl IntoIterator<Item = PlanDiagnostic>) {
        self.0.extend(diagnostics);
    }

    fn iter(&self) -> impl Iterator<Item = &PlanDiagnostic> {
        self.0.iter()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
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
    role: Option<(String, String)>,
    component_name: &str,
) -> EmittedTemplatePlan {
    let mut ctx = AnalysisCtx {
        row_plan_assignments: row_plan_assignments.to_vec(),
        ..AnalysisCtx::default()
    };
    let mut emissions = Emissions {
        role: role.clone(),
        ..Emissions::default()
    };
    let owned_content_outlet_path = analyze_owned_content_outlet(ast, &mut ctx);
    let mut path: Vec<u16> = Vec::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, &mut ctx, &mut emissions, &mut path);
        }
    }
    let diagnostics = ctx
        .diagnostics
        .iter()
        .fold(TokenStream::new(), |mut out, diagnostic| {
            let title = format!("pocopine: template plan error in component `{component_name}`");
            let rendered = match diagnostic.byte_range.clone() {
                Some(range) if range.start < range.end && range.end <= ast.source.len() => {
                    match diagnostic.context.as_ref() {
                        Some(context)
                            if context.byte_range.start < context.byte_range.end
                                && context.byte_range.end <= ast.source.len() =>
                        {
                            crate::diagnostics::render_template_error_plain_with_context(
                                &ast.source,
                                &ast.file_path,
                                range,
                                &title,
                                &diagnostic.message,
                                context.byte_range.clone(),
                                &context.label,
                            )
                        }
                        _ => crate::diagnostics::render_template_error_plain(
                            &ast.source,
                            &ast.file_path,
                            range,
                            &title,
                            &diagnostic.message,
                        ),
                    }
                }
                _ => format!("{title} (`{}`): {}", ast.file_path, diagnostic.message),
            };
            let lit = proc_macro2::Literal::string(&rendered);
            out.extend(quote! { ::core::compile_error!(#lit); });
            out
        });
    if !ctx.has_any_entry()
        && row_plan_assignments.is_empty()
        && owned_content_outlet_path.is_none()
    {
        return EmittedTemplatePlan {
            plan_tokens: None,
            cleaned_html: None,
            slot_fragment_fns: TokenStream::new(),
            if_body_fns: TokenStream::new(),
            specialized_mount_body: None,
            diagnostics,
            ref_names: ctx.ref_names_dedup(),
            warnings: ctx.warnings.clone(),
            owned_content_outlet_path,
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
    let specialized_mount_body = ctx.emit_specialized_mount_body();
    let ref_names = ctx.ref_names_dedup();
    EmittedTemplatePlan {
        plan_tokens,
        cleaned_html: Some(cleaned_html),
        slot_fragment_fns,
        if_body_fns,
        specialized_mount_body,
        diagnostics,
        ref_names,
        warnings: ctx.warnings.clone(),
        owned_content_outlet_path,
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
    refs: Vec<RefLite>,
    /// RFC-058 Phase 3.3 — child-component mount sites the
    /// classifier discovered (every non-HTML5 tag inside the
    /// plan-eligible portion of the template, excluding
    /// `<slot>` / `<template>`-block wrappers / pp-for /
    /// pp-if / pp-teleport subtrees). The runtime applier
    /// invokes [`crate::mount::mount_child_component`] for
    /// each before the mount recurses, and the mount's
    /// `__pp_mounted` guard makes the discovery a no-op for
    /// any tag the plan already mounted.
    child_mounts: Vec<ChildMountLite>,
    /// RFC-058 Phase 4.1b — `pp-if` controller sites the
    /// classifier lifted out of the runtime mount's
    /// directive-dispatch path. Each entry pins a
    /// `<template>`'s `node_path` + the truthy expression
    /// source; the runtime applier resolves the template,
    /// parses the expression, and calls
    /// [`crate::directives::if_::install`].
    if_plans: Vec<IfPlanLite>,
    /// RFC-094 Phase 3 — `pp-match` dispatch sites.
    match_plans: Vec<MatchPlanLite>,
    /// RFC-058 Phase 4.2 — `pp-for` controller sites the
    /// classifier lifted out of the runtime mount's
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
    /// recursive mount to discover `<slot>` elements.
    slot_outlets: Vec<SlotOutletLite>,
    /// RFC-058 Phase 3 hardening — runtime-only directives the
    /// macro can lift into a compile-time entry instead of
    /// preserving them on the cleaned HTML and forcing
    /// requires_walker. Allowlist-driven (currently `pp-roving`,
    /// `pp-resize`, `pp-intersect`, `pp-anchor`, `pp-flip`); the
    /// runtime applier dispatches each through
    /// [`crate::directives::lookup`] after slot materialisation.
    opaque_directives: Vec<OpaqueDirectiveLite>,
    /// RFC-058 Phase 6.2 — `{{expr}}` text interpolation lifted
    /// to compile time. Each entry pre-parses one text-node
    /// child's segment list; the applier installs effects per
    /// dynamic segment.
    interps: Vec<InterpLite>,
    /// RFC-058 Phase 6.5 — `pp-model[.modifier]="field"` on a
    /// native input/textarea/select. The previously mount-only
    /// directive lifts into a static plan entry the applier
    /// installs via `directives::model::install_native`.
    /// Component-target `pp-model` (registered tag, with or
    /// without an arg) stays on `ChildHostModelLite`; this
    /// vec covers only native targets.
    native_models: Vec<NativeModelLite>,
    /// RFC-094 — chain build errors surfaced as compile_error!
    /// items (orphan/double/misplaced else, bad member shape).
    diagnostics: PlanDiagnostics,
    /// Build warnings surfaced via the `#[deprecated]`-const trick
    /// (RFC 045 §9.4) — currently the JS-equality desugar notes.
    /// Unlike `diagnostics`, these don't stop the build.
    warnings: Vec<String>,
    /// RFC-094 — node paths of consumed pp-else-if/pp-else
    /// member templates (kept in cleaned HTML until the
    /// controller detaches them; the serializer stamps them
    /// `hidden` like every structural template).
    chain_member_paths: Vec<Vec<u16>>,
    /// Set of (node_path, attr_name) entries the cleaned-HTML
    /// serializer should drop. Lookup is O(scan) per attribute
    /// — fine at typical template sizes.
    stripped: Vec<StrippedAttr>,
    /// RFC-058 §6.2 — `(template_node_path, plan_id)` pairs
    /// from the row-plan analyser. The cleaned-HTML serializer
    /// stamps `data-pp-row-plan="<id>"` onto each pp-for
    /// `<template>` opening tag whose path matches an entry,
    /// so the runtime row-plan registry lookup still finds its
    /// target after the template-plan rewrite.
    row_plan_assignments: Vec<(Vec<u16>, u32)>,
    /// RFC 081 Phase 2 Codex-P2 fix — `pp-ref` names harvested
    /// from lifted-body and slot-fragment subtrees that don't
    /// share `self.refs` (each nested `walk` collects into its
    /// own `AnalysisCtx`). Aggregated as the outer plan is
    /// built so the macro-emitted `<ComponentName>Refs` struct
    /// still gets accessors for pp-refs inside `pp-if` /
    /// `pp-for` / `pp-teleport` / dynamic-slot bodies. Names
    /// only — node_paths inside lifted fragments aren't
    /// meaningful at the parent's plan layer.
    refs_from_lifted: Vec<String>,
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
    /// RFC-058 Phase 6.5 — `(role_tag, role_attrs)` for the
    /// component's `<root>` placeholder, copied from the
    /// `#[component(role = "...")]` attribute. The runtime
    /// `compile_template` substitutes `<root>` in the parent's
    /// registered HTML; lifted body fragments + slot fragments
    /// stamp their own cleaned HTML directly via `set_inner_html`
    /// and need the substitution applied at compile time so the
    /// fragment root materialises with the right tag.
    role: Option<(String, String)>,
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

/// RFC-094 Phase 3 — one `pp-match` site.
struct MatchPlanLite {
    template_node_path: Vec<u16>,
    expr_src: String,
    teleport_selector: Option<String>,
    /// (tags — empty = `_`, pp-let bind name, body fragment).
    cases: Vec<(Vec<String>, Option<String>, Option<syn::Ident>)>,
    bodies_need_proxy: bool,
}

struct IfPlanLite {
    template_node_path: Vec<u16>,
    expr_src: String,
    teleport_selector: Option<String>,
    /// RFC-094 — pp-else-if branches: (expr_src, body fragment).
    else_if: Vec<(String, Option<syn::Ident>)>,
    /// RFC-094 — pp-else present? (body may still be None when
    /// unliftable — the runtime clones the member template).
    has_else: bool,
    else_body: Option<syn::Ident>,
    /// Consumed chain-member templates following the head.
    consumed_count: u16,
    /// Recursive proxy-need over every lifted branch body.
    bodies_need_proxy: bool,
    /// RFC-058 Phase 4.1d — `Some` when the body subtree was
    /// lift-eligible and the macro emitted a body fragment fn
    /// the `StaticIfPlan` literal should reference. `None`
    /// when the body falls outside the compiled envelope. The runtime may
    /// clone that body as static HTML, but no generic walker installs native
    /// directives; it records a plan failure instead.
    body_fn_ident: Option<syn::Ident>,
}

struct ForPlanLite {
    template_node_path: Vec<u16>,
    item_name: String,
    items_expr: String,
    key_expr: Option<String>,
    stagger_ms: u32,
    /// RFC-094 parity with Cond/MatchPlanLite — the lifted row
    /// body's own proxy need (slot outlets, child mounts, …)
    /// must flow into the host plan's `needs_proxy`.
    bodies_need_proxy: bool,
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

struct OpaqueDirectiveLite {
    node_path: Vec<u16>,
    /// Directive head after `pp-` strip (e.g. `"roving"`).
    name: String,
    /// Argument after the first `:` in the attribute name.
    arg: Option<String>,
    /// Modifiers after each `.` in the attribute name.
    modifiers: Vec<String>,
    /// Attribute value verbatim.
    value: String,
}

/// RFC-058 Phase 6.2 — accumulated `{{expr}}` text interpolation
/// site. `text_index` counts the target text-node among the
/// element's direct text children (skipping element / comment
/// children); the runtime applier resolves it the same way.
struct InterpLite {
    node_path: Vec<u16>,
    text_index: u16,
    segments: Vec<InterpSegment>,
}

enum InterpSegment {
    Static(String),
    Dynamic(String),
}

/// RFC-058 Phase 6.5 — accumulated `pp-model[.modifier]="field"`
/// site on a native input/textarea/select. Component-target
/// `pp-model` (registered tag) keeps using `ChildHostModelLite`.
struct NativeModelLite {
    node_path: Vec<u16>,
    expr_src: String,
    number: bool,
    lazy: bool,
}

struct ChildMountLite {
    node_path: Vec<u16>,
    tag: String,
    /// `(slot_name, generated_fn_ident)` pairs for every
    /// statically-eligible slot the macro lifted into a
    /// fragment function. Empty when the parent left no
    /// children inside the custom tag, or when the children
    /// contained anything mount-only (any `pp-*` / `@` /
    /// `:` directive, any non-HTML5 tag, any `<slot>` / pp-let
    /// / pp-for / pp-if / pp-teleport descendant).
    ///
    /// The idents match entries in
    /// [`AnalysisCtx::slot_fragment_emissions`] so the
    /// `StaticSlotFragment.fragment` literal in the plan
    /// references the same `fn` the macro emits below.
    ///
    /// The third tuple slot is the parent's `pp-let` identifier
    /// when the slot was authored as `<template pp-slot="NAME"
    /// pp-let="ident">…</template>` (RFC-058 Phase 3.5g);
    /// `None` for plain default + named slots. The runtime
    /// uses it to construct a [`SlotScope`] before invoking the
    /// fragment.
    slot_fragments: Vec<(String, syn::Ident, Option<String>)>,
    host_shows: Vec<String>,
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
            || !self.refs.is_empty()
            || !self.child_mounts.is_empty()
            || !self.if_plans.is_empty()
            || !self.match_plans.is_empty()
            || !self.for_plans.is_empty()
            || !self.teleport_plans.is_empty()
            || !self.slot_outlets.is_empty()
            || !self.opaque_directives.is_empty()
            || !self.interps.is_empty()
            || !self.native_models.is_empty()
    }

    /// RFC 081 — every distinct `pp-ref="name"` collected from
    /// the template, in template-order. Two refs with the same
    /// name in different `pp-for` rows (last-wins resolution at
    /// runtime) collapse to one accessor here — the generated
    /// `fn <name>(&self) -> RefAccessor` then resolves whichever
    /// row's element happened to win.
    ///
    /// Includes refs from lifted bodies (`pp-if` / `pp-for` /
    /// `pp-teleport` subtrees, dynamic slot fragments) via the
    /// `refs_from_lifted` aggregator — those nested
    /// `AnalysisCtx`s register against the parent's runtime
    /// scope at install time, so the typed API must expose
    /// them too.
    fn ref_names_dedup(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(self.refs.len() + self.refs_from_lifted.len());
        for r in &self.refs {
            if seen.insert(r.name.clone()) {
                out.push(r.name.clone());
            }
        }
        for name in &self.refs_from_lifted {
            if seen.insert(name.clone()) {
                out.push(name.clone());
            }
        }
        out
    }

    /// Absorb metadata that must escape a lifted subtree's otherwise-isolated
    /// plan context. Ref names feed the outer typed refs API; diagnostics must
    /// reach the top-level `compile_error!` emission instead of disappearing
    /// when a nested body is moved into its own fragment plan.
    fn absorb_lifted_metadata(&mut self, nested: &AnalysisCtx) {
        for r in &nested.refs {
            self.refs_from_lifted.push(r.name.clone());
        }
        for name in &nested.refs_from_lifted {
            self.refs_from_lifted.push(name.clone());
        }
        self.diagnostics.extend(nested.diagnostics.iter().cloned());
        self.warnings.extend(nested.warnings.iter().cloned());
    }

    fn emit_specialized_mount_body(&self) -> Option<TokenStream> {
        self.emit_specialized_install_pass(quote! {
            let __poc_plan = <Self>::__POC_TEMPLATE_PLAN;
        })
    }

    /// Emit the unrolled install pass given a `prelude` that
    /// binds `__poc_plan` to a `&'static StaticTemplatePlan`.
    /// Component mounts use `Self::__POC_TEMPLATE_PLAN`; per-
    /// fragment closures (RFC 064 §5.1) bind a per-fragment
    /// `const PLAN`. Both share the same install body.
    fn emit_specialized_install_pass(&self, prelude: TokenStream) -> Option<TokenStream> {
        let slot_capacity = self.slot_outlets.len();
        let interp_capacity = self.interps.len();
        let slot_outlets = self
            .slot_outlets
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_slot_outlet(idx, &entry.node_path));
        let refs = self
            .refs
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_ref(idx, &entry.node_path));
        let bindings = self
            .bindings
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_binding(idx, &entry.node_path));
        let listeners = self
            .listeners
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_listener(idx, &entry.node_path));
        let child_mounts = self
            .child_mounts
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_child_mount(idx, &entry.node_path));
        // RFC-094 §5.4 — structural controllers (for / teleport /
        // cond) mutate element structure when their first effect
        // run mounts a clone (or when the cond controller swaps
        // its template for the comment anchor), which would shift
        // the element-child indices any LATER path resolution
        // depends on. Two defenses: every other entry resolves
        // its path before the structural block runs (see the
        // emission order below), and the structural installs
        // themselves run in REVERSE document order so no
        // install's mutation precedes a structural sibling's
        // resolution.
        let mut structural: Vec<(Vec<u16>, TokenStream)> = Vec::new();
        for (idx, entry) in self.for_plans.iter().enumerate() {
            structural.push((
                entry.template_node_path.clone(),
                emit_specialized_for_plan(idx, &entry.template_node_path),
            ));
        }
        for (idx, entry) in self.teleport_plans.iter().enumerate() {
            structural.push((
                entry.template_node_path.clone(),
                emit_specialized_teleport_plan(idx, &entry.template_node_path),
            ));
        }
        for (idx, entry) in self.if_plans.iter().enumerate() {
            structural.push((
                entry.template_node_path.clone(),
                emit_specialized_if_plan(idx, &entry.template_node_path),
            ));
        }
        for (idx, entry) in self.match_plans.iter().enumerate() {
            structural.push((
                entry.template_node_path.clone(),
                emit_specialized_match_plan(idx, &entry.template_node_path),
            ));
        }
        structural.sort_by(|a, b| b.0.cmp(&a.0));
        let structural = structural.into_iter().map(|(_, tokens)| tokens);
        let materialize_slots = (0..self.slot_outlets.len()).map(|idx| {
            let idx = syn::Index::from(idx);
            quote! {
                ::pocopine::__private::materialize_static_slot_outlet(&__poc_slot_outlets[#idx]);
            }
        });
        let opaque_directives = self
            .opaque_directives
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_opaque_directive(idx, &entry.node_path));
        let interps = self
            .interps
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_interp(idx, &entry.node_path));
        let install_interps = (0..self.interps.len()).map(|idx| {
            let idx = syn::Index::from(idx);
            quote! {
                ::pocopine::__private::install_static_interp_target(
                    &__poc_interp_targets[#idx],
                    scope_id,
                    proxy,
                );
            }
        });
        let native_models = self
            .native_models
            .iter()
            .enumerate()
            .map(|(idx, entry)| emit_specialized_native_model(idx, &entry.node_path));

        Some(quote! {
            #prelude
            let mut __poc_slot_outlets: ::std::vec::Vec<::pocopine::__private::web_sys::Element> =
                ::std::vec::Vec::with_capacity(#slot_capacity);
            let mut __poc_interp_targets: ::std::vec::Vec<::pocopine::__private::StaticInterpTarget> =
                ::std::vec::Vec::with_capacity(#interp_capacity);
            #(#slot_outlets)*
            #(#refs)*
            #(#bindings)*
            #(#listeners)*
            #(#child_mounts)*
            // RFC-094 — interp targets are CAPTURED (path-resolved)
            // and native models installed before any structural
            // mutation can shift element indices; this closes the
            // resolve-after-mutate latent hole.
            #(#interps)*
            #(#native_models)*
            #(#structural)*
            #(#materialize_slots)*
            #(#opaque_directives)*
            #(#install_interps)*
        })
    }

    fn is_stripped(&self, node_path: &[u16], attr_name: &str) -> bool {
        self.stripped
            .iter()
            .any(|s| s.node_path == node_path && s.name == attr_name)
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
    let refs_tokens = ctx.refs.iter().map(emit_ref);
    let child_mounts_tokens = ctx.child_mounts.iter().map(emit_child_mount);
    let if_plans_tokens = ctx.if_plans.iter().map(emit_if_plan);
    let match_plans_tokens = ctx.match_plans.iter().map(emit_match_plan);
    let for_plans_tokens = ctx.for_plans.iter().map(emit_for_plan);
    let teleport_plans_tokens = ctx.teleport_plans.iter().map(emit_teleport_plan);
    let slot_outlets_tokens = ctx.slot_outlets.iter().map(emit_slot_outlet);
    let opaque_tokens = ctx.opaque_directives.iter().map(emit_opaque_directive);
    let interp_tokens = ctx.interps.iter().map(emit_interp);
    let native_model_tokens = ctx.native_models.iter().map(emit_native_model);
    let needs_proxy = plan_needs_proxy(ctx);
    quote! {
        ::pocopine::__private::StaticTemplatePlan {
            bindings: &[ #(#bindings_tokens),* ],
            listeners: &[ #(#listeners_tokens),* ],
            refs: &[ #(#refs_tokens),* ],
            child_mounts: &[ #(#child_mounts_tokens),* ],
            if_plans: &[ #(#if_plans_tokens),* ],
            match_plans: &[ #(#match_plans_tokens),* ],
            for_plans: &[ #(#for_plans_tokens),* ],
            teleport_plans: &[ #(#teleport_plans_tokens),* ],
            slot_outlets: &[ #(#slot_outlets_tokens),* ],
            opaque_directives: &[ #(#opaque_tokens),* ],
            interps: &[ #(#interp_tokens),* ],
            native_models: &[ #(#native_model_tokens),* ],
            needs_proxy: #needs_proxy,
        }
    }
}

/// RFC-095 W3b — conservative proxy-need analysis. `false` only
/// when every install in the plan is provably proxy-free at
/// runtime: bindings / interps / refs, with every expression
/// `$`-free so the W1 scoped root reader owns every root segment
/// and the evaluator's proxy fallback is unreachable. Everything
/// else — listeners (dispatch-time evaluation), structural
/// controllers (effects capture the proxy), child mounts (slot
/// fragments), slot outlets, opaque directives, native models
/// (write side goes through the set trap) — keeps the eager
/// mint. Err on `true`: a wrong `true` costs one Proxy per
/// mount; a wrong `false` breaks a binding.
fn plan_needs_proxy(ctx: &AnalysisCtx) -> bool {
    // RFC-096 S2 — the scoped access is read-complete ($-roots
    // included) and write-complete (the S1 mirror), so listeners,
    // native models, and $-rooted expressions no longer need an
    // eager proxy. What remains: structural controllers (their
    // effects and body fragments still thread the proxy value),
    // child mounts (slot-fragment plumbing), slot outlets, and
    // opaque directives.
    !ctx.child_mounts.is_empty()
        || ctx.if_plans.iter().any(|c| c.bodies_need_proxy)
        || ctx.match_plans.iter().any(|m| m.bodies_need_proxy)
        || ctx.for_plans.iter().any(for_plan_needs_proxy)
        || !ctx.teleport_plans.is_empty()
        || !ctx.slot_outlets.is_empty()
        || !ctx.opaque_directives.is_empty()
}

/// RFC-094 Phase 4 — a `pp-for` site forces the parent's eager
/// proxy only where the controller actually threads it: `$`-rooted
/// items expressions (conservative — the magic fallback) and
/// external `pp-key` expressions, which `KeyResolver` resolves
/// against the parent proxy. Row bodies bind to per-row LoopScopes
/// (read-complete via `read_scope_key`) and never touch the
/// parent's proxy. The key-shape test mirrors `KeyResolver::parse`
/// exactly: `$index`, the bare item name, and `item.path` shapes
/// are item-rooted; everything else is `External`.
fn for_plan_needs_proxy(f: &ForPlanLite) -> bool {
    if f.bodies_need_proxy {
        return true;
    }
    if f.items_expr.trim_start().starts_with('$') {
        return true;
    }
    let Some(key) = f.key_expr.as_deref() else {
        return false;
    };
    let key = key.trim();
    let is_item_rooted = key == "$index"
        || key == f.item_name
        || (key.len() > f.item_name.len() + 1
            && key.starts_with(&f.item_name)
            && key.as_bytes().get(f.item_name.len()) == Some(&b'.'));
    !is_item_rooted
}

fn emit_native_model(nm: &NativeModelLite) -> TokenStream {
    let path_tokens = emit_node_path(&nm.node_path);
    let expr_lit = proc_macro2::Literal::string(&nm.expr_src);
    let number = nm.number;
    let lazy = nm.lazy;
    quote! {
        ::pocopine::__private::StaticNativeModel {
            node_path: #path_tokens,
            expr_src: #expr_lit,
            number: #number,
            lazy: #lazy,
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
        // RFC 064 §5.1 (Phase 1) — inline the unrolled install
        // pass into a closure handed to `stamp_if_body_with`. The
        // generic fragment applier no longer runs for pp-if,
        // pp-for, or pp-teleport body fragments; the
        // per-fragment closure uses `emit_specialized_install_pass`
        // (the same code path RFC 062 component mount
        // specialization uses) against a local `const PLAN`.
        let install_pass = emission
            .plan
            .emit_specialized_install_pass(quote! {
                const PLAN: ::pocopine::__private::StaticTemplatePlan = #plan_literal;
                let __poc_plan = &PLAN;
                let __poc_template_name = "<pp-if body>";
            })
            .unwrap_or_else(|| {
                // The body has no plan-eligible entries — emit a
                // no-op closure body so the `stamp_if_body_with`
                // call still type-checks. Same shape as the
                // empty-plan case in RFC 062.
                quote! {}
            });
        quote! {
            fn #ident(
                scope_id: ::pocopine::ScopeId,
                proxy: &::pocopine::__private::JsValue,
                ctx_parent_id: ::pocopine::ScopeId,
            ) -> ::core::option::Option<::pocopine::__private::web_sys::Element> {
                ::pocopine::__private::stamp_if_body_with(
                    #html_lit,
                    scope_id,
                    proxy,
                    ctx_parent_id,
                    |root, scope_id, proxy| {
                        let _ = root;
                        let _ = scope_id;
                        let _ = proxy;
                        #install_pass
                    },
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
                // RFC 064 §5.1 (Phase 1.B) — inline the unrolled
                // install pass into a closure handed to
                // `stamp_dynamic_slot_with`, mirroring the body
                // fragment shape. The generic fragment applier no
                // longer runs for dynamic slot fragments.
                let install_pass = plan
                    .emit_specialized_install_pass(quote! {
                        const PLAN: ::pocopine::__private::StaticTemplatePlan = #plan_literal;
                        let __poc_plan = &PLAN;
                        let __poc_template_name = "<slot>";
                    })
                    .unwrap_or_else(|| quote! {});
                quote! {
                    fn #ident(ctx: ::pocopine::__private::SlotMountCtx<'_>) {
                        ::pocopine::__private::stamp_dynamic_slot_with(
                            ctx.host,
                            #html_lit,
                            ctx.parent_scope_id,
                            ctx.parent_proxy,
                            ctx.child_scope_id,
                            |root, scope_id, proxy| {
                                let _ = root;
                                let _ = scope_id;
                                let _ = proxy;
                                #install_pass
                            },
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

fn emit_specialized_resolve(path: &[u16]) -> TokenStream {
    let steps = path.iter().map(|idx| {
        let siblings = (0..*idx).map(|_| {
            quote! {
                let Some(__poc_next) = __poc_child.next_element_sibling() else {
                    ::pocopine::__private::record_plan_failure();
                    return;
                };
                __poc_child = __poc_next;
            }
        });
        quote! {
            let Some(mut __poc_child) = __poc_current.first_element_child() else {
                ::pocopine::__private::record_plan_failure();
                return;
            };
            #(#siblings)*
            __poc_current = __poc_child;
        }
    });
    quote! {
        let __poc_el = {
            let mut __poc_current = ::core::clone::Clone::clone(root);
            #(#steps)*
            __poc_current
        };
    }
}

fn emit_specialized_ref(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_ref(
            &__poc_el,
            scope_id,
            &__poc_plan.refs[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_binding(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_binding(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.bindings[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_listener(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_listener(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.listeners[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_child_mount(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_child_mount(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.child_mounts[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_for_plan(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_for_plan(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.for_plans[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_teleport_plan(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_teleport_plan(
            &__poc_el,
            &__poc_plan.teleport_plans[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_if_plan(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_if_plan(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.if_plans[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_match_plan(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_match_plan(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.match_plans[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_slot_outlet(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        if let ::core::option::Option::Some(__poc_slot) =
            ::pocopine::__private::capture_static_slot_outlet(
                &__poc_el,
                &__poc_plan.slot_outlets[#idx],
                __poc_template_name,
            )
        {
            __poc_slot_outlets.push(__poc_slot);
        }
    }
}

fn emit_specialized_opaque_directive(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_opaque_directive(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.opaque_directives[#idx],
            __poc_template_name,
        );
    }
}

fn emit_specialized_interp(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        if let ::core::option::Option::Some(__poc_target) =
            ::pocopine::__private::capture_static_interp_target(
                &__poc_el,
                &__poc_plan.interps[#idx],
                __poc_template_name,
            )
        {
            __poc_interp_targets.push(__poc_target);
        }
    }
}

fn emit_specialized_native_model(idx: usize, path: &[u16]) -> TokenStream {
    let idx = syn::Index::from(idx);
    let resolve = emit_specialized_resolve(path);
    quote! {
        #resolve
        ::pocopine::__private::install_static_native_model(
            &__poc_el,
            scope_id,
            proxy,
            &__poc_plan.native_models[#idx],
        );
    }
}

fn emit_binding(b: &BindingLite) -> TokenStream {
    let path = emit_node_path(&b.node_path);
    let expr = proc_macro2::Literal::string(&b.expr_src);
    let compiled = emit_compiled_expr_option(&b.expr_src);
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
            compiled: #compiled,
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

fn emit_compiled_expr_option(src: &str) -> TokenStream {
    // Parse failures are surfaced as compile errors so the directive
    // messages from `pocopine-expr` (e.g. "`===` is not supported")
    // fail the build instead of silently emitting `compiled: None`
    // and panicking at runtime via `templates_plan::fail()`. Valid
    // expressions that simply aren't compile-time-representable
    // (multi-segment paths, ternaries, calls, …) still fall through
    // to runtime evaluation by emitting `Option::None`.
    match pocopine_expr::parse(src) {
        Ok(expr) => match emit_compiled_expr(&expr) {
            Some(compiled) => quote! { ::core::option::Option::Some(#compiled) },
            None => quote! { ::core::option::Option::None },
        },
        Err(err) => {
            let hint = err
                .hint
                .as_deref()
                .map(|h| format!("\n  hint: {h}"))
                .unwrap_or_default();
            let msg = format!(
                "pocopine: pine-expr parse error in `{src}`: {message} (at {start}..{end}){hint}",
                message = err.message,
                start = err.span.start,
                end = err.span.end,
            );
            quote! { ::core::compile_error!(#msg) }
        }
    }
}

fn emit_compiled_expr(expr: &pocopine_expr::Spanned<pocopine_expr::Expr>) -> Option<TokenStream> {
    use pocopine_expr::{BinOp, Expr, Literal};

    match &expr.value {
        Expr::Literal(literal) => {
            let lit = match literal {
                Literal::Null => quote! { ::pocopine::__private::StaticLiteral::Null },
                Literal::Bool(value) => {
                    quote! { ::pocopine::__private::StaticLiteral::Bool(#value) }
                }
                Literal::Number(value) => {
                    let value = proc_macro2::Literal::f64_unsuffixed(*value);
                    quote! { ::pocopine::__private::StaticLiteral::Number(#value) }
                }
                Literal::String(value) => {
                    let value = proc_macro2::Literal::string(value);
                    quote! { ::pocopine::__private::StaticLiteral::String(#value) }
                }
            };
            Some(quote! { &::pocopine::__private::StaticExpr::Literal(#lit) })
        }
        Expr::Path(segments) if (1..=2).contains(&segments.len()) => {
            let segments = segments.iter().map(|segment| {
                let segment = proc_macro2::Literal::string(segment);
                quote! { #segment }
            });
            Some(quote! { &::pocopine::__private::StaticExpr::Path(&[ #(#segments),* ]) })
        }
        Expr::Not(inner) => {
            let inner = emit_compiled_expr(inner)?;
            Some(quote! { &::pocopine::__private::StaticExpr::Not(#inner) })
        }
        Expr::BinOp(op, lhs, rhs) => {
            let op = match op {
                BinOp::And => quote! { ::pocopine::__private::StaticBinOp::And },
                BinOp::Or => quote! { ::pocopine::__private::StaticBinOp::Or },
                BinOp::Eq => quote! { ::pocopine::__private::StaticBinOp::Eq },
                BinOp::Ne => quote! { ::pocopine::__private::StaticBinOp::Ne },
                BinOp::Lt => quote! { ::pocopine::__private::StaticBinOp::Lt },
                BinOp::Le => quote! { ::pocopine::__private::StaticBinOp::Le },
                BinOp::Gt => quote! { ::pocopine::__private::StaticBinOp::Gt },
                BinOp::Ge => quote! { ::pocopine::__private::StaticBinOp::Ge },
                BinOp::Plus => return None,
            };
            let lhs = emit_compiled_expr(lhs)?;
            let rhs = emit_compiled_expr(rhs)?;
            Some(quote! {
                &::pocopine::__private::StaticExpr::BinOp {
                    op: #op,
                    lhs: #lhs,
                    rhs: #rhs,
                }
            })
        }
        Expr::Ternary(_, _, _) | Expr::Call(_, _) | Expr::Assign(_, _) | Expr::Seq(_) => None,
        Expr::Path(_) => None,
    }
}

fn emit_if_plan(ip: &IfPlanLite) -> TokenStream {
    let path = emit_node_path(&ip.template_node_path);
    let expr = proc_macro2::Literal::string(&ip.expr_src);
    let compiled = emit_compiled_expr_option(&ip.expr_src);
    let teleport_selector_tokens = match ip.teleport_selector.as_deref() {
        Some(selector) => {
            let selector = proc_macro2::Literal::string(selector);
            quote! { ::core::option::Option::Some(#selector) }
        }
        None => quote! { ::core::option::Option::None },
    };
    let opt_body = |ident: &Option<syn::Ident>| match ident {
        Some(ident) => quote! { ::core::option::Option::Some(#ident) },
        None => quote! { ::core::option::Option::None },
    };
    let body_tokens = opt_body(&ip.body_fn_ident);
    let else_if_tokens = ip.else_if.iter().map(|(expr_src, body)| {
        let e = proc_macro2::Literal::string(expr_src);
        let c = emit_compiled_expr_option(expr_src);
        let b = opt_body(body);
        quote! {
            ::pocopine::__private::CondBranch {
                expr_src: #e,
                compiled: #c,
                body: #b,
            }
        }
    });
    let has_else = ip.has_else;
    let else_body_tokens = opt_body(&ip.else_body);
    let consumed_count = ip.consumed_count;
    quote! {
        ::pocopine::__private::StaticCondPlan {
            template_node_path: #path,
            expr_src: #expr,
            compiled: #compiled,
            body: #body_tokens,
            else_if: &[ #(#else_if_tokens),* ],
            has_else: #has_else,
            else_body: #else_body_tokens,
            consumed_count: #consumed_count,
            teleport_selector: #teleport_selector_tokens,
        }
    }
}

fn emit_match_plan(mp: &MatchPlanLite) -> TokenStream {
    let path = emit_node_path(&mp.template_node_path);
    let expr = proc_macro2::Literal::string(&mp.expr_src);
    let compiled = emit_compiled_expr_option(&mp.expr_src);
    let teleport_selector_tokens = match mp.teleport_selector.as_deref() {
        Some(selector) => {
            let selector = proc_macro2::Literal::string(selector);
            quote! { ::core::option::Option::Some(#selector) }
        }
        None => quote! { ::core::option::Option::None },
    };
    let case_tokens = mp.cases.iter().map(|(tags, bind_name, body)| {
        let tag_lits = tags.iter().map(|t| proc_macro2::Literal::string(t));
        let bind_tokens = match bind_name.as_deref() {
            Some(name) => {
                let lit = proc_macro2::Literal::string(name);
                quote! { ::core::option::Option::Some(#lit) }
            }
            None => quote! { ::core::option::Option::None },
        };
        let body_tokens = match body {
            Some(ident) => quote! { ::core::option::Option::Some(#ident) },
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            ::pocopine::__private::MatchCase {
                tags: &[ #(#tag_lits),* ],
                bind_name: #bind_tokens,
                body: #body_tokens,
            }
        }
    });
    quote! {
        ::pocopine::__private::StaticMatchPlan {
            template_node_path: #path,
            expr_src: #expr,
            compiled: #compiled,
            cases: &[ #(#case_tokens),* ],
            teleport_selector: #teleport_selector_tokens,
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

fn emit_interp(ip: &InterpLite) -> TokenStream {
    let path = emit_node_path(&ip.node_path);
    let text_index = ip.text_index;
    let segment_tokens = ip.segments.iter().map(|s| match s {
        InterpSegment::Static(text) => {
            let lit = proc_macro2::Literal::string(text);
            quote! { ::pocopine::__private::PlannedSegment::Static(#lit) }
        }
        InterpSegment::Dynamic(src) => {
            let lit = proc_macro2::Literal::string(src);
            quote! { ::pocopine::__private::PlannedSegment::Dynamic(#lit) }
        }
    });
    quote! {
        ::pocopine::__private::StaticInterp {
            node_path: #path,
            text_index: #text_index,
            segments: &[ #(#segment_tokens),* ],
        }
    }
}

fn emit_opaque_directive(d: &OpaqueDirectiveLite) -> TokenStream {
    let path = emit_node_path(&d.node_path);
    let name = proc_macro2::Literal::string(&d.name);
    let arg_tokens = match &d.arg {
        Some(a) => {
            let lit = proc_macro2::Literal::string(a);
            quote! { Some(#lit) }
        }
        None => quote! { None },
    };
    let modifiers_tokens = d.modifiers.iter().map(|m| {
        let lit = proc_macro2::Literal::string(m);
        quote! { #lit }
    });
    let value = proc_macro2::Literal::string(&d.value);
    quote! {
        ::pocopine::__private::StaticOpaqueDirective {
            node_path: #path,
            name: #name,
            arg: #arg_tokens,
            modifiers: &[ #(#modifiers_tokens),* ],
            value: #value,
        }
    }
}

fn emit_child_mount(c: &ChildMountLite) -> TokenStream {
    let path = emit_node_path(&c.node_path);
    let tag = proc_macro2::Literal::string(&c.tag);
    let slot_tokens = c.slot_fragments.iter().map(|(name, ident, pp_let)| {
        let name_lit = proc_macro2::Literal::string(name);
        let scoped_let_tokens = match pp_let {
            Some(let_ident) => {
                let lit = proc_macro2::Literal::string(let_ident);
                quote! { Some(#lit) }
            }
            None => quote! { None },
        };
        quote! {
            ::pocopine::__private::StaticSlotFragment {
                name: #name_lit,
                fragment: #ident,
                scoped_let: #scoped_let_tokens,
            }
        }
    });
    let show_tokens = c.host_shows.iter().map(|expr_src| {
        let expr = proc_macro2::Literal::string(expr_src);
        let compiled = emit_compiled_expr_option(expr_src);
        quote! {
            ::pocopine::__private::StaticChildHostShow {
                expr_src: #expr,
                compiled: #compiled,
            }
        }
    });
    let binding_tokens = c.host_bindings.iter().map(|b| {
        let arg = proc_macro2::Literal::string(&b.arg);
        let expr = proc_macro2::Literal::string(&b.expr_src);
        let compiled = emit_compiled_expr_option(&b.expr_src);
        quote! {
            ::pocopine::__private::StaticChildHostBinding {
                arg: #arg,
                expr_src: #expr,
                compiled: #compiled,
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
            shows: &[ #(#show_tokens),* ],
            bindings: &[ #(#binding_tokens),* ],
            listeners: &[ #(#listener_tokens),* ],
            models: &[ #(#model_tokens),* ],
        }
    }
}

// ─── walk + classification ───────────────────────────────────────

#[derive(Clone)]
struct OwnedContentMarker {
    path: Vec<u16>,
    opening_tag_range: std::ops::Range<usize>,
    invalid_reason: Option<String>,
}

/// Compile the generic `pp-owned-content` marker before normal directive
/// lifting starts.
///
/// This is deliberately a whole-template pass. Normal template-plan walking
/// hands structural bodies and projected slot fragments to isolated analysis
/// contexts, but an owned-content outlet must be proven relative to the
/// component's one stable rendered root. Seeing it only inside a lifted
/// context would therefore be too late and could accidentally emit metadata
/// for a node that is absent, moved, or owned by another component at runtime.
fn analyze_owned_content_outlet(ast: &TemplateAst, ctx: &mut AnalysisCtx) -> Option<Vec<u16>> {
    let mut markers = Vec::new();
    if let Some(root) = ast.element_roots().next() {
        scan_owned_content_outlet(root, &mut Vec::new(), None, ctx, &mut markers);
    }

    if markers.len() > 1 {
        let first = &markers[0];
        for duplicate in markers.iter().skip(1) {
            let message = "duplicate `pp-owned-content` marker — a component template may expose exactly one owned-content outlet; remove this marker or the earlier one";
            if source_range_is_usable(ast, &duplicate.opening_tag_range)
                && source_range_is_usable(ast, &first.opening_tag_range)
            {
                ctx.diagnostics.push_at_with_context(
                    message,
                    duplicate.opening_tag_range.clone(),
                    "first `pp-owned-content` marker is here",
                    first.opening_tag_range.clone(),
                );
            } else if source_range_is_usable(ast, &duplicate.opening_tag_range) {
                ctx.diagnostics
                    .push_at(message, duplicate.opening_tag_range.clone());
            } else {
                ctx.diagnostics.push(message);
            }
        }
        return None;
    }

    let marker = markers.pop()?;
    if let Some(reason) = marker.invalid_reason {
        if source_range_is_usable(ast, &marker.opening_tag_range) {
            ctx.diagnostics
                .push_at(reason, marker.opening_tag_range.clone());
        } else {
            ctx.diagnostics.push(reason);
        }
        return None;
    }

    Some(marker.path)
}

fn source_range_is_usable(ast: &TemplateAst, range: &std::ops::Range<usize>) -> bool {
    range.start < range.end && range.end <= ast.source.len()
}

fn scan_owned_content_outlet(
    el: &Element,
    path: &mut Vec<u16>,
    inherited_invalid_reason: Option<String>,
    ctx: &mut AnalysisCtx,
    markers: &mut Vec<OwnedContentMarker>,
) {
    let marker_value = el
        .attrs
        .iter()
        .find(|(name, _)| name == "pp-owned-content")
        .map(|(_, value)| value.as_str());

    if let Some(marker_value) = marker_value {
        // Compile-only marker: it must never survive into registered HTML,
        // including diagnostics-only expansions.
        ctx.stripped.push(StrippedAttr {
            node_path: path.clone(),
            name: "pp-owned-content".to_string(),
        });

        let invalid_reason =
            owned_content_invalid_reason(el, marker_value, inherited_invalid_reason.as_deref());
        markers.push(OwnedContentMarker {
            path: path.clone(),
            opening_tag_range: el.opening_tag_range.clone(),
            invalid_reason,
        });
    }

    let descendant_invalid_reason = owned_content_descendant_boundary(el)
        .or(inherited_invalid_reason.as_deref())
        .map(str::to_string);

    for (source_index, child) in el.children.iter().enumerate() {
        let Node::Element(child_el) = child else {
            continue;
        };
        let element_index = el
            .children
            .iter()
            .take(source_index)
            .filter(|node| matches!(node, Node::Element(_)))
            .count() as u16;
        path.push(element_index);
        scan_owned_content_outlet(
            child_el,
            path,
            descendant_invalid_reason.clone(),
            ctx,
            markers,
        );
        path.pop();
    }
}

fn owned_content_invalid_reason(
    el: &Element,
    marker_value: &str,
    inherited_invalid_reason: Option<&str>,
) -> Option<String> {
    if !marker_value.trim().is_empty() {
        return Some(
            "`pp-owned-content` is a valueless compile-time marker; write `pp-owned-content` without an expression or attribute value"
                .to_string(),
        );
    }
    if !is_plan_native(&el.tag) {
        if el.tag == "pp-component" {
            return Some(
                "`pp-owned-content` cannot be placed on `<pp-component>` because its child type and DOM are dynamic; mark one unconditional native element in the owning component shell"
                    .to_string(),
            );
        }
        return Some(format!(
            "`pp-owned-content` cannot be placed on component tag `<{}>`; mark one unconditional native element inside the owning component template",
            el.tag
        ));
    }
    if el.tag == "slot" {
        return Some(
            "`pp-owned-content` cannot be placed on `<slot>` because slot materialization replaces that node; mark an unconditional native element outside the slot"
                .to_string(),
        );
    }
    if let Some(reason) = owned_content_local_boundary(el) {
        return Some(reason.to_string());
    }
    if el.tag == "template" {
        return Some(
            "`pp-owned-content` cannot be placed on `<template>` because template contents are not a stable live element; mark one unconditional native element in the mounted shell"
                .to_string(),
        );
    }
    if is_void_element(&el.tag) {
        return Some(format!(
            "`pp-owned-content` cannot be placed on void element `<{}>` because it cannot own child DOM; use a non-void native container",
            el.tag
        ));
    }
    inherited_invalid_reason.map(str::to_string)
}

fn owned_content_descendant_boundary(el: &Element) -> Option<&'static str> {
    if !is_plan_native(&el.tag) {
        return if el.tag == "pp-component" {
            Some(
                "`pp-owned-content` cannot appear inside `<pp-component>` content because the selected component owns that dynamic subtree; move the outlet into the owning component's unconditional native shell",
            )
        } else {
            Some(
                "`pp-owned-content` cannot appear in projected component content because the child component owns and may replace that subtree; move the outlet into this component's own unconditional native shell",
            )
        };
    }
    if let Some(reason) = owned_content_local_boundary(el) {
        return Some(reason);
    }
    if el.tag == "slot" {
        return Some(
            "`pp-owned-content` cannot appear in slot fallback/projected content because slot materialization does not preserve a stable root-relative path; move the outlet outside the slot",
        );
    }
    if el.tag == "template" {
        return Some(
            "`pp-owned-content` cannot appear inside a `<template>` because its contents are detached, cloned, or conditionally materialized; move the outlet into the unconditional mounted shell",
        );
    }
    None
}

fn owned_content_local_boundary(el: &Element) -> Option<&'static str> {
    if el.attrs.iter().any(|(name, _)| name == "pp-as") {
        return Some(
            "`pp-owned-content` cannot appear on or below `pp-as` because the rendered root is dynamically hoisted; move the outlet into an unconditional native shell without `pp-as`",
        );
    }
    if el.attrs.iter().any(|(name, _)| {
        matches!(
            name.as_str(),
            "pp-if" | "pp-else-if" | "pp-else" | "pp-for" | "pp-match" | "pp-case" | "pp-teleport"
        )
    }) {
        return Some(
            "`pp-owned-content` must be unconditional and cannot appear on or inside `pp-if`, `pp-for`, `pp-match`/`pp-case`, or `pp-teleport`; move the outlet outside the structural branch",
        );
    }
    if el
        .attrs
        .iter()
        .any(|(name, _)| matches!(name.as_str(), "pp-text" | "pp-html"))
    {
        return Some(
            "`pp-owned-content` cannot appear on or below `pp-text`/`pp-html` because that directive replaces child DOM; move the outlet outside the replacing directive",
        );
    }
    None
}

fn walk(el: &Element, ctx: &mut AnalysisCtx, emissions: &mut Emissions, path: &mut Vec<u16>) {
    if el.synthetic {
        // Synthetic elements (html5ever auto-inserted) confuse
        // the path-indexing model since the runtime walks
        // authored structure. Skip them entirely — every
        // directive on or under a synthetic node falls back to
        // the mount.
        return;
    }

    // RFC-058 Phase 4.2 — `pp-for` on a `<template>` host
    // graduates into a `StaticForPlan` entry. Same eligibility
    // shape as Phase 4.1's pp-if: must be on `<template>`,
    // parseable `<item> in <items>`, no co-occurring
    // `pp-teleport` (defer that combo to the mount — the
    // applier doesn't capture teleport targets in v1). The
    // `data-pp-row-plan` attribute the §6.2 layering bakes
    // into the cleaned HTML stays alongside the strip so the
    // RFC-054 row-plan registry still resolves keyed lists.
    // RFC-094 — a stray pp-case is a build error: cases only
    // live as direct children of a `<template pp-match>`.
    if el.attrs.iter().any(|(n, _)| n == "pp-case") {
        ctx.diagnostics.push(
            "`pp-case` is only valid as a direct child of a `<template pp-match>` (RFC-094)"
                .to_string(),
        );
        return;
    }

    // RFC-094 — a pp-else-if / pp-else template reaching the
    // normal walk means no adjacent chain head consumed it.
    if let Some((is_else, _)) = chain_member_kind(el) {
        ctx.diagnostics.push(format!(
            "`pp-{}` has no adjacent `<template pp-if>` / `<template pp-else-if>` \
             sibling — RFC-094 chains are contiguous <template> siblings \
             (whitespace and comments between members are fine)",
            if is_else { "else" } else { "else-if" },
        ));
        return;
    }

    if let Some(for_attr) = pp_for_value(el) {
        if el.tag != "template" {
            // RFC-011 iterated slots: `<li pp-for="file in files">`
            // wrapping a `<slot>` outlet is the documented shape
            // (docs/guides/components/03-composition.md) — the slot
            // publication machinery owns that subtree, not the plan.
            // The pp-for value itself is still validated; directives
            // deeper in the exempt subtree stay slot-machinery-owned.
            if subtree_contains_slot(el) {
                match parse_pp_for(&for_attr) {
                    Some((_, items_expr)) => {
                        let _ = check_template_expr(&items_expr, "pp-for", ctx);
                    }
                    None => ctx.diagnostics.push(format!(
                        "`pp-for=\"{for_attr}\"` does not parse — expected `item in items`"
                    )),
                }
                return;
            }
            ctx.diagnostics
                .push("`pp-for` is only valid on a `<template>` (RFC-004)".to_string());
            return;
        }
        if el.attrs.iter().any(|(n, _)| n == "pp-teleport") {
            ctx.diagnostics.push(
                "`pp-teleport` cannot be combined with `pp-for` on the same `<template>` \
                 — teleport a wrapper around the list instead"
                    .to_string(),
            );
            return;
        }
        let Some((item_name, items_expr)) = parse_pp_for(&for_attr) else {
            ctx.diagnostics.push(format!(
                "`pp-for=\"{for_attr}\"` does not parse — expected `item in items`"
            ));
            return;
        };
        let Some(items_expr) = check_template_expr(&items_expr, "pp-for", ctx) else {
            return;
        };
        {
            check_branch_body_roots("pp-for", el, ctx);
            let key_expr = el
                .attrs
                .iter()
                .find(|(n, _)| n == "pp-key")
                .map(|(_, v)| v.clone())
                .filter(|s| !s.trim().is_empty())
                .and_then(|expr| check_template_expr(&expr, "pp-key", ctx));
            let stagger_ms = match el
                .attrs
                .iter()
                .find(|(n, _)| n == "pp-stagger")
                .map(|(_, v)| v.trim().to_string())
            {
                // Empty is a truncation, not a request for 0ms —
                // diagnosed like every other junk value.
                Some(v) => v.parse::<u32>().unwrap_or_else(|_| {
                    ctx.diagnostics.push(format!(
                        "`pp-stagger=\"{v}\"` expects milliseconds, e.g. pp-stagger=\"40\""
                    ));
                    0
                }),
                None => 0,
            };
            // RFC-058 Phase 4.2c — try to lift the row body
            // into a fragment fn. Skip when an RFC-054 row
            // plan claims this site (the row-plan fast path
            // is strictly better than per-row body closures:
            // proxy elision, no
            // per-row effect creation, etc.).
            let row_plan_claims_site = ctx
                .row_plan_assignments
                .iter()
                .any(|(p, _)| p.as_slice() == path.as_slice());
            // A `None` body means it fell outside the
            // lifting envelope — the applier surfaces it via
            // `record_plan_failure` at install time and
            // renders the subtree empty.
            let mut bodies_need_proxy = false;
            let body_fn_ident = if row_plan_claims_site {
                None
            } else {
                analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                    ctx.absorb_lifted_metadata(&body_ctx);
                    bodies_need_proxy |= plan_needs_proxy(&body_ctx);
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
                bodies_need_proxy,
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
            // RFC-054 row plan (when present) or the lifted body
            // fragment. Either way the template-plan classifier
            // doesn't follow `<template>` content.
            return;
        }
    }

    // RFC-058 Phase 4.3 — `pp-teleport` on a `<template>` host
    // graduates into a `StaticTeleportPlan` entry. Eligibility:
    // host is `<template>`, no co-occurring `pp-if` (pp-if owns
    // the mount cycle in that combo and reads pp-teleport
    // itself). A co-occurring `pp-for` was diagnosed above.
    if let Some(selector) = pp_teleport_value(el) {
        let has_if = el.attrs.iter().any(|(n, _)| n == "pp-if");
        let has_for = el.attrs.iter().any(|(n, _)| n == "pp-for");
        if el.tag != "template" {
            ctx.diagnostics
                .push("`pp-teleport` is only valid on a `<template>` (RFC-006)".to_string());
            return;
        }
        if selector.trim().is_empty() {
            ctx.diagnostics.push(
                "`pp-teleport` requires a CSS selector target: pp-teleport=\"#overlay\""
                    .to_string(),
            );
            return;
        }
        if has_if && !has_for {
            // The pp-if classifier below owns the combined
            // `pp-if` + `pp-teleport` site. It records the
            // selector on StaticIfPlan and strips both source
            // attrs so runtime discovery cannot double-install
            // either controller.
        } else if !has_if && !has_for {
            // RFC-058 Phase 4.3c — try to lift the teleport
            // body into a fragment fn (same v1 envelope as
            // pp-if / pp-for body lifting).
            check_branch_body_roots("pp-teleport", el, ctx);
            let body_fn_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                ctx.absorb_lifted_metadata(&body_ctx);
                let ident = emissions.alloc_if_body_ident("teleport_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                });
                ident
            });
            // A `None` body_fn means the body fell outside the
            // lifting envelope — the applier surfaces it via
            // `record_plan_failure` at install time.
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
        // Only the `pp-if` combo continues past here — the pp-if
        // classifier below owns that site.
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

    // RFC-094 Phase 3 — `pp-match` on a `<template>` host: the
    // direct children must be `<template pp-case>` arms; each
    // arm's body lifts like a pp-if body. The whole subtree is
    // plan-owned; the walk does not descend.
    if let Some(match_expr) = el
        .attrs
        .iter()
        .find(|(n, _)| n == "pp-match")
        .map(|(_, v)| v.trim().to_string())
    {
        if el.tag != "template" {
            ctx.diagnostics
                .push("`pp-match` is only valid on a `<template>` (RFC-094)".to_string());
            return;
        }
        if match_expr.is_empty() || pocopine_expr::parse(&match_expr).is_err() {
            ctx.diagnostics.push(format!(
                "`pp-match` expression `{match_expr}` does not parse (RFC-094)",
            ));
            return;
        }
        let teleport_selector = el
            .attrs
            .iter()
            .find(|(n, _)| n == "pp-teleport")
            .map(|(_, v)| v.clone())
            .filter(|s| !s.trim().is_empty());
        let mut cases: Vec<(Vec<String>, Option<String>, Option<syn::Ident>)> = Vec::new();
        let mut bodies_need_proxy = false;
        let mut saw_wild = false;
        let mut seen_tags: Vec<String> = Vec::new();
        let mut case_elem_idx: u16 = 0;
        for child in &el.children {
            match child {
                Node::Text(t, _) if t.trim().is_empty() => {}
                Node::Comment(..) => {}
                Node::Element(case_el) => {
                    let case_path = {
                        let mut p = path.clone();
                        p.push(case_elem_idx);
                        p
                    };
                    case_elem_idx += 1;
                    let Some(arm_src) = case_el
                        .attrs
                        .iter()
                        .find(|(n, _)| n == "pp-case")
                        .map(|(_, v)| v.trim().to_string())
                    else {
                        ctx.diagnostics.push(
                            "every direct child of `<template pp-match>` must be a \
                             `<template pp-case>` arm (RFC-094)"
                                .to_string(),
                        );
                        continue;
                    };
                    if case_el.tag != "template" {
                        ctx.diagnostics.push(
                            "`pp-case` is only valid on a `<template>` (RFC-094)".to_string(),
                        );
                        continue;
                    }
                    if saw_wild {
                        ctx.diagnostics.push(
                            "unreachable `pp-case` after the `_` wildcard arm (RFC-094)"
                                .to_string(),
                        );
                    }
                    let tags: Vec<String> = if arm_src == "_" {
                        saw_wild = true;
                        Vec::new()
                    } else {
                        let parsed: Vec<String> =
                            arm_src.split('|').map(|t| t.trim().to_string()).collect();
                        let well_formed = !parsed.is_empty()
                            && parsed.iter().all(|t| {
                                !t.is_empty()
                                    && t.chars().all(|c| c.is_alphanumeric() || c == '_')
                                    && t.chars().next().is_some_and(|c| !c.is_ascii_digit())
                            });
                        if !well_formed {
                            ctx.diagnostics.push(format!(
                                "`pp-case=\"{arm_src}\"` — arms are literal variant names \
                                 (`Ready`, `Idle | Loading`) or `_`, not expressions (RFC-094)",
                            ));
                            continue;
                        }
                        for t in &parsed {
                            if seen_tags.contains(t) {
                                ctx.diagnostics
                                    .push(format!("duplicate `pp-case` variant `{t}` (RFC-094)",));
                            }
                            seen_tags.push(t.clone());
                        }
                        parsed
                    };
                    let bind_name = case_el
                        .attrs
                        .iter()
                        .find(|(n, _)| n == "pp-let")
                        .map(|(_, v)| v.trim().to_string())
                        .filter(|v| !v.is_empty());
                    check_branch_body_roots("pp-case", case_el, ctx);
                    let body_ident =
                        analyze_lift_body(case_el, emissions).map(|(html, body_ctx)| {
                            ctx.absorb_lifted_metadata(&body_ctx);
                            bodies_need_proxy |= plan_needs_proxy(&body_ctx);
                            let ident = emissions.alloc_if_body_ident("case_body");
                            emissions.if_bodies.push(IfBodyEmission {
                                ident: ident.clone(),
                                html,
                                plan: body_ctx,
                            });
                            ident
                        });
                    ctx.stripped.push(StrippedAttr {
                        node_path: case_path.clone(),
                        name: "pp-case".to_string(),
                    });
                    if bind_name.is_some() {
                        ctx.stripped.push(StrippedAttr {
                            node_path: case_path,
                            name: "pp-let".to_string(),
                        });
                    }
                    cases.push((tags, bind_name, body_ident));
                }
                _ => {
                    ctx.diagnostics.push(
                        "`<template pp-match>` may only contain `pp-case` arms \
                         (and whitespace/comments) (RFC-094)"
                            .to_string(),
                    );
                }
            }
        }
        ctx.match_plans.push(MatchPlanLite {
            template_node_path: path.clone(),
            expr_src: match_expr,
            teleport_selector: teleport_selector.clone(),
            cases,
            bodies_need_proxy,
        });
        ctx.stripped.push(StrippedAttr {
            node_path: path.clone(),
            name: "pp-match".to_string(),
        });
        if teleport_selector.is_some() {
            ctx.stripped.push(StrippedAttr {
                node_path: path.clone(),
                name: "pp-teleport".to_string(),
            });
        }
        return;
    }

    // RFC-058 Phase 4.1b — `pp-if` on a `<template>` host
    // graduates into a `StaticIfPlan` entry. The applier
    // resolves the template + parses the expression at compile
    // time; the macro strips the `pp-if` attribute from the
    // cleaned HTML so no second installer can see it. The template
    // body lives in `<template>.content` (a separate
    // `DocumentFragment` that doesn't appear in `el.children`),
    // body therefore needs its own compiled fragment function.
    if let Some(if_expr) = pp_if_value(el) {
        if el.tag != "template" {
            ctx.diagnostics
                .push("`pp-if` is only valid on a `<template>` (RFC-094)".to_string());
            return;
        }
        let Some(if_expr) = check_template_expr(&if_expr, "pp-if", ctx) else {
            return;
        };
        let teleport_selector = el
            .attrs
            .iter()
            .find(|(n, _)| n == "pp-teleport")
            .map(|(_, v)| v.clone())
            .filter(|s| !s.trim().is_empty());
        {
            // Exactly one element root, enforced at compile time —
            // the runtime clone stamps only the FIRST root, so a
            // multi-root body used to silently drop its extra roots
            // out of the conditional (and a zero-root body errors
            // at install).
            check_branch_body_roots("pp-if", el, ctx);
            // RFC-058 Phase 4.1d — try to lift the body
            // subtree into a fragment fn the runtime installer invokes.
            // `body_fn_ident = None` is a fail-fast path, not a directive
            // fallback: a static clone may render, but native directives in it
            // remain inert and the runtime records a plan failure.
            let mut bodies_need_proxy = false;
            let body_fn_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                ctx.absorb_lifted_metadata(&body_ctx);
                bodies_need_proxy |= plan_needs_proxy(&body_ctx);
                let ident = emissions.alloc_if_body_ident("if_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                });
                ident
            });
            // A `None` body_fn means the body fell outside the
            // lifting envelope — the applier surfaces it via
            // `record_plan_failure` at install time.
            ctx.if_plans.push(IfPlanLite {
                template_node_path: path.clone(),
                expr_src: if_expr,
                teleport_selector: teleport_selector.clone(),
                else_if: Vec::new(),
                has_else: false,
                else_body: None,
                consumed_count: 0,
                bodies_need_proxy,
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
    }

    if el.tag == "pp-component" {
        let has_is_binding = el
            .attrs
            .iter()
            .any(|(name, _)| matches!(name.as_str(), ":is" | "pp-bind:is"));
        if !has_is_binding {
            ctx.diagnostics.push(
                "`<pp-component>` requires a reactive `:is=\"...\"` binding (RFC-112)".to_string(),
            );
        }
        if el.attrs.iter().any(|(name, _)| name == "pp-as") {
            ctx.diagnostics.push(
                "`pp-as` is not valid on the reserved `<pp-component>` sentinel (RFC-112)"
                    .to_string(),
            );
        }
    }

    // Whole-subtree boundary: non-HTML5 tags (per council pass 3
    // amendment to RFC-058 §6.2). The element's own attributes
    // and descendants stay mount-owned (slot content is the
    // common case there), but RFC-058 Phase 3 captures the
    // mount site itself: the runtime applier calls
    // [`crate::mount::mount_child_component`] before any later
    // component-discovery pass reaches the tag; the host's
    // `__pp_mounted` guard makes that attempt a no-op.
    if !is_plan_native(&el.tag) {
        let mut host_shows = Vec::new();
        let mut host_bindings = Vec::new();
        let mut host_listeners = Vec::new();
        let mut host_models = Vec::new();
        let has_pp_as = el.attrs.iter().any(|(name, _)| name == "pp-as");
        if !has_pp_as {
            for (name, value) in &el.attrs {
                match classify_child_host_attr(name, value, ctx) {
                    ChildHostAttrOutcome::Show(expr_src) => {
                        ctx.stripped.push(StrippedAttr {
                            node_path: path.clone(),
                            name: name.clone(),
                        });
                        host_shows.push(expr_src);
                    }
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
                    ChildHostAttrOutcome::Ref(ref_name) => {
                        ctx.stripped.push(StrippedAttr {
                            node_path: path.clone(),
                            name: name.clone(),
                        });
                        ctx.refs.push(RefLite {
                            node_path: path.clone(),
                            name: ref_name,
                        });
                    }
                    ChildHostAttrOutcome::Preserved => {}
                }
            }
        }
        // RFC-058 Phase 3.5b–3.5g — partition the custom tag's
        // children into one default-slot subtree + N named-slot
        // subtrees (`<template pp-slot="NAME">…</template>`),
        // and lift each independently into a slot fragment fn.
        // The parent passes the fragments via the static-plan
        // child-mount entry; the runtime `materialize_slot`
        // looks them up by name before falling back to the
        // legacy mount capture path.
        //
        // `<template pp-slot="N" pp-let="ident">` (Phase 3.5g)
        // also lifts: we record the `pp-let` identifier on the
        // slot-fragment entry so the runtime materialiser
        // builds a `SlotScope` from the child's `<slot>`
        // bindings and invokes the fragment against the slot
        // scope's proxy. Eligibility is the same as plain
        // named slots — the body must be plan-eligible —
        // unliftable bodies surface via `record_plan_failure`
        // at install time.
        //
        // The `<template pp-slot>` element itself stays in the
        // cleaned HTML in every case so `capture_slots` can
        // still pick it up; for lifted slots, the fragment
        // registry wins the lookup race in `materialize_slot`.
        let mut slot_fragments: Vec<(String, syn::Ident, Option<String>)> = Vec::new();
        if !el.children.is_empty() {
            let mut default_children: Vec<Node> = Vec::new();
            for child in &el.children {
                let tpl = child.as_element().filter(|e| e.tag == "template");
                let Some(tpl) = tpl else {
                    default_children.push(child.clone());
                    continue;
                };
                let pp_slot = tpl
                    .attrs
                    .iter()
                    .find(|(n, _)| n == "pp-slot")
                    .map(|(_, v)| v.trim().to_string())
                    .filter(|s| !s.is_empty());
                let Some(slot_name) = pp_slot else {
                    // `<template>` without a meaningful pp-slot
                    // attr falls into the default subtree —
                    // mirrors `capture_slots`'s catch-all.
                    default_children.push(child.clone());
                    continue;
                };
                let pp_let = tpl
                    .attrs
                    .iter()
                    .find(|(n, _)| n == "pp-let")
                    .map(|(_, v)| v.trim().to_string())
                    .filter(|s| !s.is_empty());
                match analyze_slot_subtree(&tpl.children, emissions) {
                    Some(emission) => {
                        // RFC 081 P2 — absorb pp-ref names from
                        // the dynamic slot's plan so the outer
                        // `<ComponentName>Refs` exposes them.
                        if let SlotFragmentEmission::Dynamic { plan, .. } = &emission {
                            ctx.absorb_lifted_metadata(plan);
                        }
                        // Duplicate `pp-slot=NAME` at compile time:
                        // both lift fragments get pushed; the
                        // runtime `SlotSet::named` HashMap insert
                        // means the LAST one wins, matching the
                        // mount's "later wins" semantics on
                        // duplicate captures.
                        slot_fragments.push((slot_name, emission.ident().clone(), pp_let));
                        emissions.slot_fragments.push(emission);
                    }
                    None => {
                        // Slot body falls outside the lift envelope
                        // (e.g. pp-route content): no fragment is
                        // emitted; the runtime falls back to the
                        // light-DOM capture path for this slot.
                    }
                }
            }
            // Skip lifting a default fragment when the leftover
            // children are pure whitespace — formatter newlines
            // around a `<template pp-slot>` block shouldn't
            // synthesise a default slot fragment the child never
            // queries (the legacy capture path's matching default
            // entry is similarly inert in that case).
            let has_meaningful_default = default_children.iter().any(|n| match n {
                Node::Text(text, _) => !text.trim().is_empty(),
                Node::Comment(_, _) => false,
                _ => true,
            });
            if has_meaningful_default {
                match analyze_slot_subtree(&default_children, emissions) {
                    Some(emission) => {
                        // RFC 081 P2 — absorb pp-ref names from
                        // the default slot's plan.
                        if let SlotFragmentEmission::Dynamic { plan, .. } = &emission {
                            ctx.absorb_lifted_metadata(plan);
                        }
                        slot_fragments.push((
                            "default".to_string(),
                            emission.ident().clone(),
                            None,
                        ));
                        emissions.slot_fragments.push(emission);
                    }
                    None => {
                        // Unliftable default slot content — no
                        // fragment is emitted; the runtime falls
                        // back to the light-DOM capture path.
                    }
                }
            }
        }
        ctx.child_mounts.push(ChildMountLite {
            node_path: path.clone(),
            tag: el.tag.clone(),
            slot_fragments,
            host_shows,
            host_bindings,
            host_listeners,
            host_models,
        });
        return;
    }

    // Classify every attribute on this element. `pp-as` is
    // component-only (RFC-019); component tags took the
    // child-mount branch above, so a native element carrying it
    // used to be skipped whole-subtree — every directive in the
    // subtree died silently with it.
    if el.attrs.iter().any(|(name, _)| name == "pp-as") && el.tag != "root" {
        ctx.diagnostics
            .push("`pp-as` is only valid on a component tag (RFC-019)".to_string());
        return;
    }
    let host_is_native = is_plan_native(&el.tag);
    for (name, value) in &el.attrs {
        if el.tag == "root" && name == "pp-as" {
            continue;
        }
        // Outcome is recorded on `ctx` (stripped entries + plan
        // vecs); nothing branch-worthy remains at this call site.
        let _ = classify_attr(name, value, path, ctx, &el.tag, host_is_native);
    }

    // RFC-058 Phase 6.2 — scan direct text-node children for
    // `{{expr}}` interpolation and lift each into an
    // `InterpLite` entry at compile time.
    // Skip elements that already use `pp-text` (RFC-025: the
    // directive owns the textContent, interpolation is
    // intentionally disabled for them).
    let owns_text = el
        .attrs
        .iter()
        .any(|(n, _)| n == "pp-text" || n == "pp-html");
    if !owns_text {
        let mut text_index: u16 = 0;
        for child in &el.children {
            let Node::Text(text, _) = child else { continue };
            if !text.contains("{{") {
                text_index += 1;
                continue;
            }
            match parse_interp_segments(text) {
                Ok(mut segments) if !segments.is_empty() => {
                    // JS-equality desugar for `{{ a === b }}` — hard
                    // parse failures stay on the emit path, which
                    // already produces the teaching compile error.
                    for segment in &mut segments {
                        let InterpSegment::Dynamic(src) = segment else {
                            continue;
                        };
                        if pocopine_expr::parse(src).is_err()
                            && let Some(rewritten) = desugar_js_equality(src)
                            && pocopine_expr::parse(&rewritten).is_ok()
                        {
                            ctx.warnings.push(format!(
                                "pocopine: `{{{{ {src} }}}}` uses JavaScript equality \
                                 (`===`/`!==`); pine-expr is Rust-style. Interpreted as \
                                 `{rewritten}` — update the template to `==`/`!=`."
                            ));
                            *src = rewritten;
                        }
                    }
                    let any_dynamic = segments
                        .iter()
                        .any(|s| matches!(s, InterpSegment::Dynamic(_)));
                    if any_dynamic {
                        ctx.interps.push(InterpLite {
                            node_path: path.clone(),
                            text_index,
                            segments,
                        });
                    }
                }
                _ => {
                    // Parse error — leave the text untouched so
                    // the mount's runtime scanner surfaces the
                    // same error message at apply time. Don't
                    // mark managed.
                }
            }
            text_index += 1;
        }
    }

    // Recurse into element children. Path indices are over
    // *element* children only — text / comments don't shift the
    // index (matches `Element.children` in JS DOM and the
    // for_plan mount's convention).
    let mut consumed_chain: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (i, child) in el.children.iter().enumerate() {
        if consumed_chain.contains(&i) {
            continue;
        }
        if let Node::Element(child_el) = child {
            let idx = el
                .children
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            walk(child_el, ctx, emissions, path);
            // RFC-094 — chain scan: if this child just classified
            // as a chain head (an eligible `<template pp-if>`),
            // fold the following pp-else-if / pp-else siblings
            // into the same plan entry, skipping their walks.
            let is_chain_head = ctx
                .if_plans
                .last()
                .map(|p| p.template_node_path.as_slice() == path.as_slice())
                .unwrap_or(false);
            if is_chain_head {
                let mut member_offset: u16 = 0;
                let mut saw_else = false;
                let mut j = i + 1;
                while j < el.children.len() {
                    match &el.children[j] {
                        Node::Text(t, _) if t.trim().is_empty() => {
                            j += 1;
                        }
                        Node::Comment(..) => {
                            j += 1;
                        }
                        Node::Element(member) => {
                            let Some((is_else, raw_expr)) = chain_member_kind(member) else {
                                break;
                            };
                            consumed_chain.insert(j);
                            member_offset += 1;
                            let member_path = {
                                let mut p = path.clone();
                                *p.last_mut().expect("chain head has an index") =
                                    idx + member_offset;
                                p
                            };
                            let kind = if is_else { "pp-else" } else { "pp-else-if" };
                            if member.tag != "template" {
                                ctx.diagnostics.push(format!(
                                    "`{kind}` is only valid on a `<template>` (RFC-094)",
                                ));
                                j += 1;
                                continue;
                            }
                            if saw_else {
                                ctx.diagnostics.push(format!(
                                    "`{kind}` after `pp-else` — `pp-else` must be the \
                                     final branch of its chain (RFC-094)",
                                ));
                            }
                            if member.attrs.iter().any(|(n, _)| n == "pp-teleport") {
                                ctx.diagnostics.push(
                                    "`pp-teleport` belongs on the chain head only; it \
                                     applies to every branch (RFC-094)"
                                        .to_string(),
                                );
                            }
                            if member
                                .attrs
                                .iter()
                                .any(|(n, _)| n == "pp-for" || n == "pp-match" || n == "pp-if")
                            {
                                ctx.diagnostics.push(format!(
                                    "chain member `{kind}` cannot also carry a structural \
                                     directive (RFC-094)",
                                ));
                            }
                            let expr_trim = raw_expr.trim().to_string();
                            if is_else && !expr_trim.is_empty() {
                                ctx.diagnostics.push(
                                    "`pp-else` takes no expression — use `pp-else-if` \
                                     (RFC-094)"
                                        .to_string(),
                                );
                            }
                            if !is_else
                                && (expr_trim.is_empty()
                                    || pocopine_expr::parse(&expr_trim).is_err())
                            {
                                ctx.diagnostics.push(format!(
                                    "`pp-else-if` expression `{expr_trim}` does not parse \
                                     (RFC-094)",
                                ));
                            }
                            check_branch_body_roots(kind, member, ctx);
                            let mut member_needs = false;
                            let body_ident =
                                analyze_lift_body(member, emissions).map(|(html, body_ctx)| {
                                    ctx.absorb_lifted_metadata(&body_ctx);
                                    member_needs = plan_needs_proxy(&body_ctx);
                                    let ident = emissions.alloc_if_body_ident(if is_else {
                                        "else_body"
                                    } else {
                                        "else_if_body"
                                    });
                                    emissions.if_bodies.push(IfBodyEmission {
                                        ident: ident.clone(),
                                        html,
                                        plan: body_ctx,
                                    });
                                    ident
                                });
                            ctx.stripped.push(StrippedAttr {
                                node_path: member_path.clone(),
                                name: kind.to_string(),
                            });
                            ctx.chain_member_paths.push(member_path);
                            let plan = ctx.if_plans.last_mut().expect("chain head pushed");
                            plan.consumed_count += 1;
                            plan.bodies_need_proxy |= member_needs;
                            if is_else {
                                saw_else = true;
                                plan.has_else = true;
                                plan.else_body = body_ident;
                            } else {
                                plan.else_if.push((expr_trim, body_ident));
                            }
                            j += 1;
                        }
                        _ => break,
                    }
                }
            }
            path.pop();
        }
    }
}

fn is_html5_native(tag: &str) -> bool {
    crate::HTML5_ELEMENTS.binary_search(&tag).is_ok()
}

fn is_plan_native(tag: &str) -> bool {
    tag == "root" || is_html5_native(tag) || is_svg_native(tag)
}

fn is_svg_native(tag: &str) -> bool {
    matches!(
        tag,
        "animate"
            | "animateMotion"
            | "animatemotion"
            | "animateTransform"
            | "animatetransform"
            | "circle"
            | "clipPath"
            | "clippath"
            | "defs"
            | "desc"
            | "ellipse"
            | "feBlend"
            | "feblend"
            | "feColorMatrix"
            | "fecolormatrix"
            | "feComponentTransfer"
            | "fecomponenttransfer"
            | "feComposite"
            | "fecomposite"
            | "feConvolveMatrix"
            | "feconvolvematrix"
            | "feDiffuseLighting"
            | "fediffuselighting"
            | "feDisplacementMap"
            | "fedisplacementmap"
            | "feDistantLight"
            | "fedistantlight"
            | "feDropShadow"
            | "fedropshadow"
            | "feFlood"
            | "feflood"
            | "feFuncA"
            | "fefunca"
            | "feFuncB"
            | "fefuncb"
            | "feFuncG"
            | "fefuncg"
            | "feFuncR"
            | "fefuncr"
            | "feGaussianBlur"
            | "fegaussianblur"
            | "feImage"
            | "feimage"
            | "feMerge"
            | "femerge"
            | "feMergeNode"
            | "femergenode"
            | "feMorphology"
            | "femorphology"
            | "feOffset"
            | "feoffset"
            | "fePointLight"
            | "fepointlight"
            | "feSpecularLighting"
            | "fespecularlighting"
            | "feSpotLight"
            | "fespotlight"
            | "feTile"
            | "fetile"
            | "feTurbulence"
            | "feturbulence"
            | "filter"
            | "foreignObject"
            | "foreignobject"
            | "g"
            | "image"
            | "line"
            | "linearGradient"
            | "lineargradient"
            | "marker"
            | "mask"
            | "metadata"
            | "mpath"
            | "path"
            | "pattern"
            | "polygon"
            | "polyline"
            | "radialGradient"
            | "radialgradient"
            | "rect"
            | "script"
            | "set"
            | "stop"
            | "style"
            | "svg"
            | "switch"
            | "symbol"
            | "text"
            | "textPath"
            | "textpath"
            | "title"
            | "tspan"
            | "use"
            | "view"
    )
}

/// RFC-094 — is `el` a chain-member template? Returns
/// `(is_else, raw attr value)`.
fn chain_member_kind(el: &Element) -> Option<(bool, String)> {
    for (n, v) in &el.attrs {
        if n == "pp-else-if" {
            return Some((false, v.clone()));
        }
        if n == "pp-else" {
            return Some((true, v.clone()));
        }
    }
    None
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
    Stripped,
    Preserved,
}

/// Rewrite JS-style `===` / `!==` to `==` / `!=` outside string
/// literals. Returns `Some(rewritten)` when at least one rewrite
/// happened, `None` when the source has no JS equality operator.
/// Byte-level splices are safe: the replacements are ASCII and
/// never split a UTF-8 sequence.
fn desugar_js_equality(src: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut changed = false;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b);
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            out.push(b);
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => {
                quote = Some(b);
                out.push(b);
                i += 1;
            }
            b'=' if bytes[i..].starts_with(b"===") => {
                out.extend_from_slice(b"==");
                i += 3;
                changed = true;
            }
            b'!' if bytes[i..].starts_with(b"!==") => {
                out.extend_from_slice(b"!=");
                i += 3;
                changed = true;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    changed.then(|| String::from_utf8(out).expect("ASCII-only splices preserve UTF-8"))
}

/// Parse a directive expression the plan is about to install.
///
/// - Parses clean → `Some(source unchanged)`.
/// - JS `===`/`!==` and otherwise clean → `Some(rewritten)` plus a
///   build warning (the plan installs the Rust-style spelling).
/// - Anything else → a compile diagnostic and `None`. Before this
///   check, a parse failure left the attribute `Preserved` — dead
///   markup since RFC-058 Phase 6.5 retired the runtime dispatch.
fn check_template_expr(value: &str, directive: &str, ctx: &mut AnalysisCtx) -> Option<String> {
    match pocopine_expr::parse(value) {
        Ok(_) => Some(value.to_string()),
        Err(err) => {
            if let Some(rewritten) = desugar_js_equality(value)
                && pocopine_expr::parse(&rewritten).is_ok()
            {
                ctx.warnings.push(format!(
                    "pocopine: `{directive}=\"{value}\"` uses JavaScript equality \
                     (`===`/`!==`); pine-expr is Rust-style. Interpreted as \
                     `{rewritten}` — update the template to `==`/`!=`."
                ));
                return Some(rewritten);
            }
            let hint = err
                .hint
                .as_deref()
                .map(|h| format!(" — {h}"))
                .unwrap_or_default();
            ctx.diagnostics.push(format!(
                "`{directive}` expression `{value}` does not parse: {}{hint}",
                err.message
            ));
            None
        }
    }
}

/// Closest live directive head for a did-you-mean suggestion, or
/// `None` when nothing is within edit distance 2.
fn nearest_directive(head: &str) -> Option<&'static str> {
    nearest_of(head, pocopine_directives::DIRECTIVES.iter().map(|d| d.name))
}

fn nearest_of<'a>(input: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .map(|c| (strsim::levenshtein(input, c), c))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, _)| *d <= 2)
        .map(|(_, c)| c)
}

/// Listener control modifiers the applier implements
/// (`pocopine-core/src/directives/on.rs`). Everything outside this
/// set participates in the RFC-013 key filter, which only ever
/// matches on keyboard events.
const LISTENER_CONTROL_MODIFIERS: &[&str] = &[
    "prevent", "stop", "self", "once", "window", "document", "outside", "capture",
];

fn is_keyboard_event(event: &str) -> bool {
    matches!(event, "keydown" | "keyup" | "keypress")
}

/// Named key modifiers the runtime normalizes (`on.rs named_key_for`).
const NAMED_KEY_MODIFIERS: &[&str] = &[
    "escape",
    "esc",
    "enter",
    "tab",
    "space",
    "backspace",
    "delete",
    "del",
    "arrow-up",
    "up",
    "arrow-down",
    "down",
    "arrow-left",
    "left",
    "arrow-right",
    "right",
    "home",
    "end",
    "page-up",
    "page-down",
];

/// Event-aware compile-time mirror of the runtime listener modifier
/// semantics (`pocopine-core/src/directives/on.rs`). Control
/// modifiers work on any event; a number is a debounce delay only
/// directly after `.debounce` (the runtime pairs them positionally);
/// EVERYTHING else — system keys included — rides the RFC-013 key
/// filter, which downcasts to `KeyboardEvent` and therefore never
/// matches on any other event. Pushes a diagnostic and returns
/// `false` for every shape that would install a listener that can
/// never fire.
fn validate_listener_modifiers(
    event: &str,
    modifiers: &[String],
    display: &str,
    ctx: &mut AnalysisCtx,
) -> bool {
    let keyboard = is_keyboard_event(event);
    for (i, m) in modifiers.iter().enumerate() {
        let m = m.as_str();
        if LISTENER_CONTROL_MODIFIERS.contains(&m) || m == "debounce" {
            continue;
        }
        // Multi-digit numbers are debounce delays; the runtime pairs
        // one only when it directly follows `.debounce` — anywhere
        // else it installs as a key filter for a key named "300".
        if is_debounce_ms(m) && m.len() > 1 {
            if i > 0 && modifiers[i - 1] == "debounce" {
                continue;
            }
            ctx.diagnostics.push(format!(
                "`{display}`: `.{m}` only sets a debounce delay directly after \
                 `.debounce` — write `.debounce.{m}`"
            ));
            return false;
        }
        if keyboard {
            if matches!(m, "ctrl" | "shift" | "alt" | "meta")
                || NAMED_KEY_MODIFIERS.contains(&m)
                || m.len() == 1
            {
                continue;
            }
            if is_word_key(m) {
                // Any word is a legal literal key filter, but a
                // near-miss of a control modifier or named key is a
                // typo filtering on a key that can't exist.
                if let Some(s) = nearest_of(
                    m,
                    LISTENER_CONTROL_MODIFIERS
                        .iter()
                        .copied()
                        .chain(NAMED_KEY_MODIFIERS.iter().copied()),
                )
                .filter(|s| m.len() >= 3 && strsim::levenshtein(m, s) <= 1)
                {
                    ctx.diagnostics.push(format!(
                        "`{display}`: `.{m}` would install a key filter for a key \
                         named \"{m}\", which looks like a typo — did you mean `.{s}`?"
                    ));
                    return false;
                }
                continue;
            }
            // Not word-shaped and not a named key (e.g. `arrow_up`) —
            // it can never match a normalized key name.
            let suggestion = nearest_of(
                m,
                NAMED_KEY_MODIFIERS
                    .iter()
                    .copied()
                    .chain(LISTENER_CONTROL_MODIFIERS.iter().copied()),
            )
            .map(|s| format!(" — did you mean `.{s}`?"))
            .unwrap_or_default();
            ctx.diagnostics.push(format!(
                "`{display}`: `.{m}` is not a control modifier or a recognized key \
                 filter{suggestion}"
            ));
            return false;
        }
        // Non-keyboard event: everything below is a key filter, and
        // the runtime key filter downcasts to KeyboardEvent — it can
        // never match here, so the listener would never fire.
        let message = if matches!(m, "ctrl" | "shift" | "alt" | "meta") {
            format!(
                "`{display}`: `.{m}` is a key filter and only matches keyboard events \
                 (keydown/keyup/keypress) — on `{event}` the listener would never fire"
            )
        } else {
            let suggestion = nearest_of(
                m,
                LISTENER_CONTROL_MODIFIERS
                    .iter()
                    .copied()
                    .chain(["debounce"]),
            )
            .map(|s| format!(" — did you mean `.{s}`?"))
            .unwrap_or_default();
            format!(
                "`{display}`: `.{m}` is not a control modifier, and key filters only \
                 match keyboard events — on `{event}` the listener would never \
                 fire{suggestion}"
            )
        };
        ctx.diagnostics.push(message);
        return false;
    }
    true
}

fn classify_attr(
    name: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
    host_tag: &str,
    host_is_native: bool,
) -> ClassifyOutcome {
    // The whole-template owned-content analysis validates and strips this
    // compile-time marker before normal directive classification. Treat it as
    // consumed here so the unknown-directive backstop does not report a second,
    // contradictory diagnostic.
    if name == "pp-owned-content" {
        return ClassifyOutcome::Stripped;
    }
    // RFC-020 listener shorthand: `@event[.mod]`.
    if let Some(rest) = name.strip_prefix('@') {
        return classify_listener(rest, value, path, ctx);
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
            let Some(expr_src) = check_template_expr(value, "pp-text", ctx) else {
                return ClassifyOutcome::Preserved;
            };
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Text,
                expr_src,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // pp-html="<expr>"
        if rest == "html" {
            let Some(expr_src) = check_template_expr(value, "pp-html", ctx) else {
                return ClassifyOutcome::Preserved;
            };
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Html,
                expr_src,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // pp-show="<expr>"
        if rest == "show" {
            let Some(expr_src) = check_template_expr(value, "pp-show", ctx) else {
                return ClassifyOutcome::Preserved;
            };
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Show,
                expr_src,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // pp-bind:<arg>="<expr>"
        if let Some(arg) = rest.strip_prefix("bind:") {
            return classify_bind_full(name, arg, value, path, ctx);
        }
        // pp-on:<event>[.<mod>]="<expr>"
        if let Some(rest) = rest.strip_prefix("on:") {
            return classify_listener(rest, value, path, ctx);
        }
        // pp-ref="<name>"
        if rest == "ref" {
            // `pp-ref` value is a static name, not an expression.
            let trimmed = value.trim();
            if trimmed.is_empty() {
                ctx.diagnostics
                    .push("`pp-ref` requires a non-empty name: pp-ref=\"my_ref\"".to_string());
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
            return ClassifyOutcome::Stripped;
        }
        // RFC-058 Phase 3 hardening — allowlisted runtime
        // directives (`pp-roving`, `pp-resize`, `pp-intersect`,
        // `pp-anchor`, `pp-flip`) lift into a StaticOpaqueDirective
        // entry so the macro stops flipping requires_walker for
        // them. The applier dispatches each through the same
        // `directives::lookup` path the mount uses.
        //
        // Eligibility: the attr name must parse cleanly into
        // `(head, arg, modifiers)` and the head must be in the
        // allowlist. Anything else (pp-route / pp-cloak
        // / pp-stagger / pp-transition / unknown) stays preserved
        // and forces requires_walker.
        if let Some((head, arg, modifiers)) = parse_pp_directive_name(rest)
            && is_lift_eligible_opaque(&head)
        {
            ctx.opaque_directives.push(OpaqueDirectiveLite {
                node_path: path.to_vec(),
                name: head,
                arg,
                modifiers,
                value: value.to_string(),
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // RFC-058 Phase 6.5 — `pp-model[.modifier]="field"` on a
        // native input/textarea/select. Component-target
        // `pp-model` (registered tag, with or without a `:arg`)
        // stays on the runtime mount for now and is collected
        // by `parse_child_host_model` on the parent
        // `ChildMountLite`. Lifting only fires when host is
        // native AND the directive has no `:arg` (the arg form
        // is for component target prop selection, not native
        // inputs).
        if host_is_native && rest.starts_with("model:") {
            // The `:arg` form selects a component prop; a native
            // input has no props to select. Used to be preserved —
            // dead markup since the runtime dispatch retired.
            ctx.diagnostics.push(format!(
                "`pp-{rest}`: the `pp-model:<prop>` form targets component models; \
                 native inputs take bare `pp-model=\"field\"`"
            ));
            return ClassifyOutcome::Preserved;
        }
        if host_is_native && (rest == "model" || rest.starts_with("model.")) {
            let modifiers: Vec<&str> = rest.split('.').skip(1).collect();
            for m in &modifiers {
                match *m {
                    "number" | "lazy" => {}
                    // Registry-documented, but `NativeModelLite` has no
                    // trim lane and the applier never implemented one —
                    // the modifier would be silently dropped.
                    "trim" => ctx.diagnostics.push(
                        "`pp-model.trim` is not supported by compiled templates — \
                         trim in the handler instead"
                            .to_string(),
                    ),
                    other => {
                        let suggestion = nearest_of(other, ["number", "lazy"].into_iter())
                            .map(|s| format!(" — did you mean `.{s}`?"))
                            .unwrap_or_default();
                        ctx.diagnostics.push(format!(
                            "unknown `pp-model` modifier `.{other}` — supported: \
                             `.number`, `.lazy`{suggestion}"
                        ));
                    }
                }
            }
            if value.trim().is_empty() {
                ctx.diagnostics
                    .push("`pp-model` requires a field expression: pp-model=\"field\"".to_string());
                return ClassifyOutcome::Preserved;
            }
            let Some(expr_src) = check_template_expr(value, "pp-model", ctx) else {
                return ClassifyOutcome::Preserved;
            };
            let number = modifiers.contains(&"number");
            let lazy = modifiers.contains(&"lazy");
            ctx.native_models.push(NativeModelLite {
                node_path: path.to_vec(),
                expr_src,
                number,
                lazy,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // Every other pp-* attribute. RFC-058 Phase 6.5 retired the
        // runtime pp-* dispatch, so `Preserved` is dead markup unless
        // something else (slot capture, the router's DOM queries, the
        // plan applier) explicitly consumes the attribute. Check the
        // shared directive registry: an unknown head is a typo, and a
        // live head on the wrong host class can never activate.
        if let Some(parsed) = pocopine_directives::parse_directive_attr(name) {
            if pocopine_directives::removed_message(&parsed.head).is_some() {
                // The RFC-063 forbidden-directives pass owns the
                // migration error for these.
                return ClassifyOutcome::Preserved;
            }
            match pocopine_directives::lookup(&parsed.head) {
                None => {
                    let suggestion = nearest_directive(&parsed.head)
                        .map(|n| format!(" — did you mean `pp-{n}`?"))
                        .unwrap_or_default();
                    ctx.diagnostics.push(format!(
                        "unknown directive `pp-{}`{suggestion}",
                        parsed.head
                    ));
                }
                Some(spec) => match spec.host {
                    pocopine_directives::Host::TemplateOnly if host_tag != "template" => {
                        ctx.diagnostics.push(format!(
                            "`pp-{}` is only valid on a `<template>`",
                            spec.name
                        ));
                    }
                    pocopine_directives::Host::ComponentOnly => {
                        // Component tags took the child-mount branch,
                        // so this host is a native element.
                        ctx.diagnostics.push(format!(
                            "`pp-{}` is only valid on a component tag",
                            spec.name
                        ));
                    }
                    _ => {}
                },
            }
        }
        return ClassifyOutcome::Preserved;
    }
    // Plain HTML attribute — preserved.
    ClassifyOutcome::Preserved
}

/// Parse the post-`pp-` part of an attribute name into
/// `(head, arg, modifiers)`. Mirrors the runtime
/// `directives::parse_attr` so the lift-eligibility check sees
/// the same shape the dispatcher would.
fn parse_pp_directive_name(rest: &str) -> Option<(String, Option<String>, Vec<String>)> {
    let (head_part, rest) = match rest.split_once(':') {
        Some((h, r)) => (h, Some(r)),
        None => (rest, None),
    };
    let mut head_parts = head_part.split('.');
    let name = head_parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let head_mods: Vec<String> = head_parts.map(str::to_string).collect();
    let (arg, tail_mods) = if let Some(rest) = rest {
        let mut it = rest.split('.');
        let a = it.next().map(str::to_string).filter(|s| !s.is_empty());
        (a, it.map(str::to_string).collect::<Vec<_>>())
    } else {
        (None, Vec::new())
    };
    let mut mods = head_mods;
    mods.extend(tail_mods);
    Some((name, arg, mods))
}

/// Compile-time `{{expr}}` tokenizer (RFC-040 grammar)
/// in pocopine-core.
///
/// Tokenises `input` into alternating static + dynamic segments;
/// both parsers must agree on the escape rules and the
/// unclosed-`{{` handling, asserted by the unit tests below
/// alongside the runtime crate's own tests.
fn parse_interp_segments(input: &str) -> Result<Vec<InterpSegment>, String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut static_buf = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 2 < bytes.len() {
            let n1 = bytes[i + 1];
            let n2 = bytes[i + 2];
            if (n1 == b'{' && n2 == b'{') || (n1 == b'}' && n2 == b'}') {
                static_buf.push(n1 as char);
                static_buf.push(n2 as char);
                i += 3;
                continue;
            }
        }
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            static_buf.push('\\');
            i += 2;
            continue;
        }
        if b == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if !static_buf.is_empty() {
                out.push(InterpSegment::Static(std::mem::take(&mut static_buf)));
            }
            let start = i + 2;
            let mut j = start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'}' && bytes[j + 1] == b'}' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return Err("unclosed `{{` in text".into());
            }
            let src = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| "non-UTF-8 text")?
                .trim()
                .to_string();
            if src.is_empty() {
                return Err("empty `{{}}` interpolation".into());
            }
            out.push(InterpSegment::Dynamic(src));
            i = j + 2;
            continue;
        }
        static_buf.push(b as char);
        i += 1;
    }
    if !static_buf.is_empty() {
        out.push(InterpSegment::Static(static_buf));
    }
    Ok(out)
}

/// Whether any descendant is a `<slot>` outlet — the marker for the
/// RFC-011 iterated-slot shape (`pp-for` on a native element).
fn subtree_contains_slot(el: &Element) -> bool {
    el.children.iter().any(|n| match n {
        Node::Element(c) => c.tag == "slot" || subtree_contains_slot(c),
        _ => false,
    })
}

/// Allowlist of runtime directives that are safe to lift into a
/// compile-time `StaticOpaqueDirective` entry. Each entry is a
/// pure DOM-side effect that installs once and self-manages —
/// no scope-chain quirks, no mount-discovery dependencies, no
/// install-order constraints beyond "after slot materialisation"
/// (which the applier honours by running the opaque dispatch
/// last).
fn is_lift_eligible_opaque(head: &str) -> bool {
    matches!(head, "roving" | "resize" | "intersect" | "anchor" | "flip")
}

fn classify_bind(arg: &str, value: &str, path: &[u16], ctx: &mut AnalysisCtx) -> ClassifyOutcome {
    classify_bind_inner(&format!(":{arg}"), arg, value, path, ctx)
}

fn classify_bind_full(
    full_name: &str,
    arg: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
) -> ClassifyOutcome {
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
    // Bind takes no `.modifiers` (RFC-020) — a suffixed binding
    // used to be preserved, i.e. dead markup.
    if let Some((real_arg, mods)) = arg.split_once('.') {
        ctx.diagnostics.push(format!(
            "`{attr_to_strip}`: attribute bindings take no `.modifiers` \
             (got `.{mods}`) — write `:{real_arg}=\"…\"`"
        ));
        return ClassifyOutcome::Preserved;
    }
    let Some(expr_src) = check_template_expr(value, attr_to_strip, ctx) else {
        return ClassifyOutcome::Preserved;
    };
    ctx.bindings.push(BindingLite {
        node_path: path.to_vec(),
        kind: BindingKindLite::Bind {
            arg: arg.to_string(),
        },
        expr_src,
    });
    ctx.stripped.push(StrippedAttr {
        node_path: path.to_vec(),
        name: attr_to_strip.to_string(),
    });
    ClassifyOutcome::Stripped
}

fn classify_listener(
    rest: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
) -> ClassifyOutcome {
    // `rest` is `event[.mod1.mod2…]`.
    let mut parts = rest.split('.');
    let event = match parts.next() {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => {
            ctx.diagnostics
                .push("`@` listener requires an event name: `@click=\"…\"`".to_string());
            return ClassifyOutcome::Preserved;
        }
    };
    let modifiers: Vec<String> = parts.map(|m| m.to_string()).collect();
    if !validate_listener_modifiers(&event, &modifiers, &format!("@{rest}"), ctx) {
        return ClassifyOutcome::Preserved;
    }
    let Some(expr_src) = check_listener_expr(value, rest, &modifiers, ctx) else {
        return ClassifyOutcome::Preserved;
    };
    let stripped_name = rest.to_string();
    ctx.listeners.push(ListenerLite {
        node_path: path.to_vec(),
        event,
        modifiers,
        expr_src,
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
    ClassifyOutcome::Stripped
}

enum ChildHostAttrOutcome {
    /// `pp-show` controls the custom host element itself in the parent scope,
    /// but installs after the child mount so fallthrough cannot move its
    /// inline display declaration to the rendered root.
    Show(String),
    Binding(ChildHostBindingLite),
    Listener(ChildHostListenerLite),
    Model(ChildHostModelLite),
    /// RFC-058 Phase 3 hardening — `pp-ref="<name>"` on a
    /// custom-host element. The runtime semantic matches the
    /// native-element case: register the host DOM element under
    /// `name` in the parent scope's ref table. The macro emits a
    /// regular `RefLite` against the host's node_path so the
    /// generated install pass runs the same `refs::register`
    /// call the mount would.
    Ref(String),
    Preserved,
}

fn classify_child_host_attr(
    name: &str,
    value: &str,
    ctx: &mut AnalysisCtx,
) -> ChildHostAttrOutcome {
    // `analyze_owned_content_outlet` already rejects component-host markers
    // and strips them from emitted HTML. Do not reinterpret that compiler-only
    // marker as an unknown runtime directive here.
    if name == "pp-owned-content" {
        return ChildHostAttrOutcome::Preserved;
    }
    if let Some(rest) = name.strip_prefix('@') {
        return classify_child_host_listener(rest, value, ctx);
    }
    if let Some(arg) = name.strip_prefix(':') {
        return classify_child_host_bind(name, arg, value, ctx);
    }
    let Some(rest) = name.strip_prefix("pp-") else {
        return ChildHostAttrOutcome::Preserved;
    };
    if rest == "show" {
        let Some(expr_src) = check_template_expr(value, "pp-show", ctx) else {
            return ChildHostAttrOutcome::Preserved;
        };
        return ChildHostAttrOutcome::Show(expr_src);
    }
    if let Some(arg) = rest.strip_prefix("bind:") {
        return classify_child_host_bind(name, arg, value, ctx);
    }
    if let Some(rest) = rest.strip_prefix("on:") {
        return classify_child_host_listener(rest, value, ctx);
    }
    if rest == "model" || rest.starts_with("model.") || rest.starts_with("model:") {
        let (arg, modifiers) = parse_child_host_model(rest);
        // The component install path (`model::install_component`)
        // takes no modifiers at all — `.number`/`.trim`/`.lazy` on a
        // component host are silently dropped at runtime.
        if let Some(m) = modifiers.first() {
            let display = if modifiers.len() == 1 {
                format!(".{m}")
            } else {
                format!(".{}", modifiers.join("."))
            };
            ctx.diagnostics.push(format!(
                "component `pp-model` takes no `{display}` modifier — the component \
                 install path ignores modifiers; coerce inside the child component"
            ));
            return ChildHostAttrOutcome::Preserved;
        }
        if value.trim().is_empty() {
            ctx.diagnostics
                .push("`pp-model` requires a field expression: pp-model=\"field\"".to_string());
            return ChildHostAttrOutcome::Preserved;
        }
        let Some(expr_src) = check_template_expr(value, "pp-model", ctx) else {
            return ChildHostAttrOutcome::Preserved;
        };
        return ChildHostAttrOutcome::Model(ChildHostModelLite {
            arg,
            modifiers,
            expr_src,
        });
    }
    if rest == "ref" {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            ctx.diagnostics
                .push("`pp-ref` requires a non-empty name: pp-ref=\"my_ref\"".to_string());
            return ChildHostAttrOutcome::Preserved;
        }
        return ChildHostAttrOutcome::Ref(trimmed.to_string());
    }
    // Same registry backstop as the native fall-through: an unknown
    // pp-* head on a component host used to be preserved dead markup.
    if let Some(parsed) = pocopine_directives::parse_directive_attr(name)
        && pocopine_directives::removed_message(&parsed.head).is_none()
    {
        match pocopine_directives::lookup(&parsed.head) {
            None => {
                let suggestion = nearest_directive(&parsed.head)
                    .map(|n| format!(" — did you mean `pp-{n}`?"))
                    .unwrap_or_default();
                ctx.diagnostics.push(format!(
                    "unknown directive `pp-{}`{suggestion}",
                    parsed.head
                ));
            }
            Some(spec) if spec.host == pocopine_directives::Host::TemplateOnly => {
                ctx.diagnostics.push(format!(
                    "`pp-{}` is only valid on a `<template>`",
                    spec.name
                ));
            }
            Some(_) => {}
        }
    }
    ChildHostAttrOutcome::Preserved
}

fn classify_child_host_bind(
    full_name: &str,
    arg: &str,
    value: &str,
    ctx: &mut AnalysisCtx,
) -> ChildHostAttrOutcome {
    if arg.is_empty() {
        return ChildHostAttrOutcome::Preserved;
    }
    if let Some((real_arg, mods)) = arg.split_once('.') {
        ctx.diagnostics.push(format!(
            "`{full_name}`: attribute bindings take no `.modifiers` \
             (got `.{mods}`) — write `:{real_arg}=\"…\"`"
        ));
        return ChildHostAttrOutcome::Preserved;
    }
    let Some(expr_src) = check_template_expr(value, full_name, ctx) else {
        return ChildHostAttrOutcome::Preserved;
    };
    ChildHostAttrOutcome::Binding(ChildHostBindingLite {
        arg: arg.to_string(),
        expr_src,
    })
}

fn classify_child_host_listener(
    rest: &str,
    value: &str,
    ctx: &mut AnalysisCtx,
) -> ChildHostAttrOutcome {
    let mut parts = rest.split('.');
    let Some(event) = parts.next().filter(|s| !s.is_empty()) else {
        ctx.diagnostics
            .push("`@` listener requires an event name: `@click=\"…\"`".to_string());
        return ChildHostAttrOutcome::Preserved;
    };
    let modifiers: Vec<String> = parts.map(str::to_string).collect();
    if !validate_listener_modifiers(event, &modifiers, &format!("@{rest}"), ctx) {
        return ChildHostAttrOutcome::Preserved;
    }
    let Some(expr_src) = check_listener_expr(value, rest, &modifiers, ctx) else {
        return ChildHostAttrOutcome::Preserved;
    };
    ChildHostAttrOutcome::Listener(ChildHostListenerLite {
        event: event.to_string(),
        modifiers,
        expr_src,
    })
}

/// Listener expression check with the effect-only carve-out: an empty
/// value is valid when the modifier chain carries `.prevent`/`.stop`
/// (the modifiers ARE the behavior — e.g. `@pointerdown.stop`). The
/// plan stores an empty `expr_src`; the runtime installs the listener
/// without an evaluation step.
fn check_listener_expr(
    value: &str,
    rest: &str,
    modifiers: &[String],
    ctx: &mut AnalysisCtx,
) -> Option<String> {
    if value.trim().is_empty() {
        if modifiers.iter().any(|m| m == "prevent" || m == "stop") {
            return Some(String::new());
        }
        ctx.diagnostics.push(format!(
            "`@{rest}` has no handler expression — an empty listener only makes sense \
             with an effect modifier (`.prevent` / `.stop`)"
        ));
        return None;
    }
    check_template_expr(value, &format!("@{rest}"), ctx)
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

fn is_word_key(m: &str) -> bool {
    !m.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric())
}

fn is_debounce_ms(m: &str) -> bool {
    m.parse::<u32>().is_ok()
}

// ─── pp-if body lift eligibility + analysis ──────────────────────

/// RFC-058 Phase 4.1d v1 envelope — `true` when every node in
/// the lifted body subtree is safe to install via the Phase 1
/// helpers and the generated specialized install closure. Anything outside
/// this envelope can only produce an uninstalled static clone plus a runtime
/// plan-failure signal; there is no generic walker fallback.
///
/// RFC-058 Phase 6.5 expansion: `<slot>` elements are now
/// allowed. The macro records them in the body fragment's
/// `slot_outlets`; the runtime applier materialises each via
/// `mount::materialize_compiled_slot_outlet`, which falls
/// through to the same `slot_fragment::lookup` path the parent
/// template uses. The body's stamped `CTX_PARENT_KEY` carries
/// the slot owner scope through to descendants so nested
/// inject chains resolve correctly.
///
/// Excludes:
///   * `pp-route` / `pp-model` (component scope
///     boundaries — the body fragment installs against the
///     enclosing scope only).
fn if_body_subtree_is_eligible(el: &Element) -> bool {
    if el.synthetic {
        for child in &el.children {
            if let Node::Element(child_el) = child
                && !if_body_subtree_is_eligible(child_el)
            {
                return false;
            }
        }
        return true;
    }
    // Phase 3.5d expansion: non-HTML5 tags are allowed here.
    // `walk()` emits child_mount entries for them into the
    // body fragment's own static plan. The child component then runs its own
    // specialized mount function; no parent-side fallback walk is involved.
    let _ = is_plan_native(&el.tag); // kept for symmetry with slot eligibility
    for (name, _) in &el.attrs {
        if name == "pp-route" {
            return false;
        }
        // RFC-058 Phase 6.5 — both branches of `pp-model` are
        // compile-time handled now: native targets lift to
        // `NativeModelLite`, component targets to
        // `ChildHostModelLite`. No need to gate either form.
    }
    for child in &el.children {
        if let Node::Element(child_el) = child
            && !if_body_subtree_is_eligible(child_el)
        {
            return false;
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
/// Push the compile-time diagnostic for a structural `<template>` body whose
/// element-root count breaks the exactly-one rule (the branch analogue of the
/// component single-root rule). Non-whitespace top-level TEXT is rejected on
/// the same grounds: the runtime clone stamps only the single element root,
/// so `<template pp-if>Prefix <span>…</span></template>` would silently drop
/// `Prefix` from the branch.
fn check_branch_body_roots(directive: &str, template_el: &Element, ctx: &mut AnalysisCtx) {
    let element_roots: Vec<&Element> = template_el
        .children
        .iter()
        .filter_map(|child| match child {
            Node::Element(element) => Some(element),
            _ => None,
        })
        .collect();
    if element_roots.len() != 1 {
        let message = format!(
            "`{directive}` template must have exactly one root element (found {}) — \
             wrap the branch body in a single container",
            element_roots.len(),
        );
        if let Some(extra_root) = element_roots.get(1) {
            ctx.diagnostics.push_at_with_context(
                message,
                extra_root.opening_tag_range.clone(),
                format!("`{directive}` template body starts here"),
                template_el.opening_tag_range.clone(),
            );
        } else {
            ctx.diagnostics
                .push_at(message, template_el.opening_tag_range.clone());
        }
        return;
    }
    let has_loose_text = template_el
        .children
        .iter()
        .any(|child| matches!(child, Node::Text(text, _) if !text.trim().is_empty()));
    if has_loose_text {
        // Text-node byte ranges are not mapped by the shared parser yet.
        // Anchor on the owning template rather than fabricating a plausible
        // but wrong source coordinate from its 0..0 placeholder range.
        ctx.diagnostics.push_at(
            format!(
                "`{directive}` template has text beside its root element — the text would \
                 silently drop out of the branch; move it inside the root container",
            ),
            template_el.opening_tag_range.clone(),
        );
    }
}

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
    if let Some((tag, attrs)) = emissions.role.as_ref() {
        html = apply_role_substitution(&html, tag, attrs);
    }
    Some((html, ctx))
}

/// Compile-time mirror of `pocopine_core::templates::compile_template`'s
/// `<root>` rewrite. Body fragments and dynamic slot fragments stamp
/// their cleaned HTML directly via `set_inner_html` (no runtime
/// `compile_template` pass), so the macro applies the same
/// substitution before the literal lands in the fragment fn.
fn apply_role_substitution(html: &str, tag: &str, attrs: &str) -> String {
    let with_open_self_close = html
        .replace("<root>", &format!("<{tag} {attrs}>"))
        .replace("<root/>", &format!("<{tag} {attrs}/>"))
        .replace("<root ", &format!("<{tag} {attrs} "));
    with_open_self_close.replace("</root>", &format!("</{tag}>"))
}

// ─── slot subtree eligibility + emission ─────────────────────────

/// RFC-058 Phase 3.5b + 3.5c — analyse a `<custom-tag>`'s
/// children for slot fragment lifting. Returns `None` when
/// anything in the subtree falls outside the v1 envelope (`<slot>`,
/// `pp-route` / `pp-model`). Otherwise returns
/// `Some(SlotFragmentEmission)` — `Static` when the subtree
/// has no plan-eligible directive (3.5b path), `Dynamic` when
/// it does (3.5c path: stamps cleaned HTML + runs a
/// per-fragment specialized install closure against the parent scope).
///
/// Multi-root subtrees are fine — the macro emits one fragment
/// fn per slot site, not per element. The runtime
/// `stamp_dynamic_slot_with` helper wraps the children in a
/// temporary `<div>` so generated path resolution has a single
/// element root.
fn analyze_slot_subtree(nodes: &[Node], emissions: &mut Emissions) -> Option<SlotFragmentEmission> {
    if !slot_subtree_is_lift_eligible(nodes) {
        return None;
    }
    let mut ctx = AnalysisCtx::default();
    let mut path: Vec<u16> = Vec::new();
    // RFC-058 Phase 6.2 parity for slot content: scan TOP-LEVEL text
    // nodes for `{{expr}}` interpolation. `walk` only scans the text
    // children of elements it visits, so bare text passed directly as
    // slot content (`<x-chip>{{ label }}</x-chip>`) was never collected
    // and rendered as raw braces. The entry anchors at the fragment
    // root (empty `node_path`): `stamp_dynamic_slot_with` resolves
    // paths against its temporary root element, whose text children
    // are exactly these nodes.
    let mut text_index: u16 = 0;
    for node in nodes {
        let Node::Text(text, _) = node else { continue };
        if text.contains("{{")
            && let Ok(segments) = parse_interp_segments(text)
            && segments
                .iter()
                .any(|s| matches!(s, InterpSegment::Dynamic(_)))
        {
            ctx.interps.push(InterpLite {
                node_path: Vec::new(),
                text_index,
                segments,
            });
        }
        text_index += 1;
    }
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
    let html = if let Some((tag, attrs)) = emissions.role.as_ref() {
        apply_role_substitution(&html, tag, attrs)
    } else {
        html
    };
    // Anything the analyzer collected must survive into an installed
    // plan — a `Static` emission drops the plan wholesale, which used
    // to silently discard a slot whose only dynamic content was
    // interpolation (or a native pp-model). `has_any_entry` is the
    // exhaustive "collected anything" predicate, so a new entry kind
    // can't be forgotten here again.
    // A diagnostics-only context must stay attached to the parent long enough
    // for `absorb_lifted_metadata` to propagate its errors. Treat it as
    // dynamic even though compilation will stop before the fragment runs.
    let is_dynamic = ctx.has_any_entry() || !ctx.diagnostics.is_empty();
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
/// (`pp-route` / `pp-model` are component scope
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
                // Phase 3.5e — a `<slot>` outlet inside slot content (the
                // compound-component shape: the author's outlet forwarded
                // through a child, `<x-menu><slot></slot></x-menu>`) lifts
                // like any template-level outlet: `walk` emits a
                // `SlotOutletLite`, and the fragment's install pass
                // materialises it against the author scope (the stamped
                // fragment's top-level children carry the author's borrowed
                // scope binding, so `enclosing_scope` resolves ownership
                // correctly). Rejecting it here used to poison the WHOLE
                // nested fragment tree — the consumer's projected content
                // silently vanished and an inert `<slot>` landed in the DOM.
                return true;
            }
            // Phase 3.5d expansion: non-HTML5 tags = nested
            // child mounts are now allowed. The mount
            // handles them via `analyze_slot_subtree` recursion
            // (which lands at the same `walk()` path that
            // emits child_mount entries + nested slot
            // fragments into the shared `Emissions` queue).
            let _ = is_plan_native(&el.tag); // kept for parity with body eligibility
            for (name, _) in &el.attrs {
                if name == "pp-route" {
                    return false;
                }
                // RFC-058 Phase 6.5 — `pp-model` lifts in both
                // forms (native via `NativeModelLite`, component
                // via `ChildHostModelLite`). No mount needed.
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
        // their children. The runtime mount doesn't see them
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
    // RFC-058 §6.2 layering — stamp `data-pp-row-plan="<id>"`
    // when the row-plan analyser claimed this `<template
    // pp-for>` site. Replaces the byte-position rewrite
    // `for_plan::apply_stamps` would have done on the raw
    // source — when the template-plan classifier owns the
    // serialization, it owns this stamp too.
    if let Some(plan_id) = ctx.row_plan_id(path) {
        out.push_str(&format!(" data-pp-row-plan=\"{plan_id}\""));
    }
    // RFC-094 Phase 0 — stamp `hidden` on structural `<template>`
    // anchors so Stylekit's `> :not([hidden]) ~ :not([hidden])`
    // sibling selectors (space-*/divide-*) stop counting them as
    // phantom siblings. Visually inert (templates are UA-hidden
    // already); removed per-site as comment anchors land.
    let is_structural_template = el.tag == "template"
        && (ctx
            .if_plans
            .iter()
            .any(|e| e.template_node_path.as_slice() == path.as_slice())
            || ctx
                .for_plans
                .iter()
                .any(|e| e.template_node_path.as_slice() == path.as_slice())
            || ctx
                .teleport_plans
                .iter()
                .any(|e| e.template_node_path.as_slice() == path.as_slice())
            || ctx
                .match_plans
                .iter()
                .any(|e| e.template_node_path.as_slice() == path.as_slice())
            || ctx
                .chain_member_paths
                .iter()
                .any(|p| p.as_slice() == path.as_slice()));
    if is_structural_template && !el.attrs.iter().any(|(n, _)| n == "hidden") {
        out.push_str(" hidden=\"\"");
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

#[cfg(test)]
mod tests {
    use super::{
        NAMED_KEY_MODIFIERS, emit_compiled_expr_option, is_lift_eligible_opaque,
        parse_pp_directive_name,
    };

    fn analyze(source: &str) -> super::EmittedTemplatePlan {
        let (ast, errors) = crate::template_parser::parse(source, "test.poco");
        assert!(errors.is_empty(), "template must parse: {errors:?}");
        super::analyze_template_plan(&ast, &[], None, "test-component")
    }

    fn diagnostics(plan: &super::EmittedTemplatePlan) -> String {
        plan.diagnostics.to_string()
    }

    #[test]
    fn pp_component_requires_reactive_is_and_emits_a_child_mount() {
        let missing = analyze("<div><pp-component></pp-component></div>");
        let errors = diagnostics(&missing);
        assert!(errors.contains("compile_error"), "{errors}");
        assert!(errors.contains("reactive"), "{errors}");

        let valid = analyze(r#"<div><pp-component :is="active"></pp-component></div>"#);
        assert!(
            !diagnostics(&valid).contains("compile_error"),
            "{:?}",
            diagnostics(&valid),
        );
        let plan = valid.plan_tokens.unwrap().to_string();
        assert!(plan.contains("pp-component"), "{plan}");
        assert!(plan.contains("active"), "{plan}");
    }

    #[test]
    fn owned_content_marker_compiles_to_a_stable_element_child_path() {
        let emitted = analyze(
            "<section><header>chrome</header><main><div pp-owned-content></div></main></section>",
        );
        assert_eq!(emitted.owned_content_outlet_path, Some(vec![1, 0]));
        assert!(
            !diagnostics(&emitted).contains("compile_error"),
            "{}",
            diagnostics(&emitted)
        );
        let cleaned = emitted
            .cleaned_html
            .as_deref()
            .expect("the compile-only marker requires cleaned HTML");
        assert!(!cleaned.contains("pp-owned-content"), "{cleaned}");
        assert!(
            emitted.plan_tokens.is_none(),
            "metadata-only markers do not need a runtime directive plan"
        );

        let root = analyze("<main pp-owned-content></main>");
        assert_eq!(root.owned_content_outlet_path, Some(Vec::new()));
    }

    #[test]
    fn owned_content_marker_rejects_every_unstable_ownership_boundary() {
        let cases = [
            (
                "<div><template pp-if=\"open\"><main pp-owned-content></main></template></div>",
                "must be unconditional",
            ),
            (
                "<div><template pp-for=\"item in items\"><main pp-owned-content></main></template></div>",
                "must be unconditional",
            ),
            (
                "<div><template pp-teleport=\"#target\"><main pp-owned-content></main></template></div>",
                "must be unconditional",
            ),
            (
                "<div><x-child><main pp-owned-content></main></x-child></div>",
                "projected component content",
            ),
            (
                "<div><slot><main pp-owned-content></main></slot></div>",
                "slot fallback/projected content",
            ),
            (
                "<div><pp-component :is=\"active\"><main pp-owned-content></main></pp-component></div>",
                "inside `<pp-component>`",
            ),
            (
                "<div pp-as><main pp-owned-content></main></div>",
                "on or below `pp-as`",
            ),
            (
                "<div pp-html=\"html\"><main pp-owned-content></main></div>",
                "replaces child DOM",
            ),
        ];

        for (template, expected) in cases {
            let emitted = analyze(template);
            let errors = diagnostics(&emitted);
            assert!(errors.contains("compile_error"), "{template}: {errors}");
            assert!(errors.contains(expected), "{template}: {errors}");
            assert_eq!(emitted.owned_content_outlet_path, None, "{template}");
        }
    }

    #[test]
    fn owned_content_marker_rejects_component_void_and_duplicate_targets() {
        let component = analyze("<div><x-child pp-owned-content></x-child></div>");
        assert!(
            diagnostics(&component).contains("cannot be placed on component tag `<x-child>`"),
            "{}",
            diagnostics(&component)
        );

        let void = analyze("<div><input pp-owned-content></div>");
        assert!(
            diagnostics(&void).contains("cannot be placed on void element `<input>`"),
            "{}",
            diagnostics(&void)
        );

        let duplicate =
            analyze("<div><main pp-owned-content></main><aside pp-owned-content></aside></div>");
        assert!(
            diagnostics(&duplicate).contains("duplicate `pp-owned-content` marker"),
            "{}",
            diagnostics(&duplicate)
        );
        assert_eq!(duplicate.owned_content_outlet_path, None);
    }

    #[test]
    fn pp_show_on_child_host_uses_the_host_directive_plan() {
        let emitted = analyze(r#"<div><x-button pp-show="visible">Delete</x-button></div>"#);
        let cleaned = emitted
            .cleaned_html
            .as_deref()
            .expect("child mount should emit cleaned HTML");
        assert!(!cleaned.contains("pp-show"), "{cleaned}");

        let plan = emitted.plan_tokens.unwrap().to_string();
        assert!(plan.contains("StaticChildHostShow"), "{plan}");
        assert!(plan.contains("StaticChildMount"), "{plan}");
    }

    #[test]
    fn pp_if_body_can_be_a_slotted_child_component() {
        let emitted = analyze(
            r#"<div><template pp-if="open"><x-button @click="remove">Delete</x-button></template></div>"#,
        );
        let body_fns = emitted.if_body_fns.to_string();
        assert!(!diagnostics(&emitted).contains("compile_error"));
        assert!(body_fns.contains("stamp_if_body_with"), "{body_fns}");
        assert!(body_fns.contains("StaticChildMount"), "{body_fns}");
        assert!(
            emitted
                .slot_fragment_fns
                .to_string()
                .contains("stamp_static_html"),
            "projected text must be emitted as a slot fragment",
        );
    }

    #[test]
    fn multi_root_pp_if_body_is_a_compile_error() {
        // The runtime clone stamps only the FIRST root — extra roots used
        // to silently drop out of the conditional. Now a diagnostic.
        let plan =
            analyze("<div><template pp-if=\"open\"><span>a</span><span>b</span></template></div>");
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("exactly one root element"), "{out}");

        // Zero element roots errors too (the runtime install would fail).
        let plan = analyze("<div><template pp-if=\"open\">text only</template></div>");
        let out = diagnostics(&plan);
        assert!(out.contains("exactly one root element"), "{out}");

        // Non-whitespace text beside the single root errors too — the
        // runtime stamps only the element root, silently dropping the text.
        let plan = analyze("<div><template pp-if=\"open\">Prefix <span>a</span></template></div>");
        let out = diagnostics(&plan);
        assert!(out.contains("text beside its root element"), "{out}");

        // A single root (whitespace/comments around it are fine) stays clean.
        let plan = analyze(
            "<div><template pp-if=\"open\">\n  <!-- note -->\n  <span>a</span>\n</template></div>",
        );
        let out = diagnostics(&plan);
        assert!(!out.contains("compile_error"), "{out}");
    }

    #[test]
    fn multi_root_chain_member_and_case_bodies_are_compile_errors() {
        // pp-else members share the single-root rule with the chain head.
        let plan = analyze(
            "<div><template pp-if=\"open\"><span>a</span></template>\
             <template pp-else><span>b</span><span>c</span></template></div>",
        );
        let out = diagnostics(&plan);
        assert!(
            out.contains("`pp-else` template must have exactly one root element"),
            "{out}"
        );

        // pp-case arms too.
        let plan = analyze(
            "<div><template pp-match=\"state\">\
             <template pp-case=\"Ready\"><b>y</b><b>z</b></template>\
             </template></div>",
        );
        let out = diagnostics(&plan);
        assert!(
            out.contains("`pp-case` template must have exactly one root element"),
            "{out}"
        );
    }

    #[test]
    fn multi_root_for_and_teleport_bodies_are_compile_errors() {
        let plan = analyze(
            "<div><template pp-for=\"item in items\">\
             <span>a</span><span>b</span></template></div>",
        );
        let out = diagnostics(&plan);
        assert!(
            out.contains("`pp-for` template must have exactly one root element"),
            "{out}"
        );

        let plan = analyze(
            "<div><template pp-teleport=\"#target\">\
             <span>a</span><span>b</span></template></div>",
        );
        let out = diagnostics(&plan);
        assert!(
            out.contains("`pp-teleport` template must have exactly one root element"),
            "{out}"
        );
    }

    #[test]
    fn nested_branch_body_diagnostics_reach_the_component_expansion() {
        // Each lifted structural body gets its own AnalysisCtx. A diagnostic
        // raised by the pp-case below must cross the enclosing pp-if body's
        // context boundary and reach the top-level compile_error! emission.
        let plan = analyze(
            "<div><template pp-if=\"open\"><section>\
             <template pp-match=\"state\">\
             <template pp-case=\"Ready\"><b>y</b><b>z</b></template>\
             </template></section></template></div>",
        );
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(
            out.contains("`pp-case` template must have exactly one root element"),
            "{out}"
        );
    }

    #[test]
    fn diagnostics_only_top_level_context_still_emits_a_compile_error() {
        let plan = analyze("<div><template pp-case=\"Ready\"><span>orphan</span></template></div>");
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("only valid as a direct child"), "{out}");
    }

    #[test]
    fn diagnostics_only_slot_context_reaches_the_component_expansion() {
        let plan = analyze(
            "<div><x-child><template pp-case=\"Ready\">\
             <span>orphan</span></template></x-child></div>",
        );
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("only valid as a direct child"), "{out}");
    }

    #[test]
    fn bare_interpolation_in_slot_content_lifts_into_a_dynamic_fragment() {
        // `<x-chip>{{ label }}</x-chip>` — bare text passed as slot content
        // used to render as raw braces: `walk` never visited the text (it
        // only scans element children), and an interp-only ctx was emitted
        // as a Static fragment that dropped the plan.
        let plan = analyze("<div><x-chip>{{ label }}</x-chip></div>");
        let out = plan.slot_fragment_fns.to_string();
        assert!(
            out.contains("install_static_interp_target"),
            "interp-only slot content must emit a dynamic fragment: {out}"
        );

        // Element-wrapped interp inside slot content: collected by `walk`
        // but previously discarded by the Static emission path.
        let plan = analyze("<div><x-chip><span>Hi {{ name }}</span></x-chip></div>");
        let out = plan.slot_fragment_fns.to_string();
        assert!(
            out.contains("install_static_interp_target"),
            "element-wrapped slot interp must survive into the plan: {out}"
        );
    }

    #[test]
    fn parse_failure_emits_compile_error_with_directive_hint() {
        // The Group 1 teaching errors from `pocopine-expr` must reach
        // the user at `cargo check` time — not silently fall through
        // to a runtime panic via `templates_plan::fail()`. Pin the
        // emitted token shape.
        let out = emit_compiled_expr_option("status === 'queued'").to_string();
        assert!(
            out.contains("compile_error"),
            "parse failure should emit a `compile_error!` token, got: {out}"
        );
        assert!(
            out.contains("===") && out.contains("=="),
            "compile_error message should quote the offending operator and suggest `==`: {out}"
        );
    }

    #[test]
    fn parse_failure_emits_arithmetic_directive() {
        let out = emit_compiled_expr_option("progress * 100").to_string();
        assert!(
            out.contains("compile_error"),
            "arithmetic should emit compile_error, got: {out}"
        );
        assert!(
            out.contains("computed"),
            "arithmetic hint should point at `#[computed]`: {out}"
        );
    }

    #[test]
    fn valid_but_not_compile_time_representable_falls_through() {
        // `a + b` parses fine but `emit_compiled_expr` can't lower a
        // non-literal `+` to a `StaticExpr`. The fallthrough path
        // must still emit `Option::None` (runtime evaluation), NOT a
        // compile error.
        let out = emit_compiled_expr_option("a + b").to_string();
        assert!(
            !out.contains("compile_error"),
            "valid expression must not emit compile_error, got: {out}"
        );
        assert!(
            out.contains("None"),
            "non-representable expression must fall through to Option::None: {out}"
        );
    }

    #[test]
    fn literal_expression_compiles_to_static() {
        // Sanity: simple literal stays on the compile-time fast path.
        let out = emit_compiled_expr_option("true").to_string();
        assert!(
            !out.contains("compile_error"),
            "literal must not emit compile_error: {out}"
        );
        assert!(
            out.contains("Some") && out.contains("StaticExpr"),
            "literal must compile to a `Some(StaticExpr::...)`: {out}"
        );
    }

    #[test]
    fn supported_listener_modifiers_include_runtime_named_keys() {
        // Parity pin against `on.rs named_key_for` — every named key
        // the runtime normalizes must be admitted at compile time.
        for modifier in [
            "backspace",
            "delete",
            "del",
            "esc",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "page-up",
            "page-down",
        ] {
            assert!(
                NAMED_KEY_MODIFIERS.contains(&modifier),
                "{modifier} should compile instead of requiring mount fallback"
            );
        }
    }

    // ─── silent-drop hardening ───────────────────────────────────
    // RFC-058 Phase 6.5 retired the runtime pp-* dispatch, so a
    // `Preserved` directive attribute is dead markup, not a fallback.
    // Every directive the author wrote that the plan cannot install
    // must surface as a diagnostic (or a build warning for the
    // JS-equality desugar) — never a silent no-op.

    fn warnings(plan: &super::EmittedTemplatePlan) -> String {
        plan.warnings.join("\n")
    }

    #[test]
    fn unknown_directive_head_is_a_compile_error_with_suggestion() {
        let plan = analyze(r#"<div><button pp-shwo="isOpen">Toggle</button></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("pp-shwo"), "{out}");
        assert!(out.contains("pp-show"), "suggestion expected: {out}");
    }

    #[test]
    fn registry_live_preserved_directives_stay_clean() {
        let plan =
            analyze(r#"<div><a pp-route href="/x">x</a><span pp-transition="fade">y</span></div>"#);
        let out = diagnostics(&plan);
        assert!(!out.contains("compile_error"), "{out}");
    }

    #[test]
    fn template_only_directives_on_a_native_host_are_compile_errors() {
        let plan = analyze(r#"<div><div pp-if="open">body</div></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("pp-if") && out.contains("template"), "{out}");

        let plan = analyze(r#"<div><div pp-for="item in items">row</div></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("pp-for") && out.contains("template"), "{out}");

        // RFC-011 iterated slots: pp-for on a native element wrapping
        // a `<slot>` outlet is the documented shape — stays clean.
        let plan = analyze(r#"<ul><li pp-for="file in files"><slot name="row"></slot></li></ul>"#);
        assert!(
            !diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );

        // …but the carve-out still validates the pp-for value itself.
        let plan = analyze(r#"<ul><li pp-for="file of files"><slot name="row"></slot></li></ul>"#);
        assert!(
            diagnostics(&plan).contains("item in items"),
            "{}",
            diagnostics(&plan)
        );

        let plan = analyze(r##"<div><div pp-teleport="#target">t</div></div>"##);
        let out = diagnostics(&plan);
        assert!(
            out.contains("pp-teleport") && out.contains("template"),
            "{out}"
        );

        // Orphan template-only helpers on native hosts die silently too.
        let plan = analyze(r#"<div><div pp-slot="header">x</div></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("pp-slot") && out.contains("template"), "{out}");
    }

    #[test]
    fn malformed_pp_for_syntax_is_a_compile_error() {
        let plan =
            analyze(r#"<div><template pp-for="item of items"><span>r</span></template></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("item in items"), "{out}");
    }

    #[test]
    fn unparseable_directive_expression_is_a_compile_error() {
        let plan = analyze(r#"<div><span pp-show="(((">x</span></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("pp-show"), "{out}");

        let plan = analyze(r#"<div><button :disabled="(((">x</button></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        let plan = analyze(r#"<div><button @click="(((">x</button></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn js_triple_equality_desugars_with_a_build_warning() {
        let plan = analyze(r#"<div><button :disabled="status === 'loading'">Save</button></div>"#);
        let out = diagnostics(&plan);
        assert!(!out.contains("compile_error"), "{out}");
        let warns = warnings(&plan);
        assert!(
            warns.contains("==="),
            "warning should name the JS operator: {warns}"
        );
        let tokens = plan
            .plan_tokens
            .expect("bind should still plan")
            .to_string();
        assert!(
            tokens.contains("status == 'loading'"),
            "expr_src should be rewritten to Rust-style equality: {tokens}"
        );

        let plan =
            analyze(r#"<div><template pp-if="state !== 'done'"><span>x</span></template></div>"#);
        assert!(!diagnostics(&plan).contains("compile_error"));
        assert!(warnings(&plan).contains("!=="), "{}", warnings(&plan));
    }

    #[test]
    fn interp_js_equality_desugars_with_a_build_warning() {
        let plan = analyze("<div><span>{{ state === 'on' }}</span></div>");
        assert!(!diagnostics(&plan).contains("compile_error"));
        assert!(warnings(&plan).contains("==="), "{}", warnings(&plan));
    }

    #[test]
    fn listener_modifier_that_can_never_fire_is_a_compile_error() {
        // Misspelled control modifier: the runtime would treat it as a
        // keyboard key filter, and a click never carries a key.
        let plan = analyze(r#"<div><button @click.prevnt="save()">s</button></div>"#);
        let out = diagnostics(&plan);
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("prevnt"), "{out}");
        assert!(out.contains("prevent"), "suggestion expected: {out}");

        // Named keys on non-keyboard events can never match.
        let plan = analyze(r#"<div><button @click.enter="save()">s</button></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        // `.passive` is registry-documented but the applier never
        // implemented it — the listener would install as a dead
        // key filter.
        let plan = analyze(r#"<div><div @scroll.passive="track()">s</div></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn effect_only_listener_compiles_without_expression() {
        // `@pointerdown.stop` with no handler is a real idiom (pine's
        // own trigger components use it) — the modifiers ARE the
        // behavior. It used to be preserved, i.e. silently dead.
        let plan = analyze(r#"<div><button @pointerdown.stop="">x</button></div>"#);
        assert!(
            !diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );
        let tokens = plan
            .plan_tokens
            .expect("effect-only listener should plan")
            .to_string();
        assert!(tokens.contains("pointerdown"), "{tokens}");

        let plan = analyze(r#"<div><button @mousedown.prevent="">x</button></div>"#);
        assert!(
            !diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );

        // Without an effect modifier an empty listener does nothing —
        // that stays an error.
        let plan = analyze(r#"<div><button @click="">x</button></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn supported_listener_shapes_stay_clean() {
        let plan = analyze(
            r#"<div>
                 <input @keydown.enter="submit()" />
                 <input @keyup.q="quick()" />
                 <input @keydown.ctrl.enter="send()" />
                 <input @input.debounce.300="search()" />
                 <button @click.outside="close()">y</button>
               </div>"#,
        );
        let out = diagnostics(&plan);
        assert!(!out.contains("compile_error"), "{out}");
    }

    #[test]
    fn runtime_dead_listener_shapes_are_compile_errors() {
        // System keys ride the RFC-013 key filter, which bails on any
        // non-KeyboardEvent — `@click.ctrl` never fires at runtime.
        let plan = analyze(r#"<div><button @click.ctrl="save()">x</button></div>"#);
        assert!(
            diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );

        // The debounce delay pairs positionally: a number anywhere
        // else installs as a never-matching key filter.
        let plan = analyze(r#"<div><input @input.300.debounce="search()" /></div>"#);
        assert!(
            diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );
        let plan = analyze(r#"<div><input @keydown.enter.300="go()" /></div>"#);
        assert!(
            diagnostics(&plan).contains("compile_error"),
            "{}",
            diagnostics(&plan)
        );

        // Near-miss control modifiers on keyboard events are typos,
        // not key filters.
        let plan = analyze(r#"<div><input @keydown.prevnt="submit()" /></div>"#);
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("prevent"),
            "{out}"
        );

        // Misspelled named keys get a key-name suggestion, and the
        // message must not claim keydown is not a keyboard event.
        let plan = analyze(r#"<div><input @keydown.arrow_up="move()" /></div>"#);
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("arrow-up"),
            "{out}"
        );
        assert!(!out.contains("not a keyboard event"), "{out}");
    }

    #[test]
    fn native_model_bad_modifier_is_a_compile_error() {
        let plan = analyze(r#"<div><input pp-model.numbr="age" /></div>"#);
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("numbr"),
            "{out}"
        );
        assert!(out.contains("number"), "suggestion expected: {out}");

        // `.trim` is registry-documented but the compiled lift drops it.
        let plan = analyze(r#"<div><input pp-model.trim="name" /></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        // The prop-selection arg form targets component models only.
        let plan = analyze(r#"<div><input pp-model:value="age" /></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        let plan = analyze(r#"<div><input pp-model.number.lazy="age" /></div>"#);
        assert!(!diagnostics(&plan).contains("compile_error"));

        // The model value itself is validated: empty or unparseable
        // used to compile green as a dead two-way binding.
        let plan = analyze(r#"<div><input pp-model="" /></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
        let plan = analyze(r#"<div><input pp-model="1 +" /></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn empty_pp_ref_is_a_compile_error() {
        let plan = analyze(r#"<div><span pp-ref=" ">x</span></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn bind_arg_modifier_is_a_compile_error() {
        let plan = analyze(r#"<div><span :class.once="cls">x</span></div>"#);
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("once"),
            "{out}"
        );
    }

    #[test]
    fn child_host_attrs_share_the_hardening() {
        let plan = analyze(r#"<div><x-btn @click.prevnt="go()">x</x-btn></div>"#);
        assert!(diagnostics(&plan).contains("prevnt"));

        let plan = analyze(r#"<div><x-btn :disabled="a === 'b'">x</x-btn></div>"#);
        assert!(!diagnostics(&plan).contains("compile_error"));
        assert!(warnings(&plan).contains("==="), "{}", warnings(&plan));

        let plan = analyze(r#"<div><x-input pp-model:value.numbr="v">x</x-input></div>"#);
        assert!(diagnostics(&plan).contains("numbr"));

        let plan = analyze(r#"<div><x-input pp-model="">x</x-input></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        // The component install path drops ALL pp-model modifiers —
        // `.trim`/`.number`/`.lazy` on a component host are dead.
        let plan = analyze(r#"<div><x-input pp-model:value.trim="v">x</x-input></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));
        let plan = analyze(r#"<div><x-input pp-model.number="v">x</x-input></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        // Unknown directive heads on component hosts get the same
        // registry check + suggestion as native elements.
        let plan = analyze(r#"<div><pine-button pp-shwo="is_open">x</pine-button></div>"#);
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("pp-show"),
            "{out}"
        );
    }

    #[test]
    fn pp_stagger_junk_value_is_a_compile_error() {
        let plan = analyze(
            r#"<div><template pp-for="i in items" pp-stagger="fast"><span>r</span></template></div>"#,
        );
        let out = diagnostics(&plan);
        assert!(
            out.contains("compile_error") && out.contains("pp-stagger"),
            "{out}"
        );

        // Empty is a truncation, not a request for 0ms.
        let plan = analyze(
            r#"<div><template pp-for="i in items" pp-stagger=""><span>r</span></template></div>"#,
        );
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    #[test]
    fn pp_teleport_misuse_is_a_compile_error() {
        let plan = analyze(r#"<div><template pp-teleport=" "><span>b</span></template></div>"#);
        assert!(diagnostics(&plan).contains("compile_error"));

        let plan = analyze(
            r##"<div><template pp-for="i in items" pp-teleport="#t"><span>r</span></template></div>"##,
        );
        assert!(diagnostics(&plan).contains("compile_error"));
    }

    /// `parse_pp_directive_name` mirrors the runtime
    /// `directives::parse_attr` so the lift-eligibility check sees
    /// the same shape the dispatcher would. Pin the documented
    /// edge cases so the two parsers don't drift silently.
    #[test]
    fn parse_pp_directive_name_handles_documented_shapes() {
        // Bare directive name.
        let (name, arg, mods) = parse_pp_directive_name("roving").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg, None);
        assert!(mods.is_empty());

        // Modifiers without arg (`pp-roving.both`).
        let (name, arg, mods) = parse_pp_directive_name("roving.both").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg, None);
        assert_eq!(mods, vec!["both"]);

        // Multiple modifiers (`pp-roving.both.nowrap`).
        let (name, arg, mods) = parse_pp_directive_name("roving.both.nowrap").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg, None);
        assert_eq!(mods, vec!["both", "nowrap"]);

        // Arg only (`pp-roving:listbox`).
        let (name, arg, mods) = parse_pp_directive_name("roving:listbox").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg.as_deref(), Some("listbox"));
        assert!(mods.is_empty());

        // Arg + modifiers (`pp-roving:listbox.virtual`).
        let (name, arg, mods) = parse_pp_directive_name("roving:listbox.virtual").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg.as_deref(), Some("listbox"));
        assert_eq!(mods, vec!["virtual"]);

        // Head modifiers + arg (`pp-resize.border-box:host` —
        // the head's modifiers come before the arg in the
        // result vec, then the arg's tail modifiers, mirroring
        // `directives::parse_attr`).
        let (name, arg, mods) = parse_pp_directive_name("resize.border-box:host.foo").unwrap();
        assert_eq!(name, "resize");
        assert_eq!(arg.as_deref(), Some("host"));
        assert_eq!(mods, vec!["border-box", "foo"]);

        // Empty arg (`pp-roving:`) collapses to None — the
        // runtime parser keeps `Some("")` here, but for the
        // currently allowlisted directives an empty arg is
        // never meaningful, so the macro normalises to None.
        // Pin the divergence so it stays intentional.
        let (name, arg, _) = parse_pp_directive_name("roving:").unwrap();
        assert_eq!(name, "roving");
        assert_eq!(arg, None);

        // Empty input rejects.
        assert!(parse_pp_directive_name("").is_none());
        // Leading dot rejects (would yield empty head).
        assert!(parse_pp_directive_name(".both").is_none());
    }

    /// `parse_interp_segments` is the sole `{{expr}}` tokenizer
    /// (the runtime scanner was removed with the walker —
    /// RFC-058 Phase 6.5). Pin the documented RFC-040 edge
    /// cases so the grammar doesn't drift silently.
    #[test]
    fn parse_interp_segments_handles_documented_shapes() {
        use super::{InterpSegment, parse_interp_segments};

        fn render(segs: &[InterpSegment]) -> String {
            let mut s = String::new();
            for seg in segs {
                match seg {
                    InterpSegment::Static(t) => s.push_str(t),
                    InterpSegment::Dynamic(t) => s.push_str(&format!("<<{t}>>")),
                }
            }
            s
        }

        // Bare expression.
        assert_eq!(render(&parse_interp_segments("{{x}}").unwrap()), "<<x>>");
        // Static + dynamic + static.
        assert_eq!(
            render(&parse_interp_segments("hi {{name}}!").unwrap()),
            "hi <<name>>!",
        );
        // Whitespace inside braces is trimmed.
        assert_eq!(
            render(&parse_interp_segments("{{   spaced   }}").unwrap()),
            "<<spaced>>",
        );
        // Single braces stay literal.
        assert_eq!(
            render(&parse_interp_segments("a { b } c").unwrap()),
            "a { b } c",
        );
        // Escaped opener stays literal.
        assert_eq!(
            render(&parse_interp_segments(r"\{{literal}}").unwrap()),
            "{{literal}}",
        );
        // Backslash escape for backslash.
        assert_eq!(render(&parse_interp_segments(r"a \\ b").unwrap()), r"a \ b",);
        // Unclosed `{{` is an error.
        assert!(parse_interp_segments("oops {{ no end").is_err());
        // Empty `{{}}` is an error.
        assert!(parse_interp_segments("{{}}").is_err());
        // Pure static is fine and yields a single Static segment.
        let segs = parse_interp_segments("just text").unwrap();
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0], InterpSegment::Static(ref s) if s == "just text"));
    }

    /// Each entry in the opaque-lift allowlist must look like a
    /// directive name the runtime registry could resolve. The
    /// runtime side asserts the actual `directives::lookup`
    /// resolution lives in `crates/pocopine/tests/template_plan.rs`
    /// (`opaque_lift_allowlist_matches_runtime_registry`); this
    /// side just guards the spelling.
    #[test]
    fn opaque_lift_allowlist_is_non_empty_and_lowercase() {
        for name in ["roving", "resize", "intersect", "anchor", "flip"] {
            assert!(
                is_lift_eligible_opaque(name),
                "{name} should be in the opaque-lift allowlist",
            );
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "directive name `{name}` should be lowercase ASCII (matches the runtime registry key style)",
            );
        }
        assert!(!is_lift_eligible_opaque("text"));
        assert!(!is_lift_eligible_opaque("model"));
    }
}
