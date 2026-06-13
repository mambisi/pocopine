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
