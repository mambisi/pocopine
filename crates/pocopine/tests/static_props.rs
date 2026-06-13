//! Static authored attributes must coerce according to the target prop type.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test static_props`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

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

// RFC-044 §5.10 — `#[prop(flatten)]` exposes a struct-typed field's
// leaves as per-leaf wire keys (inbound only, no `pp:update:`).
#[derive(Default, Clone, Serialize, Deserialize)]
struct FlattenLeaves {
    label: String,
    code: String,
    enabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "prop-flatten-target",
    template_inline = r#"<div>
        <span class="leaf-label" pp-text="label"></span>
        <span class="leaf-code" pp-text="code"></span>
        <span class="leaf-enabled" pp-text="enabled"></span>
    </div>"#
)]
struct PropFlattenTarget {
    #[prop(flatten = ["label", "code", "enabled"])]
    leaves: FlattenLeaves,
}

#[handlers]
impl PropFlattenTarget {}

#[wasm_bindgen_test]
async fn prop_flatten_leaves_round_trip_through_static_attrs() {
    PropFlattenTarget::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(
        r#"<prop-flatten-target
            label="Sales"
            code="2024"
            enabled="true"></prop-flatten-target>"#,
    );
    body.append_child(&host).unwrap();
    let el = host.query_selector("prop-flatten-target").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "prop-flatten-target");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    tick().await;

    // Each leaf reaches `self.leaves.<leaf>` and renders through its
    // bare wire name. `code="2024"` is a numeric-looking value on a
    // `String` leaf — the per-leaf `static_prop_kind` probe must
    // classify it `String` so it is not coerced to a number and
    // dropped by the leaf's serde round-trip.
    assert_eq!(read(&host, ".leaf-label"), "Sales");
    assert_eq!(read(&host, ".leaf-code"), "2024");
    assert_eq!(read(&host, ".leaf-enabled"), "true");

    // `#[prop(flatten)]` is inbound only — leaves are props, not
    // models, so they carry no `pp:update:` channel.
    let target = PropFlattenTarget::default();
    assert!(
        <PropFlattenTarget as pocopine::__private::ComponentState>::is_prop(&target, "label"),
        "flatten leaf must be a prop",
    );
    assert!(
        !<PropFlattenTarget as pocopine::__private::ComponentState>::is_model(&target, "label"),
        "#[prop(flatten)] leaf must not be a model",
    );

    host.remove();
}
