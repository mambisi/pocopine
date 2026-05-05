//! RFC 076 — app plugin lifecycle.
//!
//! Pins the app-level extension surface used by optional crates:
//! plugins can be installed through the fluent `App` builder and
//! through the compiled `app!{}` macro without patching core startup.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Promise;
use pocopine::prelude::*;
use pocopine::App;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, HtmlElement};

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

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-observed-home",
    template_inline = r#"<div class="app-plugin-observed-home">observed</div>"#
)]
struct AppPluginObservedHome {}

#[handlers]
impl AppPluginObservedHome {
    pub fn on_ready(
        &self,
        analytics: Plugin<TestAnalytics>,
        missing: Option<Plugin<MissingPlugin>>,
    ) {
        assert!(
            missing.is_none(),
            "Option<Plugin<T>> should be None when the plugin is not installed"
        );
        analytics.record("ready");
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-hook-target",
    template_inline = r#"<div class="app-plugin-hook-target">target</div>"#
)]
struct AppPluginHookTarget {}

#[handlers]
impl AppPluginHookTarget {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-hook-other",
    template_inline = r#"<div class="app-plugin-hook-other">other</div>"#
)]
struct AppPluginHookOther {}

#[handlers]
impl AppPluginHookOther {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-cta-button",
    template_inline = r#"<button class="app-plugin-cta" @click="track_click">CTA</button>"#
)]
struct AppPluginCtaButton {}

#[handlers]
impl AppPluginCtaButton {
    pub fn on_ready(&self, cta: Option<Plugin<CtaTracking>>) {
        if let Some(cta) = cta {
            cta.impression("cta");
        }
    }

    pub fn track_click(&self) {
        if let Some(cta) = optional_plugin::<CtaTracking>() {
            cta.click("cta");
        }
    }
}

struct MissingPlugin;

#[derive(Clone)]
struct TestAnalytics {
    events: Rc<RefCell<Vec<String>>>,
}

impl TestAnalytics {
    fn new(events: Rc<RefCell<Vec<String>>>) -> Self {
        Self { events }
    }

    fn record(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }
}

impl Hook<ComponentMounted> for TestAnalytics {
    fn call(&self, event: ComponentMounted) {
        assert!(
            event.duration_ms >= 0.0,
            "component mount duration should be non-negative"
        );
        self.record(format!("mounted:{}", event.component));
    }
}

impl Hook<ComponentUnmounted> for TestAnalytics {
    fn call(&self, event: ComponentUnmounted) {
        self.record(format!("unmounted:{}", event.component));
    }
}

#[derive(Clone)]
struct HookRecorder {
    events: Rc<RefCell<Vec<String>>>,
}

impl HookRecorder {
    fn new(events: Rc<RefCell<Vec<String>>>) -> Self {
        Self { events }
    }

    fn record(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }
}

impl Hook<ComponentSetup> for HookRecorder {
    fn call(&self, event: ComponentSetup) {
        self.record(format!("setup:{}", event.component));
    }
}

impl Hook<ComponentReady> for HookRecorder {
    fn call(&self, event: ComponentReady) {
        self.record(format!("ready:{}", event.component));
    }
}

impl Hook<ForComponent<AppPluginHookTarget, ComponentMounted>> for HookRecorder {
    fn call(&self, event: ForComponent<AppPluginHookTarget, ComponentMounted>) {
        self.record(format!("target-mounted:{}", event.component));
    }
}

impl Hook<ForComponent<AppPluginHookTarget, ComponentReady>> for HookRecorder {
    fn call(&self, event: ForComponent<AppPluginHookTarget, ComponentReady>) {
        self.record(format!("target-ready:{}", event.component));
    }
}

#[derive(Clone)]
struct CtaTracking {
    events: Rc<RefCell<Vec<String>>>,
}

impl CtaTracking {
    fn new(events: Rc<RefCell<Vec<String>>>) -> Self {
        Self { events }
    }

    fn impression(&self, id: &str) {
        self.events.borrow_mut().push(format!("impression:{id}"));
    }

    fn click(&self, id: &str) {
        self.events.borrow_mut().push(format!("click:{id}"));
    }
}

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

fn analytics_plugin(events: Rc<RefCell<Vec<String>>>) -> impl AppPlugin {
    move |app: App| {
        app.provide_plugin(TestAnalytics::new(events))
            .hook_plugin::<TestAnalytics, ComponentMounted>()
            .hook_plugin::<TestAnalytics, ComponentUnmounted>()
    }
}

fn component_hook_plugin(events: Rc<RefCell<Vec<String>>>) -> impl AppPlugin {
    move |app: App| {
        app.provide_plugin(HookRecorder::new(events))
            .hook_plugin::<HookRecorder, ComponentSetup>()
            .hook_plugin::<HookRecorder, ComponentReady>()
            .hook_component_plugin::<HookRecorder, AppPluginHookTarget, ComponentMounted>()
            .hook_component_plugin::<HookRecorder, AppPluginHookTarget, ComponentReady>()
    }
}

fn cta_plugin(events: Rc<RefCell<Vec<String>>>) -> impl AppPlugin {
    move |app: App| app.provide_plugin(CtaTracking::new(events))
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
async fn plugin_extractor_and_framework_hooks_use_provided_service() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let app_root = app_host("");

    App::new().plugin(analytics_plugin(events.clone())).run();

    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let handle = App::mount_subtree::<AppPluginObservedHome>(&host);
    next_microtask().await;
    handle.unmount();

    assert_eq!(
        events.borrow().as_slice(),
        &[
            "mounted:app-plugin-observed-home".to_string(),
            "ready".to_string(),
            "unmounted:app-plugin-observed-home".to_string(),
        ]
    );

    host.remove();
    app_root.remove();
}

#[wasm_bindgen_test(async)]
async fn component_hooks_can_be_global_or_filtered_by_component_type() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let app_root = app_host("");

    App::new()
        .plugin(component_hook_plugin(events.clone()))
        .run();

    let target_host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&target_host).unwrap();
    let target = App::mount_subtree::<AppPluginHookTarget>(&target_host);

    let other_host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&other_host).unwrap();
    let other = App::mount_subtree::<AppPluginHookOther>(&other_host);

    next_microtask().await;
    target.unmount();
    other.unmount();

    assert_eq!(
        events.borrow().as_slice(),
        &[
            "setup:app-plugin-hook-target".to_string(),
            "target-mounted:app-plugin-hook-target".to_string(),
            "setup:app-plugin-hook-other".to_string(),
            "ready:app-plugin-hook-target".to_string(),
            "target-ready:app-plugin-hook-target".to_string(),
            "ready:app-plugin-hook-other".to_string(),
        ]
    );

    target_host.remove();
    other_host.remove();
    app_root.remove();
}

#[wasm_bindgen_test(async)]
async fn reusable_components_can_opt_into_plugin_capabilities() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let app_root = app_host("");

    App::new().plugin(cta_plugin(events.clone())).run();

    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let handle = App::mount_subtree::<AppPluginCtaButton>(&host);
    next_microtask().await;

    let button = host
        .query_selector(".app-plugin-cta")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    button.click();

    assert_eq!(
        events.borrow().as_slice(),
        &["impression:cta".to_string(), "click:cta".to_string(),]
    );

    handle.unmount();
    host.remove();
    app_root.remove();
}

#[wasm_bindgen_test]
#[should_panic(expected = "is not installed")]
fn required_plugin_extractor_panics_when_missing() {
    let el = doc().create_element("div").unwrap();
    let ctx = pocopine::LifecycleContext::__new(&el, ScopeId(0), pocopine::LifecyclePhase::Ready);
    let _: Plugin<MissingPlugin> = ctx.into();
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
