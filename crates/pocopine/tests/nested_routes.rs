//! RFC-089 Phase 2/3 browser coverage: route chains, owned nested outlets,
//! guard/loader ordering, and common-prefix preservation.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};

use js_sys::Promise;
use pocopine::flush_sync;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static LAYOUT_MOUNTS: Cell<u32> = const { Cell::new(0) };
    static LAYOUT_UNMOUNTS: Cell<u32> = const { Cell::new(0) };
    static SAFE_NAV_UNMOUNTS: Cell<u32> = const { Cell::new(0) };
    static SAFE_GUARD_ALLOW: Cell<bool> = const { Cell::new(true) };
}

fn event(value: impl Into<String>) {
    EVENTS.with(|events| events.borrow_mut().push(value.into()));
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-admin-layout",
    template = poco! {<section class="nr-layout">
        <span class="nr-layout-data" pp-text="loader_data"></span>
        <span class="nr-layout-count" pp-text="count"></span>
        <button class="nr-layout-bump" @click="bump">bump</button>
        <pp-outlet></pp-outlet>
    </section>}
)]
struct NestedAdminLayout {
    loader_data: String,
    count: u32,
}

#[handlers]
impl NestedAdminLayout {
    pub fn on_setup(&mut self, data: Loader<String>) {
        self.loader_data = data.to_string();
        LAYOUT_MOUNTS.with(|count| count.set(count.get() + 1));
    }

    pub fn on_unmount(&mut self) {
        LAYOUT_UNMOUNTS.with(|count| count.set(count.get() + 1));
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }
}

impl RouteComponent for NestedAdminLayout {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(|_: &RouteContext<'_>| {
                event("guard:layout");
                RouteGuardDecision::Allow
            })
            .loader(|_: LoaderContext| async move {
                event("loader:layout");
                Ok("layout-data".to_string())
            })
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "nr-admin-index",
    template = poco! {<div class="nr-index">index</div>}
)]
struct NestedAdminIndex {}

#[handlers]
impl NestedAdminIndex {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-admin-user",
    template = poco! {<div class="nr-user">
        <span class="nr-user-id" pp-text="user_id"></span>
        <span class="nr-user-data" pp-text="loader_data"></span>
    </div>}
)]
struct NestedAdminUser {
    #[prop]
    user_id: String,
    loader_data: String,
}

#[handlers]
impl NestedAdminUser {
    pub fn on_setup(&mut self, data: Loader<String>) {
        self.loader_data = data.to_string();
    }
}

impl RouteComponent for NestedAdminUser {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(|ctx: &RouteContext<'_>| {
                event(format!(
                    "guard:user:{}",
                    ctx.params.get("user_id").cloned().unwrap_or_default(),
                ));
                RouteGuardDecision::Allow
            })
            .loader(|ctx: LoaderContext| async move {
                let id = ctx.params.get("user_id").cloned().unwrap_or_default();
                event(format!("loader:user:{id}"));
                Ok(format!("user-data:{id}"))
            })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-admin-settings",
    template = poco! {<div class="nr-settings" pp-text="loader_data"></div>}
)]
struct NestedAdminSettings {
    loader_data: String,
}

#[handlers]
impl NestedAdminSettings {
    pub fn on_setup(&mut self, data: Loader<String>) {
        self.loader_data = data.to_string();
    }
}

impl RouteComponent for NestedAdminSettings {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(|_: &RouteContext<'_>| {
                event("guard:settings");
                RouteGuardDecision::Allow
            })
            .loader(|_: LoaderContext| async move {
                event("loader:settings");
                Ok("settings-data".to_string())
            })
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "nr-safe-nav-source",
    template = poco! {<div class="nr-safe-nav-source">
        <button class="nr-safe-navigate" @click="go_navigate">navigate</button>
        <button class="nr-safe-push" @click="go_push">push</button>
        <button class="nr-safe-replace" @click="go_replace">replace</button>
    </div>}
)]
struct SafeNavigationSource {}

#[handlers]
impl SafeNavigationSource {
    pub fn go_navigate(&mut self) {
        event("handler:navigate:start");
        pocopine::navigate("/nr-safe-nav-navigate");
        event("handler:navigate:return");
    }

    pub fn go_push(&mut self) {
        event("handler:push:start");
        let result = pocopine::push("/nr-safe-nav-push");
        event(if result.is_ok() {
            "handler:push:return"
        } else {
            "handler:push:error"
        });
    }

    pub fn go_replace(&mut self) {
        event("handler:replace:start");
        let result = pocopine::replace("/nr-safe-nav-replace");
        event(if result.is_ok() {
            "handler:replace:return"
        } else {
            "handler:replace:error"
        });
    }

    pub fn on_unmount(&mut self) {
        event("source:unmount");
        SAFE_NAV_UNMOUNTS.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "nr-safe-nav-navigate-target",
    template = poco! {<div class="nr-safe-nav-navigate-target">navigate target</div>}
)]
struct SafeNavigateTarget {}

#[handlers]
impl SafeNavigateTarget {
    pub fn on_setup(&mut self) {
        event("target:navigate:setup");
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "nr-safe-nav-push-target",
    template = poco! {<div class="nr-safe-nav-push-target">push target</div>}
)]
struct SafePushTarget {}

#[handlers]
impl SafePushTarget {
    pub fn on_setup(&mut self) {
        event("target:push:setup");
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "nr-safe-nav-replace-target",
    template = poco! {<div class="nr-safe-nav-replace-target">replace target</div>}
)]
struct SafeReplaceTarget {}

#[handlers]
impl SafeReplaceTarget {
    pub fn on_setup(&mut self) {
        event("target:replace:setup");
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-safe-guard-source",
    template = poco! {<button class="nr-safe-guard-reevaluate" @click="reject">reject</button>}
)]
struct SafeGuardSource {}

#[handlers]
impl SafeGuardSource {
    pub fn reject(&mut self) {
        event("handler:guard:start");
        SAFE_GUARD_ALLOW.with(|allow| allow.set(false));
        pocopine::router::reevaluate_current();
        event("handler:guard:return");
    }

    pub fn on_unmount(&mut self) {
        event("guard-source:unmount");
    }
}

impl RouteComponent for SafeGuardSource {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| {
            if SAFE_GUARD_ALLOW.with(Cell::get) {
                RouteGuardDecision::Allow
            } else {
                RouteGuardDecision::Reject(RouteRejection::Forbidden("safe_point_test"))
            }
        })
    }
}

fn document() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn replace_url(path: &str) {
    window()
        .unwrap()
        .history()
        .unwrap()
        .replace_state_with_url(&JsValue::NULL, "", Some(path))
        .unwrap();
}

fn app_host() -> Element {
    let host = document().create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<pp-outlet></pp-outlet>");
    document().body().unwrap().append_child(&host).unwrap();
    host
}

async fn settle() {
    for _ in 0..8 {
        JsFuture::from(Promise::resolve(&JsValue::NULL))
            .await
            .unwrap();
    }
}

fn click(root: &Element, selector: &str) {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
}

fn text(root: &Element, selector: &str) -> String {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap_or_default()
}

fn assert_event_before(events: &[String], first: &str, second: &str) {
    let first = events
        .iter()
        .position(|event| event == first)
        .unwrap_or_else(|| panic!("missing `{first}` in {events:?}"));
    let second = events
        .iter()
        .position(|event| event == second)
        .unwrap_or_else(|| panic!("missing `{second}` in {events:?}"));
    assert!(first < second, "expected event order in {events:?}");
}

#[wasm_bindgen_test(async)]
async fn nested_routes_mount_in_owned_outlets_and_preserve_the_layout_prefix() {
    EVENTS.with(|events| events.borrow_mut().clear());
    LAYOUT_MOUNTS.with(|count| count.set(0));
    LAYOUT_UNMOUNTS.with(|count| count.set(0));
    pocopine::animate::disable_transitions();
    replace_url("/nr-admin/users/42");
    let host = app_host();

    App::new()
        .layout::<NestedAdminLayout>("/nr-admin", |admin| {
            admin.index::<NestedAdminIndex>();
            admin.route::<NestedAdminUser>("users/:user_id");
            admin.route::<NestedAdminSettings>("settings");
        })
        .run();
    settle().await;

    assert_eq!(text(&host, ".nr-layout-data"), "layout-data");
    assert_eq!(text(&host, ".nr-user-id"), "42");
    assert_eq!(text(&host, ".nr-user-data"), "user-data:42");
    assert_eq!(LAYOUT_MOUNTS.with(Cell::get), 1);
    assert_eq!(
        EVENTS.with(|events| events.borrow().clone()),
        vec![
            "guard:layout",
            "guard:user:42",
            "loader:layout",
            "loader:user:42",
        ],
    );

    click(&host, ".nr-layout-bump");
    flush_sync();
    assert_eq!(text(&host, ".nr-layout-count"), "1");
    let layout = host.query_selector("nr-admin-layout").unwrap().unwrap();

    EVENTS.with(|events| events.borrow_mut().clear());
    let location = pocopine::push("/nr-admin/settings").unwrap();
    assert_eq!(location.matched.len(), 2);
    assert_eq!(
        location.matched.as_slice()[0].component_name,
        NestedAdminLayout::NAME
    );
    assert_eq!(
        location.matched.as_slice()[1].component_name,
        NestedAdminSettings::NAME,
    );
    settle().await;

    let preserved = host.query_selector("nr-admin-layout").unwrap().unwrap();
    assert!(layout.is_same_node(Some(preserved.as_ref())));
    assert_eq!(text(&host, ".nr-layout-count"), "1");
    assert_eq!(text(&host, ".nr-settings"), "settings-data");
    assert_eq!(LAYOUT_MOUNTS.with(Cell::get), 1);
    assert_eq!(LAYOUT_UNMOUNTS.with(Cell::get), 0);
    assert_eq!(
        EVENTS.with(|events| events.borrow().clone()),
        vec!["guard:layout", "guard:settings", "loader:settings"],
    );

    EVENTS.with(|events| events.borrow_mut().clear());
    pocopine::push("/nr-admin").unwrap();
    settle().await;
    assert!(host.query_selector(".nr-index").unwrap().is_some());
    assert_eq!(text(&host, ".nr-layout-count"), "1");
    assert_eq!(LAYOUT_MOUNTS.with(Cell::get), 1);

    pocopine::push("/").unwrap();
    settle().await;
    assert_eq!(LAYOUT_UNMOUNTS.with(Cell::get), 1);
    host.remove();
    replace_url("/");
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test(async)]
async fn handler_navigation_defers_route_teardown_until_after_the_state_borrow() {
    EVENTS.with(|events| events.borrow_mut().clear());
    SAFE_NAV_UNMOUNTS.with(|count| count.set(0));
    pocopine::animate::disable_transitions();
    replace_url("/");
    let host = app_host();

    App::new()
        .route_component::<SafeNavigationSource>("/nr-safe-nav-source")
        .route_component::<SafeNavigateTarget>("/nr-safe-nav-navigate")
        .route_component::<SafePushTarget>("/nr-safe-nav-push")
        .route_component::<SafeReplaceTarget>("/nr-safe-nav-replace")
        .run();

    pocopine::replace("/nr-safe-nav-source").unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());
    click(&host, ".nr-safe-navigate");
    settle().await;
    let events = EVENTS.with(|events| events.borrow().clone());
    assert_event_before(&events, "handler:navigate:return", "source:unmount");
    assert!(
        host.query_selector(".nr-safe-nav-navigate-target")
            .unwrap()
            .is_some()
    );
    assert_eq!(SAFE_NAV_UNMOUNTS.with(Cell::get), 1);

    pocopine::replace("/nr-safe-nav-source").unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());
    click(&host, ".nr-safe-push");
    settle().await;
    let events = EVENTS.with(|events| events.borrow().clone());
    assert_event_before(&events, "handler:push:return", "source:unmount");
    assert!(
        host.query_selector(".nr-safe-nav-push-target")
            .unwrap()
            .is_some()
    );
    assert_eq!(SAFE_NAV_UNMOUNTS.with(Cell::get), 2);

    pocopine::replace("/nr-safe-nav-source").unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());
    click(&host, ".nr-safe-replace");
    settle().await;
    let events = EVENTS.with(|events| events.borrow().clone());
    assert_event_before(&events, "handler:replace:return", "source:unmount");
    assert!(
        host.query_selector(".nr-safe-nav-replace-target")
            .unwrap()
            .is_some()
    );
    assert_eq!(SAFE_NAV_UNMOUNTS.with(Cell::get), 3);

    pocopine::replace("/").unwrap();
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test(async)]
async fn guard_reevaluation_defers_rejection_teardown_until_after_the_handler() {
    EVENTS.with(|events| events.borrow_mut().clear());
    SAFE_GUARD_ALLOW.with(|allow| allow.set(true));
    pocopine::animate::disable_transitions();
    replace_url("/");
    let host = app_host();

    App::new()
        .route_component::<SafeGuardSource>("/nr-safe-guard-source")
        .run();
    pocopine::replace("/nr-safe-guard-source").unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());

    click(&host, ".nr-safe-guard-reevaluate");
    settle().await;

    let events = EVENTS.with(|events| events.borrow().clone());
    assert_event_before(&events, "handler:guard:return", "guard-source:unmount");
    assert!(
        host.query_selector("nr-safe-guard-source")
            .unwrap()
            .is_none(),
        "rejected route component must be removed",
    );

    SAFE_GUARD_ALLOW.with(|allow| allow.set(true));
    pocopine::replace("/").unwrap();
    host.remove();
    pocopine::animate::enable_transitions();
}
