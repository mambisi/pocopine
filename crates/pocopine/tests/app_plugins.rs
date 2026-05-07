//! RFC 076 — app plugin lifecycle.
//!
//! Pins the app-level extension surface used by optional crates:
//! plugins can be installed through the fluent `App` builder and
//! through the compiled `app!{}` macro without patching core startup.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Promise, Reflect};
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

impl RouteComponent for AppPluginDirectHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "app-plugin-macro-home",
    template_inline = r#"<div class="app-plugin-macro-home">macro</div>"#
)]
struct AppPluginMacroHome {}

#[handlers]
impl AppPluginMacroHome {}

impl RouteComponent for AppPluginMacroHome {}

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
        if let Some(cta) = self.plugins().get::<CtaTracking>() {
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

struct DuplicateService;

struct DuplicateProvider {
    name: &'static str,
}

impl AppPlugin for DuplicateProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(DuplicateService)
    }
}

struct MissingHookService;

impl Hook<ComponentMounted> for MissingHookService {
    fn call(&self, _: ComponentMounted) {}
}

struct MissingHookPlugin;

impl AppPlugin for MissingHookPlugin {
    fn name(&self) -> &'static str {
        "missing-hook-plugin"
    }

    fn install(self, app: App) -> App {
        app.hook_plugin::<MissingHookService, ComponentMounted>()
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
struct AppEventRecorder {
    events: Rc<RefCell<Vec<String>>>,
}

impl AppEventRecorder {
    fn new(events: Rc<RefCell<Vec<String>>>) -> Self {
        Self { events }
    }

    fn record(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }
}

impl Hook<AppBootStarted> for AppEventRecorder {
    fn call(&self, event: AppBootStarted) {
        self.record(format!(
            "boot-start:{}:{}",
            event.component_count, event.route_count
        ));
    }
}

impl Hook<AppBootCompleted> for AppEventRecorder {
    fn call(&self, event: AppBootCompleted) {
        assert!(
            event.duration_ms >= 0.0,
            "app boot duration should be non-negative"
        );
        self.record("boot-completed");
    }
}

impl Hook<AppBootFailed> for AppEventRecorder {
    fn call(&self, event: AppBootFailed) {
        self.record(format!("boot-failed:{}", event.reason));
    }
}

impl Hook<RouteNavigationStarted> for AppEventRecorder {
    fn call(&self, event: RouteNavigationStarted) {
        self.record(format!(
            "route-start:{}:{}:{}",
            event.path,
            event.route_pattern.unwrap_or_default(),
            event.component.unwrap_or_default(),
        ));
    }
}

impl Hook<RouteNavigationCompleted> for AppEventRecorder {
    fn call(&self, event: RouteNavigationCompleted) {
        assert!(
            event.duration_ms >= 0.0,
            "route navigation duration should be non-negative"
        );
        self.record(format!(
            "route-completed:{}:{}:{}",
            event.path,
            event.route_pattern.unwrap_or_default(),
            event.component.unwrap_or_default(),
        ));
    }
}

impl Hook<RouteNavigationFailed> for AppEventRecorder {
    fn call(&self, event: RouteNavigationFailed) {
        assert!(
            event.duration_ms >= 0.0,
            "failed route navigation duration should be non-negative"
        );
        self.record(format!("route-failed:{}:{}", event.path, event.reason));
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

fn current_path_and_search() -> String {
    let location = window().unwrap().location();
    format!(
        "{}{}",
        location.pathname().unwrap_or_else(|_| "/".to_string()),
        location.search().unwrap_or_default()
    )
}

fn replace_url(path: &str) {
    window()
        .unwrap()
        .history()
        .unwrap()
        .replace_state_with_url(&JsValue::NULL, "", Some(path))
        .unwrap();
}

fn remove_pp_app_roots() {
    let roots = doc().query_selector_all("[pp-app]").unwrap();
    for i in 0..roots.length() {
        if let Some(root) = roots.item(i) {
            if let Ok(el) = root.dyn_into::<web_sys::Element>() {
                el.remove();
            }
        }
    }
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

fn boot_event_plugin(events: Rc<RefCell<Vec<String>>>) -> impl AppPlugin {
    move |app: App| {
        app.provide_plugin(AppEventRecorder::new(events))
            .hook_plugin::<AppEventRecorder, AppBootStarted>()
            .hook_plugin::<AppEventRecorder, AppBootCompleted>()
            .hook_plugin::<AppEventRecorder, AppBootFailed>()
    }
}

fn route_event_plugin(events: Rc<RefCell<Vec<String>>>) -> impl AppPlugin {
    move |app: App| {
        app.provide_plugin(AppEventRecorder::new(events))
            .hook_plugin::<AppEventRecorder, RouteNavigationStarted>()
            .hook_plugin::<AppEventRecorder, RouteNavigationCompleted>()
            .hook_plugin::<AppEventRecorder, RouteNavigationFailed>()
    }
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
fn plugin_free_mount_does_not_stamp_plugin_metadata() {
    let app_root = app_host("");
    App::new().run();

    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let handle = App::mount_subtree::<AppPluginDirectHome>(&host);
    let root = host
        .first_element_child()
        .expect("mounted component should render a root");

    // Component names live in a thread-local side-table (keyed by
    // `ScopeId`) and are only populated when `needs_component_name`
    // is set; the timing stamp stays in the DOM and is the only
    // surface visible from outside the crate. Asserting its absence
    // is enough to prove the gated stamping path skips work.
    assert!(
        Reflect::get(root.as_ref(), &JsValue::from_str("__pp_mount_start_ms"))
            .unwrap()
            .is_undefined(),
        "plugin-free mounts should not stamp mount timing for plugin events"
    );

    handle.unmount();
    host.remove();
    app_root.remove();
}

#[wasm_bindgen_test]
#[should_panic(expected = "first provider: `first-plugin`, second provider: `second-plugin`")]
fn duplicate_plugin_services_name_their_providers() {
    let _ = App::new()
        .plugin(DuplicateProvider {
            name: "first-plugin",
        })
        .plugin(DuplicateProvider {
            name: "second-plugin",
        });
}

#[wasm_bindgen_test]
fn missing_hook_service_renders_plugin_boot_error() {
    App::new().plugin(MissingHookPlugin).run();

    let banner = doc()
        .query_selector("[data-pocopine-boot-error=\"plugin\"]")
        .unwrap()
        .expect("missing hook service should render plugin boot error");
    let text = banner.text_content().unwrap_or_default();
    assert!(
        text.contains("missing-hook-plugin"),
        "diagnostic should name the plugin that installed the invalid hook"
    );
    assert!(
        text.contains("MissingHookService"),
        "diagnostic should name the missing service type"
    );

    banner.remove();
}

#[wasm_bindgen_test]
fn app_boot_hooks_fire_on_success() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let host = app_host("");

    App::new().plugin(boot_event_plugin(events.clone())).run();

    assert_eq!(
        events.borrow().as_slice(),
        &["boot-start:0:0".to_string(), "boot-completed".to_string()]
    );
    host.remove();
}

#[wasm_bindgen_test]
fn app_boot_failed_hook_fires_for_missing_pp_app_root() {
    let events = Rc::new(RefCell::new(Vec::new()));
    remove_pp_app_roots();

    App::new().plugin(boot_event_plugin(events.clone())).run();

    assert_eq!(
        events.borrow().as_slice(),
        &[
            "boot-start:0:0".to_string(),
            "boot-failed:missing_pp_app_root".to_string(),
        ]
    );
    if let Some(banner) = doc()
        .query_selector("[data-pocopine-boot-error=\"missing-pp-app\"]")
        .unwrap()
    {
        banner.remove();
    }
}

#[wasm_bindgen_test]
fn route_navigation_hooks_fire_for_matched_routes() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let original = current_path_and_search();
    let host = app_host("<pp-outlet></pp-outlet>");

    App::new()
        .route::<AppPluginDirectHome>("/plugin-route-events")
        .plugin(route_event_plugin(events.clone()))
        .run();
    events.borrow_mut().clear();

    pocopine::navigate("/plugin-route-events");

    assert_eq!(
        events.borrow().as_slice(),
        &[
            "route-start:/plugin-route-events:/plugin-route-events:app-plugin-direct-home"
                .to_string(),
            "route-completed:/plugin-route-events:/plugin-route-events:app-plugin-direct-home"
                .to_string(),
        ]
    );
    replace_url(&original);
    host.remove();
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

#[wasm_bindgen_test(async)]
async fn validation_failure_clears_previous_app_plugin_registry() {
    // App #1 succeeds and activates a registry that holds
    // TestAnalytics + ComponentMounted/ComponentUnmounted hooks.
    let app1_events = Rc::new(RefCell::new(Vec::new()));
    let host1 = app_host("");
    App::new()
        .plugin(analytics_plugin(app1_events.clone()))
        .run();

    // Sanity: app #1's registry resolves Plugin<TestAnalytics>.
    {
        let el = doc().create_element("div").unwrap();
        let ctx =
            pocopine::LifecycleContext::__new(&el, ScopeId(0), pocopine::LifecyclePhase::Ready);
        let resolved: Option<Plugin<TestAnalytics>> = ctx.into();
        assert!(
            resolved.is_some(),
            "after a successful App::run, Plugin<TestAnalytics> must resolve"
        );
    }
    host1.remove();

    // App #2 has a hook for a service that was never provided —
    // validation must fail.
    let host2 = app_host("");
    App::new().plugin(MissingHookPlugin).run();

    // The framework refused to mount; the active registry must
    // now be empty so app #1's plugin context cannot leak into
    // anything that runs after the failure.
    {
        let el = doc().create_element("div").unwrap();
        let ctx =
            pocopine::LifecycleContext::__new(&el, ScopeId(0), pocopine::LifecyclePhase::Ready);
        let resolved: Option<Plugin<TestAnalytics>> = ctx.into();
        assert!(
            resolved.is_none(),
            "validation failure must reset the active registry; \
             stale Plugin<TestAnalytics> from a previous app leaked"
        );
    }

    // App #1's `mounted:` / `unmounted:` hooks must not fire on a
    // post-failure subtree mount either — they were dropped along
    // with the registry. Use a component that doesn't extract any
    // `Plugin<T>` so the test exercises the global hook dispatch
    // without depending on any specific plugin service surviving.
    let mount_host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&mount_host).unwrap();
    let handle = App::mount_subtree::<AppPluginDirectHome>(&mount_host);
    next_microtask().await;
    handle.unmount();

    assert!(
        app1_events.borrow().is_empty(),
        "stale ComponentMounted/Unmounted hooks from a previous app fired \
         after validation failure: {:?}",
        app1_events.borrow()
    );

    mount_host.remove();
    host2.remove();
    if let Some(banner) = doc()
        .query_selector("[data-pocopine-boot-error=\"plugin\"]")
        .unwrap()
    {
        banner.remove();
    }
}
