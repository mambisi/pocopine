//! Router mutations invalidate cached path/parameter/query projections.
#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(name = "route-state-page", template = poco! {
    <section>
        <p id="route-state-path" pp-text="$route.path"></p>
        <p id="route-state-param" pp-text="$route.params.id"></p>
        <p id="route-state-query" pp-text="$route.query.term"></p>
    </section>
})]
struct RouteStatePage {}

#[handlers]
impl RouteStatePage {}

#[wasm_bindgen_test]
fn navigation_invalidates_all_cached_route_fields() {
    let window = web_sys::window().unwrap();
    let document = window.document().unwrap();
    window
        .history()
        .unwrap()
        .replace_state_with_url(&JsValue::NULL, "", Some("/route-state/one?term=first"))
        .unwrap();
    let host = document.create_element("main").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<pp-outlet></pp-outlet>");
    document.body().unwrap().append_child(&host).unwrap();
    App::new().route::<RouteStatePage>("/route-state/:id").run();
    let read = |id| {
        document
            .get_element_by_id(id)
            .unwrap()
            .text_content()
            .unwrap()
    };
    assert_eq!(read("route-state-path"), "/route-state/one");
    assert_eq!(read("route-state-param"), "one");
    assert_eq!(read("route-state-query"), "first");
    pocopine::router::push("/route-state/two?term=second").unwrap();
    pocopine::flush_sync();
    assert_eq!(read("route-state-path"), "/route-state/two");
    assert_eq!(read("route-state-param"), "two");
    assert_eq!(read("route-state-query"), "second");
}
