//! RFC 061 follow-up — focused coverage for the reduced
//! light-DOM slot replay helper that survived walker removal.
//!
//! These tests do not exercise raw adopted-DOM discovery. They
//! mount typed components through `App::mount_subtree::<C>` and
//! assert only the local slot-capture/replay behavior.

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

thread_local! {
    static GHOST_LIGHT_DOM_SETUP_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static GHOST_LIGHT_DOM_OUTER_SETUP_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-child",
    template_inline = r#"<span class="lds-child" data-mounted="yes">child mounted</span>"#
)]
struct LdsChild {}

#[handlers]
impl LdsChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-ghost-inner",
    template_inline = r#"<span class="lds-ghost-inner">inner</span>"#
)]
struct LdsGhostInner {}

#[handlers]
impl LdsGhostInner {
    pub fn on_setup(&mut self) {
        GHOST_LIGHT_DOM_SETUP_COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-ghost-outer",
    template_inline = r#"<span class="lds-ghost-outer">outer</span>"#
)]
struct LdsGhostOuter {}

#[handlers]
impl LdsGhostOuter {
    pub fn on_setup(&mut self) {
        GHOST_LIGHT_DOM_OUTER_SETUP_COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-ghost-host",
    template_inline = r#"<section class="lds-ghost-host"><slot></slot></section>"#,
    uses = [LdsGhostInner, LdsGhostOuter],
)]
struct LdsGhostHost {}

#[handlers]
impl LdsGhostHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-default-host",
    template_inline = r#"
<section class="lds-default-host">
  <slot></slot>
</section>
"#,
    uses = [LdsChild],
)]
struct LdsDefaultHost {}

#[handlers]
impl LdsDefaultHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-named-host",
    template_inline = r#"
<section class="lds-named-host">
  <header><slot name="title">fallback title</slot></header>
  <main><slot></slot></main>
</section>
"#
)]
struct LdsNamedHost {}

#[handlers]
impl LdsNamedHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-double-host",
    template_inline = r#"
<section class="lds-double-host">
  <div class="lds-first"><slot></slot></div>
  <div class="lds-second"><slot></slot></div>
</section>
"#,
    uses = [LdsChild],
)]
struct LdsDoubleHost {}

#[handlers]
impl LdsDoubleHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-if-slot-host",
    template_inline = r#"
<section class="lds-if-slot-host">
  <button class="lds-toggle" @click="toggle">toggle</button>
  <template pp-if="open">
    <slot></slot>
  </template>
</section>
"#,
    uses = [LdsChild],
)]
struct LdsIfSlotHost {
    open: bool,
}

#[handlers]
impl LdsIfSlotHost {
    pub fn on_setup(&mut self) {
        self.open = true;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "lds-captured-scoped-host",
    template_inline = r#"
<section class="lds-captured-scoped-host">
  <slot name="row" :label="label"></slot>
</section>
"#
)]
struct LdsCapturedScopedHost {
    label: String,
}

#[handlers]
impl LdsCapturedScopedHost {
    pub fn on_setup(&mut self) {
        self.label = "published".to_string();
    }
}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount_with_light_dom<C: pocopine::__private::Component>(
    light_dom: &str,
) -> (Element, Element, pocopine::SubtreeHandle) {
    let host = doc().create_element("div").unwrap();
    let root = doc().create_element(C::NAME).unwrap();
    root.set_inner_html(light_dom);
    host.append_child(&root).unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let handle = pocopine::App::mount_subtree::<C>(&root);
    (host, root, handle)
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

#[wasm_bindgen_test]
async fn default_light_dom_slot_mounts_custom_child() {
    let (host, _root, handle) = mount_with_light_dom::<LdsDefaultHost>("<lds-child></lds-child>");
    tick().await;

    let child = host
        .query_selector(".lds-child[data-mounted=\"yes\"]")
        .unwrap()
        .expect("custom child in captured default slot mounted");
    assert_eq!(child.text_content().as_deref(), Some("child mounted"));

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn captured_light_dom_discovery_skips_detached_nested_candidates() {
    GHOST_LIGHT_DOM_SETUP_COUNT.with(|count| count.set(0));
    GHOST_LIGHT_DOM_OUTER_SETUP_COUNT.with(|count| count.set(0));
    let (host, _root, handle) = mount_with_light_dom::<LdsGhostHost>(
        "<div><lds-ghost-outer><lds-ghost-inner></lds-ghost-inner></lds-ghost-outer></div>",
    );
    tick().await;

    assert_eq!(
        GHOST_LIGHT_DOM_OUTER_SETUP_COUNT.with(std::cell::Cell::get),
        1,
        "captured outer candidate must mount before its stale child is skipped"
    );
    assert!(
        host.query_selector(".lds-ghost-outer").unwrap().is_some(),
        "captured outer replacement DOM must be present"
    );
    assert_eq!(
        GHOST_LIGHT_DOM_SETUP_COUNT.with(std::cell::Cell::get),
        0,
        "mounting the outer captured candidate detached its child, so child setup must not run"
    );
    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn named_and_default_light_dom_slots_replay_to_matching_outlets() {
    let (host, _root, handle) = mount_with_light_dom::<LdsNamedHost>(
        r#"
        <template pp-slot="title"><span class="lds-title">Title</span></template>
        <p class="lds-default">Body</p>
        "#,
    );
    tick().await;

    assert_eq!(
        host.query_selector(".lds-title")
            .unwrap()
            .and_then(|el| el.text_content())
            .as_deref(),
        Some("Title"),
    );
    assert_eq!(
        host.query_selector(".lds-default")
            .unwrap()
            .and_then(|el| el.text_content())
            .as_deref(),
        Some("Body"),
    );
    assert!(
        !host
            .text_content()
            .unwrap_or_default()
            .contains("fallback title"),
        "named light-DOM slot should replace the compiled fallback",
    );

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn captured_light_dom_slot_is_cloned_for_each_matching_outlet() {
    let (host, _root, handle) = mount_with_light_dom::<LdsDoubleHost>("<lds-child></lds-child>");
    tick().await;

    let children = host
        .query_selector_all(".lds-child[data-mounted=\"yes\"]")
        .unwrap();
    assert_eq!(
        children.length(),
        2,
        "captured default slot fragment should be cloned for both outlets",
    );
    assert!(
        host.query_selector(".lds-first .lds-child")
            .unwrap()
            .is_some(),
        "first default outlet received a mounted child",
    );
    assert!(
        host.query_selector(".lds-second .lds-child")
            .unwrap()
            .is_some(),
        "second default outlet received a mounted child",
    );

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn captured_light_dom_slot_inside_pp_if_replays_initially_and_after_reopen() {
    pocopine::__private::reset_plan_failure_count();
    let (host, _root, handle) = mount_with_light_dom::<LdsIfSlotHost>("<lds-child></lds-child>");
    tick().await;

    assert!(
        host.query_selector(".lds-child[data-mounted=\"yes\"]")
            .unwrap()
            .is_some(),
        "captured default slot inside an initially true pp-if body should materialize",
    );

    let toggle = host.query_selector(".lds-toggle").unwrap().unwrap();
    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert!(
        host.query_selector(".lds-child").unwrap().is_none(),
        "captured default slot should unmount when pp-if closes",
    );

    toggle.dyn_ref::<HtmlElement>().unwrap().click();
    tick().await;
    assert!(
        host.query_selector(".lds-child[data-mounted=\"yes\"]")
            .unwrap()
            .is_some(),
        "captured default slot should replay when pp-if reopens",
    );
    assert_eq!(
        pocopine::__private::plan_failure_count(),
        0,
        "root slot in a lifted pp-if body must resolve through the body plan",
    );

    handle.unmount();
    host.remove();
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[wasm_bindgen_test]
async fn captured_scoped_slot_releases_its_slot_scope_across_multiple_roots() {
    let scopes_before = pocopine_core::scope::Scope::count();
    let (host, _root, handle) = mount_with_light_dom::<LdsCapturedScopedHost>(
        r#"<template pp-slot="row" pp-let="ctx">
            leading text
            <span class="lds-captured-scoped-first">first</span>
            <b class="lds-captured-scoped-second">second</b>
            trailing text
        </template>"#,
    );
    tick().await;

    assert!(
        host.query_selector(".lds-captured-scoped-first")
            .unwrap()
            .is_some()
    );
    assert!(
        host.query_selector(".lds-captured-scoped-second")
            .unwrap()
            .is_some()
    );
    assert!(
        pocopine_core::scope::Scope::count() > scopes_before,
        "component and captured SlotScope must both be live before teardown"
    );

    handle.unmount();

    assert_eq!(
        pocopine_core::scope::Scope::count(),
        scopes_before,
        "captured scoped-slot teardown must release text and element roots plus SlotScope"
    );
    host.remove();
}
