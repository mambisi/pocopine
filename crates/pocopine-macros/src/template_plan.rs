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
//! v1 envelope per RFC-057 §6 (deferred to RFC-058 §6.2):
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
//! Every other attribute survives unchanged on the rewritten
//! HTML — it's the runtime mount's job to handle them as
//! today (attribute-preserved fallback, RFC-057 §8.1).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::template_parser::{Element, Node, TemplateAst};

/// Result of analysing one component's template.
pub(crate) struct EmittedTemplatePlan {
    /// `Some(quoted &'static StaticTemplatePlan)` when at least
    /// one plan entry was emitted; `None` when the template has
    /// nothing eligible (every directive is mount-owned or the
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
    /// RFC 062 — unrolled component mount body. Component
    /// mounts use this generated body directly; the generic
    /// plan applier remains for lifted fragment internals only.
    pub specialized_mount_body: Option<TokenStream>,
    /// RFC 081 — every `pp-ref="name"` collected from the
    /// template, dedup'd in template-order. The consuming
    /// macro emits a `<ComponentName>Refs` struct with one
    /// `fn <name>(&self) -> RefAccessor` per entry so handlers
    /// can write `refs.body()` instead of
    /// `refs::get_component::<T>("body")`.
    pub ref_names: Vec<String>,
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
) -> EmittedTemplatePlan {
    let mut ctx = AnalysisCtx {
        row_plan_assignments: row_plan_assignments.to_vec(),
        ..AnalysisCtx::default()
    };
    let mut emissions = Emissions {
        role: role.clone(),
        ..Emissions::default()
    };
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
            specialized_mount_body: None,
            ref_names: ctx.ref_names_dedup(),
        };
    }
    let cleaned_html = serialize_cleaned(&ast.roots, &ctx);
    let slot_fragment_fns = emit_slot_fragment_fns(&emissions);
    let mut if_body_fns = emit_if_body_fns(&emissions);
    // RFC-094 — chain build errors surface as compile_error!
    // items alongside the emitted fragments.
    for msg in &ctx.diagnostics {
        let lit = proc_macro2::Literal::string(msg);
        if_body_fns.extend(quote! { ::core::compile_error!(#lit); });
    }
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
        ref_names,
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
    diagnostics: Vec<String>,
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
    /// RFC-099 Phase 3 — emit the create-path body `fn`? `false` for a
    /// KEYED `pp-for` row whose RFC-054 row-plan owns the client create
    /// path: we still emit the `_HTML` / `_PLAN` consts (so the SSR
    /// stamper + claim can read the row as data) but skip the unused
    /// create closure to keep it out of the wasm bundle.
    emit_fn: bool,
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
    /// when the body falls outside the v1 envelope (`<slot>`,
    /// `pp-route`, native `pp-model`, etc.) — the
    /// runtime installer falls back to the legacy
    /// `clone_template_body` + `mount::walk` path.
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
    /// RFC-099 Phase 3 — `Some` when the row body lifted (whether or
    /// not a row-plan claimed the create path); supplies the
    /// `StaticForPlan.body_plan` / `body_html` data the SSR stamper +
    /// claim read. Equals `body_fn_ident` for unkeyed rows; for keyed
    /// rows `body_fn_ident` is `None` but this is `Some`.
    body_data_ident: Option<syn::Ident>,
    /// RFC-099 Phase 3 — the assigned RFC-054 row-plan id, so the
    /// claim path can resolve the `CompiledRowPlan` without the
    /// (now-gone) `<template>`'s `data-pp-row-plan` attribute.
    row_plan_id: Option<u32>,
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

    /// Drain a nested context's ref names (its own `refs` plus
    /// anything it already aggregated from deeper lifts) into
    /// this context's `refs_from_lifted`. Called after each
    /// `analyze_lift_body` / `analyze_slot_subtree` return so
    /// refs inside lifted subtrees still reach the outer
    /// `<ComponentName>Refs` codegen.
    fn absorb_lifted_refs(&mut self, nested: &AnalysisCtx) {
        for r in &nested.refs {
            self.refs_from_lifted.push(r.name.clone());
        }
        for name in &nested.refs_from_lifted {
            self.refs_from_lifted.push(name.clone());
        }
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
        // RFC-099 Phase 3 — lift the body's cleaned HTML and its
        // per-fragment plan into module-level consts so BOTH the
        // create-path body fn AND the structural plan's
        // `body_plan` / `body_html` fields can reference the same
        // data (no duplication: the html/plan are needed in the
        // bundle for the client create path regardless, and the
        // host SSR stamper + client claimer read them through the
        // plan fields). See `body_const_idents`.
        let (html_const, plan_const) = body_const_idents(ident);
        let html_lit = proc_macro2::Literal::string(&emission.html);
        let plan_literal = emit_static_template_plan_literal(&emission.plan);
        // RFC 064 §5.1 (Phase 1) — inline the unrolled install
        // pass into a closure handed to `stamp_if_body_with`. The
        // generic fragment applier no longer runs for pp-if,
        // pp-for, or pp-teleport body fragments; the
        // per-fragment closure uses `emit_specialized_install_pass`
        // (the same code path RFC 062 component mount
        // specialization uses) against the body's plan const.
        let install_pass = emission
            .plan
            .emit_specialized_install_pass(quote! {
                let __poc_plan = &#plan_const;
                let __poc_template_name = "<pp-if body>";
            })
            .unwrap_or_else(|| {
                // The body has no plan-eligible entries — emit a
                // no-op closure body so the `stamp_if_body_with`
                // call still type-checks. Same shape as the
                // empty-plan case in RFC 062.
                quote! {}
            });
        // The create-path body fn — skipped for a keyed `pp-for` row
        // whose RFC-054 row-plan owns the create path (the consts still
        // ship as the SSR/claim data source). `emit_fn = false` keeps
        // this unused closure out of the wasm bundle.
        let body_fn = if emission.emit_fn {
            quote! {
                fn #ident(
                    scope_id: ::pocopine::ScopeId,
                    proxy: &::pocopine::__private::JsValue,
                    ctx_parent_id: ::pocopine::ScopeId,
                ) -> ::core::option::Option<::pocopine::__private::web_sys::Element> {
                    ::pocopine::__private::stamp_if_body_with(
                        #html_const,
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
        } else {
            quote! {}
        };
        quote! {
            const #html_const: &'static str = #html_lit;
            const #plan_const: ::pocopine::__private::StaticTemplatePlan = #plan_literal;
            #body_fn
        }
    });
    quote! { #(#items)* }
}

/// RFC-099 Phase 3 — the per-body `_HTML` / `_PLAN` const idents
/// derived from a body fragment fn ident. Emitted alongside the
/// body fn in [`emit_if_body_fns`]; referenced from the structural
/// plan literals (`emit_if_plan` / `emit_match_plan` /
/// `emit_for_plan`) so the SSR stamper and client claimer can read
/// the body as data. Const-to-const references resolve
/// order-independently, so nested controllers inside a body work
/// without ordering constraints.
fn body_const_idents(body_fn_ident: &syn::Ident) -> (syn::Ident, syn::Ident) {
    (
        format_ident!("{}_HTML", body_fn_ident),
        format_ident!("{}_PLAN", body_fn_ident),
    )
}

/// RFC-099 Phase 3 — `(body_plan, body_html)` field token pair for
/// a structural body, given its body fragment fn ident. `Some(id)`
/// references the `_PLAN` / `_HTML` consts emitted in
/// [`emit_if_body_fns`]; `None` (body outside the lift envelope)
/// yields `None` fields and the SSR stamper leaves the construct
/// unexpanded (the client mounts it client-side, as before).
fn body_data_tokens(body_fn_ident: &Option<syn::Ident>) -> (TokenStream, TokenStream) {
    match body_fn_ident {
        Some(id) => {
            let (html_const, plan_const) = body_const_idents(id);
            (
                quote! { ::core::option::Option::Some(&#plan_const) },
                quote! { ::core::option::Option::Some(#html_const) },
            )
        }
        None => (
            quote! { ::core::option::Option::None },
            quote! { ::core::option::Option::None },
        ),
    }
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
    let (body_plan_tokens, body_html_tokens) = body_data_tokens(&ip.body_fn_ident);
    let else_if_tokens = ip.else_if.iter().map(|(expr_src, body)| {
        let e = proc_macro2::Literal::string(expr_src);
        let c = emit_compiled_expr_option(expr_src);
        let b = opt_body(body);
        let (bp, bh) = body_data_tokens(body);
        quote! {
            ::pocopine::__private::CondBranch {
                expr_src: #e,
                compiled: #c,
                body: #b,
                body_plan: #bp,
                body_html: #bh,
            }
        }
    });
    let has_else = ip.has_else;
    let else_body_tokens = opt_body(&ip.else_body);
    let (else_body_plan_tokens, else_body_html_tokens) = body_data_tokens(&ip.else_body);
    let consumed_count = ip.consumed_count;
    quote! {
        ::pocopine::__private::StaticCondPlan {
            template_node_path: #path,
            expr_src: #expr,
            compiled: #compiled,
            body: #body_tokens,
            body_plan: #body_plan_tokens,
            body_html: #body_html_tokens,
            else_if: &[ #(#else_if_tokens),* ],
            has_else: #has_else,
            else_body: #else_body_tokens,
            else_body_plan: #else_body_plan_tokens,
            else_body_html: #else_body_html_tokens,
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
        let (bp, bh) = body_data_tokens(body);
        quote! {
            ::pocopine::__private::MatchCase {
                tags: &[ #(#tag_lits),* ],
                bind_name: #bind_tokens,
                body: #body_tokens,
                body_plan: #bp,
                body_html: #bh,
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
    // RFC-099 Phase 3 — body_plan/body_html come from body_data_ident
    // (Some for keyed rows even though body_fn_ident is None), so keyed
    // lists server-render and the claim can read the row body.
    let (body_plan_tokens, body_html_tokens) = body_data_tokens(&fp.body_data_ident);
    let row_plan_id_tokens = match fp.row_plan_id {
        Some(id) => {
            let id = proc_macro2::Literal::u32_unsuffixed(id);
            quote! { ::core::option::Option::Some(#id) }
        }
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
            body_plan: #body_plan_tokens,
            body_html: #body_html_tokens,
            row_plan_id: #row_plan_id_tokens,
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
                // RFC-099 Phase 3 — lift the row body to DATA whether or
                // not a row-plan claims the site, so KEYED lists still
                // server-render (their `body_fn` stays `None`, but the
                // SSR stamper + claim read `body_plan` / `body_html`).
                // `emit_fn` is false for the row-plan case so the unused
                // create closure stays out of the wasm bundle; the
                // create path there is the RFC-054 row-plan fast path.
                // Only NON-row-plan bodies feed `bodies_need_proxy` /
                // ref-forwarding, preserving the row-plan proxy-elision
                // contract.
                let body_data_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                    if !row_plan_claims_site {
                        ctx.absorb_lifted_refs(&body_ctx);
                        bodies_need_proxy |= plan_needs_proxy(&body_ctx);
                    }
                    let ident = emissions.alloc_if_body_ident("for_body");
                    emissions.if_bodies.push(IfBodyEmission {
                        ident: ident.clone(),
                        html,
                        plan: body_ctx,
                        emit_fn: !row_plan_claims_site,
                    });
                    ident
                });
                let body_fn_ident = if row_plan_claims_site {
                    None
                } else {
                    body_data_ident.clone()
                };
                let row_plan_id = ctx.row_plan_id(path);
                ctx.for_plans.push(ForPlanLite {
                    template_node_path: path.clone(),
                    item_name,
                    items_expr,
                    key_expr,
                    stagger_ms,
                    bodies_need_proxy,
                    body_fn_ident,
                    body_data_ident,
                    row_plan_id,
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
                // RFC-054 row plan (when present) or the mount
                // (when absent). Either way the template-plan
                // classifier doesn't follow `<template>` content.
                return;
            }
        }
        // Ineligible (wrong host, has pp-teleport, or expr
        // doesn't parse) — fall through to block-boundary skip
        // so today's mount dispatch handles it.
        return;
    }

    // RFC-058 Phase 4.3 — `pp-teleport` on a `<template>` host
    // graduates into a `StaticTeleportPlan` entry. Eligibility:
    // host is `<template>`, no co-occurring `pp-if` (pp-if owns
    // the mount cycle in that combo and reads pp-teleport
    // itself), no co-occurring `pp-for` (pp-for graduated above
    // and shouldn't be paired with pp-teleport on the same
    // element — degenerate case, leave on mount).
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
                ctx.absorb_lifted_refs(&body_ctx);
                let ident = emissions.alloc_if_body_ident("teleport_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                    emit_fn: true,
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
        // Ineligible (wrong host, has pp-if/pp-for, empty
        // selector) — leave the mount to dispatch.
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
                    let body_ident =
                        analyze_lift_body(case_el, emissions).map(|(html, body_ctx)| {
                            ctx.absorb_lifted_refs(&body_ctx);
                            bodies_need_proxy |= plan_needs_proxy(&body_ctx);
                            let ident = emissions.alloc_if_body_ident("case_body");
                            emissions.if_bodies.push(IfBodyEmission {
                                ident: ident.clone(),
                                html,
                                plan: body_ctx,
                                emit_fn: true,
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
    // cleaned HTML so the runtime mount's directive-dispatch
    // path doesn't double-install the effect. The template
    // body lives in `<template>.content` (a separate
    // `DocumentFragment` that doesn't appear in `el.children`),
    // so body content stays on the mount — exactly like
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
            // `mount::walk`. v1 envelope is narrow (HTML5
            // natives + plan-eligible directives only); when
            // the body falls outside, `body_fn_ident` stays
            // `None` and the legacy clone+walk path runs.
            let mut bodies_need_proxy = false;
            let body_fn_ident = analyze_lift_body(el, emissions).map(|(html, body_ctx)| {
                ctx.absorb_lifted_refs(&body_ctx);
                bodies_need_proxy |= plan_needs_proxy(&body_ctx);
                let ident = emissions.alloc_if_body_ident("if_body");
                emissions.if_bodies.push(IfBodyEmission {
                    ident: ident.clone(),
                    html,
                    plan: body_ctx,
                    emit_fn: true,
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
        // Ineligible (wrong host, has pp-teleport, or expr
        // doesn't parse) — fall through to mount as today.
        return;
    }

    // Whole-subtree boundary: non-HTML5 tags (per council pass 3
    // amendment to RFC-058 §6.2). The element's own attributes
    // and descendants stay mount-owned (slot content is the
    // common case there), but RFC-058 Phase 3 captures the
    // mount site itself: the runtime applier calls
    // [`crate::mount::mount_child_component`] before the
    // mount's recursive descent reaches the tag, and the
    // mount's `__pp_mounted` guard turns the discovery into a
    // no-op afterwards.
    if !is_plan_native(&el.tag) {
        let mut host_bindings = Vec::new();
        let mut host_listeners = Vec::new();
        let mut host_models = Vec::new();
        let has_pp_as = el.attrs.iter().any(|(name, _)| name == "pp-as");
        if !has_pp_as {
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
                            ctx.absorb_lifted_refs(plan);
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
                        // Slot body falls outside the lift
                        // envelope — surfaced via
                        // `record_plan_failure` at install time.
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
                            ctx.absorb_lifted_refs(plan);
                        }
                        slot_fragments.push((
                            "default".to_string(),
                            emission.ident().clone(),
                            None,
                        ));
                        emissions.slot_fragments.push(emission);
                    }
                    None => {
                        // Unliftable default slot content —
                        // surfaced via `record_plan_failure`
                        // at install time.
                    }
                }
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

    // Classify every attribute on this element. `pp-as` hosts
    // (outside the template root) are skipped whole-subtree —
    // the dynamic-component path owns them.
    if el.attrs.iter().any(|(name, _)| name == "pp-as") && el.tag != "root" {
        return;
    }
    let mut listener_unsupported_modifier = false;
    let host_is_native = is_plan_native(&el.tag);
    for (name, value) in &el.attrs {
        if el.tag == "root" && name == "pp-as" {
            continue;
        }
        // Outcome is recorded on `ctx` (stripped entries + plan
        // vecs); nothing branch-worthy remains at this call site.
        let _ = classify_attr(
            name,
            value,
            path,
            ctx,
            host_is_native,
            &mut listener_unsupported_modifier,
        );
    }
    let _ = listener_unsupported_modifier; // already handled per-attr

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
                Ok(segments) if !segments.is_empty() => {
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
                            let mut member_needs = false;
                            let body_ident =
                                analyze_lift_body(member, emissions).map(|(html, body_ctx)| {
                                    ctx.absorb_lifted_refs(&body_ctx);
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
                                        emit_fn: true,
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

fn classify_attr(
    name: &str,
    value: &str,
    path: &[u16],
    ctx: &mut AnalysisCtx,
    host_is_native: bool,
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
            return ClassifyOutcome::Stripped;
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
            return ClassifyOutcome::Stripped;
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
            return ClassifyOutcome::Stripped;
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
            // mount surface it.
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
        if let Some((head, arg, modifiers)) = parse_pp_directive_name(rest) {
            if is_lift_eligible_opaque(&head) {
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
        if host_is_native && (rest == "model" || rest.starts_with("model.")) {
            // Parse modifiers (`.number`, `.lazy`). Accept any
            // unknown modifier silently — runtime currently does
            // the same; surfacing is a future enhancement.
            let modifiers: Vec<&str> = rest.split('.').skip(1).collect();
            let number = modifiers.contains(&"number");
            let lazy = modifiers.contains(&"lazy");
            ctx.native_models.push(NativeModelLite {
                node_path: path.to_vec(),
                expr_src: value.to_string(),
                number,
                lazy,
            });
            ctx.stripped.push(StrippedAttr {
                node_path: path.to_vec(),
                name: name.to_string(),
            });
            return ClassifyOutcome::Stripped;
        }
        // Every other pp-* attribute (pp-cloak,
        // pp-route, pp-transition:*, pp-stagger, etc.) —
        // preserved on the cleaned HTML and handled by the
        // runtime mount as today.
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
    ClassifyOutcome::Stripped
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
        // mount picks it up unchanged.
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
    ClassifyOutcome::Stripped
}

enum ChildHostAttrOutcome {
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
    if rest == "ref" {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return ChildHostAttrOutcome::Preserved;
        }
        return ChildHostAttrOutcome::Ref(trimmed.to_string());
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
            | "esc"
            | "tab"
            | "space"
            | "backspace"
            | "delete"
            | "del"
            | "arrow-up"
            | "arrow-down"
            | "arrow-left"
            | "arrow-right"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "page-up"
            | "page-down"
    ) || m.len() == 1
        || is_word_key(m)
}

fn is_debounce_modifier(m: &str) -> bool {
    m == "debounce"
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
/// helpers and the generated specialized install closure. Anything outside this
/// envelope falls back to the legacy `clone_template_body` +
/// `mount::walk` path the controller already drives.
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
            if let Node::Element(child_el) = child {
                if !if_body_subtree_is_eligible(child_el) {
                    return false;
                }
            }
        }
        return true;
    }
    // Phase 3.5d expansion: non-HTML5 tags are allowed here.
    // `walk()` emits child_mount entries for them into the
    // body fragment's own static plan, and the runtime fallback
    // walk over the cleaned fragment binds any preserved
    // directives inside the mounted child template.
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
                return false;
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
        emit_compiled_expr_option, is_lift_eligible_opaque, is_supported_modifier,
        parse_pp_directive_name,
    };

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
                is_supported_modifier(modifier),
                "{modifier} should compile instead of requiring mount fallback"
            );
        }
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
        use super::{parse_interp_segments, InterpSegment};

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
