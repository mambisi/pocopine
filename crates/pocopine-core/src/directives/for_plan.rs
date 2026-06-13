//! RFC 054 — Compiled `pp-for` row plans.
//!
//! Runtime-side data types and registry for pre-compiled keyed-list
//! row plans. The `#[component]` proc-macro walks each `.poco`
//! template at expansion time, identifies eligible `<template
//! pp-for>` rows (RFC 054 §5.2 envelope), and emits a `&'static
//! [StaticRowPlan]` constant alongside the template registration.
//! Each emitted plan stamps the source `<template>` opening tag
//! with `data-pp-row-plan="<id>"` so this module can match a
//! runtime `pp-for` directive call back to its compiled plan.
//!
//! At first use, each `expr_src` is parsed once via
//! [`crate::expr::parse_cached`] and the resulting AST is shared
//! across every row instance. Per-row mount then walks the
//! pre-recorded `node_path`s, evaluates ASTs, and patches DOM —
//! bypassing the generic mount's per-row attribute scan and
//! directive dispatch.
//!
//! v1 fast path covers the RFC §9 example shape: flat keyed table
//! rows with `pp-text`, `:class` / `pp-bind:class`, and bare `@<event>`
//! listeners. Anything else falls back to the generic mount
//! silently — `lookup_for_template` returns `None` when the
//! template isn't stamped, and `run_keyed` keeps its existing
//! body.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Object, Reflect};
use smallvec::SmallVec;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, Event, EventTarget};

use crate::expr::{self, Expr, Spanned};
use crate::loop_scope::LoopScope;
use crate::reactive::ScopeId;
use crate::scope::{with_current_el, Scope};

// ─── macro-emitted static shape ─────────────────────────────────

/// Kind of binding the macro recognised. Drives the patch
/// strategy at mount/reuse time.
///
/// `Text` and `Class` are the RFC-054 row-plan-only variants
/// emitted by the keyed-list compiler. The other variants are
/// RFC-058 Phase 2 template-plan additions; they're carried
/// in this same enum so both compilers share the runtime
/// hydration path even though they target different scopes
/// (per-row instance vs whole-template).
#[derive(Clone, Copy, Debug)]
pub enum BindingKind {
    /// `pp-text="<expr>"` — stringify the value, write
    /// `textContent`. Memoised against the last value.
    Text,
    /// `:class="<expr>"` / `pp-bind:class="<expr>"` —
    /// serialise via [`crate::directives::bind`]'s class
    /// shaping (string OR object form), write the result
    /// to `class`. Memoised against the serialised form.
    /// **RFC-054 row-plan only** — RFC-058 template plans
    /// emit `Bind { arg: "class" }` instead so a single grep
    /// shows which compiler emitted the entry.
    Class,
    /// `pp-html="<expr>"` — write the stringified value to
    /// `innerHTML`. RFC-058 Phase 2 template-plan addition.
    Html,
    /// `pp-bind:<arg>="<expr>"` / `:<arg>="<expr>"` — bind a
    /// native HTML attribute (data-/aria-/class/style/plain
    /// attr) to the expression, with the
    /// [`crate::directives::bind`] memoisation path.
    /// RFC-058 Phase 2 — child-component prop targets are
    /// **not** eligible (whole element falls back to mount).
    Bind { arg: &'static str },
    /// `pp-show="<expr>"` — toggle `display` style by
    /// truthiness; routes through transition presets when
    /// present. RFC-058 Phase 2 template-plan addition.
    Show,
}

/// Static-lifetime binding descriptor emitted by the macro. The
/// `expr_src` field is parsed once at registration into a
/// [`Spanned<Expr>`] cached in [`CompiledBinding`].
#[doc(hidden)]
pub struct StaticBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    pub expr_src: &'static str,
    pub compiled: Option<&'static expr::StaticExpr>,
}

/// Static-lifetime listener descriptor.
///
/// `modifiers` is the post-`.` token list — `prevent`, `stop`,
/// `self`, `once`, `window`, `document`, `outside`, `capture`,
/// key modifiers, and the `debounce` + numeric-ms pair (two
/// adjacent slots). The set must match what the runtime
/// [`crate::directives::on::install`] helper expects — the
/// macro-side classifier validates ahead of time, so reaching
/// the install with an unknown token is a framework bug.
///
/// RFC-054 row plans emit an empty `modifiers: &[]`; RFC-058
/// template plans populate it from the parsed `@event.<mod>`
/// chain.
#[doc(hidden)]
pub struct StaticListener {
    pub node_path: &'static [u16],
    pub event: &'static str,
    pub modifiers: &'static [&'static str],
    pub expr_src: &'static str,
}

/// Static-lifetime descriptor for a `pp-ref="<name>"` entry.
/// Resolves the named ref against the current scope at mount
/// time via [`crate::refs::register`]. RFC-058 Phase 2 template-
/// plan addition.
#[doc(hidden)]
pub struct StaticRef {
    pub node_path: &'static [u16],
    pub name: &'static str,
}

/// Static-lifetime descriptor for `pp-model[.modifier]="field"`
/// on a native input/textarea/select. RFC-058 Phase 6.5 — lifts
/// the previously mount-only directive into the static plan so
/// compiled-only apps wire two-way input bindings without going
/// through `mount::dispatch`. Component-target `pp-model` (with
/// or without an arg) is a separate entry on `StaticChildMount`
/// (see [`StaticChildHostModel`]); this type covers only native
/// elements, the path that mount dispatch routed via
/// `model::run_native`.
#[doc(hidden)]
pub struct StaticNativeModel {
    pub node_path: &'static [u16],
    pub expr_src: &'static str,
    /// `.number` modifier — coerce input string to f64 on write.
    pub number: bool,
    /// `.lazy` modifier — listen on `change` instead of `input`.
    pub lazy: bool,
}

/// Static-lifetime descriptor for a child-component mount site.
///
/// RFC-058 Phase 3 — emitted by the macro for every non-HTML5
/// tag inside a plan-eligible template, so the runtime applier
/// can call [`crate::mount::mount_child_component`] explicitly
/// instead of leaving the tag for the mount's auto-discovery
/// pass to find. Today the mount still walks the subtree; its
/// `__pp_mounted` guard makes the discovery a no-op for any tag
/// the plan already mounted. Phase 6 (`legacy-dom` quarantine)
/// drops the mount discovery and the plan-driven path becomes
/// the sole entry.
///
/// `slots` carries the parent-authored slot fragments from
/// RFC-058 Phase 3.5b. Empty slice means the parent didn't
/// emit any fragments (either the slot content was dynamic and
/// the macro left it on the mount's capture path, or the
/// custom tag had no children). Non-empty slice flips the
/// applier onto [`crate::mount::mount_child_component_with_slots`]
/// so the mount's `materialize_slot` invokes the parent's
/// fragment instead of replaying captured DOM.
#[doc(hidden)]
pub struct StaticChildMount {
    pub node_path: &'static [u16],
    pub tag: &'static str,
    pub slots: &'static [StaticSlotFragment],
    pub bindings: &'static [StaticChildHostBinding],
    pub listeners: &'static [StaticChildHostListener],
    pub models: &'static [StaticChildHostModel],
}

/// Parent-scope `pp-bind:<prop>` / `:<prop>` planned on a
/// child-component host. Installed after the child mounts so the
/// bind helper can resolve the child scope and write through the
/// prop gate.
#[doc(hidden)]
pub struct StaticChildHostBinding {
    pub arg: &'static str,
    pub expr_src: &'static str,
    pub compiled: Option<&'static expr::StaticExpr>,
}

/// Parent-scope event listener planned on a child-component host.
#[doc(hidden)]
pub struct StaticChildHostListener {
    pub event: &'static str,
    pub modifiers: &'static [&'static str],
    pub expr_src: &'static str,
}

/// `pp-model[:field]` planned on a child-component host.
#[doc(hidden)]
pub struct StaticChildHostModel {
    pub arg: Option<&'static str>,
    pub modifiers: &'static [&'static str],
    pub expr_src: &'static str,
}

/// One macro-emitted slot fragment binding within a
/// [`StaticChildMount`]. RFC-058 Phase 3.5b.
///
/// `name` matches the wire-name the runtime mount uses to
/// look up slot content (`"default"` for the unnamed slot,
/// otherwise the value of `pp-slot="<name>"` on the parent's
/// `<template>` wrapper).
///
/// `fragment` is a stateless `fn` pointer the macro wired into
/// the parent's generated `register()` body — invoking it
/// stamps the parent-authored slot DOM into the
/// [`crate::slot_fragment::SlotMountCtx::host`] buffer.
///
/// `scoped_let` is `Some(ident)` when the parent authored
/// `<template pp-slot="NAME" pp-let="ident">…</template>`
/// (RFC-058 Phase 3.5g). The runtime materialiser builds a
/// [`crate::slot_scope::SlotScope`] from the child's `<slot>`
/// element bindings and invokes the fragment against the slot
/// scope's proxy so `ident.field` resolves through RFC-011
/// scoped-slot semantics. `None` for plain default + named
/// slots, which run against the parent proxy directly.
#[doc(hidden)]
pub struct StaticSlotFragment {
    pub name: &'static str,
    pub fragment: crate::slot_fragment::SlotFragment,
    pub scoped_let: Option<&'static str>,
}

/// Plan entry for `{{expr}}` text interpolation lifted out of
/// the runtime mount (RFC-058 Phase 6.2).
///
/// The macro scans an element's direct text-node children at
/// compile time, parses `{{...}}` segments, and emits one entry
/// per interpolated text node. The applier uses
/// [`crate::directives::interp::install_planned`] to bind the
/// segments to the resolved text node — same install path the
/// mount would have run, just with the segment list pre-parsed.
///
/// `node_path` resolves the parent element in the generated
/// install pass. `text_index` selects which text-node child of
/// the parent (counting only Text nodes — element / comment
/// children don't shift the index).
#[doc(hidden)]
pub struct StaticInterp {
    pub node_path: &'static [u16],
    pub text_index: u16,
    pub segments: &'static [crate::directives::interp::PlannedSegment],
}

/// Plan entry for a runtime-only directive the macro doesn't
/// have structured handling for (RFC-058 Phase 3 hardening).
///
/// The runtime directive registry already ships container
/// behaviours (`pp-roving`, `pp-resize`, `pp-intersect`,
/// `pp-anchor`, `pp-flip`) whose semantics don't depend on
/// scope-chain quirks — they're DOM-side effects that install
/// once and self-manage. Before this entry existed, the macro
/// preserved them on the cleaned HTML and flipped
/// `requires_walker = true` so the mount could discover and
/// dispatch them. Now the macro lifts allowlisted directives
/// into this entry; the applier invokes
/// [`crate::directives::lookup`] just like
/// [`crate::mount::dispatch`] would.
///
/// `name` is the directive head (after `pp-` strip), e.g.
/// `"roving"`. `arg` is the part after the first `:` in the
/// attribute name (`pp-roving:<listbox-id>` → `Some("listbox")`).
/// `modifiers` are the `.`-separated suffixes (e.g. `["both"]`
/// for `pp-roving.both`). `value` is the attribute value verbatim.
#[doc(hidden)]
pub struct StaticOpaqueDirective {
    pub node_path: &'static [u16],
    pub name: &'static str,
    pub arg: Option<&'static str>,
    pub modifiers: &'static [&'static str],
    pub value: &'static str,
}

/// Static-lifetime descriptor for a `<slot>` outlet inside a
/// compiled component template. RFC-058 Phase 3.5e.
///
/// The macro leaves the `<slot>` element in the cleaned HTML
/// so named slots, default fallback children, and scoped-slot
/// `:prop` attributes keep the same runtime shape. The template
/// plan records the outlet path so generated mount code can
/// materialise it explicitly after all other path-based plan
/// entries have resolved. That removes `<slot>` from the
/// recursive mount's discovery path for planned templates while
/// retaining the legacy materialiser as the scoped/fallback
/// bridge.
#[doc(hidden)]
pub struct StaticSlotOutlet {
    pub node_path: &'static [u16],
    pub name: &'static str,
}

/// RFC-094 — one conditional chain: `pp-if [pp-else-if…]
/// [pp-else]`. A bare `pp-if` is the `else_if: &[]`,
/// `else_body: None` case (head-branch fields stay flat so
/// single-branch consumers read `expr_src` / `body` /
/// `teleport_selector` unchanged). The classifier folds the
/// whole sibling chain into ONE entry; the runtime controller
/// owns one effect, one active index, one comment anchor.
pub struct StaticCondPlan {
    pub template_node_path: &'static [u16],
    /// Head (pp-if) branch expression.
    pub expr_src: &'static str,
    pub compiled: Option<&'static expr::StaticExpr>,
    /// Head branch body fragment.
    pub body: Option<IfBodyFn>,
    /// `pp-else-if` branches, in authored order.
    pub else_if: &'static [CondBranch],
    /// Whether the chain has a `pp-else` (tracked separately
    /// from `else_body` — an else whose body fell outside the
    /// lifting envelope still claims the fallback slot).
    pub has_else: bool,
    /// `pp-else` body fragment (`None` with `has_else` = legacy
    /// clone fallback from the consumed member template).
    pub else_body: Option<IfBodyFn>,
    /// How many consumed chain-member `<template>`s follow the
    /// head as element siblings in the cleaned HTML. The
    /// controller detaches them at install (keeping them alive
    /// for legacy clone fallback) before swapping the head for
    /// the comment anchor.
    pub consumed_count: u16,
    pub teleport_selector: Option<&'static str>,
}

/// RFC-094 — one `pp-match` dispatch site. The matched value's
/// tag is extracted per serde's externally-tagged encoding
/// (string ⇒ the tag; one-key object ⇒ key + payload); the first
/// case whose `tags` contains it wins; `tags: &[]` is the `_`
/// wildcard. Nothing renders without a match.
pub struct StaticMatchPlan {
    pub template_node_path: &'static [u16],
    pub expr_src: &'static str,
    pub compiled: Option<&'static expr::StaticExpr>,
    pub cases: &'static [MatchCase],
    pub teleport_selector: Option<&'static str>,
}

/// One `pp-case` arm of a [`StaticMatchPlan`].
pub struct MatchCase {
    /// Variant names this arm matches; empty = `_` wildcard.
    pub tags: &'static [&'static str],
    /// `pp-let` payload binding name.
    pub bind_name: Option<&'static str>,
    pub body: Option<IfBodyFn>,
}

/// One `pp-else-if` branch of a [`StaticCondPlan`].
pub struct CondBranch {
    pub expr_src: &'static str,
    pub compiled: Option<&'static expr::StaticExpr>,
    pub body: Option<IfBodyFn>,
}

/// Macro-emitted constructor for a `pp-if` body. Returns the
/// freshly-stamped root element with all directives installed
/// against the parent scope, ready for the runtime installer
/// to insert + drive transitions on.
///
/// `None` return signals a runtime stamp failure (no document,
/// HTML didn't parse to a single root element, etc.) — the
/// caller treats it the same as a `clone_template_body` miss.
/// `ctx_parent_id` overrides the `CTX_PARENT_KEY` stamp on the
/// body root so RFC-027 inject lookups from descendants chain
/// through the slot OWNER when the controller was authored in
/// slot content. Equals `scope_id` when the controller is in
/// the host's own template (no override needed).
pub type IfBodyFn =
    fn(scope_id: ScopeId, proxy: &JsValue, ctx_parent_id: ScopeId) -> Option<web_sys::Element>;

/// Static-lifetime descriptor for a `<template pp-teleport="...">`
/// site without a co-occurring `pp-if`. RFC-058 Phase 4.3
/// (controller) + 4.3c (body lifting).
///
/// `selector` is the target query string the runtime resolves
/// once at install time via [`crate::directives::teleport::resolve_target`].
/// The macro guarantees this entry only graduates when no
/// `pp-if` is present on the same element — when both are
/// present, `pp-if`'s install path consults the teleport
/// attribute directly and pp-teleport stays on the mount.
///
/// `body` is the macro-emitted [`TeleportBodyFn`] when the
/// teleport body subtree qualified for Phase 4.3c lifting.
/// `None` falls back to today's `clone_template_body` +
/// `mount::walk` path the runtime installer already drives.
#[doc(hidden)]
pub struct StaticTeleportPlan {
    pub template_node_path: &'static [u16],
    pub selector: &'static str,
    pub body: Option<TeleportBodyFn>,
}

/// Macro-emitted constructor for a `pp-teleport` body. Same
/// shape as [`IfBodyFn`] / [`ForBodyFn`] — stamps cleaned HTML
/// then runs the macro-emitted specialized install closure
/// against the enclosing scope passed in by the installer.
pub type TeleportBodyFn =
    fn(scope_id: ScopeId, proxy: &JsValue, ctx_parent_id: ScopeId) -> Option<web_sys::Element>;

/// Static-lifetime descriptor for a `<template pp-for="...">`
/// site. RFC-058 Phase 4.2 (controller) + 4.2c (row body lifting).
///
/// The macro parses `pp-for="<item> in <items>"` at compile
/// time and hands both halves to the applier. `key_expr` is
/// `None` for unkeyed lists (the runtime falls back to the
/// naive rebuild path); `stagger_ms` is `0` when no
/// `pp-stagger` attribute was present.
///
/// The applier resolves the `<template>` element via
/// `template_node_path` and calls
/// [`crate::directives::for_::install`]. Coexists with the
/// RFC-054 row-plan registry: when a row plan is registered
/// for the same `<template>`, the RFC-054 fast path still
/// fires because the §6.2-layered cleaned HTML preserves the
/// `data-pp-row-plan="<id>"` attribute the registry lookup
/// keys on.
///
/// `body` is the macro-emitted [`ForBodyFn`] when the row
/// body subtree qualified for Phase 4.2c lifting AND no
/// RFC-054 row plan claimed the same site (the row-plan fast
/// path is strictly better than per-row body closures).
/// Per-row mounts invoke the fragment to materialise + install
/// directives against the row's `LoopScope` instead of going
/// through `clone_template_body` + `mount::walk`. `None`
/// falls back to today's clone+walk.
#[doc(hidden)]
pub struct StaticForPlan {
    pub template_node_path: &'static [u16],
    pub item_name: &'static str,
    pub items_expr: &'static str,
    pub key_expr: Option<&'static str>,
    pub stagger_ms: u32,
    pub body: Option<ForBodyFn>,
}

/// Macro-emitted constructor for a `pp-for` row body. Called
/// per row mount with the row's `LoopScope` id + proxy.
///
/// Same signature as [`IfBodyFn`] — both stamp cleaned HTML
/// then run the macro-emitted specialized install closure
/// against the passed scope.
/// Distinct types so call sites stay legible: pp-if body
/// installs against the parent scope once per mount cycle;
/// pp-for body installs against a fresh `LoopScope` per row
/// each iteration.
pub type ForBodyFn =
    fn(scope_id: ScopeId, proxy: &JsValue, ctx_parent_id: ScopeId) -> Option<web_sys::Element>;

/// One macro-emitted row plan. The macro emits
/// `pub static __POC_ROW_PLANS_<COMPONENT>: &[StaticRowPlan] =
/// &[ … ];` and a registration call alongside
/// `register_template`.
#[doc(hidden)]
pub struct StaticRowPlan {
    /// Sequential `pp-for` index within the template, starting
    /// at 0. Matches the `data-pp-row-plan="<id>"` attribute the
    /// macro stamps on the source `<template>` tag.
    pub plan_id: u32,
    /// `pp-for="<v> in <items>"` — preserved so the runtime can
    /// publish it on the row scope without re-parsing the
    /// directive value.
    pub item_name: &'static str,
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
}

// ─── runtime hydrated form ──────────────────────────────────────

pub(crate) struct CompiledBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    pub ast: Spanned<Expr>,
    fast: Option<FastExpr>,
    /// Top-level parent-scope field names referenced anywhere in
    /// this binding's expression. Empty when the binding only
    /// reads loop-local data (`row.*`, `$index`, `$first`,
    /// `$last`).
    ///
    /// Non-empty entries make `mount_row_compiled` install one
    /// `effect()` per row for this binding — the effect reads
    /// each named field through `parent_proxy` to subscribe via
    /// the parent proxy's `get` trap, then evaluates the binding
    /// and patches DOM. Without this, RFC 054 M2's effect-less
    /// per-row eval path silently drops parent-state reactivity:
    /// changes to `selected_id` (e.g. selection styling on
    /// `:class="selected_id == row.id ? 'danger' : ''"`) wouldn't
    /// re-fire the row binding because the row's `reuse` only
    /// runs when the loop item itself changes.
    parent_field_paths: Box<[String]>,
}

pub(crate) struct CompiledListener {
    pub node_path: &'static [u16],
    pub event: &'static str,
    /// Shared across every row of the plan — the AST is identical for
    /// all rows and only ever read, so rows clone the `Rc` (pointer
    /// bump) rather than deep-copying the parsed expression per row.
    pub ast: Rc<Spanned<Expr>>,
}

pub struct CompiledRowPlan {
    pub plan_id: u32,
    pub item_name: &'static str,
    pub(crate) bindings: Vec<CompiledBinding>,
    pub(crate) listeners: Vec<CompiledListener>,
    /// RFC-095 W4 — lazily resolved mutation-channel descriptor
    /// slot. `Unknown` until the first eligible mount asks.
    pub(crate) channel: std::cell::Cell<ChannelState>,
}

/// Tri-state cache for [`CompiledRowPlan::channel`].
#[derive(Clone, Copy)]
pub(crate) enum ChannelState {
    Unknown,
    Ineligible,
    Slot(u32),
}

impl CompiledRowPlan {
    /// True when every binding in this plan resolves through the
    /// typed `FastExpr` evaluator — meaning none of the per-row
    /// hot path needs the `js_sys::Proxy` for slow-path
    /// `expr::evaluate`. The runtime can skip `Scope::into_proxy`
    /// entirely at mount and lazy-mint only if a delegated
    /// listener actually fires for this row.
    pub(crate) fn is_proxy_elision_eligible(&self) -> bool {
        self.bindings.iter().all(|b| b.fast.is_some())
    }
}

// ─── registry ────────────────────────────────────────────────────

thread_local! {
    static ROW_PLANS: RefCell<HashMap<(String, u32), Rc<CompiledRowPlan>>> =
        RefCell::new(HashMap::new());
}

/// Hydrate every `StaticRowPlan` for a component into a
/// [`CompiledRowPlan`] (parsed AST, owned listener data) and
/// store it in the per-component registry. Called by macro-emitted
/// code immediately after `register_template`.
///
/// Idempotent — repeat calls overwrite the prior entry. Parse
/// failures here would indicate a macro bug; we log and skip the
/// plan so the runtime falls back to the generic mount.
pub fn register_row_plans(component_name: &str, plans: &'static [StaticRowPlan]) {
    if plans.is_empty() {
        return;
    }
    ROW_PLANS.with(|registry| {
        let mut r = registry.borrow_mut();
        for sp in plans {
            let mut bindings: Vec<CompiledBinding> = Vec::with_capacity(sp.bindings.len());
            let mut ok = true;
            for b in sp.bindings {
                match expr::parse_cached(b.expr_src) {
                    Ok(ast) => {
                        let fast = compile_fast_expr(sp.item_name, &ast);
                        let parent_field_paths =
                            collect_parent_field_paths(sp.item_name, &ast);
                        bindings.push(CompiledBinding {
                            node_path: b.node_path,
                            kind: b.kind,
                            ast,
                            fast,
                            parent_field_paths,
                        });
                    }
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "rfc-054: row plan {component_name}#{} binding parse failed (macro bug): {} at {}..{}",
                            sp.plan_id, e.message, e.span.start, e.span.end,
                        )));
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let mut listeners: Vec<CompiledListener> = Vec::with_capacity(sp.listeners.len());
            for l in sp.listeners {
                match expr::parse_cached(l.expr_src) {
                    Ok(ast) => listeners.push(CompiledListener {
                        node_path: l.node_path,
                        event: l.event,
                        ast: Rc::new(ast),
                    }),
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "rfc-054: row plan {component_name}#{} listener parse failed (macro bug): {} at {}..{}",
                            sp.plan_id, e.message, e.span.start, e.span.end,
                        )));
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            r.insert(
                (component_name.to_string(), sp.plan_id),
                Rc::new(CompiledRowPlan {
                    plan_id: sp.plan_id,
                    item_name: sp.item_name,
                    bindings,
                    listeners,
                    channel: std::cell::Cell::new(ChannelState::Unknown),
                }),
            );
        }
    });
}

#[derive(Clone)]
enum FastExpr {
    Path(FastPath),
    TernaryEq {
        lhs: FastPath,
        rhs: FastPath,
        then_value: JsValue,
        else_value: JsValue,
        invert: bool,
    },
}

#[derive(Clone)]
struct FastPath {
    root: FastPathRoot,
    keys: Box<[JsValue]>,
}

#[derive(Clone, Copy)]
enum FastPathRoot {
    LoopItem,
    LoopIndex,
    LoopFirst,
    LoopLast,
    Parent,
}

/// Walk a parsed binding expression and collect every distinct
/// top-level field name that resolves against the **parent**
/// scope (i.e. anything other than the loop variable, `$index`,
/// `$first`, `$last`). The runtime uses this list to subscribe
/// each row's parent-touching binding to changes on those
/// fields — without it, RFC 054 M2's effect-less binding eval
/// path drops parent-state reactivity.
///
/// Returns `Box<[]>` for purely-loop-local expressions; those
/// bindings stay on the direct mount/reuse path with no
/// effect overhead.
fn collect_parent_field_paths(item_name: &str, expr: &Spanned<Expr>) -> Box<[String]> {
    let mut out: Vec<String> = Vec::new();
    walk_collect(item_name, expr, &mut out);
    out.into_boxed_slice()
}

fn walk_collect(item_name: &str, expr: &Spanned<Expr>, out: &mut Vec<String>) {
    match &expr.value {
        Expr::Path(segments) => {
            let Some(first) = segments.first() else {
                return;
            };
            if first == item_name || first == "$index" || first == "$first" || first == "$last" {
                return;
            }
            if !out.iter().any(|existing| existing == first) {
                out.push(first.clone());
            }
        }
        Expr::Not(inner) => walk_collect(item_name, inner, out),
        Expr::BinOp(_, lhs, rhs) => {
            walk_collect(item_name, lhs, out);
            walk_collect(item_name, rhs, out);
        }
        Expr::Ternary(c, t, e) => {
            walk_collect(item_name, c, out);
            walk_collect(item_name, t, out);
            walk_collect(item_name, e, out);
        }
        Expr::Call(_, args) => {
            // The call name itself is a handler, not a parent
            // field — handlers route through `LoopScope::invoke`
            // and don't need reactive subscriptions. Only the
            // args' field reads matter.
            for arg in args {
                walk_collect(item_name, arg, out);
            }
        }
        Expr::Assign(_, rhs) => {
            // Assignment target is a write, not a tracked read.
            walk_collect(item_name, rhs, out);
        }
        Expr::Seq(stmts) => {
            for s in stmts {
                walk_collect(item_name, s, out);
            }
        }
        Expr::Literal(_) => {}
    }
}

fn compile_fast_expr(item_name: &str, expr: &Spanned<Expr>) -> Option<FastExpr> {
    match &expr.value {
        Expr::Path(segments) => Some(FastExpr::Path(compile_fast_path(item_name, segments)?)),
        Expr::Ternary(cond, then_e, else_e) => {
            let Expr::BinOp(op, lhs, rhs) = &cond.value else {
                return None;
            };
            let invert = match op {
                crate::expr::BinOp::Eq => false,
                crate::expr::BinOp::Ne => true,
                _ => return None,
            };
            let lhs = match &lhs.value {
                Expr::Path(segments) => compile_fast_path(item_name, segments)?,
                _ => return None,
            };
            let rhs = match &rhs.value {
                Expr::Path(segments) => compile_fast_path(item_name, segments)?,
                _ => return None,
            };
            Some(FastExpr::TernaryEq {
                lhs,
                rhs,
                then_value: literal_fast_value(then_e)?,
                else_value: literal_fast_value(else_e)?,
                invert,
            })
        }
        _ => None,
    }
}

fn compile_fast_path(item_name: &str, segments: &[String]) -> Option<FastPath> {
    let (root, rest) = match segments.first().map(String::as_str)? {
        first if first == item_name => (FastPathRoot::LoopItem, &segments[1..]),
        "$index" => (FastPathRoot::LoopIndex, &segments[1..]),
        "$first" => (FastPathRoot::LoopFirst, &segments[1..]),
        "$last" => (FastPathRoot::LoopLast, &segments[1..]),
        _ => (FastPathRoot::Parent, segments),
    };
    Some(FastPath {
        root,
        keys: rest
            .iter()
            .map(|segment| JsValue::from_str(segment))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    })
}

fn literal_fast_value(expr: &Spanned<Expr>) -> Option<JsValue> {
    match &expr.value {
        Expr::Literal(crate::expr::Literal::Null) => Some(JsValue::NULL),
        Expr::Literal(crate::expr::Literal::Bool(v)) => Some(JsValue::from_bool(*v)),
        Expr::Literal(crate::expr::Literal::Number(v)) => Some(JsValue::from_f64(*v)),
        Expr::Literal(crate::expr::Literal::String(v)) => Some(JsValue::from_str(v)),
        _ => None,
    }
}

fn evaluate_binding(
    binding: &CompiledBinding,
    proxy: &JsValue,
    loop_state: Option<&LoopScope>,
) -> JsValue {
    if let (Some(fast), Some(loop_state)) = (&binding.fast, loop_state) {
        return evaluate_fast_expr(fast, loop_state);
    }
    expr::evaluate(&binding.ast, proxy)
}

fn evaluate_fast_expr(expr: &FastExpr, loop_state: &LoopScope) -> JsValue {
    match expr {
        FastExpr::Path(path) => evaluate_fast_path(path, loop_state),
        FastExpr::TernaryEq {
            lhs,
            rhs,
            then_value,
            else_value,
            invert,
        } => {
            let eq = Object::is(
                &evaluate_fast_path(lhs, loop_state),
                &evaluate_fast_path(rhs, loop_state),
            );
            if eq ^ *invert {
                then_value.clone()
            } else {
                else_value.clone()
            }
        }
    }
}

fn fast_expr_depends_on_loop_position(expr: &FastExpr) -> bool {
    match expr {
        FastExpr::Path(path) => fast_path_depends_on_loop_position(path),
        FastExpr::TernaryEq { lhs, rhs, .. } => {
            fast_path_depends_on_loop_position(lhs) || fast_path_depends_on_loop_position(rhs)
        }
    }
}

fn fast_path_depends_on_loop_position(path: &FastPath) -> bool {
    matches!(
        path.root,
        FastPathRoot::LoopIndex | FastPathRoot::LoopFirst | FastPathRoot::LoopLast
    )
}

impl CompiledRowPlan {
    /// True when any compiled binding reads `$index`, `$first`, or
    /// `$last`. Reconcile can still update the row's LoopScope for
    /// listener correctness, but when this is false an index/total-only
    /// change does not require reapplying DOM bindings.
    pub(crate) fn depends_on_loop_position(&self) -> bool {
        self.bindings.iter().any(|binding| {
            binding
                .fast
                .as_ref()
                .map(fast_expr_depends_on_loop_position)
                // Non-fast bindings may read positional fields through
                // the generic evaluator, so stay conservative.
                .unwrap_or(true)
        })
    }
}

fn evaluate_fast_path(path: &FastPath, loop_state: &LoopScope) -> JsValue {
    let mut cur = match path.root {
        FastPathRoot::LoopItem => loop_state.item.clone(),
        FastPathRoot::LoopIndex => JsValue::from_f64(loop_state.index as f64),
        FastPathRoot::LoopFirst => JsValue::from_bool(loop_state.index == 0),
        FastPathRoot::LoopLast => JsValue::from_bool(loop_state.index + 1 == loop_state.total),
        FastPathRoot::Parent => {
            // RFC-096 S2 — the first key is a parent FIELD;
            // resolve it through the shared read mirror (tracks
            // at the parent scope, no proxy). Remaining keys walk
            // the plain value below.
            let mut iter = path.keys.iter();
            let Some(first) = iter.next().and_then(|k| k.as_string()) else {
                return JsValue::UNDEFINED;
            };
            let mut cur = crate::scope::read_scope_key(loop_state.parent_scope_id, &first);
            for key in iter {
                cur = Reflect::get(&cur, key).unwrap_or(JsValue::UNDEFINED);
            }
            return cur;
        }
    };
    for key in path.keys.iter() {
        cur = Reflect::get(&cur, key).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

pub(crate) fn ensure_delegated_listeners(
    plan: &Rc<CompiledRowPlan>,
    template_el: &Element,
    parent_node: &web_sys::Node,
) {
    let Some(parent_el) = parent_node.dyn_ref::<Element>() else {
        return;
    };
    for event in unique_listener_events(plan) {
        let marker = format!("data-pp-delegated-{event}");
        if template_el.has_attribute(&marker) {
            continue;
        }
        let _ = template_el.set_attribute(&marker, "");
        install_list_delegated_listener(parent_el.clone(), event);
    }
}

fn unique_listener_events(plan: &CompiledRowPlan) -> Vec<&'static str> {
    let mut events: Vec<&'static str> = Vec::new();
    for listener in &plan.listeners {
        if !events.contains(&listener.event) {
            events.push(listener.event);
        }
    }
    events
}

fn install_list_delegated_listener(parent_el: Element, event: &'static str) {
    let parent_for_track = parent_el.clone();
    let closure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
        else {
            return;
        };
        let mut cursor = target.dyn_ref::<Element>().cloned();
        while let Some(el) = cursor {
            // Use the id-only variant so a click on a proxy-elided
            // row doesn't force a `Scope::into_proxy` mint just to
            // discard the proxy here. The handler dispatch path
            // (`dispatch_delegated_event` →`instance_proxy`)
            // lazy-mints only if the listener AST actually needs it.
            if let Some(scope_id) = crate::mount::scope_id_of_element(&el) {
                dispatch_delegated_event(scope_id, event, &target, &ev);
                return;
            }
            cursor = el.parent_element();
        }
    }) as Box<dyn FnMut(Event)>);

    let target: EventTarget = parent_el.into();
    crate::mount::track_listener_on(&parent_for_track, target, event, false, closure);
}

fn dispatch_delegated_event(
    scope_id: ScopeId,
    event: &'static str,
    target: &web_sys::Node,
    ev: &Event,
) {
    ROW_INSTANCES.with(|m| {
        let map = m.borrow();
        let Some(instance) = map.get(&scope_id) else {
            return;
        };
        let Some(proxy) = instance_proxy(instance, scope_id) else {
            return;
        };
        let ev_js: JsValue = {
            let r: &JsValue = ev.as_ref();
            r.clone()
        };
        for route in instance.listener_routes.iter().rev() {
            if route.event != event {
                continue;
            }
            let route_node: &web_sys::Node = route.node.as_ref();
            if !route_node.contains(Some(target)) {
                continue;
            }
            with_current_el(&route.node, || {
                crate::scope::with_current_scope_id(scope_id, || {
                    crate::magics::with_current_event(&ev_js, || {
                        // RFC-096 S2 — delegated dispatch rides the
                        // scoped access like every other listener.
                        let access = crate::scope::scoped_root_reader(scope_id);
                        expr::evaluate_with(&route.ast, &proxy, access.as_ref());
                    });
                });
            });
        }
    });
}

/// Resolve the compiled plan for a `<template pp-for>` element.
/// Returns `None` when the template isn't stamped (eligibility
/// rejection at macro time, or the consumer crate's macro version
/// predates RFC 054) — the caller falls back to the generic mount.
pub fn lookup_for_template(template_el: &Element) -> Option<Rc<CompiledRowPlan>> {
    let plan_id = template_el
        .get_attribute("data-pp-row-plan")
        .and_then(|s| s.parse::<u32>().ok())?;
    let component_name = nearest_component_name(template_el)?;
    ROW_PLANS.with(|r| r.borrow().get(&(component_name, plan_id)).cloned())
}

/// Fetch (or lazy-mint) the row's `js_sys::Proxy`. Compiled-row
/// instances may have been mounted with `proxy = None` when the
/// plan is FastExpr-only; the first listener fire / slow-path
/// binding eval routes through here, mints the proxy on demand
/// from the live `Scope`, and caches it on the `RowInstance`.
fn instance_proxy(instance: &RowInstance, scope_id: ScopeId) -> Option<JsValue> {
    if let Some(p) = instance.proxy.borrow().as_ref() {
        return Some(p.clone());
    }
    let scope = Scope::find(scope_id)?;
    let proxy = scope.into_proxy();
    *instance.proxy.borrow_mut() = Some(proxy.clone());
    Some(proxy)
}

/// Walk up from `el` looking for the nearest ancestor (inclusive)
/// stamped with `data-pp-scope-id="<name>"` — that's the host
/// component. Used at directive-bind time to find which
/// component's plan table to consult; runs once per
/// `<template pp-for>`, not per row.
fn nearest_component_name(el: &Element) -> Option<String> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(name) = e.get_attribute("data-pp-scope-id") {
            return Some(name);
        }
        let tag = e.tag_name().to_ascii_lowercase();
        if tag.contains('-') {
            return Some(tag);
        }
        cur = e.parent_element();
    }
    None
}

// ─── per-row instance state ─────────────────────────────────────

/// Per-row state owned by the compiled fast path. Replaces the
/// per-binding `effect()` closures the M1 path used: bindings
/// evaluate explicitly via `reuse_row_compiled` instead of via
/// `trigger_scope`, so update cycles skip the entire reactive
/// dispatch + Reflect::get tracking machinery and just walk a
/// flat array of cached values.
/// RFC-101 P1 — the per-row binding cache, inline for the common small
/// row (≤4 bindings) so it costs zero heap blocks; spills to the heap
/// only for wide rows. jsbench rows have 3 bindings.
type BindingCache = SmallVec<[Option<Rc<str>>; 4]>;
/// RFC-101 P1 — per-row delegated listener routes, inline for ≤2
/// listeners (jsbench rows have 2: select + remove).
type ListenerRoutes = SmallVec<[RowListenerRoute; 2]>;
/// RFC-101 P2 — the per-row binding-node handles resolved at mount,
/// inline for ≤4 bindings so the common row holds them with no heap
/// block (was `Box<[Element]>`, one allocation per row). Spills only
/// for wide rows.
pub(crate) type BindingNodes = SmallVec<[Element; 4]>;
/// RFC-101 P2 — the transient per-row listener-node handles the
/// channel returns, inline for ≤2 listeners (was `Vec<Element>`, one
/// allocation per row, consumed building [`ListenerRoutes`]).
pub(crate) type ListenerNodes = SmallVec<[Element; 2]>;

struct RowInstance {
    plan: Rc<CompiledRowPlan>,
    /// Element handles resolved from `plan.bindings[i].node_path`
    /// once at mount. RFC-101 P2 — inline for ≤4 bindings.
    binding_nodes: BindingNodes,
    /// Last serialised value written per binding. `None` means
    /// the slot is currently absent / not-yet-written. Compared
    /// against the new evaluation each tick to skip DOM writes
    /// when nothing changed.
    binding_cache: BindingCache,
    /// `js_sys::Proxy` for the row scope. Lazy-minted: when the
    /// plan is FastExpr-only (see [`is_proxy_elision_eligible`]),
    /// this stays `None` through mount and is built on first
    /// listener fire / slow-path binding eval. Mounting 10K rows
    /// without minting saves the per-row `Object::new ×2 + 2 trap
    /// closures + Proxy::new` (~24K wasm-js bridge ops).
    proxy: RefCell<Option<JsValue>>,
    /// Typed loop scope for compiled binding fast paths. Generic
    /// expression evaluation still uses `proxy`; fast paths read the
    /// loop item and parent proxy directly to avoid row-proxy traps.
    loop_state: Option<Rc<RefCell<LoopScope>>>,
    /// Per-row listener routes used by the list-level delegated
    /// listener installed for compiled row plans.
    listener_routes: ListenerRoutes,
    /// `(parent_scope_id, plan_ptr)` membership key for the
    /// list-level parent-state watcher (see [`ListWatcherKey`]).
    /// Lets the watcher iterate exactly its rows on re-fire and
    /// lets unmount drop the row from the per-list index without
    /// scanning the whole `ROW_INSTANCES` map.
    list_key: ListWatcherKey,
}

struct RowListenerRoute {
    event: &'static str,
    node: Element,
    /// Shared `Rc` from the plan's [`CompiledListener`]; cloning a row
    /// route is a pointer bump, not an AST deep-copy.
    ast: Rc<Spanned<Expr>>,
}

/// Key for the per-(parent_scope, plan) list-level watcher
/// registry. The plan pointer disambiguates multiple `pp-for`s
/// in the same parent component; the parent scope id covers the
/// case of two different component instances each rendering
/// their own list.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
struct ListWatcherKey {
    parent_scope_id: ScopeId,
    plan_ptr: usize,
}

impl ListWatcherKey {
    fn new(parent_scope_id: ScopeId, plan: &Rc<CompiledRowPlan>) -> Self {
        Self {
            parent_scope_id,
            plan_ptr: Rc::as_ptr(plan) as usize,
        }
    }
}

/// One entry per `(parent_scope_id, plan)` pair. Holds the live
/// effect id (so unmount can release it once the list is
/// fully empty) and the rows currently belonging to this list
/// (so the effect's body iterates only its own rows on
/// re-fire instead of scanning the whole `ROW_INSTANCES` map).
struct ListWatcher {
    /// `effect()` id that subscribes to every parent-state
    /// field referenced by any binding in `plan`. Released via
    /// `crate::reactive::release` once the last row in the
    /// list unmounts.
    effect_id: crate::reactive::EffectId,
    /// Live row scope ids for this list. Mutated in lockstep
    /// with `ROW_INSTANCES`: mount adds, unmount removes.
    members: Vec<ScopeId>,
}

thread_local! {
    /// Per-row state, keyed by the row's loop scope id. Populated
    /// on `mount_row_compiled`, drained on `unmount_row_compiled`
    /// when the for_.rs reconcile loop drops a row.
    static ROW_INSTANCES: RefCell<HashMap<ScopeId, RowInstance>> =
        RefCell::new(HashMap::new());

    /// One watcher per (parent_scope_id, plan) pair. RFC 054
    /// patches parent-state reactivity at the list level rather
    /// than per row: a single effect subscribes to every
    /// distinct parent-field name any of the plan's bindings
    /// touch, then on fire iterates the list's `members` and
    /// re-evaluates only the parent-touching bindings on each.
    /// One closure allocation per list instead of one per row —
    /// crucial for `runLots(10000)` mount cost.
    static LIST_WATCHERS: RefCell<HashMap<ListWatcherKey, ListWatcher>> =
        RefCell::new(HashMap::new());
}

// ─── per-row mount ──────────────────────────────────────────────

/// Resolve a `node_path` against a freshly cloned row root.
/// Walks `Element::children().item(idx)` per hop. Returns
/// `None` if any hop falls off the end (defensive against macro
/// emission bugs).
fn resolve_node_path(root: &Element, path: &[u16]) -> Option<Element> {
    let mut cur: Element = root.clone();
    for &idx in path {
        let children = cur.children();
        let next = children.item(idx as u32)?;
        cur = next;
    }
    Some(cur)
}

/// Install the bindings + listeners for a freshly cloned row.
/// `proxy` is the row's loop scope proxy if the caller already
/// minted one (non-elision path); `None` means the caller skipped
/// `Scope::into_proxy` because the plan is FastExpr-only and the
/// per-row hot path doesn't need a proxy. `scope_id` is the row
/// scope's id, used to lazy-mint the proxy on demand.
///
/// RFC 054 §5.5 — bindings evaluate **synchronously** here with
/// no `effect()` wrapper. The plan + per-row [`RowInstance`]
/// cache replaces reactive dispatch on update; reuse goes
/// through [`reuse_row_compiled`] which walks the cache and
/// patches DOM only on real change. This skips the per-row
/// effect-machinery + Reflect::get tracking that dominates
/// `update every 10th` cost in the generic mount.
pub(crate) fn mount_row_compiled(
    plan: &Rc<CompiledRowPlan>,
    row_root: &Element,
    scope_id: ScopeId,
    proxy: Option<&JsValue>,
) {
    mount_rows_compiled(plan, &[(row_root.clone(), scope_id, proxy.cloned())]);
}

/// Batch variant of [`mount_row_compiled`] for `pp-for` reconciles
/// that insert many fresh compiled rows at once. It keeps the same
/// per-row semantics, but amortizes row-instance table borrows,
/// list-watcher setup, and parent-binding refresh across the whole
/// inserted batch.
pub(crate) fn mount_rows_compiled(
    plan: &Rc<CompiledRowPlan>,
    rows: &[(Element, ScopeId, Option<JsValue>)],
) {
    if rows.is_empty() {
        return;
    }

    let mut instances: Vec<(ScopeId, RowInstance)> = Vec::with_capacity(rows.len());
    let mut watched_members: Vec<ScopeId> = Vec::new();
    let mut watcher: Option<ListWatcherKey> = None;

    let node_path_start = crate::profiler::mount::start();
    let mut resolved_binding_nodes: Vec<BindingNodes> = Vec::with_capacity(rows.len());
    for (row_root, _, _) in rows {
        let mut binding_nodes: BindingNodes = SmallVec::with_capacity(plan.bindings.len());
        for b in &plan.bindings {
            let Some(node) = resolve_node_path(row_root, b.node_path) else {
                console::warn_1(&JsValue::from_str(&format!(
                    "rfc-054: row binding node_path {:?} did not resolve",
                    b.node_path,
                )));
                return;
            };
            binding_nodes.push(node);
        }
        resolved_binding_nodes.push(binding_nodes);
    }
    crate::profiler::mount::record_node_path_resolution(node_path_start);

    let binding_start = crate::profiler::mount::start();
    let mut binding_caches: Vec<BindingCache> = Vec::with_capacity(rows.len());
    let mut loop_states: Vec<Option<Rc<RefCell<LoopScope>>>> = Vec::with_capacity(rows.len());
    let mut parent_links: Vec<Option<ScopeId>> = Vec::with_capacity(rows.len());
    for ((_, scope_id, proxy), binding_nodes) in rows.iter().zip(resolved_binding_nodes.iter()) {
        let loop_state = Scope::find(*scope_id).and_then(|scope| scope.typed::<LoopScope>());
        let parent_link = loop_state
            .as_ref()
            .map(|state| state.borrow().parent_scope_id);
        let mut binding_cache: BindingCache = SmallVec::from_elem(None, plan.bindings.len());
        {
            let loop_borrow = loop_state.as_ref().map(|state| state.borrow());
            let loop_ref = loop_borrow.as_deref();
            let placeholder_proxy = JsValue::UNDEFINED;
            let proxy_for_eval = proxy.as_ref().unwrap_or(&placeholder_proxy);
            for (i, b) in plan.bindings.iter().enumerate() {
                if !b.parent_field_paths.is_empty() {
                    continue;
                }
                let v = evaluate_binding(b, proxy_for_eval, loop_ref);
                binding_cache[i] = apply_binding(&binding_nodes[i], b.kind, &v, None);
            }
        }
        binding_caches.push(binding_cache);
        loop_states.push(loop_state);
        parent_links.push(parent_link);
    }
    crate::profiler::mount::record_initial_binding_apply(binding_start);

    let listener_start = crate::profiler::mount::start();
    let mut listener_routes: Vec<ListenerRoutes> = Vec::with_capacity(rows.len());
    for (row_root, _, _) in rows {
        listener_routes.push(resolve_listener_routes(plan, row_root));
        crate::profiler::mount::record_compiled_row_mounted();
    }
    crate::profiler::mount::record_listener_installation(listener_start);

    for (
        (((((_, scope_id, proxy), binding_nodes), binding_cache), listener_routes), loop_state),
        parent_link,
    ) in rows
        .iter()
        .zip(resolved_binding_nodes.into_iter())
        .zip(binding_caches.into_iter())
        .zip(listener_routes.into_iter())
        .zip(loop_states.into_iter())
        .zip(parent_links.into_iter())
    {
        let initial_proxy: RefCell<Option<JsValue>> = RefCell::new(proxy.clone());

        let list_key = match parent_link {
            Some(parent_scope_id) => {
                let list_key = ListWatcherKey::new(parent_scope_id, plan);
                if watcher.is_none() {
                    watcher = Some(list_key);
                }
                watched_members.push(*scope_id);
                list_key
            }
            None => ListWatcherKey::new(ScopeId(0), plan),
        };

        instances.push((
            *scope_id,
            RowInstance {
                plan: plan.clone(),
                binding_nodes,
                binding_cache,
                proxy: initial_proxy,
                loop_state,
                listener_routes,
                list_key,
            },
        ));
    }

    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        for (scope_id, instance) in instances {
            map.insert(scope_id, instance);
        }
    });

    if let Some(list_key) = watcher {
        ensure_list_watcher(plan, list_key);
        LIST_WATCHERS.with(|m| {
            if let Some(watcher) = m.borrow_mut().get_mut(&list_key) {
                watcher.members.extend(watched_members.iter().copied());
            }
        });
        refresh_parent_bindings_many(&watched_members);
    }
}

/// Install (idempotently) the list-level watcher for a
/// `(parent_scope_id, plan)` pair. The watcher subscribes to
/// every distinct top-level parent field name any of the
/// plan's bindings touch — fired exactly once per
/// `Handle::update` on the parent that touches one of those
/// keys.
///
/// The effect's body iterates the list's current `members`
/// (the row scope ids tracked alongside the watcher), looks up
/// each row's `RowInstance`, and re-evaluates only the
/// parent-touching bindings — the existing serialised-value
/// cache on `RowInstance.binding_cache` skips DOM writes when
/// nothing actually changed.
///
/// First-mount invariant: when the watcher is installed the
/// effect runs immediately, but `members` is still empty — the
/// row that triggered the install only appends itself *after*
/// `ensure_list_watcher` returns. So that first effect run
/// observes no rows and writes nothing.
///
/// Parent-touching bindings are skipped by the per-row initial
/// loop in `mount_row_compiled` (rows whose `parent_field_paths`
/// are non-empty defer to the watcher). To prevent the row's
/// first-mount DOM from being stale until the next parent
/// update, the caller invokes `refresh_parent_bindings` for
/// the just-mounted scope right before pushing it into
/// `members`.
///
/// From the second mount onward the watcher already exists,
/// no new effect is allocated, and the row simply appends to
/// `members` — subsequent parent updates re-fire the effect
/// against the full list.
fn ensure_list_watcher(plan: &Rc<CompiledRowPlan>, list_key: ListWatcherKey) {
    let already = LIST_WATCHERS.with(|m| m.borrow().contains_key(&list_key));
    if !already {
        // Distinct top-level parent fields across all bindings
        // in the plan.
        let mut field_keys: Vec<String> = Vec::new();
        for binding in &plan.bindings {
            for name in binding.parent_field_paths.iter() {
                if !field_keys.iter().any(|existing| existing == name) {
                    field_keys.push(name.clone());
                }
            }
        }
        if field_keys.is_empty() {
            return;
        }
        let parent_scope_id = list_key.parent_scope_id;
        let list_key_for_effect = list_key;
        let effect_id = crate::reactive::effect(move || {
            // RFC-096 S2 — subscribe via the shared read mirror
            // (same tracking the parent proxy's get trap
            // delegates to; no proxy required).
            for k in &field_keys {
                let _ = crate::scope::read_scope_key(parent_scope_id, k);
            }
            // Iterate this list's current rows and re-evaluate
            // their parent-touching bindings.
            let members: Vec<ScopeId> = LIST_WATCHERS.with(|m| {
                m.borrow()
                    .get(&list_key_for_effect)
                    .map(|w| w.members.clone())
                    .unwrap_or_default()
            });
            for row_scope in members {
                refresh_parent_bindings(row_scope);
            }
        });
        LIST_WATCHERS.with(|m| {
            m.borrow_mut().insert(
                list_key,
                ListWatcher {
                    effect_id,
                    members: Vec::new(),
                },
            );
        });
    }

    // First-mount eval for the row that triggered install
    // happens after the caller appends to `members`. See
    // `mount_row_compiled` for the explicit
    // `refresh_parent_bindings(scope_id)` call right after the
    // push.
}

/// Re-evaluate every parent-touching binding for the row in
/// `ROW_INSTANCES` keyed by `scope_id`. Used by the list
/// watcher's effect body and by `reuse_row_compiled`'s
/// fallback path. No-op if the row was already unmounted.
fn refresh_parent_bindings(scope_id: ScopeId) {
    refresh_parent_bindings_many(&[scope_id]);
}

fn refresh_parent_bindings_many(scope_ids: &[ScopeId]) {
    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        for scope_id in scope_ids {
            let Some(instance) = map.get_mut(scope_id) else {
                continue;
            };
            let plan = instance.plan.clone();
            // Slow-path eval (non-FastExpr) needs a real proxy. For
            // FastExpr-only plans the placeholder is never consulted —
            // every `evaluate_binding` short-circuits via `fast.is_some()`.
            let proxy_value = instance.proxy.borrow().clone();
            let placeholder = JsValue::UNDEFINED;
            let proxy_ref = proxy_value.as_ref().unwrap_or(&placeholder);
            let loop_borrow = instance.loop_state.as_ref().map(|state| state.borrow());
            let loop_ref = loop_borrow.as_deref();
            for (i, binding) in plan.bindings.iter().enumerate() {
                if binding.parent_field_paths.is_empty() {
                    continue;
                }
                let v = evaluate_binding(binding, proxy_ref, loop_ref);
                let prev = instance.binding_cache[i].as_deref();
                instance.binding_cache[i] =
                    apply_binding(&instance.binding_nodes[i], binding.kind, &v, prev);
            }
        }
    });
}

/// Re-run the row plan's bindings against the (mutated) loop
/// scope. Evaluates each binding's AST, compares against the
/// cached serialisation, and patches DOM only on real change.
/// Replaces `trigger_scope(scope_id)` on the compiled path —
/// no effect dispatch, no Reflect::get dep tracking, just a
/// flat array walk over `plan.bindings`.
///
/// Returns `true` if the row was tracked (compiled-path row);
/// `false` lets the caller fall back to `trigger_scope` for any
/// row whose mount went through the generic mount.
pub(crate) fn reuse_row_compiled(scope_id: ScopeId) -> bool {
    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        let Some(instance) = map.get_mut(&scope_id) else {
            return false;
        };
        let plan = instance.plan.clone();
        // Same proxy-or-placeholder dance as `refresh_parent_bindings`:
        // FastExpr-only plans never consult the proxy; mixed plans
        // mounted with a real proxy use it for slow-path evals.
        let proxy_value = instance.proxy.borrow().clone();
        let placeholder = JsValue::UNDEFINED;
        let proxy_ref = proxy_value.as_ref().unwrap_or(&placeholder);
        let loop_borrow = instance.loop_state.as_ref().map(|state| state.borrow());
        let loop_ref = loop_borrow.as_deref();
        for (i, b) in plan.bindings.iter().enumerate() {
            let v = evaluate_binding(b, proxy_ref, loop_ref);
            let prev = instance.binding_cache[i].as_deref();
            instance.binding_cache[i] = apply_binding(&instance.binding_nodes[i], b.kind, &v, prev);
        }
        true
    })
}

/// Drop a row's per-instance state. Called from `for_.rs` when
/// reconcile permanently retires a row. The mount's
/// `release_subtree` already tears down `addEventListener`
/// registrations via the element-scoped listener side-table; we
/// drop this row from its list watcher's `members` index here,
/// and when the last row in a list unmounts we release the
/// list-level effect too so its parent-state subscription
/// doesn't outlive the list.
pub fn unmount_row_compiled(scope_id: ScopeId) {
    let list_key = ROW_INSTANCES.with(|m| {
        m.borrow_mut()
            .remove(&scope_id)
            .map(|instance| instance.list_key)
    });
    let Some(list_key) = list_key else {
        return;
    };
    let dropped_effect = LIST_WATCHERS.with(|m| {
        let mut map = m.borrow_mut();
        let watcher = map.get_mut(&list_key)?;
        watcher.members.retain(|id| *id != scope_id);
        if watcher.members.is_empty() {
            // Last row in this list — tear the watcher down so
            // its parent-state subscription stops firing.
            map.remove(&list_key).map(|w| w.effect_id)
        } else {
            None
        }
    });
    if let Some(effect_id) = dropped_effect {
        crate::reactive::release(effect_id);
    }
}

/// Bulk teardown for the bulk-clear fast path in
/// `for_.rs::run_keyed`. Drops every `RowInstance` for the given
/// scope ids in a single `ROW_INSTANCES.with` borrow, then drops
/// the entire list-level watcher in a single `LIST_WATCHERS.with`
/// borrow.
///
/// Why this matters: per-row `unmount_row_compiled` calls
/// `watcher.members.retain(|id| *id != scope_id)` which is O(N)
/// over the list's members. Looping that N times is O(N²) — for a
/// 10K-row clear that's ~100M ops, and the slowest-run profile
/// pinned **358ms** of `clear`'s 401ms in this loop. Releasing
/// the watcher in one shot collapses it to O(N).
pub fn unmount_rows_bulk(scope_ids: &[ScopeId]) {
    if scope_ids.is_empty() {
        return;
    }
    // Pull `list_key` out of every removed instance. They should
    // all share the same key for the bulk-clear case (one
    // `pp-for` list going empty), but the API is correct even if
    // multiple lists are torn down together.
    let mut list_keys: Vec<ListWatcherKey> = ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        scope_ids
            .iter()
            .filter_map(|id| map.remove(id).map(|instance| instance.list_key))
            .collect()
    });
    if list_keys.is_empty() {
        return;
    }
    list_keys.sort_unstable_by_key(|k| (k.parent_scope_id.0, k.plan_ptr));
    list_keys.dedup();

    // Drop the list-level watcher(s) in one borrow. Capture the
    // effect ids to release outside the LIST_WATCHERS borrow
    // (release may indirectly read other thread_local state).
    let dropped_effects: Vec<crate::reactive::EffectId> = LIST_WATCHERS.with(|m| {
        let mut map = m.borrow_mut();
        list_keys
            .iter()
            .filter_map(|key| map.remove(key).map(|w| w.effect_id))
            .collect()
    });
    for effect_id in dropped_effects {
        crate::reactive::release(effect_id);
    }
}

/// Apply one binding's evaluated value to its DOM node. `prev`
/// is the cached serialisation from the last write; returns the
/// new cache slot to store. `None` return means "value shape
/// unrecognised — leave DOM alone, leave cache untouched".
fn apply_binding(
    el: &Element,
    kind: BindingKind,
    v: &JsValue,
    prev: Option<&str>,
) -> Option<Rc<str>> {
    match kind {
        BindingKind::Text => {
            let next = js_to_string(v);
            if prev == Some(next.as_str()) {
                return Some(Rc::from(next));
            }
            el.set_text_content(Some(&next));
            Some(Rc::from(next))
        }
        BindingKind::Class => {
            let serialised = serialise_class_value(v)?;
            if prev == Some(serialised.as_str()) {
                return Some(Rc::from(serialised));
            }
            if serialised.is_empty() {
                let _ = el.remove_attribute("class");
            } else {
                let _ = el.set_attribute("class", &serialised);
            }
            Some(Rc::from(serialised))
        }
        // RFC-058 Phase 2 template-plan variants. The keyed row
        // compiler never emits these (it only emits `Text` /
        // `Class`), so reaching this branch on the row-plan
        // path means a macro bug let a template-plan binding
        // ride a `StaticRowPlan`. Treat it as a framework bug
        // rather than silently dropping.
        BindingKind::Html | BindingKind::Bind { .. } | BindingKind::Show => {
            console::warn_1(&JsValue::from_str(
                "rfc-054: row plan received an RFC-058 template-plan-only \
                 BindingKind (Html / Bind / Show); skipping",
            ));
            None
        }
    }
}

fn resolve_listener_routes(plan: &CompiledRowPlan, row_root: &Element) -> ListenerRoutes {
    let mut routes: ListenerRoutes = SmallVec::with_capacity(plan.listeners.len());
    for listener in &plan.listeners {
        let Some(node) = resolve_node_path(row_root, listener.node_path) else {
            console::warn_1(&JsValue::from_str(&format!(
                "rfc-054: row listener node_path {:?} did not resolve",
                listener.node_path,
            )));
            continue;
        };
        routes.push(RowListenerRoute {
            event: listener.event,
            node,
            ast: listener.ast.clone(),
        });
    }
    routes
}

// ─── RFC-095 W4 — mutation-channel bridge ───────────────────────

/// Resolve (and cache) the channel descriptor slot for `plan`.
/// `None` = ineligible: a binding kind outside Text/Class, or a
/// non-skip binding whose fast expression isn't item-rooted.
/// Parent-dependent bindings are descriptor-marked `skip` — the
/// interpreter resolves their node but writes nothing, exactly
/// like the direct mount path, and the list watcher paints them
/// right after registration.
pub(crate) fn channel_slot(plan: &CompiledRowPlan) -> Option<u32> {
    match plan.channel.get() {
        ChannelState::Slot(s) => Some(s),
        ChannelState::Ineligible => None,
        ChannelState::Unknown => {
            let slot = build_channel_descriptor(plan);
            plan.channel.set(match slot {
                Some(s) => ChannelState::Slot(s),
                None => ChannelState::Ineligible,
            });
            slot
        }
    }
}

thread_local! {
    /// RFC-095 W4c — per-descriptor parent-rooted fast paths,
    /// keyed by channel slot. Row-invariant: resolved ONCE per
    /// flush into the `parentVals` array the interpreter indexes.
    static CHANNEL_PARENT_PATHS: RefCell<HashMap<u32, Box<[FastPath]>>> =
        RefCell::new(HashMap::new());
}

/// Convert one fast path for the descriptor. Item-family roots
/// carry their key chain; a Parent root is resolved Rust-side
/// per flush, so its descriptor entry is just `{root: 4, keys:
/// [idx]}` — an index into the flush's `parentVals`.
fn channel_fast_path(p: &FastPath, parent_paths: &mut Vec<FastPath>) -> (u8, Vec<JsValue>) {
    let root = match p.root {
        FastPathRoot::LoopItem => 0,
        FastPathRoot::LoopIndex => 1,
        FastPathRoot::LoopFirst => 2,
        FastPathRoot::LoopLast => 3,
        FastPathRoot::Parent => {
            // Magic roots ($store.x / $route.x) resolve through
            // magic_scope_access, not ComponentState::get — the
            // untracked flush-time resolver below can't serve
            // them. Refuse the channel; the direct path handles
            // these correctly.
            if p.keys
                .first()
                .and_then(|k| k.as_string())
                .is_none_or(|f| f.starts_with('$'))
            {
                return (255, Vec::new());
            }
            let idx = parent_paths.len() as f64;
            parent_paths.push(p.clone());
            return (4, vec![JsValue::from_f64(idx)]);
        }
    };
    (root, p.keys.to_vec())
}

/// Resolve a Parent-rooted fast path WITHOUT tracking — the
/// channel flush runs inside the pp-for effect, and a tracked
/// read here would subscribe the whole reconcile to e.g.
/// `selected_id` (the list watcher owns that subscription).
fn parent_path_value_untracked(path: &FastPath, parent_scope_id: ScopeId) -> JsValue {
    let mut keys = path.keys.iter();
    let Some(first) = keys.next().and_then(|k| k.as_string()) else {
        return JsValue::UNDEFINED;
    };
    let Some(scope) = Scope::find(parent_scope_id) else {
        return JsValue::UNDEFINED;
    };
    let mut cur = scope.state.borrow().get(&first);
    for k in keys {
        cur = js_sys::Reflect::get(&cur, k).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

/// The per-flush parent values for a registered descriptor.
pub(crate) fn channel_parent_vals(slot: u32, parent_scope_id: ScopeId) -> js_sys::Array {
    let out = js_sys::Array::new();
    CHANNEL_PARENT_PATHS.with(|m| {
        if let Some(paths) = m.borrow().get(&slot) {
            for p in paths.iter() {
                out.push(&parent_path_value_untracked(p, parent_scope_id));
            }
        }
    });
    out
}

fn build_channel_descriptor(plan: &CompiledRowPlan) -> Option<u32> {
    use crate::mutation_channel::{DescriptorBinding, DescriptorExpr};
    let mut bindings: Vec<DescriptorBinding> = Vec::with_capacity(plan.bindings.len());
    let mut parent_paths: Vec<FastPath> = Vec::new();
    for b in &plan.bindings {
        if !matches!(b.kind, BindingKind::Text | BindingKind::Class) {
            return None;
        }
        // Parent-dependent bindings paint at mount too (W4c) —
        // their parent side is row-invariant, resolved once per
        // flush into `parentVals`. The list watcher still owns
        // post-mount updates.
        let expr = match b.fast.as_ref()? {
            FastExpr::Path(p) => {
                let (root, keys) = channel_fast_path(p, &mut parent_paths);
                if root == 255 {
                    return None;
                }
                DescriptorExpr::Path { root, keys }
            }
            FastExpr::TernaryEq {
                lhs,
                rhs,
                then_value,
                else_value,
                invert,
            } => {
                let lhs = channel_fast_path(lhs, &mut parent_paths);
                let rhs = channel_fast_path(rhs, &mut parent_paths);
                if lhs.0 == 255 || rhs.0 == 255 {
                    return None;
                }
                DescriptorExpr::TernaryEq {
                    lhs,
                    rhs,
                    then_value: then_value.clone(),
                    else_value: else_value.clone(),
                    invert: *invert,
                }
            }
        };
        bindings.push(DescriptorBinding {
            node_path: b.node_path,
            kind: b.kind,
            skip: false,
            expr: Some(expr),
        });
    }
    let listener_paths: Vec<&'static [u16]> = plan.listeners.iter().map(|l| l.node_path).collect();
    let slot = crate::mutation_channel::register_plan(&bindings, &listener_paths);
    CHANNEL_PARENT_PATHS.with(|m| {
        m.borrow_mut().insert(slot, parent_paths.into_boxed_slice());
    });
    Some(slot)
}

/// One channel-mounted row's handles, as returned by the
/// interpreter, plus the Rust-side scope the caller minted.
pub(crate) struct ChannelRow {
    pub scope_id: ScopeId,
    pub binding_nodes: BindingNodes,
    pub listener_nodes: ListenerNodes,
    pub loop_state: Rc<RefCell<LoopScope>>,
}

/// Register channel-mounted rows: the bookkeeping half of
/// [`mount_rows_compiled`] with the DOM half already done
/// JS-side. Binding caches start empty (`None`) — the values
/// were written by the interpreter; the first reuse pass
/// re-writes once instead of comparing, which is correct and
/// costs one DOM write per binding per row, once.
pub(crate) fn register_channel_rows(
    plan: &Rc<CompiledRowPlan>,
    rows: Vec<ChannelRow>,
    parent_scope_id: ScopeId,
) {
    if rows.is_empty() {
        return;
    }
    let list_key = ListWatcherKey::new(parent_scope_id, plan);
    let mut watched: Vec<ScopeId> = Vec::with_capacity(rows.len());
    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        for row in rows {
            let listener_routes: ListenerRoutes = plan
                .listeners
                .iter()
                .zip(row.listener_nodes)
                .map(|(l, node)| RowListenerRoute {
                    event: l.event,
                    node,
                    ast: l.ast.clone(),
                })
                .collect();
            watched.push(row.scope_id);
            crate::profiler::mount::record_compiled_row_mounted();
            map.insert(
                row.scope_id,
                RowInstance {
                    plan: plan.clone(),
                    binding_nodes: row.binding_nodes,
                    binding_cache: SmallVec::from_elem(None, plan.bindings.len()),
                    proxy: RefCell::new(None),
                    loop_state: Some(row.loop_state),
                    listener_routes,
                    list_key,
                },
            );
        }
    });
    ensure_list_watcher(plan, list_key);
    LIST_WATCHERS.with(|m| {
        if let Some(watcher) = m.borrow_mut().get_mut(&list_key) {
            watcher.members.extend(watched.iter().copied());
        }
    });
    // No refresh_parent_bindings_many here (unlike
    // mount_rows_compiled): the interpreter painted parent-
    // dependent bindings with flush-time `parentVals`, so the
    // mount-time repaint would be a 10K-row no-op walk. The
    // watcher handles every later parent change.
}

// ─── helpers ────────────────────────────────────────────────────

/// Stringify a JsValue for `pp-text`. Mirrors text.rs.
fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(crate::text::js_number_string))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}

/// Class serialisation — mirrors `directives/bind.rs::serialise_class`
/// behaviour. String values pass through; object values join their
/// truthy keys.
///
/// Returns `None` for shapes the runtime explicitly leaves alone
/// (matching the existing `bind` behaviour — undefined / null /
/// false values get the attribute removed via the empty string;
/// non-string non-object values yield `None` so we don't touch DOM).
fn serialise_class_value(v: &JsValue) -> Option<String> {
    if v.is_undefined() || v.is_null() || v == &JsValue::FALSE {
        return Some(String::new());
    }
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if v.is_object() {
        use js_sys::{Object, Reflect};
        use wasm_bindgen::JsCast;
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out: Vec<String> = Vec::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            let truthy = Reflect::get(&obj, &k)
                .map(|val| val.as_bool().unwrap_or(!val.is_falsy()))
                .unwrap_or(false);
            if truthy
                && let Some(s) = k.as_string() {
                    out.push(s);
                }
        }
        return Some(out.join(" "));
    }
    None
}
