//! RFC 070 — `#[derive(Props)]` plus component-side `#[prop(flatten)]`.

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Clone, Default, Serialize, Deserialize, pocopine::Props)]
struct AxisProps {
    #[prop]
    x_label: String,

    #[prop]
    y_label: String,

    hidden: String,
}

#[derive(Clone, Default, Serialize, Deserialize, pocopine::Props)]
struct SizeProps {
    #[prop]
    width: f64,

    #[prop]
    height: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "props-flatten-host",
    template_inline = r#"<div class="props-flatten-host"></div>"#
)]
struct PropsFlattenHost {
    #[prop]
    label: String,

    #[prop(flatten)]
    axis: AxisProps,

    #[prop(flatten)]
    size: SizeProps,
}

#[handlers]
impl PropsFlattenHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "props-flatten-narrow-host",
    template_inline = r#"<div class="props-flatten-narrow-host"></div>"#
)]
struct PropsFlattenNarrowHost {
    #[prop(flatten = ["x_label"])]
    axis: AxisProps,
}

#[handlers]
impl PropsFlattenNarrowHost {}

#[wasm_bindgen_test]
fn derive_props_exposes_only_marked_fields() {
    assert_eq!(AxisProps::KEYS, &["x_label", "y_label"]);

    let mut props = AxisProps::default();
    props.set_prop("x_label", JsValue::from_str("Week"));
    props.set_prop("hidden", JsValue::from_str("internal"));

    assert_eq!(props.x_label, "Week");
    assert_eq!(props.hidden, "");
    assert_eq!(
        props.get_prop("x_label").as_string().as_deref(),
        Some("Week")
    );
    assert!(props.get_prop("hidden").is_undefined());
}

#[wasm_bindgen_test]
fn component_flatten_imports_props_as_prop_only_leaves() {
    let mut host = PropsFlattenHost::default();

    assert!(host.is_prop("label"));
    assert!(host.is_prop("x_label"));
    assert!(host.is_prop("y_label"));
    assert!(host.is_prop("width"));
    assert!(host.is_prop("height"));
    assert!(!host.is_prop("axis"));
    assert!(!host.is_prop("hidden"));
    assert!(!host.is_model("x_label"));

    host.set("x_label", JsValue::from_str("Week"));
    host.set("y_label", JsValue::from_str("Revenue"));
    host.set("width", JsValue::from_f64(720.0));
    host.set("height", JsValue::from_f64(360.0));

    assert_eq!(host.axis.x_label, "Week");
    assert_eq!(host.axis.y_label, "Revenue");
    assert_eq!(host.size.width, 720.0);
    assert_eq!(host.size.height, 360.0);
    assert_eq!(host.get("x_label").as_string().as_deref(), Some("Week"));
    assert_eq!(host.get("width").as_f64(), Some(720.0));

    let keys = host.keys();
    assert!(keys.contains(&"axis"));
    assert!(keys.contains(&"x_label"));
    assert!(keys.contains(&"width"));
    assert!(!keys.contains(&"hidden"));
}

#[wasm_bindgen_test]
fn explicit_flatten_include_list_narrows_imported_props() {
    let mut host = PropsFlattenNarrowHost::default();

    assert!(host.is_prop("x_label"));
    assert!(!host.is_prop("y_label"));

    host.set("x_label", JsValue::from_str("Week"));
    host.set("y_label", JsValue::from_str("Revenue"));

    assert_eq!(host.axis.x_label, "Week");
    assert_eq!(host.axis.y_label, "");
    assert_eq!(host.get("x_label").as_string().as_deref(), Some("Week"));
    assert!(host.get("y_label").is_undefined());
}
