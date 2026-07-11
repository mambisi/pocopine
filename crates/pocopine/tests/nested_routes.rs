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
}

fn event(value: impl Into<String>) {
    EVENTS.with(|events| events.borrow_mut().push(value.into()));
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-admin-layout",
    template_inline = r#"<section class="nr-layout">
        <span class="nr-layout-data" pp-text="loader_data"></span>
        <span class="nr-layout-count" pp-text="count"></span>
        <button class="nr-layout-bump" @click="bump">bump</button>
        <pp-outlet></pp-outlet>
    </section>"#
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
    template_inline = r#"<div class="nr-index">index</div>"#
)]
struct NestedAdminIndex {}

#[handlers]
impl NestedAdminIndex {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "nr-admin-user",
    template_inline = r#"<div class="nr-user">
        <span class="nr-user-id" pp-text="user_id"></span>
        <span class="nr-user-data" pp-text="loader_data"></span>
    </div>"#
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
    template_inline = r#"<div class="nr-settings" pp-text="loader_data"></div>"#
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
