//! RFC-099 Phase 2c — the SSR ↔ hydration differential harness, the
//! end-to-end gate that ties the whole pipeline together:
//!
//!   1. **Server**: `pocopine_ssr::render_to_string(&component)` stamps
//!      the component's plan over its cleaned HTML, host-side.
//!   2. **Client**: drop that HTML into the DOM and
//!      `hydrate::hydrate_subtree` — instantiate the scope, load the
//!      server state, and run the claim walk over the existing nodes.
//!   3. **Assert**: `outerHTML` is byte-equal *before and after*
//!      hydration. That single check proves two things at once — hydrate
//!      is non-mutating, AND the client re-evaluated every binding to the
//!      exact value the server rendered (a parity mismatch would change
//!      the DOM). Then mutate a field and confirm the binding is live.
//!
//! Run with `wasm-pack test --firefox --headless crates/pocopine --test
//! ssr_hydration`.

#![cfg(target_arch = "wasm32")]

use pocopine::flush_sync;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-demo",
    template_inline = r#"<div class="card">
        <span class="t" pp-text="title"></span>
        <b :data-n="count"></b>
        <em pp-show="hidden">x</em>
        <i>{{label}}!</i>
    </div>"#
)]
struct SsrDemo {
    title: String,
    count: f64,
    hidden: bool,
    label: String,
}

#[handlers]
impl SsrDemo {}

#[wasm_bindgen_test]
fn server_render_hydrates_byte_equal_and_stays_reactive() {
    SsrDemo::register();
    let demo = SsrDemo {
        title: "Hello".into(),
        count: 7.0,
        hidden: false, // pp-show truthy=visible? hidden=false → falsy → display:none
        label: "world".into(),
    };

    // 1. Server render.
    let page = pocopine_ssr::render_to_string(&demo).expect("component is registered");
    assert!(
        page.body.contains(">Hello<"),
        "server stamped title: {}",
        page.body
    );
    assert!(
        page.body.contains("data-n=\"7\""),
        "server stamped count: {}",
        page.body
    );
    assert!(
        page.body.contains("world!"),
        "server stamped interp: {}",
        page.body
    );

    // 2. Place the server HTML into the DOM.
    let doc = window().unwrap().document().unwrap();
    let container = doc.create_element("div").unwrap();
    container.set_inner_html(&page.body);
    let root: Element = container
        .first_element_child()
        .expect("server HTML has a root element");

    let before = root.outer_html();

    // 3. Hydrate the same state onto the existing DOM.
    let state = serde_json::to_value(&demo).unwrap();
    let scope_id = pocopine_core::hydrate::hydrate_subtree(&root, SsrDemo::NAME, &state)
        .expect("hydration scope");
    flush_sync();
    let after = root.outer_html();

    // The whole point: hydrate changed nothing — proving it is
    // non-mutating AND that the client re-evaluated every binding to the
    // server's value (a mismatch would have rewritten the DOM).
    assert_eq!(
        before, after,
        "hydration mutated the DOM (server↔client parity broken)"
    );

    // 4. Reactivity is live: mutate a field on the hydrated scope and
    // the bound attribute updates.
    pocopine_core::scope::write_field(scope_id, "count", &JsValue::from_f64(8.0));
    flush_sync();
    assert!(
        root.outer_html().contains("data-n=\"8\""),
        "binding not live after hydrate: {}",
        root.outer_html()
    );
}

// ─── RFC-099 Phase 3 — structural claim (pp-if) ─────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-cond-demo",
    template_inline = r#"<div class="root">
        <template pp-if="big"><p class="b" pp-text="label"></p></template>
        <template pp-else><span class="s" pp-text="label"></span></template>
    </div>"#
)]
struct SsrCondDemo {
    big: bool,
    label: String,
}

#[handlers]
impl SsrCondDemo {}

#[wasm_bindgen_test]
fn server_render_hydrates_pp_if_chain_byte_equal_and_stays_reactive() {
    SsrCondDemo::register();
    let demo = SsrCondDemo {
        big: true,
        label: "hi".into(),
    };

    // 1. Server render — the head branch is active, with its body's
    //    pp-text stamped + the labelled comment anchor.
    let page = pocopine_ssr::render_to_string(&demo).expect("registered");
    assert!(
        page.body.contains(r#"<p class="b">hi</p>"#),
        "server stamped active branch: {}",
        page.body
    );
    assert!(
        page.body.contains("<!--pp:cond:0-->"),
        "server emitted decision anchor: {}",
        page.body
    );
    assert!(
        !page.body.contains("class=\"s\""),
        "else branch absent: {}",
        page.body
    );

    // 2. Into the DOM.
    let doc = window().unwrap().document().unwrap();
    let container = doc.create_element("div").unwrap();
    container.set_inner_html(&page.body);
    let root: Element = container.first_element_child().expect("root");
    let before = root.outer_html();

    // 3. Hydrate — claim the chain: adopt the <p>, install its body
    //    pp-text, seed the controller. No DOM mutation.
    let state = serde_json::to_value(&demo).unwrap();
    let scope_id =
        pocopine_core::hydrate::hydrate_subtree(&root, SsrCondDemo::NAME, &state).expect("scope");
    flush_sync();
    assert_eq!(
        before,
        root.outer_html(),
        "pp-if hydration mutated the DOM (claim not byte-equal)"
    );

    // 4. Body binding is live after claim: change `label`, the adopted
    //    branch's pp-text updates in place (no remount).
    pocopine_core::scope::write_field(scope_id, "label", &JsValue::from_str("yo"));
    flush_sync();
    assert!(
        root.outer_html().contains(r#"<p class="b">yo</p>"#),
        "claimed branch body not reactive: {}",
        root.outer_html()
    );

    // 5. Structural reactivity: flip the condition → the controller
    //    swaps the head branch for the pp-else branch (created via its
    //    macro body fn, since the server dropped the member template).
    pocopine_core::scope::write_field(scope_id, "big", &JsValue::from_bool(false));
    flush_sync();
    let html = root.outer_html();
    assert!(
        html.contains(r#"<span class="s">yo</span>"#),
        "else branch did not mount after flip: {html}"
    );
    assert!(
        !html.contains("class=\"b\""),
        "head branch not removed: {html}"
    );
}

// ─── RFC-099 Phase 3 — structural claim (pp-match + pp-let) ──────────

#[derive(Serialize, Deserialize)]
enum Phase {
    Idle,
    Loading,
    Ready(String),
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Idle
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-match-demo",
    template_inline = r#"<div class="root">
        <template pp-match="phase">
            <template pp-case="Idle | Loading"><p class="pending">wait</p></template>
            <template pp-case="Ready" pp-let="msg"><p class="ready" pp-text="msg"></p></template>
            <template pp-case="_"><p class="other">other</p></template>
        </template>
    </div>"#
)]
struct SsrMatchDemo {
    phase: Phase,
}

#[handlers]
impl SsrMatchDemo {}

#[wasm_bindgen_test]
fn server_render_hydrates_pp_match_byte_equal_and_stays_reactive() {
    SsrMatchDemo::register();
    let demo = SsrMatchDemo {
        phase: Phase::Ready("hi".into()),
    };

    // 1. Server render — the `Ready` arm with its pp-let payload stamped.
    let page = pocopine_ssr::render_to_string(&demo).expect("registered");
    assert!(
        page.body.contains(r#"<p class="ready">hi</p>"#),
        "server stamped pp-let case: {}",
        page.body
    );
    assert!(
        page.body.contains("<!--pp:match:0-->"),
        "decision anchor: {}",
        page.body
    );

    // 2. Into the DOM.
    let doc = window().unwrap().document().unwrap();
    let container = doc.create_element("div").unwrap();
    container.set_inner_html(&page.body);
    let root: Element = container.first_element_child().expect("root");
    let before = root.outer_html();

    // 3. Hydrate — claim the case, rebuild the payload scope, install the
    //    body. No DOM mutation.
    let state = serde_json::to_value(&demo).unwrap();
    let scope_id =
        pocopine_core::hydrate::hydrate_subtree(&root, SsrMatchDemo::NAME, &state).expect("scope");
    flush_sync();
    assert_eq!(
        before,
        root.outer_html(),
        "pp-match hydration mutated the DOM (claim not byte-equal)"
    );

    // 4. Structural reactivity: a tag change makes the controller swap
    //    to the matching arm (created via its macro body fn).
    pocopine_core::scope::write_field(scope_id, "phase", &JsValue::from_str("Loading"));
    flush_sync();
    let html = root.outer_html();
    assert!(
        html.contains(r#"<p class="pending">wait</p>"#),
        "arm did not swap after tag change: {html}"
    );
    assert!(
        !html.contains("class=\"ready\""),
        "old arm not removed: {html}"
    );

    // 5. Flip back to a payload arm → the body re-mounts with the new
    //    payload (fresh PayloadScope), proving the claimed controller
    //    drives normal mounts after hydrate.
    let ready_yo = js_sys::Object::new();
    js_sys::Reflect::set(&ready_yo, &"Ready".into(), &"yo".into()).unwrap();
    pocopine_core::scope::write_field(scope_id, "phase", &ready_yo);
    flush_sync();
    assert!(
        root.outer_html().contains(r#"<p class="ready">yo</p>"#),
        "payload arm did not re-mount after flip: {}",
        root.outer_html()
    );
}

// ─── RFC-099 Phase 3 — structural claim (unkeyed pp-for) ────────────

#[derive(Clone, Default, Serialize, Deserialize)]
struct ForItem {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-for-demo",
    template_inline = r#"<ul class="root">
        <template pp-for="item in items"><li class="row" pp-text="item.label"></li></template>
    </ul>"#
)]
struct SsrForDemo {
    items: Vec<ForItem>,
}

#[handlers]
impl SsrForDemo {}

fn for_item(arr: &js_sys::Array, id: u32, label: &str) {
    let o = js_sys::Object::new();
    js_sys::Reflect::set(&o, &"id".into(), &JsValue::from_f64(id as f64)).unwrap();
    js_sys::Reflect::set(&o, &"label".into(), &label.into()).unwrap();
    arr.push(&o);
}

#[wasm_bindgen_test]
fn server_render_hydrates_pp_for_byte_equal_and_stays_reactive() {
    SsrForDemo::register();
    let demo = SsrForDemo {
        items: vec![
            ForItem {
                id: 1,
                label: "alpha".into(),
            },
            ForItem {
                id: 2,
                label: "beta".into(),
            },
        ],
    };

    // 1. Server render — one row clone per item + the labelled anchor.
    let page = pocopine_ssr::render_to_string(&demo).expect("registered");
    assert!(
        page.body.contains(r#"<li class="row">alpha</li>"#)
            && page.body.contains(r#"<li class="row">beta</li>"#),
        "server stamped rows: {}",
        page.body
    );
    assert!(
        page.body.contains("<!--pp:for:0-->"),
        "anchor: {}",
        page.body
    );

    // 2. Into the DOM.
    let doc = window().unwrap().document().unwrap();
    let container = doc.create_element("div").unwrap();
    container.set_inner_html(&page.body);
    let root: Element = container.first_element_child().expect("root");
    let before = root.outer_html();

    // 3. Hydrate — adopt the rows (each in a fresh LoopScope), seed the
    //    controller. No DOM mutation.
    let state = serde_json::to_value(&demo).unwrap();
    let scope_id =
        pocopine_core::hydrate::hydrate_subtree(&root, SsrForDemo::NAME, &state).expect("scope");
    flush_sync();
    assert_eq!(
        before,
        root.outer_html(),
        "pp-for hydration mutated the DOM (claim not byte-equal)"
    );

    // 4. List reactivity: reassign `items` → the controller reconciles
    //    (drops the adopted rows, rebuilds from the new list).
    let next = js_sys::Array::new();
    for_item(&next, 10, "x");
    for_item(&next, 11, "y");
    for_item(&next, 12, "z");
    pocopine_core::scope::write_field(scope_id, "items", &next);
    flush_sync();
    let html = root.outer_html();
    assert!(
        html.contains(r#"<li class="row">x</li>"#)
            && html.contains(r#"<li class="row">y</li>"#)
            && html.contains(r#"<li class="row">z</li>"#),
        "rows did not reconcile after list change: {html}"
    );
    assert_eq!(
        html.matches(r#"class="row""#).count(),
        3,
        "exactly three rows after change: {html}"
    );
    assert!(
        !html.contains(">alpha<") && !html.contains(">beta<"),
        "old rows remain: {html}"
    );
}
