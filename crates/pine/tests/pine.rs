//! Pine browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine`.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

// ─── helpers ──────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount(host_html: &str) -> Element {
    pine::register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    pocopine_core::start(&host);
    host
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

// ─── PineButton ───────────────────────────────────────────────────

/// Variants and size props flow onto `data-*` attributes; the
/// native `disabled` attribute lands on the inner `<button>`.
#[wasm_bindgen_test]
async fn button_renders_data_attrs_and_disabled() {
    let host = mount(
        "<pine-button variant=\"primary\" size=\"sm\" disabled=\"true\">Save</pine-button>",
    );
    tick().await;

    let btn = host.query_selector("button.pine-btn").unwrap().unwrap();
    assert_eq!(
        btn.get_attribute("data-variant").as_deref(),
        Some("primary"),
        "variant → data-variant"
    );
    assert_eq!(btn.get_attribute("data-size").as_deref(), Some("sm"));
    assert!(
        btn.has_attribute("data-disabled"),
        "boolean disabled prop renders as data-disabled present-or-absent"
    );
    assert!(
        btn.has_attribute("disabled"),
        "disabled prop writes the native disabled attribute"
    );
    assert_eq!(
        btn.get_attribute("type").as_deref(),
        Some("button"),
        "default button type is 'button' (safe for non-form usage)"
    );

    host.remove();
}

/// `pp-as` on the component tag replaces the template's `<button>`
/// with the author's single child element — merging class attrs.
#[wasm_bindgen_test]
async fn button_pp_as_hoists_author_element() {
    let host = mount(
        "<pine-button pp-as variant=\"ghost\"><a href=\"#\" class=\"mine\">Docs</a></pine-button>",
    );
    tick().await;

    let tag = host.query_selector("pine-button").unwrap().unwrap();
    let children = tag.children();
    assert_eq!(children.length(), 1, "tag has exactly one child");
    let root = children.item(0).unwrap();
    assert_eq!(root.local_name(), "a", "hoisted to <a>");

    let cls = root.get_attribute("class").unwrap_or_default();
    assert!(cls.split_whitespace().any(|c| c == "mine"));
    assert!(
        cls.split_whitespace().any(|c| c == "pine-btn"),
        "template class merged onto <a>"
    );
    assert_eq!(root.get_attribute("data-variant").as_deref(), Some("ghost"));

    host.remove();
}

/// Clicks on the inner `<button>` bubble up through the
/// `<pine-button>` custom element tag — so `@click` (or any
/// directly-attached listener) on the tag catches them. This is
/// what lets authors write `<pine-button @click="save">` without
/// any prop-drilling.
#[wasm_bindgen_test]
async fn button_clicks_bubble_through_pine_button_tag() {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    let host = mount("<pine-button>Hit me</pine-button>");
    tick().await;

    let tag = host.query_selector("pine-button").unwrap().unwrap();
    let inner = host.query_selector("button.pine-btn").unwrap().unwrap();

    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    let cb: Closure<dyn FnMut(web_sys::Event)> =
        Closure::wrap(Box::new(move |_| f.set(f.get() + 1)));
    let target: &web_sys::EventTarget = tag.as_ref();
    target
        .add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .unwrap();

    inner.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(
        fired.get(),
        1,
        "click on inner <button> bubbled to the <pine-button> tag"
    );

    host.remove();
}
