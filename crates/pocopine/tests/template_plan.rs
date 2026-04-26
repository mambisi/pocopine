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

/// Whole-subtree walker-owned by virtue of the `<template
/// pp-for>` boundary. Drives test #4 — the macro v1 prefers
/// row-plan stamping over template-plan compilation when both
/// would apply, so `template_plan_for("plan-for-loop")` stays
/// `None`.
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

/// v1 trade-off (RFC-058 §6.2 layering follow-up): when a
/// template carries `pp-for` row plans, the macro skips
/// template-plan compilation and the whole subtree stays
/// walker-owned. Confirm by mount-checking + asserting no
/// plan was registered.
#[wasm_bindgen_test]
async fn template_with_pp_for_skips_template_plan() {
    register_all();
    assert!(
        template_plan_for("plan-for-loop").is_none(),
        "templates carrying `pp-for` row plans skip template-plan compilation in v1",
    );

    let host = mount("<plan-for-loop></plan-for-loop>");
    tick().await;

    let li = host
        .query_selector("li")
        .unwrap()
        .expect("walker-driven pp-for should still mount the row");
    assert_eq!(li.text_content().as_deref(), Some("alpha"));

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
    pocopine_core::walker::mount_child_component_with_slots(&child, "plan-slot-child", slots);

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
