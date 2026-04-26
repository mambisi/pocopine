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
use pocopine_core::walker::{compiled_fallback_walk_count, reset_compiled_fallback_walk_count};
use serde::{Deserialize, Serialize};
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
}

fn mount(host_html: &str) -> Element {
    register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    pocopine_core::walker::start(&host);
    host
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

    pocopine_core::walker::start(&host);
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
