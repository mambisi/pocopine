//! Client-side SPA router.
//!
//! Shape of the runtime:
//!
//! * User-declared routes live in a `Vec<Route>` behind a thread-local
//!   (see [`register_route`]). Patterns are `/foo/:id` style, with
//!   `*` as the 404-fallback.
//! * A single `<pp-outlet>` in the DOM is the mount point
//!   ([`set_outlet`]). The mount recognises the tag and hands its
//!   element to the router.
//! * Navigation goes through [`navigate`]. It pushes a new history
//!   entry, re-matches, unmounts the prior page, and creates a
//!   `<component-name>` tag with path-params as HTML attributes inside
//!   the outlet. The existing [`crate::mount::walk`] pipeline picks
//!   it up and handles tag resolution + prop coercion.
//! * A synthetic `RouteState` scope drives the `$route` magic. The
//!   router calls [`trigger_scope`] on its id so any template binding
//!   reading `$route.path` / `$route.params.<name>` / `$route.query.<name>`
//!   re-evaluates when the URL changes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect};
use once_cell::unsync::OnceCell;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{Element, Event};

use crate::app::{
    Loader, LoaderContext, RejectionSource, RouteContext, RouteErrorSurface, RouteGuard,
    RouteGuardDecision, RouteLoader, RouteRejection, RouteRejectionAction, RouteRejectionContext,
    RouteRejectionHandler,
};
use crate::mount;
use crate::reactive::{trigger_scope, ScopeId};
use crate::scope::{ComponentState, Scope};

mod return_to;

pub use return_to::ReturnTo;

// ─── route parsing ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Segment {
    Literal(String),
    Param(String),
    Wildcard,
}

#[derive(Clone)]
pub struct Route {
    pub pattern: &'static str,
    segments: Vec<Segment>,
    pub component_name: &'static str,
    config: RouteRuntimeConfig,
}

impl Route {
    fn parse(
        pattern: &'static str,
        component_name: &'static str,
        config: RouteRuntimeConfig,
    ) -> Self {
        let segments = if pattern == "*" {
            vec![Segment::Wildcard]
        } else {
            pattern
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    if let Some(name) = s.strip_prefix(':') {
                        Segment::Param(name.to_string())
                    } else {
                        Segment::Literal(s.to_string())
                    }
                })
                .collect()
        };
        Route {
            pattern,
            segments,
            component_name,
            config,
        }
    }

    fn is_wildcard(&self) -> bool {
        matches!(self.segments.as_slice(), [Segment::Wildcard])
    }

    /// Try to match `path` against this route. Returns captured params
    /// on a successful match, `None` otherwise.
    fn match_path(&self, path: &str) -> Option<HashMap<String, String>> {
        if self.is_wildcard() {
            return Some(HashMap::new());
        }
        let input: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if input.len() != self.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (seg, got) in self.segments.iter().zip(input.iter()) {
            match seg {
                Segment::Literal(s) if s == got => {}
                Segment::Literal(_) => return None,
                Segment::Param(name) => {
                    params.insert(name.clone(), (*got).to_string());
                }
                Segment::Wildcard => {}
            }
        }
        Some(params)
    }
}

#[derive(Clone, Default)]
pub(crate) struct RouteRuntimeConfig {
    pub(crate) guards: Vec<Rc<dyn RouteGuard>>,
    pub(crate) loader: Option<Rc<dyn RouteLoader>>,
}

#[derive(Clone)]
struct RouteMatch {
    component_name: &'static str,
    route_pattern: Option<&'static str>,
    params: HashMap<String, String>,
    config: RouteRuntimeConfig,
}

// ─── synthetic `$route` scope ───────────────────────────────────────

#[derive(Default)]
struct RouteState {
    path: String,
    params: HashMap<String, String>,
    query: HashMap<String, String>,
}

impl ComponentState for RouteState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "path" => JsValue::from_str(&self.path),
            "params" => map_to_object(&self.params),
            "query" => map_to_object(&self.query),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, _key: &str, _value: JsValue) {
        // $route is read-only from templates.
    }

    fn keys(&self) -> &'static [&'static str] {
        &["path", "params", "query"]
    }

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

fn map_to_object(map: &HashMap<String, String>) -> JsValue {
    let obj = Object::new();
    for (k, v) in map {
        let _ = Reflect::set(&obj, &JsValue::from_str(k), &JsValue::from_str(v));
    }
    obj.into()
}

// ─── thread-local state ─────────────────────────────────────────────

thread_local! {
    static ROUTES: RefCell<Vec<Route>> = const { RefCell::new(Vec::new()) };
    static OUTLET: RefCell<Option<Element>> = const { RefCell::new(None) };
    static ROUTE_SCOPE: OnceCell<Scope> = const { OnceCell::new() };
    static ROUTE_STATE_RC: OnceCell<Rc<RefCell<RouteState>>> =
        const { OnceCell::new() };
    static INITIALISED: Cell<bool> = const { Cell::new(false) };
    static ROUTE_REJECTION_HANDLERS: RefCell<Vec<Rc<dyn RouteRejectionHandler>>> =
        const { RefCell::new(Vec::new()) };
    /// Loader-produced data sitting between "router resolved the
    /// loader" and "the just-mounted component reads it via
    /// `Loader<T>` extractor". The router populates this slot once
    /// per navigation; the first lifecycle hook to extract a
    /// `Loader<T>` migrates the value into a per-scope slot
    /// (`LOADER_SLOTS`) keyed by the mounting component's
    /// [`ScopeId`]. Once migrated the data is shared by `Rc` and
    /// stays alive for the rest of the route's mount. Cleared on
    /// every `mount_current` entry as defense-in-depth.
    static PENDING_LOADER_DATA: RefCell<Option<Rc<dyn std::any::Any>>> =
        const { RefCell::new(None) };
    /// Per-mount loader-data slots — one entry per route component
    /// that consumed a loader result. Keyed by `ScopeId` so the
    /// slot survives every lifecycle hook on the component (setup,
    /// mount, ready, unmount) and is dropped when the scope tears
    /// down via [`release_loader_slot`].
    static LOADER_SLOTS: RefCell<std::collections::HashMap<ScopeId, Rc<dyn std::any::Any>>> =
        RefCell::new(std::collections::HashMap::new());
    /// Monotonic id incremented at every `mount_current`. Loaders
    /// capture the value at spawn and compare against the current
    /// value when they resolve; mismatch means navigation moved on
    /// while the loader was in flight, so the result is dropped
    /// rather than painted (RFC-078 §5.10.5).
    static ROUTE_TOKEN: Cell<u64> = const { Cell::new(0) };
    /// `App::route_error_component<C>()` recorded component name,
    /// painted by `paint_route_error` when set instead of the
    /// built-in HTML banner.
    static ROUTE_ERROR_COMPONENT: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// `App::not_found_component<C>()` recorded component name,
    /// mounted by `finish_route_mount` when no route (and no
    /// wildcard) matched.
    static NOT_FOUND_COMPONENT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Monotonic identity of a navigation attempt. Two
/// [`RouteToken`] values that compare equal denote the same
/// navigation; any difference means navigation moved on between
/// capture and check, and any in-flight loader's result must be
/// discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteToken(u64);

impl RouteToken {
    /// Capture the router's currently active navigation token.
    /// Loaders typically don't need to call this directly — the
    /// async spawn already captures the current token at start
    /// time; [`LoaderContext::is_navigation_active`] is the
    /// supported way to check from loader code.
    pub fn current() -> Self {
        ROUTE_TOKEN.with(|cell| RouteToken(cell.get()))
    }
}

/// Internal — bumps the router's monotonic token. Called once at
/// the top of every `mount_current` so any in-flight loader
/// captured under the previous token can recognise it has been
/// superseded.
fn bump_route_token() -> RouteToken {
    ROUTE_TOKEN.with(|cell| {
        let next = cell.get().wrapping_add(1);
        cell.set(next);
        RouteToken(next)
    })
}

/// Internal — true when `token` is still the router's current
/// navigation. Used by spawned loaders to short-circuit and by the
/// post-resolve check to drop stale results.
fn is_token_current(token: RouteToken) -> bool {
    ROUTE_TOKEN.with(|cell| cell.get() == token.0)
}

/// Crate-internal companion to [`is_token_current`] used by
/// [`crate::app::LoaderContext::is_navigation_active`]. Loader
/// closures don't construct `RouteToken` directly; the router
/// stamps the value into their context.
pub(crate) fn route_token_is_current(token: RouteToken) -> bool {
    is_token_current(token)
}

/// Stash a router-produced loader result for the next component
/// mount. Stored as `Rc` so the value can survive multiple
/// extractor reads during the component's mount. The pending slot
/// is overwritten if a previous result was never consumed (e.g.
/// mount aborted before setup ran); the per-scope `LOADER_SLOTS`
/// entries are independent and live until each scope's teardown.
pub(crate) fn put_pending_loader_data(data: Box<dyn std::any::Any>) {
    let rc: Rc<dyn std::any::Any> = Rc::from(data);
    PENDING_LOADER_DATA.with(|cell| *cell.borrow_mut() = Some(rc));
}

/// Resolve loader data for the lifecycle hook running under
/// `scope_id`. Returns `None` when no loader populated a slot for
/// this mount (component mounted via `mount_subtree`, route had no
/// loader). Subsequent calls within the same mount return the
/// **same** data — RFC §5.4's per-mount lifetime contract — by
/// migrating the one-shot pending value into a per-scope `Rc`
/// slot on the first read.
///
/// Panics when stored loader data exists but its type doesn't
/// match `T`. That indicates a mismatch between
/// `RouteConfig::loader(...)` and the component's `Loader<T>`
/// extractor and is always a programmer bug.
pub(crate) fn take_pending_loader_data<T: 'static>(scope_id: ScopeId) -> Option<Loader<T>> {
    // Per-scope slot wins: any subsequent extractor on the same
    // scope clones the existing `Rc` rather than racing the
    // pending one-shot.
    if let Some(rc) = LOADER_SLOTS.with(|map| map.borrow().get(&scope_id).cloned()) {
        return Some(loader_from_rc::<T>(rc));
    }

    // Otherwise migrate from the one-shot pending slot. The first
    // lifecycle hook (typically `on_setup`) hits this branch; any
    // later hook on the same scope hits the per-scope branch
    // above.
    let pending = PENDING_LOADER_DATA.with(|cell| cell.borrow_mut().take())?;
    LOADER_SLOTS.with(|map| {
        map.borrow_mut().insert(scope_id, pending.clone());
    });
    Some(loader_from_rc::<T>(pending))
}

fn loader_from_rc<T: 'static>(rc: Rc<dyn std::any::Any>) -> Loader<T> {
    match Rc::downcast::<T>(rc) {
        Ok(data) => Loader::__from_rc(data),
        Err(_) => panic!(
            "Loader<{}>: pending loader data did not match the extractor's \
             type. Check the loader closure registered on `RouteConfig::loader` \
             returns the same type the component's `Loader<T>` extractor reads.",
            std::any::type_name::<T>(),
        ),
    }
}

/// Drop any pending loader data. Called when the router decides
/// not to mount (rejection, navigation aborted) so the slot
/// doesn't leak across navigations. Does **not** affect per-scope
/// slots — those are managed by [`release_loader_slot`] when each
/// scope tears down.
pub(crate) fn clear_pending_loader_data() {
    PENDING_LOADER_DATA.with(|cell| cell.borrow_mut().take());
}

/// Drop the per-scope loader slot for `scope_id`. Called from
/// `mount.rs` when a route component's scope is being torn down,
/// so the loader data the component held lives for exactly the
/// component's mount and no longer.
pub fn release_loader_slot(scope_id: ScopeId) {
    LOADER_SLOTS.with(|map| {
        map.borrow_mut().remove(&scope_id);
    });
}

/// Register a route. Called from `App::route::<C>(pattern)`.
pub fn register_route(pattern: &'static str, component_name: &'static str) {
    register_route_with_config(pattern, component_name, RouteRuntimeConfig::default());
}

pub(crate) fn register_route_with_config(
    pattern: &'static str,
    component_name: &'static str,
    config: RouteRuntimeConfig,
) {
    ROUTES.with(|r| {
        let route = Route::parse(pattern, component_name, config);
        r.borrow_mut().push(route);
    });
}

pub(crate) fn set_route_rejection_handlers(handlers: Vec<Rc<dyn RouteRejectionHandler>>) {
    ROUTE_REJECTION_HANDLERS.with(|registered| {
        *registered.borrow_mut() = handlers;
    });
}

/// Configure the component the router mounts when a rejection
/// reaches the fallback. `None` reverts to the built-in
/// `RouteErrorSurface` HTML banner. Called from `App::run`.
pub(crate) fn set_route_error_component(name: Option<&'static str>) {
    ROUTE_ERROR_COMPONENT.with(|cell| cell.set(name));
}

/// Configure the component the router mounts when no route
/// matches. `None` keeps the prior behaviour (route-state update
/// only). Called from `App::run`.
pub(crate) fn set_not_found_component(name: Option<&'static str>) {
    NOT_FOUND_COMPONENT.with(|cell| cell.set(name));
}

/// Tell the router where to mount pages. Called from the mount when
/// it sees `<pp-outlet>`.
pub fn set_outlet(el: Element) {
    OUTLET.with(|o| *o.borrow_mut() = Some(el));
}

/// Navigate to `url`. Pushes a history entry and paints the matched
/// page.
pub fn navigate(url: &str) {
    let Some(win) = web_sys::window() else { return };
    if let Ok(history) = win.history() {
        let _ = history.push_state_with_url(&JsValue::NULL, "", Some(url));
    }
    mount_current();
}

/// Initialise the router: attach a `popstate` listener and paint the
/// current URL. Called once from `App::run` after the initial mount
/// pass.
pub fn init() {
    if INITIALISED.with(|b| b.replace(true)) {
        return; // idempotent
    }

    ensure_route_scope();

    // popstate → re-mount.
    let cb = Closure::wrap(Box::new(move |_: Event| {
        mount_current();
    }) as Box<dyn FnMut(Event)>);
    if let Some(win) = web_sys::window() {
        let _ = win.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    }
    cb.forget();

    mount_current();
}

fn ensure_route_scope() {
    ROUTE_SCOPE.with(|cell| {
        if cell.get().is_some() {
            return;
        }
        let state = Rc::new(RefCell::new(RouteState::default()));
        let scope = Scope::new(state.clone());
        let _ = cell.set(scope);
        ROUTE_STATE_RC.with(|s| {
            let _ = s.set(state);
        });
    });
}

/// Read-only proxy onto the `$route` scope. Magic resolver uses this.
pub fn route_proxy() -> JsValue {
    ensure_route_scope();
    ROUTE_SCOPE.with(|cell| {
        cell.get()
            .map(|s| s.into_proxy())
            .unwrap_or(JsValue::UNDEFINED)
    })
}

/// Re-run guards on the current route without changing history.
///
/// Plugins (notably `pocopine-auth-client`) **must** call this when
/// the source of truth for an authorization predicate changes —
/// canonical example: an auth plugin's `AuthSession` flips on
/// sign-in / sign-out / token expiry. The router re-matches the
/// current path, re-evaluates the guard chain, and:
///
/// - **`Allow`** → no-op. The currently mounted component stays.
///   Callers that need a forward navigation after sign-in (e.g.
///   "redirect to `/dashboard` post-login") should issue a separate
///   [`navigate`] call.
/// - **`Redirect`** / **`Reject`** → the outlet is cleared
///   synchronously so any PII in the now-rejected component leaves
///   the DOM **before** the rejection chain paints its outcome,
///   then the full mount flow re-runs through [`mount_current`] so
///   handlers, error surface, and `RouteNavigationFailed` events
///   fire exactly as they would on a fresh navigation.
///
/// No-op when the current path doesn't match any registered route
/// (no guards to re-evaluate) or when the platform is unavailable.
/// See RFC-078 §5.10.6 for the contract.
pub fn reevaluate_current() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let location = window.location();
    let path = location.pathname().unwrap_or_else(|_| "/".into());
    let search = location.search().unwrap_or_default();

    let Some(matched) = match_route(&path, false) else {
        return;
    };
    let query = parse_query(&search);

    let Some(decision) = evaluate_guards(&matched, &path, &query) else {
        return;
    };

    match decision {
        RouteGuardDecision::Allow => {
            // Identity change made the user MORE privileged or the
            // route was already permissive. Existing mount is still
            // valid; re-mounting would tear down legitimate state.
        }
        RouteGuardDecision::Redirect(_) | RouteGuardDecision::Reject(_) => {
            // Drop the rejected component synchronously so PII it
            // rendered cannot survive the next event-loop turn —
            // the rejection chain paints its outcome AFTER the
            // outlet is empty.
            clear_outlet();
            mount_current();
        }
    }
}

fn clear_outlet() {
    let Some(outlet) = OUTLET.with(|o| o.borrow().clone()) else {
        return;
    };
    outlet.set_inner_html("");
}

fn mount_current() {
    ensure_route_scope();

    // Defense in depth: drop any leftover loader data from a
    // prior navigation that didn't reach `finish_route_mount`
    // (early `missing_window` / `missing_outlet` returns, panics
    // mid-mount, etc). The success path also clears in
    // `finish_route_mount` after setup; clearing both here and
    // there makes "every navigation starts with an empty slot"
    // an enforced invariant rather than a contract that depends
    // on every code path remembering to clear.
    clear_pending_loader_data();

    // Mark this navigation. Any loader spawned by an earlier
    // `mount_current` captured the previous token at start; when it
    // resolves it'll find this new value and drop its result. The
    // token bump is the cheapest possible signal — actual abort of
    // an in-flight `fetch::call` will land with Slice G when the
    // middleware chain plumbs `AbortSignal` end-to-end.
    let nav_token = bump_route_token();

    let has_route_hooks = crate::plugin::has_route_navigation_hooks();
    let start_ms = has_route_hooks.then(js_sys::Date::now);
    let Some(win) = web_sys::window() else {
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                path: String::new(),
                route_pattern: None,
                component: None,
                reason: "missing_window",
                duration_ms: 0.0,
            });
        }
        return;
    };
    let loc = win.location();
    let path = loc.pathname().unwrap_or_else(|_| "/".into());
    let search = loc.search().unwrap_or_default();

    // Match.
    let matched = match_route(&path, has_route_hooks);
    let component_name = matched.as_ref().map(|m| m.component_name);
    let route_pattern = matched.as_ref().and_then(|m| m.route_pattern);
    let params = matched
        .as_ref()
        .map(|m| m.params.clone())
        .unwrap_or_default();
    if has_route_hooks {
        crate::plugin::emit(crate::plugin::RouteNavigationStarted {
            path: path.clone(),
            route_pattern,
            component: component_name,
        });
    }

    let query = parse_query(&search);

    if let Some(matched) = &matched {
        if let Some(decision) = evaluate_guards(matched, &path, &query) {
            match decision {
                RouteGuardDecision::Allow => {}
                RouteGuardDecision::Redirect(target) => {
                    if has_route_hooks {
                        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                            path: path.clone(),
                            route_pattern,
                            component: Some(matched.component_name),
                            reason: "guard_redirected",
                            duration_ms: elapsed_since(start_ms),
                        });
                    }
                    let target = target.into_path();
                    if target != path {
                        navigate(&target);
                    }
                    return;
                }
                RouteGuardDecision::Reject(rejection) => {
                    dispatch_route_rejection(
                        matched,
                        &path,
                        &query,
                        route_pattern,
                        &rejection,
                        RejectionSource::Guard,
                        has_route_hooks,
                        start_ms,
                    );
                    return;
                }
            }
        }

        // Guards passed (or there are no guards). If a loader is
        // registered for this route, defer the rest of the mount
        // until the loader resolves; the loader path runs through
        // `spawn_local` so the synchronous fast path is still
        // available for routes without loaders.
        if let Some(loader) = matched.config.loader.clone() {
            let loader_ctx = LoaderContext {
                path: path.clone(),
                params: params.clone(),
                query: query.clone(),
                matched_pattern: route_pattern,
                navigation_token: nav_token,
            };
            update_route_state(&path, &params, query.clone());
            let matched_for_async = matched.clone();
            let path_for_async = path.clone();
            let params_for_async = params.clone();
            let query_for_async = query.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = loader.run(loader_ctx).await;
                if !is_token_current(nav_token) {
                    // Navigation moved on while the loader was in
                    // flight. Drop the result rather than paint
                    // stale data over whatever the new navigation
                    // mounted; emitting a `RouteNavigationFailed`
                    // event would be misleading because the new
                    // navigation is healthy and already running.
                    clear_pending_loader_data();
                    return;
                }
                match result {
                    Ok(data) => {
                        put_pending_loader_data(data);
                        finish_route_mount(
                            Some(matched_for_async.component_name),
                            route_pattern,
                            &path_for_async,
                            &params_for_async,
                            has_route_hooks,
                            start_ms,
                        );
                    }
                    Err(err) => {
                        // Loader-produced rejection: dispatch through
                        // the same chain guard rejections use so a
                        // single auth handler covers both surfaces.
                        // The rejection itself is identical, but the
                        // `RouteNavigationFailed` event carries a
                        // loader-side reason ("loader_unauthorized"
                        // etc.) so observability can split the two.
                        let rejection = err.to_rejection();
                        clear_pending_loader_data();
                        dispatch_route_rejection(
                            &matched_for_async,
                            &path_for_async,
                            &query_for_async,
                            route_pattern,
                            &rejection,
                            RejectionSource::Loader,
                            has_route_hooks,
                            start_ms,
                        );
                    }
                }
            });
            return;
        }
    }

    update_route_state(&path, &params, query);
    finish_route_mount(
        component_name,
        route_pattern,
        &path,
        &params,
        has_route_hooks,
        start_ms,
    );
}

fn update_route_state(
    path: &str,
    params: &HashMap<String, String>,
    query: HashMap<String, String>,
) {
    ROUTE_STATE_RC.with(|cell| {
        if let Some(s) = cell.get() {
            let mut st = s.borrow_mut();
            st.path = path.to_string();
            st.params = params.clone();
            st.query = query;
        }
    });
    ROUTE_SCOPE.with(|cell| {
        if let Some(scope) = cell.get() {
            trigger_scope(scope.id);
        }
    });
}

/// Synchronous tail of the navigation pipeline shared by the
/// no-loader path (called inline from `mount_current`) and the
/// loader path (called from inside `spawn_local` after the loader
/// resolves successfully). Paints the matched component into the
/// outlet.
fn finish_route_mount(
    component_name: Option<&'static str>,
    route_pattern: Option<&'static str>,
    path: &str,
    params: &HashMap<String, String>,
    has_route_hooks: bool,
    start_ms: Option<f64>,
) {
    // Devtools hook — fires on every resolved route change, even
    // when there's no matching component (404). The router panel
    // uses this to build its recent-history view.
    #[cfg(feature = "devtools")]
    crate::devtools::hooks::fire_route_change(path, params);

    let Some(name) = component_name else {
        // No registered route matched. If the app configured a
        // dedicated 404 component (the lower-friction alternative
        // to a `*` wildcard route), mount it here. Otherwise the
        // outlet is left in its prior state — guards / loader
        // never ran because the route doesn't exist.
        if let Some(fallback) = NOT_FOUND_COMPONENT.with(|cell| cell.get()) {
            if mount_component_into_outlet(fallback) && has_route_hooks {
                crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                    path: path.to_string(),
                    route_pattern: None,
                    component: Some(fallback),
                    duration_ms: elapsed_since(start_ms),
                });
                return;
            }
        }
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                path: path.to_string(),
                route_pattern,
                component: None,
                duration_ms: elapsed_since(start_ms),
            });
        }
        return;
    };

    let Some(win) = web_sys::window() else { return };
    // Paint into the outlet. `replace_children` removes the previous
    // page's subtree, which the MutationObserver turns into effect +
    // scope cleanup via `mount::release_subtree`.
    let Some(outlet) = OUTLET.with(|o| o.borrow().clone()) else {
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                path: path.to_string(),
                route_pattern,
                component: Some(name),
                reason: "missing_outlet",
                duration_ms: elapsed_since(start_ms),
            });
        }
        clear_pending_loader_data();
        return;
    };
    let Some(doc) = win.document() else {
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                path: path.to_string(),
                route_pattern,
                component: Some(name),
                reason: "missing_document",
                duration_ms: elapsed_since(start_ms),
            });
        }
        clear_pending_loader_data();
        return;
    };

    // Build `<name key="value" ...>`; the mount handles the mount.
    let el = match doc.create_element(name) {
        Ok(e) => e,
        Err(_) => {
            if has_route_hooks {
                crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                    path: path.to_string(),
                    route_pattern,
                    component: Some(name),
                    reason: "create_element_failed",
                    duration_ms: elapsed_since(start_ms),
                });
            }
            clear_pending_loader_data();
            return;
        }
    };
    for (k, v) in params {
        let _ = el.set_attribute(k, v);
    }
    outlet.replace_children_with_node_1(el.as_ref());
    // RFC-058 Phase 6.5 — drive the route component's mount through
    // the compiled-only entry. The mount's recursive directive
    // scan is gone; route components must be `#[component]` types
    // so their template plan installs every binding/listener via
    // the macro-emitted entries.
    mount::mount_child_component(&el, name);
    mount::finalize_compiled_subtree(&el);

    // The component's `Loader<T>` extractor (if any) consumed the
    // pending slot during setup; for routes without a loader the
    // slot was never populated. Either way, drop any leftover so
    // the next navigation starts fresh — defensive against
    // `Option<Loader<T>>` extractors that opt out of consuming.
    clear_pending_loader_data();

    if has_route_hooks {
        crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
            path: path.to_string(),
            route_pattern,
            component: Some(name),
            duration_ms: elapsed_since(start_ms),
        });
    }
}

/// Run the rejection chain for a guard- or loader-produced
/// `RouteRejection` and apply the resulting action (Redirect /
/// Paint / AbortNavigation). Used by both the synchronous guard
/// path and the asynchronous loader-error path.
///
/// Per RFC-078 §5.10.7 the emitted `RouteNavigationFailed` event
/// carries the **rejection's** stable identifier — derived from
/// the variant + `source` — *regardless* of which action the
/// installed handler chose. A loader `Unauthorized` rejection
/// emits `loader_unauthorized` whether the handler painted a
/// surface, redirected to `/login`, or aborted; the action shape
/// is irrelevant to the closed-set reason taxonomy. Only the
/// synchronous `RouteGuardDecision::Redirect` path emits
/// `guard_redirected`, since that's a guard outcome with no
/// underlying rejection variant to reference.
#[allow(clippy::too_many_arguments)]
fn dispatch_route_rejection(
    matched: &RouteMatch,
    path: &str,
    query: &HashMap<String, String>,
    route_pattern: Option<&'static str>,
    rejection: &RouteRejection,
    source: RejectionSource,
    has_route_hooks: bool,
    start_ms: Option<f64>,
) {
    let action = handle_route_rejection(matched, path, query, rejection).unwrap_or_else(|| {
        RouteRejectionAction::Paint(RouteErrorSurface::for_rejection(rejection))
    });
    if has_route_hooks {
        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
            path: path.to_string(),
            route_pattern,
            component: Some(matched.component_name),
            reason: rejection.reason(source),
            duration_ms: elapsed_since(start_ms),
        });
    }
    match action {
        RouteRejectionAction::Redirect(target) => {
            let target = target.into_path();
            if target != path {
                navigate(&target);
            }
        }
        RouteRejectionAction::Paint(surface) => {
            paint_route_error_surface(&surface);
        }
        RouteRejectionAction::AbortNavigation => {}
    }
}

/// Find the first matching route's component name + params.
fn match_route(path: &str, include_pattern: bool) -> Option<RouteMatch> {
    ROUTES.with(|r| {
        let routes = r.borrow();
        // Specific routes first; wildcards as a fallback.
        for route in routes.iter().filter(|r| !r.is_wildcard()) {
            if let Some(params) = route.match_path(path) {
                return Some(RouteMatch {
                    component_name: route.component_name,
                    route_pattern: include_pattern.then_some(route.pattern),
                    params,
                    config: route.config.clone(),
                });
            }
        }
        for route in routes.iter().filter(|r| r.is_wildcard()) {
            if let Some(params) = route.match_path(path) {
                return Some(RouteMatch {
                    component_name: route.component_name,
                    route_pattern: include_pattern.then_some(route.pattern),
                    params,
                    config: route.config.clone(),
                });
            }
        }
        None
    })
}

fn evaluate_guards(
    matched: &RouteMatch,
    path: &str,
    query: &HashMap<String, String>,
) -> Option<RouteGuardDecision> {
    if matched.config.guards.is_empty() {
        return None;
    }
    let ctx = RouteContext {
        path,
        params: &matched.params,
        query,
        matched_pattern: matched.route_pattern,
    };
    for guard in &matched.config.guards {
        match guard.decide(&ctx) {
            RouteGuardDecision::Allow => {}
            other => return Some(other),
        }
    }
    Some(RouteGuardDecision::Allow)
}

fn handle_route_rejection(
    matched: &RouteMatch,
    path: &str,
    query: &HashMap<String, String>,
    rejection: &RouteRejection,
) -> Option<RouteRejectionAction> {
    let ctx = RouteRejectionContext {
        path,
        params: &matched.params,
        query,
        matched_pattern: matched.route_pattern,
    };
    ROUTE_REJECTION_HANDLERS.with(|registered| {
        for handler in registered.borrow().iter() {
            if let Some(action) = handler.handle(&ctx, rejection) {
                return Some(action);
            }
        }
        None
    })
}

fn paint_route_error_surface(surface: &RouteErrorSurface) {
    // App-configured override wins. Mount the user's component
    // through the normal route-mount path so it has a full
    // `#[component]` surface (template, handlers, lifecycle).
    if let Some(name) = ROUTE_ERROR_COMPONENT.with(|cell| cell.get()) {
        if mount_component_into_outlet(name) {
            return;
        }
    }
    paint_default_route_error_surface(surface);
}

/// Build the built-in minimal HTML banner. Used when
/// [`ROUTE_ERROR_COMPONENT`] hasn't been configured (or its mount
/// failed), so the framework still surfaces *something* on a
/// route rejection rather than leaving stale UI on screen.
fn paint_default_route_error_surface(surface: &RouteErrorSurface) {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(outlet) = OUTLET.with(|o| o.borrow().clone()) else {
        return;
    };
    let Ok(root) = doc.create_element("div") else {
        return;
    };
    let _ = root.set_attribute("data-pocopine-route-error", "");
    let _ = root.set_attribute(
        "style",
        "padding:24px;border:1px solid #d0d5dd;border-radius:8px;\
         font-family:system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;\
         color:#101828;background:#fff;",
    );
    let Ok(title) = doc.create_element("h2") else {
        return;
    };
    let _ = title.set_attribute("style", "margin:0 0 8px 0;font-size:20px;line-height:1.3;");
    title.set_text_content(Some(surface.title));
    let Ok(message) = doc.create_element("p") else {
        return;
    };
    let _ = message.set_attribute(
        "style",
        "margin:0;color:#475467;font-size:14px;line-height:1.5;",
    );
    message.set_text_content(Some(surface.message));
    let _ = root.append_child(&title);
    let _ = root.append_child(&message);
    outlet.replace_children_with_node_1(root.as_ref());
}

/// Mount a registered component by name into the current outlet,
/// replacing whatever was there. Returns `true` when the mount
/// succeeded; `false` means the platform/document/outlet wasn't
/// available or the element couldn't be created — the caller
/// should fall back to whatever its non-override path is.
fn mount_component_into_outlet(name: &'static str) -> bool {
    let Some(win) = web_sys::window() else {
        return false;
    };
    let Some(doc) = win.document() else {
        return false;
    };
    let Some(outlet) = OUTLET.with(|o| o.borrow().clone()) else {
        return false;
    };
    let Ok(el) = doc.create_element(name) else {
        return false;
    };
    outlet.replace_children_with_node_1(el.as_ref());
    mount::mount_child_component(&el, name);
    mount::finalize_compiled_subtree(&el);
    true
}

fn elapsed_since(start_ms: Option<f64>) -> f64 {
    let Some(start_ms) = start_ms else { return 0.0 };
    let elapsed = js_sys::Date::now() - start_ms;
    if elapsed.is_finite() && elapsed >= 0.0 {
        elapsed
    } else {
        0.0
    }
}

fn parse_query(search: &str) -> HashMap<String, String> {
    let stripped = search.strip_prefix('?').unwrap_or(search);
    let mut out = HashMap::new();
    if stripped.is_empty() {
        return out;
    }
    for pair in stripped.split('&') {
        let mut it = pair.splitn(2, '=');
        let Some(key) = it.next() else { continue };
        if key.is_empty() {
            continue;
        }
        let value = it.next().unwrap_or("");
        out.insert(url_decode(key), url_decode(value));
    }
    out
}

fn url_decode(s: &str) -> String {
    // Minimal percent-decode + `+` → ` `. js_sys has URLSearchParams
    // but avoiding the extra feature for the host-compat path.
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex_nib(bytes[i + 1]), hex_nib(bytes[i + 2])) {
                out.push((a << 4) | b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nib(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{RouteRejection, RouteTarget};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[test]
    fn literal_match() {
        let r = Route::parse("/about", "about", RouteRuntimeConfig::default());
        assert!(r.match_path("/about").is_some());
        assert!(r.match_path("/").is_none());
        assert!(r.match_path("/about/extra").is_none());
    }

    #[test]
    fn param_capture() {
        let r = Route::parse("/blog/:id", "blog", RouteRuntimeConfig::default());
        let caps = r.match_path("/blog/42").unwrap();
        assert_eq!(caps.get("id"), Some(&"42".to_string()));
    }

    #[test]
    fn mixed_segments() {
        let r = Route::parse(
            "/users/:uid/posts/:pid",
            "post",
            RouteRuntimeConfig::default(),
        );
        let caps = r.match_path("/users/7/posts/99").unwrap();
        assert_eq!(caps.get("uid"), Some(&"7".to_string()));
        assert_eq!(caps.get("pid"), Some(&"99".to_string()));
    }

    #[test]
    fn wildcard_matches_anything() {
        let r = Route::parse("*", "not-found", RouteRuntimeConfig::default());
        assert!(r.match_path("/").is_some());
        assert!(r.match_path("/nope/anywhere").is_some());
    }

    #[test]
    fn root_path() {
        let r = Route::parse("/", "home", RouteRuntimeConfig::default());
        assert!(r.match_path("/").is_some());
        assert!(r.match_path("/about").is_none());
    }

    #[test]
    fn query_parsing() {
        let q = parse_query("?name=Ada&hello=world%20%26%20mars");
        assert_eq!(q.get("name"), Some(&"Ada".to_string()));
        assert_eq!(q.get("hello"), Some(&"world & mars".to_string()));
    }

    #[test]
    fn guard_context_contains_route_match_data() {
        let guard: Rc<dyn RouteGuard> = Rc::new(|ctx: &RouteContext<'_>| {
            assert_eq!(ctx.path, "/users/7");
            assert_eq!(ctx.params.get("uid"), Some(&"7".to_string()));
            assert_eq!(ctx.query.get("tab"), Some(&"profile".to_string()));
            assert_eq!(ctx.matched_pattern, Some("/users/:uid"));
            RouteGuardDecision::Redirect(RouteTarget::path("/login"))
        });
        let mut params = HashMap::new();
        params.insert("uid".to_string(), "7".to_string());
        let mut query = HashMap::new();
        query.insert("tab".to_string(), "profile".to_string());
        let matched = RouteMatch {
            component_name: "user-page",
            route_pattern: Some("/users/:uid"),
            params,
            config: RouteRuntimeConfig {
                guards: vec![guard],
                loader: None,
            },
        };

        assert_eq!(
            evaluate_guards(&matched, "/users/7", &query),
            Some(RouteGuardDecision::Redirect(RouteTarget::path("/login")))
        );
    }

    #[test]
    fn guards_stop_at_first_rejection() {
        let second_guard_called = Rc::new(Cell::new(false));
        let first_guard: Rc<dyn RouteGuard> = Rc::new(|_: &RouteContext<'_>| {
            RouteGuardDecision::Reject(RouteRejection::Unauthorized)
        });
        let second_guard_called_for_guard = Rc::clone(&second_guard_called);
        let second_guard: Rc<dyn RouteGuard> = Rc::new(move |_: &RouteContext<'_>| {
            second_guard_called_for_guard.set(true);
            RouteGuardDecision::Allow
        });
        let matched = RouteMatch {
            component_name: "admin",
            route_pattern: Some("/admin"),
            params: HashMap::new(),
            config: RouteRuntimeConfig {
                guards: vec![first_guard, second_guard],
                loader: None,
            },
        };

        assert_eq!(
            evaluate_guards(&matched, "/admin", &HashMap::new()),
            Some(RouteGuardDecision::Reject(RouteRejection::Unauthorized))
        );
        assert!(!second_guard_called.get());
    }

    #[test]
    fn route_rejection_handlers_run_until_action() {
        let first_called = Rc::new(Cell::new(false));
        let second_called = Rc::new(Cell::new(false));
        let first_called_for_handler = Rc::clone(&first_called);
        let second_called_for_handler = Rc::clone(&second_called);
        let first: Rc<dyn RouteRejectionHandler> =
            Rc::new(move |_: &RouteRejectionContext<'_>, _: &RouteRejection| {
                first_called_for_handler.set(true);
                None
            });
        let second: Rc<dyn RouteRejectionHandler> = Rc::new(
            move |ctx: &RouteRejectionContext<'_>, rejection: &RouteRejection| {
                second_called_for_handler.set(true);
                assert_eq!(ctx.path, "/admin");
                assert_eq!(ctx.params.get("section"), Some(&"users".to_string()));
                assert_eq!(ctx.query.get("tab"), Some(&"active".to_string()));
                assert_eq!(ctx.matched_pattern, Some("/admin/:section"));
                assert_eq!(rejection, &RouteRejection::Unauthorized);
                Some(RouteRejectionAction::Redirect(RouteTarget::path("/login")))
            },
        );
        set_route_rejection_handlers(vec![first, second]);

        let mut params = HashMap::new();
        params.insert("section".to_string(), "users".to_string());
        let mut query = HashMap::new();
        query.insert("tab".to_string(), "active".to_string());
        let matched = RouteMatch {
            component_name: "admin",
            route_pattern: Some("/admin/:section"),
            params,
            config: RouteRuntimeConfig::default(),
        };

        assert_eq!(
            handle_route_rejection(&matched, "/admin", &query, &RouteRejection::Unauthorized),
            Some(RouteRejectionAction::Redirect(RouteTarget::path("/login")))
        );
        assert!(first_called.get());
        assert!(second_called.get());

        set_route_rejection_handlers(Vec::new());
    }

    #[test]
    fn route_token_advances_on_each_bump() {
        let before = RouteToken::current();
        let bumped = bump_route_token();
        assert_ne!(before, bumped);
        assert!(is_token_current(bumped));
        assert!(!is_token_current(before));
    }

    #[test]
    fn route_token_is_current_only_for_latest() {
        // Two consecutive navigations: each bump produces a fresh
        // token, and only the latest is "current". The earlier
        // token's loader (if any) finds itself stale on resolve.
        let first = bump_route_token();
        assert!(is_token_current(first));
        let second = bump_route_token();
        assert_ne!(first, second);
        assert!(is_token_current(second));
        assert!(!is_token_current(first));
    }

    #[test]
    fn route_token_is_copy_and_eq() {
        let t = bump_route_token();
        let copy = t;
        assert_eq!(t, copy);
        // PartialEq is value-based, not pointer-based.
        let again = RouteToken::current();
        assert_eq!(t, again);
    }
}
