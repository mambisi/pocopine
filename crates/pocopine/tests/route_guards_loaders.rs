//! RFC-078 integration tests: route guards, loaders, rejection
//! chain, configurable error/404 surfaces, and `reevaluate_current`.
//!
//! Drives real navigation in a headless Firefox via wasm-bindgen-test.
//! Each test uses a unique route pattern so the process-global
//! `ROUTES` registry doesn't leak meaning across tests; URL is
//! restored to `/` at the end of every test.

#![cfg(target_arch = "wasm32")]

use js_sys::Promise;
use pocopine::App;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

// ─── shared helpers ─────────────────────────────────────────────────

fn doc() -> web_sys::Document {
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

fn remove_existing_app_roots() {
    let roots = doc().query_selector_all("[pp-app]").unwrap();
    for i in 0..roots.length() {
        if let Some(node) = roots.item(i)
            && let Ok(el) = node.dyn_into::<web_sys::Element>()
        {
            el.remove();
        }
    }
}

fn app_host_with_outlet() -> web_sys::Element {
    remove_existing_app_roots();
    let host = doc().create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<pp-outlet></pp-outlet>");
    doc().body().unwrap().append_child(&host).unwrap();
    host
}

async fn next_microtask() {
    JsFuture::from(Promise::resolve(&JsValue::NULL))
        .await
        .unwrap();
}

async fn yield_event_loop_for_loaders() {
    // Loader spawn → microtask → mount → setup. Five turns is
    // enough headroom for Ok and the
    // rejection-chain-then-redirect path.
    for _ in 0..5 {
        next_microtask().await;
    }
}

fn outlet_html() -> String {
    doc()
        .query_selector("pp-outlet")
        .unwrap()
        .map(|o| o.inner_html())
        .unwrap_or_default()
}

/// Drive a mount for a test. `App::run` calls `router::init()`
/// which is idempotent — only the very first test in the binary
/// gets an automatic `mount_current()`. Every subsequent test
/// must trigger one explicitly via `navigate`. Doing it
/// unconditionally keeps tests order-independent.
fn navigate_to(path: &str) {
    pocopine::navigate(path);
}

// ─── Guard outcomes — Allow / Reject / Redirect ─────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-allow-home",
    template = poco! {<div class="rgl-allow-home">allow ok</div>}
)]
struct GuardAllowHome {}

#[handlers]
impl GuardAllowHome {}

impl RouteComponent for GuardAllowHome {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| RouteGuardDecision::Allow)
    }
}

#[wasm_bindgen_test(async)]
async fn guard_allow_mounts_component() {
    let host = app_host_with_outlet();
    replace_url("/test-guard-allow");

    App::new()
        .route_component::<GuardAllowHome>("/test-guard-allow")
        .run();
    navigate_to("/test-guard-allow");
    next_microtask().await;

    let html = outlet_html();
    assert!(
        html.contains("rgl-allow-home"),
        "Allow guard should mount the component: {html}"
    );

    replace_url("/");
    host.remove();
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-reject-home",
    template = poco! {<div>should never paint</div>}
)]
struct GuardRejectHome {}

#[handlers]
impl GuardRejectHome {}

impl RouteComponent for GuardRejectHome {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| {
            RouteGuardDecision::Reject(RouteRejection::Forbidden("test_secret_policy"))
        })
    }
}

#[wasm_bindgen_test(async)]
async fn guard_reject_paints_default_error_surface() {
    let host = app_host_with_outlet();
    replace_url("/test-guard-reject");

    App::new()
        .route_component::<GuardRejectHome>("/test-guard-reject")
        .run();
    navigate_to("/test-guard-reject");
    next_microtask().await;

    let html = outlet_html();
    // The default surface paints generic copy, never the dynamic
    // reason string ("test_secret_policy") — privacy invariant
    // §5.10.7.
    assert!(
        html.contains("Access denied"),
        "Reject(Forbidden) should paint the default Forbidden surface: {html}"
    );
    assert!(
        !html.contains("test_secret_policy"),
        "default surface must not leak the rejection reason string: {html}"
    );
    assert!(
        !html.contains("rgl-reject-home"),
        "rejected component must not paint: {html}"
    );

    replace_url("/");
    host.remove();
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "rgl-redirect-source", template = poco! {<div>source</div>})]
struct GuardRedirectSource {}

#[handlers]
impl GuardRedirectSource {}

impl RouteComponent for GuardRedirectSource {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| {
            RouteGuardDecision::Redirect(RouteTarget::path("/test-redirect-target"))
        })
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "rgl-redirect-target",
    template = poco! {<div class="rgl-redirect-target">at target</div>}
)]
struct GuardRedirectTarget {}

#[handlers]
impl GuardRedirectTarget {}

#[wasm_bindgen_test(async)]
async fn guard_redirect_navigates_to_target() {
    let host = app_host_with_outlet();
    replace_url("/test-redirect-source");

    App::new()
        .route_component::<GuardRedirectSource>("/test-redirect-source")
        .route_component::<GuardRedirectTarget>("/test-redirect-target")
        .run();
    navigate_to("/test-redirect-source");
    next_microtask().await;
    next_microtask().await;

    let location_path = window().unwrap().location().pathname().unwrap_or_default();
    assert_eq!(
        location_path, "/test-redirect-target",
        "Redirect guard should navigate; ended at {location_path}"
    );
    assert!(outlet_html().contains("rgl-redirect-target"));

    replace_url("/");
    host.remove();
}

// ─── Loader Ok / Unauthorized ───────────────────────────────────────

#[derive(Clone)]
struct LoadedDashboardData {
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-loader-home",
    template = poco! {<div class="rgl-loader-home">loaded:<span pp-text="label"></span></div>}
)]
struct LoaderHome {
    label: String,
}

#[handlers]
impl LoaderHome {
    pub fn on_setup(&mut self, data: Loader<LoadedDashboardData>) {
        self.label = data.label.clone();
    }
}

impl RouteComponent for LoaderHome {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().loader(|_ctx: LoaderContext| async move {
            Ok::<_, LoaderError>(LoadedDashboardData {
                label: "from-loader".into(),
            })
        })
    }
}

#[wasm_bindgen_test(async)]
async fn loader_ok_flows_into_component_setup() {
    let host = app_host_with_outlet();
    replace_url("/test-loader-ok");

    App::new()
        .route_component::<LoaderHome>("/test-loader-ok")
        .run();
    navigate_to("/test-loader-ok");
    yield_event_loop_for_loaders().await;

    let html = outlet_html();
    assert!(
        html.contains("rgl-loader-home"),
        "loader-bound component should mount on Ok: {html}"
    );
    assert!(
        html.contains("from-loader"),
        "Loader<T> data should flow into setup: {html}"
    );

    replace_url("/");
    host.remove();
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-loader-unauth",
    template = poco! {<div>should never paint</div>}
)]
struct LoaderUnauthorized {}

#[handlers]
impl LoaderUnauthorized {
    pub fn on_setup(&mut self, _data: Loader<()>) {
        // Never reached — the loader returns Err.
    }
}

impl RouteComponent for LoaderUnauthorized {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .loader(|_ctx: LoaderContext| async move { Err::<(), _>(LoaderError::Unauthorized) })
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "rgl-loader-login",
    template = poco! {<div class="rgl-loader-login">login</div>}
)]
struct LoaderLogin {}

#[handlers]
impl LoaderLogin {}

#[wasm_bindgen_test(async)]
async fn loader_unauthorized_dispatches_through_rejection_handler() {
    let host = app_host_with_outlet();
    replace_url("/test-loader-unauth");

    App::new()
        .route_component::<LoaderUnauthorized>("/test-loader-unauth")
        .route_component::<LoaderLogin>("/test-loader-login")
        .route_rejection_handler(
            |_ctx: &RouteRejectionContext<'_>, rejection: &RouteRejection| match rejection {
                RouteRejection::Unauthorized => Some(RouteRejectionAction::Redirect(
                    RouteTarget::path("/test-loader-login"),
                )),
                _ => None,
            },
        )
        .run();
    navigate_to("/test-loader-unauth");
    yield_event_loop_for_loaders().await;

    let location_path = window().unwrap().location().pathname().unwrap_or_default();
    assert_eq!(
        location_path, "/test-loader-login",
        "Loader Err(Unauthorized) should dispatch via rejection handler to /login: {location_path}"
    );
    assert!(outlet_html().contains("rgl-loader-login"));

    replace_url("/");
    host.remove();
}

// ─── Configurable not-found component ───────────────────────────────

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "rgl-not-found",
    template = poco! {<div class="rgl-not-found">404 surface</div>}
)]
struct NotFoundSurface {}

#[handlers]
impl NotFoundSurface {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(name = "rgl-known-route", template = poco! {<div>known</div>})]
struct KnownRoute {}

#[handlers]
impl KnownRoute {}

#[wasm_bindgen_test(async)]
async fn not_found_component_mounts_for_unmatched_url() {
    let host = app_host_with_outlet();
    replace_url("/test-deep-unmatched-12345");

    App::new()
        .route_component::<KnownRoute>("/test-known-route")
        .not_found_component::<NotFoundSurface>()
        .run();
    navigate_to("/test-deep-unmatched-12345");
    next_microtask().await;
    next_microtask().await;

    let html = outlet_html();
    assert!(
        html.contains("rgl-not-found"),
        "configured not_found_component should paint when no route matches: {html}"
    );

    replace_url("/");
    host.remove();
}

// ─── Configurable route_error_component ─────────────────────────────

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "rgl-error-surface",
    template = poco! {<div class="rgl-error-surface">app error UI</div>}
)]
struct CustomErrorSurface {}

#[handlers]
impl CustomErrorSurface {}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "rgl-error-rejected", template = poco! {<div>never</div>})]
struct ErrorRejectedRoute {}

#[handlers]
impl ErrorRejectedRoute {}

impl RouteComponent for ErrorRejectedRoute {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| {
            RouteGuardDecision::Reject(RouteRejection::Server("db down"))
        })
    }
}

#[wasm_bindgen_test(async)]
async fn route_error_component_overrides_default_paint() {
    let host = app_host_with_outlet();
    replace_url("/test-route-error");

    App::new()
        .route_component::<ErrorRejectedRoute>("/test-route-error")
        .route_error_component::<CustomErrorSurface>()
        .run();
    navigate_to("/test-route-error");
    next_microtask().await;
    next_microtask().await;

    let html = outlet_html();
    assert!(
        html.contains("rgl-error-surface"),
        "configured route_error_component should replace the default banner: {html}"
    );
    assert!(
        !html.contains("Route unavailable"),
        "default Server-rejection title must not appear when override is set: {html}"
    );

    replace_url("/");
    host.remove();
}

// ─── reevaluate_current after sign-out ──────────────────────────────

thread_local! {
    static AUTHED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-reeval-protected",
    template = poco! {<div class="rgl-reeval-protected">protected</div>}
)]
struct ReevalProtected {}

#[handlers]
impl ReevalProtected {}

impl RouteComponent for ReevalProtected {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(|_: &RouteContext<'_>| {
            if AUTHED.with(|c| c.get()) {
                RouteGuardDecision::Allow
            } else {
                RouteGuardDecision::Reject(RouteRejection::Unauthorized)
            }
        })
    }
}

#[wasm_bindgen_test(async)]
async fn reevaluate_current_drops_mount_when_guards_flip_to_reject() {
    let host = app_host_with_outlet();
    replace_url("/test-reeval");
    AUTHED.with(|c| c.set(true));

    App::new()
        .route_component::<ReevalProtected>("/test-reeval")
        .run();
    navigate_to("/test-reeval");
    next_microtask().await;
    let html_before = outlet_html();
    assert!(
        html_before.contains("rgl-reeval-protected"),
        "initial Allow should mount the protected component: {html_before}"
    );

    // Simulate sign-out: flip the guard's truth source, then call
    // reevaluate_current. The router should drop the mounted
    // component synchronously and dispatch the now-Unauthorized
    // rejection through the default surface.
    AUTHED.with(|c| c.set(false));
    pocopine::reevaluate_current();
    next_microtask().await;
    next_microtask().await;

    let html_after = outlet_html();
    assert!(
        !html_after.contains("rgl-reeval-protected"),
        "reevaluate_current should drop the protected component after guards flipped to Reject: {html_after}"
    );
    assert!(
        html_after.contains("Authentication required"),
        "default Unauthorized surface should paint after reevaluate: {html_after}"
    );

    replace_url("/");
    host.remove();
}

// ─── Fetch middleware: install + freeze panic ───────────────────────

#[pocopine::server(public, idempotent)]
async fn rgl_replay_safe_value() -> pocopine::ServerResult<u32> {
    Ok(0)
}

#[wasm_bindgen_test(async)]
async fn server_idempotent_marks_fetch_request_replay_safe() {
    pocopine::fetch::__reset_middleware_chain_for_test();
    let seen_replay_safe = Rc::new(RefCell::new(None));
    let seen_replay_safe_for_middleware = seen_replay_safe.clone();

    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let seen_replay_safe = seen_replay_safe_for_middleware.clone();
            async move {
                *seen_replay_safe.borrow_mut() = Some(req.is_replay_safe());
                Ok(pocopine::fetch::FetchResponse {
                    status: 200,
                    body: serde_json::to_string(&Ok::<u32, pocopine::ServerError>(42)).unwrap(),
                })
            }
        },
    );

    let value = rgl_replay_safe_value().await.unwrap();
    assert_eq!(value, 42);
    assert_eq!(
        *seen_replay_safe.borrow(),
        Some(true),
        "#[server(idempotent)] stubs should mark requests replay-safe"
    );

    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "rgl-abort-slow",
    template = poco! {<div>slow loader should never paint</div>}
)]
struct AbortSlowRoute {}

#[handlers]
impl AbortSlowRoute {}

impl RouteComponent for AbortSlowRoute {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().loader(|_ctx: LoaderContext| async move {
            let _: u32 = pocopine::fetch::call("/test-abort-never", &()).await?;
            Ok::<_, LoaderError>(())
        })
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "rgl-abort-target",
    template = poco! {<div class="rgl-abort-target">target</div>}
)]
struct AbortTargetRoute {}

#[handlers]
impl AbortTargetRoute {}

#[wasm_bindgen_test(async)]
async fn loader_fetch_signal_aborts_on_navigation_supersession() {
    pocopine::fetch::__reset_middleware_chain_for_test();
    let captured_signal = Rc::new(RefCell::new(None));
    let captured_signal_for_middleware = captured_signal.clone();

    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let captured_signal = captured_signal_for_middleware.clone();
            async move {
                *captured_signal.borrow_mut() = req.abort_signal.clone();
                std::future::pending::<Result<pocopine::fetch::FetchResponse, pocopine::ServerError>>()
                    .await
            }
        },
    );

    let host = app_host_with_outlet();
    replace_url("/");
    App::new()
        .route_component::<AbortSlowRoute>("/test-abort-loader")
        .route_component::<AbortTargetRoute>("/test-abort-target")
        .run();

    navigate_to("/test-abort-loader");
    yield_event_loop_for_loaders().await;

    let signal = captured_signal
        .borrow()
        .clone()
        .expect("loader-owned fetch::call should receive an AbortSignal");
    assert!(
        !signal.aborted(),
        "fresh loader abort signal should not be aborted before supersession"
    );

    navigate_to("/test-abort-target");
    next_microtask().await;

    assert!(
        signal.aborted(),
        "superseding navigation should abort the loader's fetch signal"
    );
    assert!(
        outlet_html().contains("rgl-abort-target"),
        "target route should paint after superseding the loader route"
    );

    replace_url("/");
    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test]
#[should_panic(expected = "called after the chain was frozen")]
fn fetch_install_middleware_panics_after_freeze() {
    pocopine::fetch::__reset_middleware_chain_for_test();
    pocopine::fetch::install_middleware(
        |req: pocopine::fetch::FetchRequest, next: pocopine::fetch::FetchNext| async move {
            next.run(req).await
        },
    );
    pocopine::fetch::freeze_middleware_chain();
    // This second install must panic per RFC-078 §5.10.3.
    pocopine::fetch::install_middleware(
        |req: pocopine::fetch::FetchRequest, next: pocopine::fetch::FetchNext| async move {
            next.run(req).await
        },
    );
}
