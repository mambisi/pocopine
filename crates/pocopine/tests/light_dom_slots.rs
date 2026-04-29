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
use web_sys::{window, Element};

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
