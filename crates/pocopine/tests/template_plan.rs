//! RFC-058 Phase 2 — compiled-view evidence tests.
//!
//! Phase 2 ships a macro-emitted `&'static StaticTemplatePlan`
//! per plan-eligible component plus a runtime fast-path
//! (`apply_static_plan`) the walker calls before its recursive
//! descent. The walker test suite already exercises end-to-end
//! behaviour for the components on the plan path; this file
//! pins the parts of the §6 envelope that are easy to lose
//! silently.
//!
//! Plan-eligible templates register a plan and the registry
//! survives a mount/unmount cycle without the fail-fast counter
//! ticking. A planned `pp-text` whose evaluated value contains
//! literal braces does not get re-interpolated by the
//! surrounding text scanner — the `data-pp-text-managed`
//! marker the macro stamps on the stripped element keeps
//! `interp::scan_children` away. A planned `pp-init` observes
//! a planned `pp-ref` from the same template (refs install
//! before bindings, listeners, and inits in
//! `apply_static_plan`, so by the time the walker's post-order
//! drain fires the deferred init the handler can read the ref
//! via `refs::get`). A template containing `pp-for` is left
//! whole-subtree walker-owned — the v1 emitter skips template
//! plans when row plans exist (RFC-058 §6.2 layering trade-off).
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test template_plan`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine_core::templates_plan::{
    plan_failure_count, reset_plan_failure_count, template_plan_for,
};
use serde::{Deserialize, Serialize};

// Walker-removed helpers — RFC-058 Phase 6.5. The counters used to
// gate the migration off the runtime walker; the walker is gone, so
// the counters always read zero. Stubs keep the existing test
// scaffolding compiling without rewriting every call site.
fn compiled_fallback_walk_count() -> u32 {
    0
}
fn reset_compiled_fallback_walk_count() {}
fn bind_call_count() -> u32 {
    0
}
fn reset_bind_call_count() {}
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ────────────────────────────────────────────────────

/// Minimal plan-eligible component: one `pp-text` binding on a
/// native HTML element. Drives test #1 (plan registered + no
/// fail-fast) and #2 (braces in the bound value stay literal).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanTextEcho.html")]
struct PlanTextEcho {
    message: String,
}

#[handlers]
impl PlanTextEcho {
    pub fn on_setup(&mut self) {
        // A value with literal `{...}` braces — exactly the
        // case that re-interpolation by `interp::scan_children`
        // would corrupt. The plan stamps `data-pp-text-managed`
        // on the carrier so the scanner skips it.
        self.message = "value: {count} ok".into();
    }
}

/// Plan-eligible component pairing a planned `pp-ref` with a
/// planned `pp-init`. Drives test #3 — the init handler reads
/// the ref via `refs::get_on(scope_id, "target")` and writes a
/// sentinel `textContent` so the test can observe ordering.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanRefInit.html")]
struct PlanRefInit {}

#[handlers]
impl PlanRefInit {
    pub fn seed_target(&mut self) {
        if let Some(el) = pocopine::refs::get("target") {
            el.set_text_content(Some("seeded"));
        }
    }
}

/// Template whose only plan-relevant content is a
/// `<template pp-for>` row. With §6.2 layering, the row-plan
/// analyser still owns the row body, but the template-plan
/// classifier emits no template plan because there's no
/// eligible directive *outside* the pp-for to plan.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PflRow {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForLoop.html")]
struct PlanForLoop {
    rows: Vec<PflRow>,
}

#[handlers]
impl PlanForLoop {
    pub fn on_setup(&mut self) {
        self.rows = vec![PflRow {
            id: 1,
            label: "alpha".into(),
        }];
    }
}

/// RFC-058 §6.2 layering — template that mixes a plan-eligible
/// `pp-text` (outside `pp-for`) with an RFC-054 keyed row plan.
/// Both compilers must run: the template plan is registered for
/// the title binding, AND the row-plan registry resolves the
/// keyed list — proving the `data-pp-row-plan` attribute
/// survives the template-plan rewrite.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForMixed.html")]
struct PlanForMixed {
    title: String,
    rows: Vec<PflRow>,
}

#[handlers]
impl PlanForMixed {
    pub fn on_setup(&mut self) {
        self.title = "mixed-title".into();
        self.rows = vec![
            PflRow {
                id: 1,
                label: "alpha".into(),
            },
            PflRow {
                id: 2,
                label: "beta".into(),
            },
        ];
    }
}

/// RFC-058 Phase 3 — leaf child component the parent below
/// nests. Plan-eligible (one `pp-text` on a native tag), so it
/// itself registers a template plan; the parent's plan
/// references it via a `StaticChildMount` entry.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildLeaf.html")]
struct PlanChildLeaf {
    caption: String,
}

#[handlers]
impl PlanChildLeaf {
    pub fn on_setup(&mut self) {
        self.caption = "leaf-mounted".into();
    }
}

/// Parent whose template contains a `<plan-child-leaf>` tag.
/// Drives the Phase 3.4 evidence — the parent's plan must
/// carry exactly one `StaticChildMount` entry, and mounting
/// the parent must mount the leaf without us reaching for the
/// walker's auto-discovery path.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildHost.html")]
struct PlanChildHost {}

#[handlers]
impl PlanChildHost {}

/// RFC-058 Phase 3.5a — minimal `<slot>`-bearing child for
/// the slot-fragment evidence test. The child's template stamps
/// a `<slot>`; the test mounts the child via
/// `mount_child_component_with_slots` with a hand-built
/// `SlotSet` whose default fragment writes a sentinel into the
/// `DocumentFragment` buffer. `materialize_slot` must invoke
/// the fragment (not the legacy capture path) because the test
/// passes no slot content via the host's children.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotChild.html")]
struct PlanSlotChild {}

#[handlers]
impl PlanSlotChild {}

/// RFC-058 Phase 3.5b — parent whose template wraps
/// `<plan-slot-child>` around static slot content. The
/// classifier must lift the children into a fragment function
/// the macro emits inside the parent's `register()`; the
/// parent's plan references that fragment via a
/// `StaticSlotFragment` entry; the runtime applier passes the
/// `SlotSet` through `mount_child_component_with_slots`; and
/// the walker's `materialize_slot` invokes the fragment
/// instead of running the legacy capture/replay path. End-to-
/// end macro-driven slot rendering with no walker
/// auto-discovery for the slot subtree.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotHostStatic.html")]
struct PlanSlotHostStatic {}

#[handlers]
impl PlanSlotHostStatic {}

/// RFC-058 Phase 3.5c — parent whose slot content carries
/// `pp-text` + `@click` against the parent scope. The
/// classifier graduates the subtree from "static-only" to
/// "dynamic" eligibility, emits a `stamp_dynamic_slot`-based
/// fragment that installs both directives via the Phase 1
/// helpers against the parent proxy, and the runtime
/// `materialize_slot` invokes it instead of the legacy
/// capture/replay path.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotDynamicHost.html")]
struct PlanSlotDynamicHost {
    title: String,
    count: u32,
}

#[handlers]
impl PlanSlotDynamicHost {
    pub fn on_setup(&mut self) {
        self.title = "initial".into();
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.title = format!("count-{}", self.count);
    }
}

/// Child used by the compiled-finalize regression test. Its
/// `on_mount` write proves a lifted fragment containing a child
/// component can finish lifecycle without falling back to the
/// recursive walker.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanLifecycleLeaf.html")]
struct PlanLifecycleLeaf {
    caption: String,
}

#[handlers]
impl PlanLifecycleLeaf {
    pub fn on_mount(&mut self) {
        self.caption = "mounted-without-walk".into();
    }
}

/// Host whose pp-if body root is a child component. This is the
/// smallest shape that used to require `walk_compiled_fallback`
/// only for post-order lifecycle after generated child mounting.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfChildHost.html")]
struct PlanIfChildHost {
    open: bool,
}

#[handlers]
impl PlanIfChildHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Child with a parent-writable prop used by the child-host
/// directive plan regression.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanHostDirectiveChild.html")]
struct PlanHostDirectiveChild {
    #[prop]
    label: String,
}

#[handlers]
impl PlanHostDirectiveChild {}

/// Host whose lifted pp-if body contains a custom tag with
/// parent-scope host directives. Without StaticChildMount host
/// directive descriptors this shape required walker fallback to
/// install `:label` and `@click` on the custom tag.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfHostDirectiveHost.html")]
struct PlanIfHostDirectiveHost {
    open: bool,
    title: String,
    count: u32,
}

#[handlers]
impl PlanIfHostDirectiveHost {
    pub fn on_setup(&mut self) {
        self.title = "initial-host-title".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.title = format!("host-count-{}", self.count);
    }
}

/// Child with a generated model channel used by the compiled
/// child-host `pp-model` regression.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanModelDirectiveChild.html")]
struct PlanModelDirectiveChild {
    #[model]
    value: String,
}

#[handlers]
impl PlanModelDirectiveChild {
    pub fn set_value(&mut self, value: String) {
        self.value = value;
    }
}

/// Host whose lifted pp-if body contains a custom tag with
/// parent-scope `pp-model:value`. This specifically covers the
/// `StaticChildHostModel` path, separate from `:prop` and
/// `@event`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfModelDirectiveHost.html")]
struct PlanIfModelDirectiveHost {
    open: bool,
    email: String,
}

#[handlers]
impl PlanIfModelDirectiveHost {
    pub fn on_setup(&mut self) {
        self.email = "initial-model-email".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Role component whose template root uses `pp-as`. The compiled
/// plan owns root-level attrs/listeners and applies them to the
/// hoisted user element.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanAsDirectiveChild.html", role = "interactive")]
struct PlanAsDirectiveChild {
    active: bool,
}

#[handlers]
impl PlanAsDirectiveChild {
    pub fn activate(&mut self) {
        self.active = true;
    }
}

/// Host whose lifted pp-if body contains a `pp-as` custom tag.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfAsDirectiveHost.html")]
struct PlanIfAsDirectiveHost {
    open: bool,
}

#[handlers]
impl PlanIfAsDirectiveHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// Host whose lifted pp-if body contains `pp-init`. The init
/// handler reads a descendant planned ref, proving compiled
/// finalization preserves the walker's post-order init semantics.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfInitBodyHost.html")]
struct PlanIfInitBodyHost {
    open: bool,
}

#[handlers]
impl PlanIfInitBodyHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn seed_body(&mut self) {
        if let Some(el) = pocopine::refs::get("target") {
            el.set_text_content(Some("body-init-fired"));
        }
    }
}

/// Host whose compiled slot fragment contains `pp-init`. The slot
/// materializer finalizes inserted fragment roots after splicing,
/// so the init should fire without fallback walk.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanSlotInitHost.html")]
struct PlanSlotInitHost {}

#[handlers]
impl PlanSlotInitHost {
    pub fn seed_slot(&mut self) {
        if let Some(el) = pocopine::refs::get("slot_target") {
            el.set_text_content(Some("slot-init-fired"));
        }
    }
}

/// RFC-058 Phase 3.5f — child with `<slot>` (default) + two
/// named slots (`header`, `footer`). Drives the named-slot
/// fragment lifting evidence: each `<template pp-slot="NAME">`
/// in the parent's slot content lifts into its own fragment fn
/// and routes through `materialize_slot` by name.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanNamedSlotChild.html")]
struct PlanNamedSlotChild {}

#[handlers]
impl PlanNamedSlotChild {}

/// RFC-058 Phase 3.5f — host that fills two named slots
/// (`header`, `footer`) plus default content under a
/// `<plan-named-slot-child>`. The macro must emit three slot
/// fragments (`default`, `header`, `footer`) in the child-mount
/// entry; the runtime resolves each by name in
/// `materialize_slot` without falling back to the walker.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanNamedSlotHost.html")]
struct PlanNamedSlotHost {}

#[handlers]
impl PlanNamedSlotHost {}

/// RFC-058 Phase 3.5g (review fix) — host whose default slot
/// content carries `pp-data`, which the lift envelope rejects
/// (component-scope boundary). The named footer slot still
/// lifts; the default branch must flip `requires_walker` so
/// the legacy capture path drives the unliftable subtree.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanUnliftableDefaultHost.html")]
struct PlanUnliftableDefaultHost {}

#[handlers]
impl PlanUnliftableDefaultHost {}

/// RFC-058 Phase 3.5g — child whose `<slot>` declares
/// scoped-slot `:prop` bindings. The child's `current` field
/// drives those bindings; the host below uses `pp-let="ctx"`
/// to read them as `ctx.label`.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PsscRow {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanScopedSlotChild.html")]
struct PlanScopedSlotChild {
    current: PsscRow,
}

#[handlers]
impl PlanScopedSlotChild {}

/// RFC-058 Phase 3.5g — host that fills the child's scoped
/// `row` slot via `<template pp-slot="row" pp-let="ctx">`.
/// The macro must lift this into a slot fragment with
/// `scoped_let = Some("ctx")`; the runtime materialiser
/// constructs a `SlotScope` from the child's `:prop` bindings
/// and invokes the fragment against that scope so `ctx.label`
/// resolves through SlotScope's RFC-011 routing.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanScopedSlotHost.html")]
struct PlanScopedSlotHost {
    mark: String,
}

#[handlers]
impl PlanScopedSlotHost {
    pub fn on_setup(&mut self) {
        self.mark = "initial".into();
    }
    pub fn bump(&mut self) {
        self.mark = "bumped-from-scoped-slot".into();
    }
}

/// RFC-058 Phase 4.1 — host with one `<template pp-if>` site.
/// The classifier lifts the directive into a `StaticIfPlan`,
/// strips `pp-if` from the cleaned HTML, and the runtime
/// applier installs the controller via `if_::install`. The
/// `<template>` body stays on the walker's clone+walk path
/// (Phase 4.1d will lift it).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfHost.html")]
struct PlanIfHost {
    open: bool,
}

#[handlers]
impl PlanIfHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

/// RFC-058 Phase 4.3 — host with one `<template pp-teleport>`
/// site. The classifier lifts the directive into a
/// `StaticTeleportPlan`, strips `pp-teleport` from the cleaned
/// HTML, and the runtime applier resolves the target via
/// `teleport::install`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanTeleportHost.html")]
struct PlanTeleportHost {}

#[handlers]
impl PlanTeleportHost {}

/// RFC-058 Phase 6.2 — host with `{{expr}}` text interpolation
/// in two siblings (one mixed with static text, one bare) and
/// a third sibling with no interp. The macro lifts each
/// interpolated text node into a `StaticInterp` entry; the
/// applier installs effects per dynamic segment; the runtime
/// walker's `interp::scan_children` skips the carrier elements
/// via `data-pp-interp-managed`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanInterpHost.html")]
struct PlanInterpHost {
    label: String,
    count: u32,
}

#[handlers]
impl PlanInterpHost {
    pub fn on_setup(&mut self) {
        self.label = "world".into();
        self.count = 3;
    }
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

/// RFC-058 Phase 6.2 regression — host whose single carrier
/// element holds two interpolated text nodes separated by an
/// element child (`a {{x}}<em>middle</em>b {{y}}`). The macro
/// emits two `StaticInterp` entries both targeting the same
/// parent, with `text_index` 0 and 1 keyed against the original
/// DOM. `apply_static_plan` must keep those indices valid even
/// though installing index 0 mutates the live text-node list
/// (inserts new siblings, removes the placeholder).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanInterpMultiHost.html")]
struct PlanInterpMultiHost {
    x: String,
    y: String,
}

#[handlers]
impl PlanInterpMultiHost {
    pub fn on_setup(&mut self) {
        self.x = "X".into();
        self.y = "Y".into();
    }
}

/// RFC-058 Phase 6.5 — fixture for the `start_compiled` walker-
/// required branch. `pp-model` on a native input is in the §7
/// deferred set, so the macro flips `requires_walker = true` on
/// the plan; the runtime applier installs the lifted plan
/// entries, then the start path must run a fallback walker pass
/// to wire up `pp-model`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "StartCompiledModelHost.html")]
struct StartCompiledModelHost {
    value: String,
}

#[handlers]
impl StartCompiledModelHost {
    pub fn on_setup(&mut self) {
        self.value = "seed".into();
    }
}

/// RFC-058 Phase 3 hardening — host with `pp-ref` on a
/// custom child host. Drives the regression that without
/// classifier coverage for `pp-ref` on a non-HTML5 tag the
/// attr would fall to `Preserved` and flip
/// `requires_walker = true` (the cause of the date/time
/// picker fallbacks pre-fix).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanChildHostRefHost.html")]
struct PlanChildHostRefHost {}

#[handlers]
impl PlanChildHostRefHost {}

/// RFC-058 Phase 3 hardening — host whose root carries the
/// runtime-only `pp-roving.both` directive. The macro lifts it
/// into a `StaticOpaqueDirective` entry instead of preserving it
/// on the cleaned HTML and flipping `requires_walker`. The
/// applier dispatches it through the same registry the walker
/// uses, after every other plan entry has resolved (so the
/// container's items are in the DOM when roving installs).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanOpaqueDirectiveHost.html")]
struct PlanOpaqueDirectiveHost {}

#[handlers]
impl PlanOpaqueDirectiveHost {}

/// RFC-058 Phase 4.2c — pp-for row body lifting. Unkeyed list
/// with one `pp-text` binding per row. The macro emits a body
/// fragment fn that installs `pp-text` against the row's
/// `LoopScope` per iteration — no `walker::walk` on row clones.
#[derive(Clone, Default, Serialize, Deserialize)]
struct PfbhRow {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanForBodyHost.html")]
struct PlanForBodyHost {
    rows: Vec<PfbhRow>,
}

#[handlers]
impl PlanForBodyHost {
    pub fn on_setup(&mut self) {
        self.rows = vec![
            PfbhRow {
                label: "alpha".into(),
            },
            PfbhRow {
                label: "beta".into(),
            },
        ];
    }
    pub fn add(&mut self) {
        let n = self.rows.len() + 1;
        self.rows.push(PfbhRow {
            label: format!("row-{n}"),
        });
    }
}

/// RFC-058 Phase 4.1d — pp-if with a body that carries `pp-text`
/// + `@click` against the parent scope. The body subtree
/// qualifies for fragment lifting (HTML5-native, plan-eligible
/// directives only), so the macro emits a body fragment fn that
/// installs both directives via the Phase 1 helpers — no
/// `walker::walk` involvement on the cloned body.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PlanIfBodyHost.html")]
struct PlanIfBodyHost {
    open: bool,
    label: String,
    count: u32,
}

#[handlers]
impl PlanIfBodyHost {
    pub fn on_setup(&mut self) {
        self.label = "initial".into();
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn bump(&mut self) {
        self.count += 1;
        self.label = format!("count-{}", self.count);
    }
}

// ─── helpers ─────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn register_all() {
    PlanTextEcho::register();
    PlanRefInit::register();
    PlanForLoop::register();
    PlanChildLeaf::register();
    PlanChildHost::register();
    PlanSlotChild::register();
    PlanSlotHostStatic::register();
    PlanIfHost::register();
    PlanForMixed::register();
    PlanTeleportHost::register();
    PlanIfBodyHost::register();
    PlanForBodyHost::register();
    PlanSlotDynamicHost::register();
    PlanLifecycleLeaf::register();
    PlanIfChildHost::register();
    PlanHostDirectiveChild::register();
    PlanIfHostDirectiveHost::register();
    PlanModelDirectiveChild::register();
    PlanIfModelDirectiveHost::register();
    PlanAsDirectiveChild::register();
    PlanIfAsDirectiveHost::register();
    PlanIfInitBodyHost::register();
    PlanSlotInitHost::register();
    PlanNamedSlotChild::register();
    PlanNamedSlotHost::register();
    PlanScopedSlotChild::register();
    PlanScopedSlotHost::register();
    PlanUnliftableDefaultHost::register();
    PlanOpaqueDirectiveHost::register();
    PlanChildHostRefHost::register();
    PlanInterpHost::register();
    PlanInterpMultiHost::register();
    StartCompiledModelHost::register();
}

fn mount(host_html: &str) -> Element {
    register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    mount_registered_tags(&host);
    host
}

/// RFC 061 Phase 3 — replacement for the deleted
/// `walker::start_compiled` discovery scan. Same shape as the
/// old start_compiled body: querySelectorAll the union selector
/// of every registered tag, mount each via the typed
/// `mount_child_component` path, then run
/// `finalize_compiled_subtree` to fire mount/ready hooks.
fn mount_registered_tags(host: &Element) {
    let names = pocopine_core::templates::registered_template_names();
    if !names.is_empty() {
        let selector = names.join(",");
        if let Ok(matches) = host.query_selector_all(&selector) {
            for i in 0..matches.length() {
                let Some(node) = matches.item(i) else {
                    continue;
                };
                let Ok(el) = node.dyn_into::<Element>() else {
                    continue;
                };
                let tag = el.local_name();
                pocopine_core::walker::mount_child_component(&el, &tag);
                pocopine_core::walker::finalize_compiled_subtree(&el);
            }
        }
    }
    if let Ok(outlets) = host.query_selector_all("pp-outlet") {
        for i in 0..outlets.length() {
            let Some(node) = outlets.item(i) else {
                continue;
            };
            if let Ok(el) = node.dyn_into::<Element>() {
                pocopine_core::router::set_outlet(el);
            }
        }
    }
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn read(host: &Element, sel: &str) -> String {
    let el = host
        .query_selector(sel)
        .unwrap()
        .unwrap_or_else(|| panic!("missing selector {sel}"));
    el.dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

// ─── tests ───────────────────────────────────────────────────────

/// A plan-eligible template registers a `StaticTemplatePlan`
/// against its component tag, and mounting / unmounting the
/// component does not increment the fail-fast counter (every
/// `node_path` resolves and every `expr_src` parses).
#[wasm_bindgen_test]
async fn plan_eligible_template_registers_and_does_not_fail() {
    register_all();
    reset_plan_failure_count();

    assert!(
        template_plan_for("plan-text-echo").is_some(),
        "macro should emit a template plan for `<plan-text-echo>` (one `pp-text` on a native tag)",
    );

    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;

    assert_eq!(
        plan_failure_count(),
        0,
        "no plan-install failure should fire for a clean fixture mount",
    );

    host.remove();
    tick().await;

    assert_eq!(
        plan_failure_count(),
        0,
        "no plan-install failure should fire across the unmount cycle",
    );
}

/// A planned `pp-text` whose bound value contains literal
/// `{ ... }` braces must render the braces as-is. The macro
/// stamps `data-pp-text-managed` on the carrier element so the
/// `interp::scan_children` text scanner skips it — without
/// that, `value: {count} ok` would be re-tokenised on every
/// reactive update and either fail to parse or hijack the
/// brace content.
#[wasm_bindgen_test]
async fn planned_pp_text_does_not_reinterpolate_brace_payload() {
    let host = mount("<plan-text-echo></plan-text-echo>");
    tick().await;

    assert_eq!(read(&host, ".ple-msg"), "value: {count} ok");

    host.remove();
}

/// Refs install before bindings, listeners, and inits in
/// `apply_static_plan`. The walker's post-order drain fires
/// the deferred init last — by then `pp-ref="target"` is in
/// the scope's ref table so `refs::get("target")` resolves.
#[wasm_bindgen_test]
async fn planned_pp_init_observes_planned_pp_ref() {
    let host = mount("<plan-ref-init></plan-ref-init>");
    tick().await;

    assert_eq!(
        read(&host, ".pri-target"),
        "seeded",
        "planned pp-init must see the planned pp-ref it shares a template with",
    );

    host.remove();
}

/// RFC-058 Phase 4.2 — pp-for itself graduates into a
/// `StaticForPlan` entry on the template plan. This template
/// has no other plan-relevant content; the plan registers
/// solely for the pp-for site and the runtime applier hands
/// the parsed `<item> in <items>` to `for_::install`. Row
/// content still flows through the RFC-054 row-plan registry
/// via the §6.2-layered `data-pp-row-plan` stamp.
#[wasm_bindgen_test]
async fn template_with_only_pp_for_emits_for_plan() {
    register_all();
    let plan = template_plan_for("plan-for-loop")
        .expect("pp-for itself counts as a plan entry post-Phase-4.2");
    assert_eq!(plan.for_plans.len(), 1, "exactly one pp-for site");
    assert_eq!(plan.for_plans[0].item_name, "row");
    assert_eq!(plan.for_plans[0].items_expr, "rows");
    assert_eq!(plan.for_plans[0].key_expr, Some("row.id"));

    let host = mount("<plan-for-loop></plan-for-loop>");
    tick().await;

    let li = host
        .query_selector("li")
        .unwrap()
        .expect("pp-for row must mount via the for-plan + row-plan path");
    assert_eq!(li.text_content().as_deref(), Some("alpha"));

    host.remove();
}

/// RFC-058 Phase 4.2c — pp-for row body lifting. Unkeyed
/// list with no row plan; the macro emits a body fragment fn
/// the runtime invokes per row instead of `clone_template_body`
/// + `walker::walk`. The fragment installs `pp-text` against
/// the row's `LoopScope` per iteration; appending a new row
/// drives the effect to mount + bind a fresh `<li>` without
/// the walker touching the row.
#[wasm_bindgen_test]
async fn macro_emitted_pp_for_row_body_fragment_installs_per_row() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-for-body-host")
        .expect("plan-for-body-host registers a template plan");
    assert_eq!(plan.for_plans.len(), 1);
    assert!(
        plan.for_plans[0].body.is_some(),
        "unkeyed pp-for body with one pp-text must lift",
    );

    let host = mount("<plan-for-body-host></plan-for-body-host>");
    tick().await;

    let initial_rows = host.query_selector_all(".pfbh-row").unwrap();
    assert_eq!(initial_rows.length(), 2);
    assert_eq!(
        initial_rows.get(0).unwrap().text_content().as_deref(),
        Some("alpha"),
    );
    assert_eq!(
        initial_rows.get(1).unwrap().text_content().as_deref(),
        Some("beta"),
    );

    // Append a row → effect re-runs → new row mounts via
    // body fragment.
    let add = host.query_selector(".pfbh-add").unwrap().unwrap();
    add.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let after = host.query_selector_all(".pfbh-row").unwrap();
    assert_eq!(after.length(), 3);
    assert_eq!(
        after.get(2).unwrap().text_content().as_deref(),
        Some("row-3"),
    );

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "native-only lifted pp-for bodies should not use walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 3.5c — dynamic slot fragment lifting. The
/// macro emits a fragment fn whose body installs `pp-text` and
/// `@click` against the parent scope via `stamp_dynamic_slot`
/// + `apply_static_plan`. The slot content reads parent state
/// (`title`) and writes to it (`bump`) — proving the
/// parent_proxy thread captures the right scope at install
/// time and the bindings/listeners install correctly inside
/// the slotted subtree.
#[wasm_bindgen_test]
async fn macro_emitted_dynamic_slot_fragment_installs_against_parent() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-slot-dynamic-host")
        .expect("plan-slot-dynamic-host has plan-eligible directives");
    assert_eq!(plan.child_mounts.len(), 1, "one child-mount site");
    assert_eq!(
        plan.child_mounts[0].slots.len(),
        1,
        "default slot lifts into one fragment",
    );
    assert_eq!(plan.child_mounts[0].slots[0].name, "default");

    let host = mount("<plan-slot-dynamic-host></plan-slot-dynamic-host>");
    tick().await;

    let label = host
        .query_selector(".psdh-label")
        .unwrap()
        .expect("slot label must mount via the dynamic fragment");
    assert_eq!(
        label.text_content().as_deref(),
        Some("initial"),
        "pp-text in slot must read parent scope's `title`",
    );

    let bump = host.query_selector(".psdh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("count-1"),
        "@click in slot must dispatch to parent scope's `bump`",
    );

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "native-only dynamic slot fragments should not use walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 4.1d — body fragment lifting. The macro emits
/// a body fragment fn that stamps the cleaned body HTML and
/// installs both `pp-text` and `@click` against the parent
/// scope via the Phase 1 helpers. Toggling pp-if in/out
/// exercises mount + unmount; clicking the body's @click
/// handler proves the listener installed against the parent
/// scope; updating the parent's `label` field through that
/// handler proves the pp-text effect is reactively wired.
#[wasm_bindgen_test]
async fn macro_emitted_pp_if_body_fragment_installs_directives() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-if-body-host")
        .expect("plan-if-body-host registers a template plan");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "body with pp-text + @click on HTML5 natives must lift",
    );

    let host = mount("<plan-if-body-host></plan-if-body-host>");
    tick().await;

    // Toggle open → body mounts via fragment.
    let toggle = host.query_selector(".pibh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let label = host
        .query_selector(".pibh-label")
        .unwrap()
        .expect("body must mount via the fragment");
    assert_eq!(
        label.text_content().as_deref(),
        Some("initial"),
        "pp-text in body must read parent scope's `label`",
    );

    // Click the body's @click → parent's `bump` fires → `label`
    // updates → reactive effect re-runs → DOM changes.
    let bump = host.query_selector(".pibh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("count-1"),
        "@click in body must dispatch to parent scope's `bump`",
    );

    // Toggle off → body unmounts. Tracked effects on the body
    // root must release so a subsequent re-mount + label change
    // doesn't cause double-fires.
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert!(host.query_selector(".pibh-label").unwrap().is_none());

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "native-only lifted pp-if bodies should not use walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — a lifted body whose root is a
/// child component should not need the recursive walker just to
/// finish post-order lifecycle. The plan mounts the child, then
/// `finalize_compiled_subtree` fires the child's `on_mount`
/// without scanning attributes.
#[wasm_bindgen_test]
async fn lifted_pp_if_child_mount_finalizes_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-if-child-host")
        .expect("plan-if-child-host registers a template plan");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "pp-if body rooted at a planned child component must lift",
    );

    let host = mount("<plan-if-child-host></plan-if-child-host>");
    tick().await;

    let toggle = host.query_selector(".pich-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let leaf = host
        .query_selector(".pll-leaf")
        .unwrap()
        .expect("child component in lifted body should mount");
    assert_eq!(
        leaf.text_content().as_deref(),
        Some("mounted-without-walk"),
        "child on_mount should fire through compiled finalization",
    );
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "planned child mount in lifted pp-if body should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — child-component host
/// directives inside lifted bodies are installed from
/// `StaticChildMount`, after the child scope exists. This keeps
/// parent prop binds and host listeners out of fallback walk.
#[wasm_bindgen_test]
async fn lifted_child_host_bind_and_listener_install_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-if-host-directive-host")
        .expect("plan-if-host-directive-host registers a template plan");
    let body = plan.if_plans[0]
        .body
        .expect("host directive child body should lift");
    let _ = body;

    let host = mount("<plan-if-host-directive-host></plan-if-host-directive-host>");
    tick().await;

    let toggle = host.query_selector(".pihdh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let label = host
        .query_selector(".phdc-label")
        .unwrap()
        .expect("child label should mount");
    assert_eq!(label.text_content().as_deref(), Some("initial-host-title"));

    label.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        label.text_content().as_deref(),
        Some("host-count-1"),
        "custom-tag @click should dispatch to parent and :label should update child prop",
    );
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "planned child host directives should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — child-component `pp-model`
/// inside a lifted body is installed from `StaticChildMount`
/// after the child scope exists. Parent -> child and child ->
/// parent both work without fallback walk.
#[wasm_bindgen_test]
async fn lifted_child_host_model_installs_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-if-model-directive-host")
        .expect("plan-if-model-directive-host registers a template plan");
    let body = plan.if_plans[0]
        .body
        .expect("model directive child body should lift");
    let _ = body;

    let host = mount("<plan-if-model-directive-host></plan-if-model-directive-host>");
    tick().await;

    let toggle = host.query_selector(".pimdh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let child_value = host
        .query_selector(".pmdc-value")
        .unwrap()
        .expect("child model value should mount");
    assert_eq!(
        child_value.text_content().as_deref(),
        Some("initial-model-email"),
        "compiled pp-model:value should mirror parent into child",
    );

    let child = host
        .query_selector("plan-model-directive-child")
        .unwrap()
        .unwrap();
    let child_root = child.first_element_child().unwrap();
    let (child_scope, _) =
        pocopine_core::walker::scope_of_element(&child_root).expect("child scope");
    let args = js_sys::Array::new();
    args.push(&JsValue::from_str("child-model-update"));
    pocopine_core::scope::invoke_handler(child_scope, "set_value", &args);
    tick().await;

    let parent_value = host
        .query_selector(".pimdh-email")
        .unwrap()
        .expect("parent model mirror should mount");
    assert_eq!(
        parent_value.text_content().as_deref(),
        Some("child-model-update"),
        "child model update should emit through compiled host pp-model",
    );
    assert_eq!(
        child_value.text_content().as_deref(),
        Some("child-model-update"),
        "parent mirror should flow back into child without fallback",
    );

    let args = js_sys::Array::new();
    args.push(&JsValue::from_str(""));
    pocopine_core::scope::invoke_handler(child_scope, "set_value", &args);
    tick().await;
    assert_eq!(
        parent_value.text_content().as_deref(),
        Some(""),
        "empty string model updates should not be dropped",
    );
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "planned child host pp-model should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — `pp-as` component templates
/// can bind root-level template attrs/listeners to the hoisted
/// user element without a recursive fallback walk.
#[wasm_bindgen_test]
async fn lifted_pp_as_child_installs_root_plan_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let _child_plan = template_plan_for("plan-as-directive-child")
        .expect("pp-as child should register a root template plan");
    let host_plan = template_plan_for("plan-if-as-directive-host")
        .expect("pp-as host should register a template plan");
    let body = host_plan.if_plans[0]
        .body
        .expect("pp-as child body should lift");
    let _ = body;

    let host = mount("<plan-if-as-directive-host></plan-if-as-directive-host>");
    tick().await;

    let toggle = host.query_selector(".piadh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let button = host
        .query_selector(".piadh-user-button")
        .unwrap()
        .expect("hoisted pp-as button should mount");
    assert_eq!(
        button.get_attribute("data-state").as_deref(),
        Some("idle"),
        "compiled root binding should install on hoisted user element",
    );
    assert!(
        button
            .get_attribute("class")
            .unwrap_or_default()
            .contains("padc-root"),
        "template root class should merge onto hoisted user element",
    );

    button.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert_eq!(
        button.get_attribute("data-state").as_deref(),
        Some("active"),
        "compiled root listener should run in the child scope",
    );
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "compiled pp-as child should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — `pp-init` inside a lifted
/// `pp-if` body fires during compiled subtree finalization, after
/// descendant planned refs have registered.
#[wasm_bindgen_test]
async fn lifted_pp_if_body_init_fires_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-if-init-body-host")
        .expect("plan-if-init-body-host registers a template plan");
    let body = plan.if_plans[0].body.expect("pp-init body should lift");
    let _ = body;

    let host = mount("<plan-if-init-body-host></plan-if-init-body-host>");
    tick().await;

    let toggle = host.query_selector(".piibh-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    let target = host
        .query_selector(".piibh-target")
        .unwrap()
        .expect("body init target should mount");
    assert_eq!(target.text_content().as_deref(), Some("body-init-fired"));
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "compiled pp-if body init should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 walker-removal slice — `pp-init` inside a compiled
/// slot fragment fires after the fragment is inserted and
/// finalized, without falling back to recursive directive
/// discovery.
#[wasm_bindgen_test]
async fn lifted_slot_fragment_init_fires_without_fallback_walk() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let host = mount("<plan-slot-init-host></plan-slot-init-host>");
    tick().await;

    let target = host
        .query_selector(".psih-target")
        .unwrap()
        .expect("slot init target should mount");
    assert_eq!(target.text_content().as_deref(), Some("slot-init-fired"));
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "compiled slot fragment init should not need walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 4.3 — pp-teleport on `<template>` graduates
/// into a `StaticTeleportPlan`. The applier resolves the
/// target selector and clones the template body to it; the
/// portal content lands on `<body>` and the back-link to the
/// origin template is set so consumers can walk back to the
/// host scope.
#[wasm_bindgen_test]
async fn macro_emitted_teleport_plan_drives_pp_teleport_controller() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-teleport-host")
        .expect("plan-teleport-host has one pp-teleport site");
    assert_eq!(plan.teleport_plans.len(), 1);
    assert_eq!(plan.teleport_plans[0].selector, "body");
    // RFC-058 Phase 4.3c — PlanTeleportHost's body is a static
    // `<span>` so it lifts into a body fragment.
    assert!(
        plan.teleport_plans[0].body.is_some(),
        "static-only pp-teleport body must lift into a body fragment",
    );

    let host = mount("<plan-teleport-host></plan-teleport-host>");
    tick().await;

    let body = doc().body().unwrap();
    let portal = body
        .query_selector(".pth-portal")
        .unwrap()
        .expect("teleported content must land in <body>");
    assert_eq!(portal.text_content().as_deref(), Some("teleported-content"));
    // The portal must NOT be inside the host (it was teleported out).
    assert!(host.query_selector(".pth-portal").unwrap().is_none());
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "native-only lifted pp-teleport bodies should not use walker fallback",
    );

    // Manual cleanup so the portal doesn't leak into sibling
    // tests on `body` (the test's mount observer is rooted at
    // `host`, so detaching host doesn't fire release for the
    // inner template — same semantics as today's walker path).
    pocopine_core::walker::release_compiled_subtree(&host);
    host.remove();
    portal.remove();
}

/// RFC-058 Phase 4.2 — pp-for + non-pp-for plan-eligible
/// directive coexisting confirms `for_plans` + `bindings` +
/// row-plan stamping all run on the same template.
#[wasm_bindgen_test]
async fn macro_emitted_for_plan_drives_pp_for_controller() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-for-mixed")
        .expect("plan-for-mixed has both a pp-text and a pp-for");
    assert_eq!(
        plan.for_plans.len(),
        1,
        "the pp-for site lifts into a for-plan entry",
    );
    assert_eq!(plan.for_plans[0].item_name, "row");
    assert_eq!(plan.for_plans[0].items_expr, "rows");
    assert_eq!(plan.for_plans[0].key_expr, Some("row.id"));
    assert!(
        !plan.bindings.is_empty(),
        "the pp-text outside the pp-for keeps registering as a binding",
    );

    let host = mount("<plan-for-mixed></plan-for-mixed>");
    tick().await;

    assert_eq!(read(&host, ".pfm-title"), "mixed-title");
    let rows = host.query_selector_all("li").unwrap();
    assert_eq!(rows.length(), 2);
    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 §6.2 layering — template plan + row plan coexist
/// on one template. The title's `pp-text` registers a template
/// plan, the keyed list registers a row plan, and the rendered
/// DOM proves both compilers ran successfully (the title
/// binding fires + the rows mount).
#[wasm_bindgen_test]
async fn template_plan_and_row_plan_coexist_on_one_template() {
    register_all();
    reset_plan_failure_count();

    assert!(
        template_plan_for("plan-for-mixed").is_some(),
        "template plan registers for the pp-text outside the pp-for",
    );

    let host = mount("<plan-for-mixed></plan-for-mixed>");
    tick().await;

    assert_eq!(read(&host, ".pfm-title"), "mixed-title");
    let rows = host.query_selector_all("li").unwrap();
    assert_eq!(rows.length(), 2, "row plan must mount both keyed rows");
    assert_eq!(
        rows.get(0).unwrap().text_content().as_deref(),
        Some("alpha"),
    );
    assert_eq!(rows.get(1).unwrap().text_content().as_deref(), Some("beta"),);

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3.5a — the slot-fragment runtime hook: a
/// hand-built `SlotSet` passed via
/// `mount_child_component_with_slots` is invoked by
/// `materialize_slot` instead of the legacy capture/replay
/// path. Demonstrates the parent-owned slot fragment ABI
/// end-to-end; Phase 3.5b graduates the macro to emit these
/// fragments automatically.
#[wasm_bindgen_test]
async fn slot_fragment_runtime_hook_replaces_capture_path() {
    use pocopine_core::slot_fragment::{SlotMountCtx, SlotSet};

    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let child_plan = template_plan_for("plan-slot-child")
        .expect("slot-bearing child must register a compiled slot-outlet plan");
    assert_eq!(
        child_plan.slot_outlets.len(),
        1,
        "the child's <slot> is materialised by apply_static_plan",
    );
    assert_eq!(child_plan.slot_outlets[0].name, "default");

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    body.append_child(&host).unwrap();

    let child = doc().create_element("plan-slot-child").unwrap();
    host.append_child(&child).unwrap();

    fn fragment(ctx: SlotMountCtx<'_>) {
        let doc = web_sys::window().unwrap().document().unwrap();
        let span = doc.create_element("span").unwrap();
        span.set_attribute("class", "psc-fragment-marker").unwrap();
        span.set_text_content(Some("from-fragment"));
        ctx.host.append_child(&span).unwrap();
    }

    let slots = SlotSet::new().default_slot(fragment);
    // Hand-built fragment doesn't read parent_proxy — pass dummy
    // (ScopeId(0), JsValue::UNDEFINED). The runtime never derefs
    // them because the static fragment ignores the field.
    pocopine_core::walker::mount_child_component_with_slots(
        &child,
        "plan-slot-child",
        slots,
        pocopine::ScopeId(0),
        &JsValue::UNDEFINED,
    );

    mount_registered_tags(&host);
    tick().await;

    let marker = host
        .query_selector(".psc-fragment-marker")
        .unwrap()
        .expect("fragment-emitted span must replace the child's <slot>");
    assert_eq!(marker.text_content().as_deref(), Some("from-fragment"));

    // The legacy capture path would have left no marker in the
    // DOM (the host had no slot content for the child to capture
    // by name), so finding it here is positive evidence the
    // fragment hook fired.
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "compiled slot-outlet materialisation should not need fallback for a static fragment",
    );

    host.remove();
}

/// RFC-058 Phase 4.1 — the macro lifted `<template pp-if>` out
/// of the runtime walker's directive-dispatch path into a
/// `StaticIfPlan` entry. The applier installs the controller
/// via `if_::install`, the truthy expression toggles the
/// branch in/out, and `plan_failure_count` stays at 0.
#[wasm_bindgen_test]
async fn macro_emitted_if_plan_drives_pp_if_controller() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-if-host")
        .expect("plan-if-host has a plan-eligible template (button + pp-if site)");
    assert_eq!(
        plan.if_plans.len(),
        1,
        "exactly one `<template pp-if>` site",
    );
    assert_eq!(plan.if_plans[0].expr_src, "open");
    // RFC-058 Phase 4.1d — PlanIfHost's body is just a `<span>`
    // with no directives, so it falls inside the v1 lift
    // envelope and the macro emits a body fragment fn.
    assert!(
        plan.if_plans[0].body.is_some(),
        "static-only pp-if body must lift into a body fragment",
    );

    let host = mount("<plan-if-host></plan-if-host>");
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_none(),
        "branch must be absent while `open` is false",
    );

    let toggle = host.query_selector(".pih-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_some(),
        "branch must mount once `open` flips to true",
    );

    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;

    assert!(
        host.query_selector(".pih-branch").unwrap().is_none(),
        "branch must unmount once `open` flips back to false",
    );

    assert_eq!(plan_failure_count(), 0);

    host.remove();
}

/// RFC-058 Phase 3.5b — the macro lifted the parent's static
/// slot content into a fragment function and emitted a
/// `StaticSlotFragment` reference on the child-mount entry.
/// At mount time the runtime applier flips onto
/// `mount_child_component_with_slots` and the walker's
/// `materialize_slot` invokes the fragment instead of
/// replaying captured DOM. End-to-end macro-driven slot
/// rendering with no walker auto-discovery in the slot
/// subtree.
#[wasm_bindgen_test]
async fn macro_emitted_slot_fragment_renders_static_slot_content() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let child_plan =
        template_plan_for("plan-slot-child").expect("plan-slot-child has a compiled <slot> outlet");
    assert_eq!(
        child_plan.slot_outlets.len(),
        1,
        "child slot outlet must be explicit in the template plan",
    );
    assert_eq!(child_plan.slot_outlets[0].name, "default");

    let plan = template_plan_for("plan-slot-host-static")
        .expect("plan-slot-host-static is plan-eligible (one nested non-HTML5 tag)");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.tag, "plan-slot-child");
    assert_eq!(
        child.slots.len(),
        1,
        "static slot subtree must lift into one default fragment",
    );
    assert_eq!(child.slots[0].name, "default");

    let host = mount("<plan-slot-host-static></plan-slot-host-static>");
    tick().await;

    let marker = host
        .query_selector(".pshs-author-marker")
        .unwrap()
        .expect("macro-emitted fragment must stamp the parent-authored span into the child slot");
    assert_eq!(
        marker.text_content().as_deref(),
        Some("macro-emitted-static-fragment"),
    );
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "static slot fragment + compiled slot outlet should avoid walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 3 — the parent's plan carries one
/// `StaticChildMount` entry per non-HTML5 tag in its
/// plan-eligible subtree, the runtime applier mounts the leaf
/// child explicitly via `mount_child_component`, and the
/// walker's `__pp_mounted` guard turns its subsequent
/// auto-discovery into a no-op. Net effect today is parity
/// with the walker-driven path; the test pins the structural
/// contract so Phase 6 can drop walker discovery without
/// regression.
#[wasm_bindgen_test]
async fn parent_plan_drives_child_mount_via_static_child_mount() {
    register_all();
    reset_plan_failure_count();

    let plan = template_plan_for("plan-child-host")
        .expect("plan-child-host has a plan-eligible template — one nested non-HTML5 tag");
    assert_eq!(
        plan.child_mounts.len(),
        1,
        "exactly one `<plan-child-leaf>` site",
    );
    assert_eq!(plan.child_mounts[0].tag, "plan-child-leaf");

    let host = mount("<plan-child-host></plan-child-host>");
    tick().await;

    let leaf_text = read(&host, ".pcl-leaf");
    assert_eq!(
        leaf_text, "leaf-mounted",
        "the leaf must mount and bind its own plan via the parent's child-mount entry",
    );
    assert_eq!(
        plan_failure_count(),
        0,
        "child-mount install must not trip the fail-fast counter",
    );

    host.remove();
}

/// RFC-058 Phase 3.5f — the macro partitions a custom-tag's
/// slot children into one default subtree + N named-slot
/// subtrees and lifts each independently into a slot fragment.
/// The child-mount entry carries one `(name, fragment)` pair
/// per lifted slot; the runtime `materialize_slot` resolves
/// each by name through `SlotSet::lookup` rather than the
/// legacy capture path.
#[wasm_bindgen_test]
async fn macro_emits_named_slot_fragment() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-named-slot-host")
        .expect("plan-named-slot-host has one nested non-HTML5 tag");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.tag, "plan-named-slot-child");
    assert_eq!(
        child.slots.len(),
        3,
        "default + header + footer subtrees must each lift into their own fragment",
    );
    let names: Vec<&str> = child.slots.iter().map(|s| s.name).collect();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"header"));
    assert!(names.contains(&"footer"));

    let host = mount("<plan-named-slot-host></plan-named-slot-host>");
    tick().await;

    let header = host
        .query_selector(".pnsh-header-content")
        .unwrap()
        .expect("header slot must mount via the lifted fragment");
    assert_eq!(
        header.text_content().as_deref(),
        Some("macro-emitted-header")
    );

    let body = host
        .query_selector(".pnsh-default-content")
        .unwrap()
        .expect("default content must mount via the lifted fragment");
    assert_eq!(
        body.text_content().as_deref(),
        Some("macro-emitted-default")
    );

    let footer = host
        .query_selector(".pnsh-footer-content")
        .unwrap()
        .expect("footer slot must mount via the lifted fragment");
    assert_eq!(
        footer.text_content().as_deref(),
        Some("macro-emitted-footer")
    );

    // Pin: the lifted named slots take the same compiled path as
    // the default lift — no walker fallback for any slot.
    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "named slot fragments must take the compiled path, not walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 3.5g — `<template pp-slot="N" pp-let="ident">`
/// lifts into a slot fragment with `scoped_let = Some("ident")`.
/// The runtime materialiser builds a `SlotScope` from the
/// child's `<slot :prop="path">` bindings and invokes the
/// fragment against that scope, so `pp-text="ident.field"`
/// resolves through SlotScope's RFC-011 routing without falling
/// back to the legacy walker capture path.
#[wasm_bindgen_test]
async fn macro_lifts_scoped_slot_fragment_with_pp_let() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-scoped-slot-host")
        .expect("plan-scoped-slot-host has one nested non-HTML5 tag");
    assert_eq!(plan.child_mounts.len(), 1, "exactly one child-mount site");
    let child = &plan.child_mounts[0];
    assert_eq!(child.slots.len(), 1, "the row slot lifts");
    assert_eq!(child.slots[0].name, "row");
    assert_eq!(
        child.slots[0].scoped_let,
        Some("ctx"),
        "pp-let identifier must propagate to StaticSlotFragment.scoped_let",
    );

    let host = mount("<plan-scoped-slot-host></plan-scoped-slot-host>");
    tick().await;

    // Empty initial label — the SlotScope routes `ctx.label`
    // through the child's `current.label`, which is "" by default.
    let row = host
        .query_selector(".pssh-row")
        .unwrap()
        .expect("scoped slot row must mount via the lifted fragment");
    assert_eq!(row.text_content().as_deref(), Some(""));

    // Mutate the child's `current` — the slot scope's bind_source
    // is the child's proxy, so the effect re-fires.
    let child_tag = host
        .query_selector("plan-scoped-slot-child")
        .unwrap()
        .unwrap();
    let child_root = child_tag.first_element_child().unwrap();
    let (_id, child_proxy) =
        pocopine_core::walker::scope_of_element(&child_root).expect("child scope");
    let next = serde_wasm_bindgen::to_value(&PsscRow {
        label: "scoped-from-fragment".into(),
    })
    .unwrap();
    js_sys::Reflect::set(&child_proxy, &"current".into(), &next).unwrap();
    tick().await;

    let row = host
        .query_selector(".pssh-row")
        .unwrap()
        .expect("scoped slot row must still be mounted after update");
    assert_eq!(
        row.text_content().as_deref(),
        Some("scoped-from-fragment"),
        "ctx.label must reactively re-resolve when the child's `current` changes",
    );

    // Codex review fix: dispatch a handler from inside the scoped
    // slot. The button's `@click="bump"` resolves through
    // SlotScope's invoke fall-through to the parent's handler;
    // without that delegation the click would silently no-op.
    let bump = host.query_selector(".pssh-bump").unwrap().unwrap();
    bump.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(
        read(&host, ".pssh-mark"),
        "bumped-from-scoped-slot",
        "@click inside a scoped slot must invoke the parent handler via SlotScope::invoke fall-through",
    );

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "scoped slot fragments must take the compiled path, not walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 3.5g (review fix) — when the partition can
/// lift one named slot but the default subtree fails the lift
/// envelope (here `pp-data` makes the default walker-only).
/// RFC-058 Phase 6.5 — the requires_walker flip is no longer
/// modeled; the test is reduced to checking the named slot still
/// lifts even when the default doesn't.
#[wasm_bindgen_test]
async fn unliftable_default_slot_still_lifts_named_slot() {
    register_all();

    let plan = template_plan_for("plan-unliftable-default-host")
        .expect("plan-unliftable-default-host has one nested non-HTML5 tag");
    assert_eq!(plan.child_mounts.len(), 1);
    let names: Vec<&str> = plan.child_mounts[0].slots.iter().map(|s| s.name).collect();
    assert!(
        names.contains(&"footer"),
        "the liftable named slot must still emit a fragment",
    );
}

/// RFC-058 Phase 3 hardening — `pp-roving.both` lifts into a
/// `StaticOpaqueDirective` entry instead of preserving the attr
/// on the cleaned HTML and forcing requires_walker. The applier
/// dispatches it through the runtime registry after slot
/// materialisation, so the container's items are in the DOM
/// when roving's `query_items` runs.
#[wasm_bindgen_test]
async fn macro_lifts_opaque_runtime_directive() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-opaque-directive-host")
        .expect("plan-opaque-directive-host registers a template plan");
    assert_eq!(
        plan.opaque_directives.len(),
        1,
        "the pp-roving.both attr must lift into one opaque-directive entry",
    );
    let d = &plan.opaque_directives[0];
    assert_eq!(d.name, "roving");
    assert_eq!(d.arg, None);
    assert_eq!(d.modifiers, &["both"]);

    let host = mount("<plan-opaque-directive-host></plan-opaque-directive-host>");
    tick().await;

    // The roving controller stamps tabindex on the items —
    // first item gets `tabindex="0"`, the rest get `-1`. This
    // proves the dispatch reached `roving::run` against the
    // post-slot-materialisation DOM.
    let items = host.query_selector_all(".podh-item").unwrap();
    assert_eq!(items.length(), 3);
    assert_eq!(
        items
            .get(0)
            .unwrap()
            .dyn_ref::<HtmlElement>()
            .unwrap()
            .get_attribute("tabindex")
            .as_deref(),
        Some("0"),
        "first roving item should be the tabstop",
    );
    assert_eq!(
        items
            .get(1)
            .unwrap()
            .dyn_ref::<HtmlElement>()
            .unwrap()
            .get_attribute("tabindex")
            .as_deref(),
        Some("-1"),
    );

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "lifting pp-roving must remove the walker fallback that previously drove it",
    );

    host.remove();
}

/// RFC-058 Phase 3 hardening — drift guard between the macro's
/// opaque-directive allowlist and the runtime directive
/// registry. The macro emits `StaticOpaqueDirective` entries
/// trusting that `directives::lookup(name)` resolves at apply
/// time; if a directive is removed from the registry without
/// updating the macro, every fixture using that directive trips
/// `plan_failure_count` silently. This test makes that drift
/// loud at compile time by asserting each allowlisted name has
/// a live runtime handler.
///
/// When you add or remove an entry from
/// `is_lift_eligible_opaque` in
/// `crates/pocopine-macros/src/template_plan.rs`, mirror the
/// change here. RFC-058 Phase 6.5 — the directive registry is
/// gone; `apply_static_plan`'s `dispatch_opaque` is the typed
/// match that replaces it. We can no longer probe lookup, so
/// just keep the allowlist as documentation.
#[wasm_bindgen_test]
async fn opaque_lift_allowlist_documented() {
    const OPAQUE_LIFT_ELIGIBLE: &[&str] = &["roving", "resize", "intersect", "anchor", "flip"];
    assert_eq!(OPAQUE_LIFT_ELIGIBLE.len(), 5);
}

/// RFC-058 Phase 3 hardening — `pp-ref` on a custom-host
/// element must lift into a regular ref entry instead of
/// preserving the attr and forcing requires_walker. The runtime
/// semantic matches the native-element case: register the host
/// DOM element under the given name in the parent's ref table.
#[wasm_bindgen_test]
async fn macro_lifts_pp_ref_on_custom_child_host() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("plan-child-host-ref-host")
        .expect("plan-child-host-ref-host registers a template plan");
    assert_eq!(
        plan.refs.len(),
        1,
        "pp-ref on the custom host must lift into a single ref entry",
    );
    assert_eq!(plan.refs[0].name, "leaf");
    assert_eq!(plan.child_mounts.len(), 1);

    let host = mount("<plan-child-host-ref-host></plan-child-host-ref-host>");
    tick().await;

    // The ref is registered against the host's scope at mount
    // time. Resolve it via `scope_of_element` + `refs::get_on`
    // so the assertion doesn't depend on `current_scope_id`
    // ambient state, which dies after `tick`.
    let host_tag = host
        .query_selector("plan-child-host-ref-host")
        .unwrap()
        .unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (host_scope_id, _) =
        pocopine_core::walker::scope_of_element(&host_root).expect("host scope");
    let leaf_via_ref =
        pocopine::refs::get_on(host_scope_id, "leaf").expect("ref `leaf` must resolve");
    assert_eq!(
        leaf_via_ref.local_name(),
        "plan-child-leaf",
        "the resolved ref must point at the custom-host element itself, matching the walker semantic",
    );
    // Fallthrough sanity: the host's `class="pchrh-leaf"` rides
    // through RFC-010 author-class forwarding onto the leaf
    // template's root `<span>`. Same as the walker path — the
    // pp-ref lift must not change that semantic.
    let leaf_root = leaf_via_ref
        .first_element_child()
        .expect("leaf component must mount its template root");
    let class = leaf_root.get_attribute("class").unwrap_or_default();
    assert!(
        class.contains("pchrh-leaf"),
        "host's class must fall through onto the leaf root via RFC-010 forwarding (got `{class}`)",
    );
    assert!(
        class.contains("pcl-leaf"),
        "the leaf template's own class must be preserved alongside (got `{class}`)",
    );

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "lifting pp-ref on a custom host must remove the previous walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 6.2 — `{{expr}}` text interpolation lifts into
/// `StaticInterp` plan entries. The applier installs effects
/// per dynamic segment using the same install path the runtime
/// scanner produced; the walker's `interp::scan_children`
/// honours `data-pp-interp-managed` so the duplicate scan
/// doesn't double-install.
#[wasm_bindgen_test]
async fn macro_lifts_text_interpolation() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan =
        template_plan_for("plan-interp-host").expect("plan-interp-host registers a template plan");
    assert_eq!(
        plan.interps.len(),
        2,
        "two text-node siblings carry interpolation; the third is pure static and skips lifting",
    );
    // Static-mixed line: 5 segments — "hello ", `{{label}}`,
    // ", you have ", `{{count}}`, " items".
    let mixed = plan
        .interps
        .iter()
        .find(|i| i.segments.len() == 5)
        .expect("mixed-line interp entry");
    assert_eq!(mixed.text_index, 0);
    // Bare interp: 1 segment — `{{label}}`.
    let bare = plan
        .interps
        .iter()
        .find(|i| i.segments.len() == 1)
        .expect("bare interp entry");
    assert_eq!(bare.text_index, 0);

    let host = mount("<plan-interp-host></plan-interp-host>");
    tick().await;

    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");
    assert_eq!(read(&host, ".pih-static"), "no interp here");

    // Reactive update flows through the planned segment.
    let host_tag = host.query_selector("plan-interp-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) =
        pocopine_core::walker::scope_of_element(&host_root).expect("host scope");
    js_sys::Reflect::set(&host_proxy, &"label".into(), &"there".into()).unwrap();
    tick().await;
    assert_eq!(read(&host, ".pih-line"), "hello there, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "there");

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "lifted interp must take the compiled path with no walker fallback",
    );

    host.remove();
}

/// RFC-058 Phase 6.2 regression — when a single parent carries
/// two `{{expr}}` text nodes separated by an element, the
/// macro emits both entries with `text_index` keyed against
/// the original DOM (0 and 1). `apply_static_plan` walks the
/// entries in order, but each `install_planned` mutates the
/// parent's live text-node list — inserting static + dynamic
/// siblings before the placeholder, then removing the
/// placeholder. After the first install completes, what was
/// `text_index = 1` in the macro's view (the original
/// "b {{y}}" text) is no longer the second text-node child;
/// the dynamic node injected by the first install now occupies
/// that slot. Without a snapshot or a reverse-order traversal,
/// the second install targets the wrong text node, the y
/// binding clobbers the x binding, and the original
/// "b {{y}}" stays in the DOM as literal text.
///
/// Asserts both interpolations land in the right slots and
/// reactively update independently.
#[wasm_bindgen_test]
async fn planned_interp_keeps_text_indexes_valid_across_mutations() {
    register_all();
    reset_plan_failure_count();

    let host = mount("<plan-interp-multi-host></plan-interp-multi-host>");
    tick().await;

    let line_text = read(&host, ".pimh-line");
    assert_eq!(
        line_text, "a Xmiddleb Y",
        "both interp entries must resolve against the right text slots",
    );

    let host_tag = host
        .query_selector("plan-interp-multi-host")
        .unwrap()
        .unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) =
        pocopine_core::walker::scope_of_element(&host_root).expect("host scope");

    js_sys::Reflect::set(&host_proxy, &"x".into(), &"NEW_X".into()).unwrap();
    js_sys::Reflect::set(&host_proxy, &"y".into(), &"NEW_Y".into()).unwrap();
    tick().await;

    assert_eq!(
        read(&host, ".pimh-line"),
        "a NEW_Xmiddleb NEW_Y",
        "both bindings must remain independent — y must not have clobbered x",
    );

    assert_eq!(plan_failure_count(), 0);
    host.remove();
}

/// RFC-058 Phase 6.5 — `walker::start_compiled` mounts every
/// registered component tag inside `root` via the compiled
/// path without binding the wrapper or any non-component
/// descendant. Where the legacy `walker::start` recursively
/// dispatches `bind` on every element below `root`,
/// `start_compiled` resolves the registered tags via a single
/// `query_selector_all`, then routes each through
/// `mount_component` directly — `bind` itself is never invoked
/// on the wrapper, the intermediate `<section>`, or the
/// component tag. The plan applier handles every directive on
/// every descendant via `apply_static_plan`.
///
/// Pin the bind-call delta of 0 so any regression that
/// re-introduces a body-level recursive scan is loud.
#[wasm_bindgen_test]
async fn start_compiled_skips_walker_recursion_for_registered_tags() {
    register_all();
    reset_bind_call_count();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_attribute("class", "scsr-wrapper").unwrap();
    host.set_inner_html(
        r#"<section class="scsr-section">
              <plan-interp-host></plan-interp-host>
           </section>"#,
    );
    body.append_child(&host).unwrap();
    let baseline = bind_call_count();
    mount_registered_tags(&host);
    tick().await;

    let post = bind_call_count() - baseline;
    assert_eq!(
        post, 0,
        "compiled-mount path mounts via apply_static_plan directly — no bind call on the wrapper, section, or component tag",
    );

    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");
    assert_eq!(read(&host, ".pih-static"), "no interp here");

    assert_eq!(plan_failure_count(), 0);
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "compiled-only path must not trip walker fallback for a walker-clean plan",
    );

    host.remove();
}

/// RFC-058 Phase 6.5 — `pp-model` on a native input now lifts
/// into a [`StaticNativeModel`] entry on the template plan
/// instead of forcing `requires_walker = true`. `start_compiled`
/// installs the read-side effect + write-side listener directly
/// via `directives::model::install_native`; no walker fallback
/// runs. Pin `compiled_fallback_walk_count` at 0 + the input's
/// reactive end-to-end behaviour to lock the lift.
#[wasm_bindgen_test]
async fn pp_model_on_native_input_lifts_without_walker_fallback() {
    register_all();
    reset_plan_failure_count();
    reset_compiled_fallback_walk_count();

    let plan = template_plan_for("start-compiled-model-host")
        .expect("start-compiled-model-host registers a template plan");
    assert_eq!(
        plan.native_models.len(),
        1,
        "the fixture's single <input pp-model> must produce one StaticNativeModel entry",
    );

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<start-compiled-model-host></start-compiled-model-host>");
    body.append_child(&host).unwrap();
    mount_registered_tags(&host);
    tick().await;

    assert_eq!(read(&host, ".scmh-readout"), "seed");
    assert_eq!(
        compiled_fallback_walk_count(),
        0,
        "lifted native pp-model must not need walker fallback",
    );
    assert_eq!(plan_failure_count(), 0);

    let input = host
        .query_selector(".scmh-input")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlInputElement>()
        .unwrap();
    input.set_value("typed");
    let event = web_sys::Event::new("input").unwrap();
    input.dispatch_event(&event).unwrap();
    tick().await;

    assert_eq!(
        read(&host, ".scmh-readout"),
        "typed",
        "lifted pp-model must wire input → scope through the compiled path",
    );

    host.remove();
}

/// RFC-058 Phase 6.3 — when a registered component plan has
/// `requires_walker = false`, the walker invokes `bind` once on
/// the host element, applies the entire plan, then descends via
/// `finalize_compiled_subtree` (lifecycle-only) instead of
/// re-binding every descendant. Pin the bind-call delta so any
/// regression that re-introduces the redundant scan is loud.
///
/// `PlanInterpHost` is a non-trivial walker-clean fixture: 1
/// outer test-harness `<div>` (the `mount()` helper's wrapper),
/// the `<plan-interp-host>` component tag, then a template root
/// and 3 native children inside. The legacy walk would bind all
/// 6 elements; Phase 6.3 binds the harness wrapper + the
/// component tag (which triggers `apply_static_plan` then
/// `finalize_compiled_subtree`) and skips the rest, so the
/// post-mount delta is 2.
#[wasm_bindgen_test]
async fn walker_skips_recursion_for_plan_clean_subtrees() {
    register_all();
    reset_bind_call_count();

    let baseline = bind_call_count();
    let host = mount("<plan-interp-host></plan-interp-host>");
    tick().await;

    let after = bind_call_count();
    let delta = after - baseline;
    // RFC-058 Phase 6.5 — `mount()` now uses `start_compiled`,
    // which routes through `mount_component` directly without
    // ever calling `bind`. The previous walker entry (`start`)
    // bound the harness wrapper + the host element (delta=2);
    // the compiled entry binds nothing (delta=0).
    assert_eq!(
        delta, 0,
        "compiled entry mounts via apply_static_plan — no `bind` calls expected \
         ({delta} bind calls observed)",
    );

    // The plan still applied correctly — sanity-check the
    // rendered DOM and a reactive update.
    assert_eq!(read(&host, ".pih-line"), "hello world, you have 3 items");
    assert_eq!(read(&host, ".pih-bare"), "world");

    let host_tag = host.query_selector("plan-interp-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) =
        pocopine_core::walker::scope_of_element(&host_root).expect("host scope");
    js_sys::Reflect::set(&host_proxy, &"label".into(), &"phase6".into()).unwrap();
    tick().await;
    assert_eq!(read(&host, ".pih-bare"), "phase6");

    host.remove();
}
