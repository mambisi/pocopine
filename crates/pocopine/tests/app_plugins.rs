//! RFC 076 — app plugin lifecycle.
//!
//! Pins the app-level extension surface used by optional crates:
//! plugins can be installed through the fluent `App` builder and
//! through the compiled `app!{}` macro without patching core startup.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use js_sys::Promise;
use pocopine::prelude::*;
use pocopine::App;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-direct-home",
    template_inline = r#"<div class="app-plugin-direct-home">direct</div>"#
)]
struct AppPluginDirectHome {}

#[handlers]
impl AppPluginDirectHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-macro-home",
    template_inline = r#"<div class="app-plugin-macro-home">macro</div>"#
)]
struct AppPluginMacroHome {}

#[handlers]
impl AppPluginMacroHome {}

fn push(event: &'static str) {
    EVENTS.with(|events| events.borrow_mut().push(event));
}

fn take_events() -> Vec<&'static str> {
    EVENTS.with(|events| events.take())
}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn app_host(inner_html: &str) -> web_sys::Element {
    let host = doc().create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html(inner_html);
    doc().body().unwrap().append_child(&host).unwrap();
    host
}

async fn next_microtask() {
    JsFuture::from(Promise::resolve(&JsValue::NULL))
        .await
        .unwrap();
}

fn direct_plugin(app: App) -> App {
    push("direct:install");
    assert!(
        app.registered_routes().contains(&"/direct"),
        "direct plugins run at the builder position selected by the app"
    );
    app.before_mount(|| push("direct:before"))
        .after_mount(|| push("direct:after"))
}

fn macro_plugin(app: App) -> App {
    push("macro:install");
    assert!(
        app.registered_components()
            .contains(&"app-plugin-macro-home"),
        "app! plugins should see the static component manifest"
    );
    assert!(
        app.registered_routes().contains(&"/macro"),
        "app! plugins should see generated route metadata"
    );
    app.before_mount(|| push("macro:before"))
        .after_mount(|| push("macro:after"))
}

#[wasm_bindgen_test(async)]
async fn app_builder_plugin_installs_lifecycle_hooks() {
    let _ = take_events();
    let host = app_host("<app-plugin-direct-home></app-plugin-direct-home>");

    App::new()
        .route::<AppPluginDirectHome>("/direct")
        .plugin(direct_plugin)
        .run();
    next_microtask().await;

    assert_eq!(
        take_events(),
        vec!["direct:install", "direct:before", "direct:after"]
    );
    host.remove();
}

#[wasm_bindgen_test(async)]
async fn app_macro_plugins_see_static_manifest_before_mount() {
    let _ = take_events();
    let host = app_host("<app-plugin-macro-home></app-plugin-macro-home>");

    pocopine::app! {
        components: [AppPluginMacroHome],
        plugins: [macro_plugin],
        routes: [
            ("/macro", AppPluginMacroHome),
        ],
    };
    next_microtask().await;

    assert_eq!(
        take_events(),
        vec!["macro:install", "macro:before", "macro:after"]
    );
    host.remove();
}
