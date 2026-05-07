//! Static authored attributes must coerce according to the target prop type.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test static_props`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "static-prop-coercion-target",
    template_inline = r#"<div>
        <span class="font-weight" pp-text="font_weight"></span>
        <span class="optional-code" pp-text="optional_code"></span>
        <span class="count" pp-text="count"></span>
        <span class="enabled" pp-text="enabled"></span>
    </div>"#
)]
struct StaticPropCoercionTarget {
    #[prop]
    font_weight: String,
    #[prop]
    optional_code: Option<String>,
    #[prop]
    count: f64,
    #[prop]
    enabled: bool,
}

#[handlers]
impl StaticPropCoercionTarget {}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount(host_html: &str) -> Element {
    StaticPropCoercionTarget::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    let el = host
        .query_selector("static-prop-coercion-target")
        .unwrap()
        .unwrap();
    pocopine_core::mount::mount_child_component(&el, "static-prop-coercion-target");
    pocopine_core::mount::finalize_compiled_subtree(&el);
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

#[wasm_bindgen_test]
async fn static_numeric_looking_string_props_preserve_authored_text() {
    let host = mount(
        r#"<static-prop-coercion-target
            font-weight="700"
            optional-code="123"
            count="42"
            enabled="true"></static-prop-coercion-target>"#,
    );
    tick().await;

    assert_eq!(read(&host, ".font-weight"), "700");
    assert_eq!(read(&host, ".optional-code"), "123");
    assert_eq!(read(&host, ".count"), "42");
    assert_eq!(read(&host, ".enabled"), "true");

    host.remove();
}
