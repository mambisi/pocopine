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
    push_encoded_route_path_segment, IntoRouteTarget, Loader, LoaderContext, PageMeta,
    PageMetaContext, PageMetaFactory, PageMetaTag, Prefetch, RejectionSource, RouteContext,
    RouteErrorSurface, RouteGuard, RouteGuardDecision, RouteLoader, RouteMeta, RouteName,
    RouteQuery, RouteRejection, RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler,
    RouteTarget, RouteTargetError,
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
                    params.insert(name.clone(), url_decode_path_segment(got));
                }
                Segment::Wildcard => {}
            }
        }
        Some(params)
    }
}

#[derive(Clone, Default)]
pub(crate) struct RouteRuntimeConfig {
    pub(crate) name: Option<RouteName>,
    pub(crate) meta: RouteMeta,
    pub(crate) page_meta: Option<PageMetaFactory>,
    pub(crate) guards: Vec<Rc<dyn RouteGuard>>,
    pub(crate) loader: Option<Rc<dyn RouteLoader>>,
    pub(crate) prefetch: Prefetch,
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
    /// Abort controller for the currently in-flight route loader.
    /// Navigation supersession aborts this before the token advances
    /// so loader-owned `fetch::call` requests stop in the browser,
    /// not just at the Rust result boundary.
    static ACTIVE_LOADER_ABORT: RefCell<Option<(RouteToken, web_sys::AbortController)>> =
        const { RefCell::new(None) };
    /// `App::route_error_component<C>()` recorded component name,
    /// painted by `paint_route_error` when set instead of the
    /// built-in HTML banner.
    static ROUTE_ERROR_COMPONENT: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// `App::not_found_component<C>()` recorded component name,
    /// mounted by `finish_route_mount` when no route (and no
    /// wildcard) matched.
    static NOT_FOUND_COMPONENT: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// URL key for a navigation stopped by `RouteGuardDecision::Pending`.
    /// When the external prerequisite later calls `reevaluate_current`,
    /// an `Allow` decision remounts this route instead of treating the
    /// already-empty outlet as a valid mounted page.
    static PENDING_GUARD_NAVIGATION: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Loader data produced by an explicit route prefetch. Keyed by
    /// full app-local URL (`path?query`) and consumed by the next
    /// navigation to the same key.
    static PREFETCHED_LOADER_DATA: RefCell<HashMap<String, Rc<dyn std::any::Any>>> =
        RefCell::new(HashMap::new());
    /// In-flight loader prefetches keyed by `path?query`. The
    /// generation prevents a late prefetch from populating the cache
    /// after a real navigation superseded it.
    static PREFETCH_IN_FLIGHT: RefCell<HashMap<String, (u64, Option<web_sys::AbortController>)>> =
        RefCell::new(HashMap::new());
    static PREFETCH_TOKEN: Cell<u64> = const { Cell::new(0) };
    /// Document title captured before Pocopine first applies page
    /// metadata. Restored for routes that do not provide a title.
    static BASE_DOCUMENT_TITLE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Normalized location returned by programmatic navigation.
#[derive(Clone, Debug)]
pub struct RouteLocation {
    pub path: String,
    pub full_path: String,
    pub query: HashMap<String, String>,
    pub hash: Option<String>,
    pub params: HashMap<String, String>,
    pub route_pattern: Option<&'static str>,
    pub component: Option<&'static str>,
    pub meta: RouteMeta,
}

/// Why a programmatic navigation could not be accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationFailure {
    Duplicated,
    InvalidTarget(RouteTargetError),
    MissingWindow,
    HistoryRejected,
    GuardPending,
    GuardRejected(RouteRejection),
    Redirected(RouteTarget),
    MountFailed(&'static str),
}

pub type NavigationResult = Result<RouteLocation, NavigationFailure>;

/// Result of an explicit prefetch request.
#[derive(Clone, Debug)]
pub enum PrefetchResult {
    Ready(RouteLocation),
    Started(RouteLocation),
    Skipped(PrefetchSkip),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefetchSkip {
    InvalidTarget(RouteTargetError),
    NotFound,
    GuardPending,
    GuardRejected(RouteRejection),
    GuardRedirected(RouteTarget),
    LoaderDisabled,
    NoLoader,
    MissingWindow,
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

    fn prefetch(token: u64) -> Self {
        RouteToken(u64::MAX - token)
    }

    fn is_prefetch(self) -> bool {
        self.0 > (u64::MAX / 2)
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
    if token.is_prefetch() {
        return PREFETCH_IN_FLIGHT.with(|cache| {
            let token = u64::MAX - token.0;
            cache
                .borrow()
                .values()
                .any(|(active_token, _)| *active_token == token)
        });
    }
    ROUTE_TOKEN.with(|cell| cell.get() == token.0)
}

/// Crate-internal companion to [`is_token_current`] used by
/// [`crate::app::LoaderContext::is_navigation_active`]. Loader
/// closures don't construct `RouteToken` directly; the router
/// stamps the value into their context.
pub(crate) fn route_token_is_current(token: RouteToken) -> bool {
    is_token_current(token)
}

fn begin_loader_abort(token: RouteToken) -> Option<web_sys::AbortSignal> {
    let controller = web_sys::AbortController::new().ok()?;
    let signal = controller.signal();
    ACTIVE_LOADER_ABORT.with(|cell| {
        *cell.borrow_mut() = Some((token, controller));
    });
    Some(signal)
}

fn abort_active_loader() {
    if let Some((_, controller)) = ACTIVE_LOADER_ABORT.with(|cell| cell.borrow_mut().take()) {
        controller.abort();
    }
}

fn clear_active_loader_abort(token: RouteToken) {
    ACTIVE_LOADER_ABORT.with(|cell| {
        let should_clear = cell
            .borrow()
            .as_ref()
            .map(|(active_token, _)| *active_token == token)
            .unwrap_or(false);
        if should_clear {
            cell.borrow_mut().take();
        }
    });
}

/// Stash a router-produced loader result for the next component
/// mount. Stored as `Rc` so the value can survive multiple
/// extractor reads during the component's mount. The pending slot
/// is overwritten if a previous result was never consumed (e.g.
/// mount aborted before setup ran); the per-scope `LOADER_SLOTS`
/// entries are independent and live until each scope's teardown.
pub(crate) fn put_pending_loader_data(data: Box<dyn std::any::Any>) {
    let rc: Rc<dyn std::any::Any> = Rc::from(data);
    put_pending_loader_rc(rc);
}

fn put_pending_loader_rc(data: Rc<dyn std::any::Any>) {
    PENDING_LOADER_DATA.with(|cell| *cell.borrow_mut() = Some(data));
}

fn take_prefetched_loader_data(key: &str) -> Option<Rc<dyn std::any::Any>> {
    PREFETCHED_LOADER_DATA.with(|cache| cache.borrow_mut().remove(key))
}

fn has_prefetched_loader_data(key: &str) -> bool {
    PREFETCHED_LOADER_DATA.with(|cache| cache.borrow().contains_key(key))
}

fn begin_prefetch(key: &str) -> Option<(u64, Option<web_sys::AbortSignal>)> {
    if PREFETCH_IN_FLIGHT.with(|cache| cache.borrow().contains_key(key)) {
        return None;
    }
    let token = PREFETCH_TOKEN.with(|cell| {
        let next = cell.get().wrapping_add(1);
        cell.set(next);
        next
    });
    let controller = web_sys::AbortController::new().ok();
    let signal = controller.as_ref().map(|controller| controller.signal());
    PREFETCH_IN_FLIGHT.with(|cache| {
        cache
            .borrow_mut()
            .insert(key.to_string(), (token, controller));
    });
    Some((token, signal))
}

fn cancel_prefetch(key: &str) {
    if let Some((_, controller)) = PREFETCH_IN_FLIGHT.with(|cache| cache.borrow_mut().remove(key)) {
        if let Some(controller) = controller {
            controller.abort();
        }
    }
}

fn clear_prefetch_state() {
    PREFETCHED_LOADER_DATA.with(|cache| cache.borrow_mut().clear());
    PREFETCH_IN_FLIGHT.with(|cache| {
        for (_, (_, controller)) in cache.borrow_mut().drain() {
            if let Some(controller) = controller {
                controller.abort();
            }
        }
    });
}

fn put_prefetched_loader_data_if_current(key: String, token: u64, data: Box<dyn std::any::Any>) {
    let still_current = PREFETCH_IN_FLIGHT.with(|cache| {
        let mut cache = cache.borrow_mut();
        let matches = cache
            .get(&key)
            .map(|(active_token, _)| *active_token == token)
            .unwrap_or(false);
        if matches {
            cache.remove(&key);
        }
        matches
    });
    if !still_current {
        return;
    }
    let rc: Rc<dyn std::any::Any> = Rc::from(data);
    PREFETCHED_LOADER_DATA.with(|cache| {
        cache.borrow_mut().insert(key, rc);
    });
}

fn finish_prefetch_without_data(key: &str, token: u64) {
    PREFETCH_IN_FLIGHT.with(|cache| {
        let mut cache = cache.borrow_mut();
        let matches = cache
            .get(key)
            .map(|(active_token, _)| *active_token == token)
            .unwrap_or(false);
        if matches {
            cache.remove(key);
        }
    });
}

fn log_prefetch_loader_error(key: &str, err: &crate::app::LoaderError) {
    web_sys::console::warn_1(&JsValue::from_str(&format!(
        "pocopine route prefetch loader failed for {key}: {err:?}"
    )));
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
        Ok(data) => Loader::from_rc(data),
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
pub(crate) fn release_loader_slot(scope_id: ScopeId) {
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
    clear_prefetch_state();
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
/// page. Kept as the source-compatible shorthand for [`push`].
pub fn navigate(url: &str) {
    let _ = push(url);
}

/// Push a new browser history entry and paint the matched route.
pub fn push(target: impl IntoRouteTarget) -> NavigationResult {
    commit_navigation(target, NavigationMode::Push)
}

/// Replace the current browser history entry and paint the matched route.
pub fn replace(target: impl IntoRouteTarget) -> NavigationResult {
    commit_navigation(target, NavigationMode::Replace)
}

/// Move in browser history. The `popstate` listener installed by
/// [`init`] drives the eventual route mount.
pub fn go(delta: i32) {
    let Some(win) = web_sys::window() else { return };
    if let Ok(history) = win.history() {
        let _ = history.go_with_delta(delta);
    }
}

#[derive(Clone, Copy)]
enum NavigationMode {
    Push,
    Replace,
}

fn commit_navigation(target: impl IntoRouteTarget, mode: NavigationMode) -> NavigationResult {
    let target = target
        .into_route_target()
        .map_err(NavigationFailure::InvalidTarget)?;
    let Some(win) = web_sys::window() else {
        return Err(NavigationFailure::MissingWindow);
    };
    let loc = win.location();
    let current_path = loc.pathname().unwrap_or_else(|_| "/".into());
    let current_search = loc.search().unwrap_or_default();
    if route_navigation_key(&current_path, &current_search)
        == navigation_key_from_url(target.as_str())
    {
        return Err(NavigationFailure::Duplicated);
    }
    let history = win
        .history()
        .map_err(|_| NavigationFailure::HistoryRejected)?;
    let result = match mode {
        NavigationMode::Push => {
            history.push_state_with_url(&JsValue::NULL, "", Some(target.as_str()))
        }
        NavigationMode::Replace => {
            history.replace_state_with_url(&JsValue::NULL, "", Some(target.as_str()))
        }
    };
    result.map_err(|_| NavigationFailure::HistoryRejected)?;
    let location = location_for_url(target.as_str());
    if let Some(failure) = mount_current() {
        return Err(failure);
    }
    Ok(location)
}

/// Explicitly prefetch a route target.
///
/// In the current single-bundle runtime, route/code prefetch is a
/// readiness check. If the matched route opted into
/// `Prefetch::loader()`, its loader runs and caches data for the next
/// navigation to the same URL.
pub fn prefetch(target: impl IntoRouteTarget) -> PrefetchResult {
    let target = match target.into_route_target() {
        Ok(target) => target,
        Err(err) => return PrefetchResult::Skipped(PrefetchSkip::InvalidTarget(err)),
    };
    let Some(_win) = web_sys::window() else {
        return PrefetchResult::Skipped(PrefetchSkip::MissingWindow);
    };
    let location = location_for_url(target.as_str());
    let Some(matched) = match_route(&location.path, true) else {
        return PrefetchResult::Skipped(PrefetchSkip::NotFound);
    };
    if let Some(decision) = evaluate_guards(&matched, &location.path, &location.query) {
        match decision {
            RouteGuardDecision::Allow => {}
            RouteGuardDecision::Pending => {
                return PrefetchResult::Skipped(PrefetchSkip::GuardPending);
            }
            RouteGuardDecision::Reject(rejection) => {
                return PrefetchResult::Skipped(PrefetchSkip::GuardRejected(rejection));
            }
            RouteGuardDecision::Redirect(target) => {
                return PrefetchResult::Skipped(PrefetchSkip::GuardRedirected(target));
            }
        }
    }
    if !matched.config.prefetch.includes_loader() {
        return PrefetchResult::Skipped(PrefetchSkip::LoaderDisabled);
    }
    let Some(loader) = matched.config.loader.clone() else {
        return PrefetchResult::Skipped(PrefetchSkip::NoLoader);
    };
    let key = loader_cache_key_from_url(target.as_str());
    if has_prefetched_loader_data(&key) {
        return PrefetchResult::Ready(location);
    }
    let Some((prefetch_token, abort_signal)) = begin_prefetch(&key) else {
        return PrefetchResult::Started(location);
    };
    let loader_ctx = LoaderContext {
        path: location.path.clone(),
        params: location.params.clone(),
        query: location.query.clone(),
        matched_pattern: location.route_pattern,
        navigation_token: RouteToken::prefetch(prefetch_token),
        abort_signal,
    };
    let location_for_result = location.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match loader.run(loader_ctx).await {
            Ok(data) => {
                put_prefetched_loader_data_if_current(key, prefetch_token, data);
            }
            Err(err) => {
                log_prefetch_loader_error(&key, &err);
                finish_prefetch_without_data(&key, prefetch_token);
            }
        }
    });
    PrefetchResult::Started(location_for_result)
}

pub(crate) fn target_for_name(
    name: RouteName,
    params: &HashMap<String, String>,
    query: RouteQuery,
) -> Result<RouteTarget, RouteTargetError> {
    let mut found: Option<Result<String, RouteTargetError>> = None;
    ROUTES.with(|r| {
        let routes = r.borrow();
        let mut named_routes = routes
            .iter()
            .filter(|route| route.config.name == Some(name));
        let Some(route) = named_routes.next() else {
            found = Some(Err(RouteTargetError::UnknownRouteName(name.as_str())));
            return;
        };
        if named_routes.next().is_some() {
            found = Some(Err(RouteTargetError::DuplicateRouteName(name.as_str())));
            return;
        }
        if route.is_wildcard() {
            found = Some(Err(RouteTargetError::UnbuildablePattern(route.pattern)));
            return;
        }
        let mut path = String::new();
        if route.segments.is_empty() {
            path.push('/');
        } else {
            for segment in &route.segments {
                path.push('/');
                match segment {
                    Segment::Literal(value) => path.push_str(value),
                    Segment::Param(param) => {
                        let Some(value) = params.get(param) else {
                            found = Some(Err(RouteTargetError::MissingParam(param.clone())));
                            return;
                        };
                        if value.is_empty() {
                            found = Some(Err(RouteTargetError::EmptyParam(param.clone())));
                            return;
                        }
                        push_encoded_route_path_segment(value, &mut path);
                    }
                    Segment::Wildcard => {
                        found = Some(Err(RouteTargetError::UnbuildablePattern(route.pattern)));
                        return;
                    }
                }
            }
        }
        query.append_to(&mut path);
        found = Some(Ok(path));
    });
    let path = found.unwrap_or_else(|| Err(RouteTargetError::UnknownRouteName(name.as_str())))?;
    RouteTarget::new(path)
}

fn location_for_url(url: &str) -> RouteLocation {
    let (path, search, hash) = split_route_url(url);
    let query = parse_query(&search);
    let matched = match_route(&path, true);
    let meta = matched
        .as_ref()
        .map(|m| m.config.meta.clone())
        .unwrap_or_default();
    RouteLocation {
        full_path: full_path_from_parts(&path, &search, hash.as_deref()),
        path,
        query,
        hash,
        params: matched
            .as_ref()
            .map(|m| m.params.clone())
            .unwrap_or_default(),
        route_pattern: matched.as_ref().and_then(|m| m.route_pattern),
        component: matched.as_ref().map(|m| m.component_name),
        meta,
    }
}

fn split_route_url(url: &str) -> (String, String, Option<String>) {
    let (before_hash, hash) = match url.split_once('#') {
        Some((before, after)) => (before, Some(after.to_string())),
        None => (url, None),
    };
    let (path, search) = match before_hash.split_once('?') {
        Some((path, query)) => (path, format!("?{query}")),
        None => (before_hash, String::new()),
    };
    let path = if path.is_empty() { "/" } else { path }.to_string();
    (path, search, hash)
}

fn full_path_from_parts(path: &str, search: &str, hash: Option<&str>) -> String {
    let mut full = String::with_capacity(path.len() + search.len() + 1);
    full.push_str(path);
    full.push_str(search);
    if let Some(hash) = hash {
        full.push('#');
        full.push_str(hash);
    }
    full
}

fn navigation_key_from_url(url: &str) -> String {
    let (path, search, _) = split_route_url(url);
    route_navigation_key(&path, &search)
}

fn loader_cache_key_from_url(url: &str) -> String {
    let (path, search, _) = split_route_url(url);
    route_navigation_key(&path, &search)
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
        let _ = mount_current();
    }) as Box<dyn FnMut(Event)>);
    if let Some(win) = web_sys::window() {
        let _ = win.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    }
    cb.forget();

    let _ = mount_current();
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
/// - **`Pending`** → record the current URL and leave the outlet
///   untouched. A later call to [`reevaluate_current`] after the
///   prerequisite completes will either mount the route or take the
///   normal redirect/reject path.
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
            if take_pending_guard_navigation(&path, &search) {
                let _ = mount_current();
            } else {
                // Identity change made the user MORE privileged or
                // the route was already permissive. Existing mount
                // is still valid; re-mounting would tear down
                // legitimate state.
            }
        }
        RouteGuardDecision::Pending => {
            record_pending_guard_navigation(&path, &search);
        }
        RouteGuardDecision::Redirect(_) | RouteGuardDecision::Reject(_) => {
            // Drop the rejected component synchronously so PII it
            // rendered cannot survive the next event-loop turn —
            // the rejection chain paints its outcome AFTER the
            // outlet is empty.
            clear_pending_guard_navigation();
            clear_outlet();
            let _ = mount_current();
        }
    }
}

fn clear_outlet() {
    let Some(outlet) = OUTLET.with(|o| o.borrow().clone()) else {
        return;
    };
    outlet.set_inner_html("");
}

fn mount_current() -> Option<NavigationFailure> {
    ensure_route_scope();

    // Abort any loader request from the previous navigation before
    // advancing the token. The stale-result check below still guards
    // correctness, but this stops browser fetches on the wire.
    abort_active_loader();

    // Defense in depth: drop any leftover loader data from a
    // prior navigation that didn't reach `finish_route_mount`
    // (early `missing_window` / `missing_outlet` returns, panics
    // mid-mount, etc). The success path also clears in
    // `finish_route_mount` after setup; clearing both here and
    // there makes "every navigation starts with an empty slot"
    // an enforced invariant rather than a contract that depends
    // on every code path remembering to clear.
    clear_pending_loader_data();

    // Mark this navigation. The abort above stops loader-owned
    // fetches on the wire; this token bump is the residual stale-result
    // fence for any previous loader that still resolves.
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
        return Some(NavigationFailure::MissingWindow);
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
                RouteGuardDecision::Pending => {
                    record_pending_guard_navigation(&path, &search);
                    if has_route_hooks {
                        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                            path: path.clone(),
                            route_pattern,
                            component: Some(matched.component_name),
                            reason: "guard_pending",
                            duration_ms: elapsed_since(start_ms),
                        });
                    }
                    apply_page_meta(None);
                    return Some(NavigationFailure::GuardPending);
                }
                RouteGuardDecision::Redirect(target) => {
                    clear_pending_guard_navigation();
                    if has_route_hooks {
                        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                            path: path.clone(),
                            route_pattern,
                            component: Some(matched.component_name),
                            reason: "guard_redirected",
                            duration_ms: elapsed_since(start_ms),
                        });
                    }
                    let current_key = route_navigation_key(&path, &search);
                    let target_key = loader_cache_key_from_url(target.as_str());
                    apply_page_meta(None);
                    if target_key != current_key {
                        let _ = push(target.clone());
                    }
                    return Some(NavigationFailure::Redirected(target));
                }
                RouteGuardDecision::Reject(rejection) => {
                    clear_pending_guard_navigation();
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
                    return Some(NavigationFailure::GuardRejected(rejection));
                }
            }
        }

        // Guards passed (or there are no guards). If a loader is
        // registered for this route, defer the rest of the mount
        // until the loader resolves; the loader path runs through
        // `spawn_local` so the synchronous fast path is still
        // available for routes without loaders.
        if let Some(loader) = matched.config.loader.clone() {
            let loader_key = route_navigation_key(&path, &search);
            cancel_prefetch(&loader_key);
            if let Some(data) = take_prefetched_loader_data(&loader_key) {
                put_pending_loader_rc(data);
                update_route_state(&path, &params, query);
                return finish_route_mount(
                    Some(matched.component_name),
                    route_pattern,
                    &path,
                    &params,
                    matched.config.page_meta.clone(),
                    has_route_hooks,
                    start_ms,
                );
            }
            let abort_signal = begin_loader_abort(nav_token);
            let loader_ctx = LoaderContext {
                path: path.clone(),
                params: params.clone(),
                query: query.clone(),
                matched_pattern: route_pattern,
                navigation_token: nav_token,
                abort_signal,
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
                clear_active_loader_abort(nav_token);
                match result {
                    Ok(data) => {
                        put_pending_loader_data(data);
                        finish_route_mount(
                            Some(matched_for_async.component_name),
                            route_pattern,
                            &path_for_async,
                            &params_for_async,
                            matched_for_async.config.page_meta.clone(),
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
            return None;
        }
    }

    update_route_state(&path, &params, query);
    finish_route_mount(
        component_name,
        route_pattern,
        &path,
        &params,
        matched.as_ref().and_then(|m| m.config.page_meta.clone()),
        has_route_hooks,
        start_ms,
    )
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
    page_meta: Option<PageMetaFactory>,
    has_route_hooks: bool,
    start_ms: Option<f64>,
) -> Option<NavigationFailure> {
    clear_pending_guard_navigation();

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
                apply_page_meta(None);
                crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                    path: path.to_string(),
                    route_pattern: None,
                    component: Some(fallback),
                    duration_ms: elapsed_since(start_ms),
                });
                return None;
            }
        }
        apply_page_meta(None);
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                path: path.to_string(),
                route_pattern,
                component: None,
                duration_ms: elapsed_since(start_ms),
            });
        }
        return None;
    };

    let Some(win) = web_sys::window() else {
        return Some(NavigationFailure::MissingWindow);
    };
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
        apply_page_meta(None);
        return Some(NavigationFailure::MountFailed("missing_outlet"));
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
        apply_page_meta(None);
        return Some(NavigationFailure::MountFailed("missing_document"));
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
            apply_page_meta(None);
            return Some(NavigationFailure::MountFailed("create_element_failed"));
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
    apply_page_meta(page_meta);

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
    None
}

fn apply_page_meta(factory: Option<PageMetaFactory>) {
    let Some(doc) = crate::dom::document() else {
        return;
    };
    let base_title = BASE_DOCUMENT_TITLE.with(|cell| {
        let mut title = cell.borrow_mut();
        title.get_or_insert_with(|| doc.title()).clone()
    });
    remove_managed_page_meta_tags(&doc);

    let Some(factory) = factory else {
        doc.set_title(&base_title);
        return;
    };

    let full_path = current_full_path();
    let location = location_for_url(&full_path);
    let ctx = PageMetaContext {
        path: &location.path,
        full_path: &location.full_path,
        params: &location.params,
        query: &location.query,
        hash: location.hash.as_deref(),
        route_pattern: location.route_pattern,
        component: location.component,
    };
    let meta = factory(&ctx);
    apply_page_meta_to_document(&doc, &base_title, &meta);
}

fn current_full_path() -> String {
    let Some(win) = web_sys::window() else {
        return "/".into();
    };
    let loc = win.location();
    let path = loc.pathname().unwrap_or_else(|_| "/".into());
    let search = loc.search().unwrap_or_default();
    let hash = loc
        .hash()
        .ok()
        .filter(|hash| !hash.is_empty())
        .map(|hash| hash.trim_start_matches('#').to_string());
    full_path_from_parts(&path, &search, hash.as_deref())
}

fn remove_managed_page_meta_tags(doc: &web_sys::Document) {
    let Some(head) = doc.head() else {
        return;
    };
    let Ok(nodes) = head.query_selector_all("[data-pocopine-page-meta]") else {
        return;
    };
    for idx in 0..nodes.length() {
        if let Some(node) = nodes.item(idx) {
            let _ = head.remove_child(&node);
        }
    }
}

fn apply_page_meta_to_document(doc: &web_sys::Document, base_title: &str, meta: &PageMeta) {
    doc.set_title(meta.title_text().unwrap_or(base_title));
    let Some(head) = doc.head() else {
        return;
    };

    for tag in meta.meta_tags() {
        let Ok(el) = doc.create_element("meta") else {
            continue;
        };
        mark_page_meta_element(&el);
        match tag {
            PageMetaTag::Name { name, content } => {
                let _ = el.set_attribute("name", name);
                let _ = el.set_attribute("content", content);
            }
            PageMetaTag::Property { property, content } => {
                let _ = el.set_attribute("property", property);
                let _ = el.set_attribute("content", content);
            }
        }
        let _ = head.append_child(&el);
    }

    for link in meta.links() {
        let Ok(el) = doc.create_element("link") else {
            continue;
        };
        mark_page_meta_element(&el);
        let _ = el.set_attribute("rel", &link.rel);
        let _ = el.set_attribute("href", &link.href);
        let _ = head.append_child(&el);
    }
}

fn mark_page_meta_element(el: &Element) {
    let _ = el.set_attribute("data-pocopine-page-meta", "");
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
            apply_page_meta(None);
            let current_key = route_navigation_key_from_query(path, query);
            let target_key = loader_cache_key_from_url(target.as_str());
            if target_key != current_key {
                let _ = push(target);
            }
        }
        RouteRejectionAction::Paint(surface) => {
            paint_route_error_surface(&surface);
        }
        RouteRejectionAction::AbortNavigation => {
            apply_page_meta(None);
        }
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

fn route_navigation_key(path: &str, search: &str) -> String {
    if search.is_empty() {
        path.to_string()
    } else {
        format!("{path}{search}")
    }
}

fn route_navigation_key_from_query(path: &str, query: &HashMap<String, String>) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let mut pairs: Vec<_> = query.iter().collect();
    pairs.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut out = path.to_string();
    out.push('?');
    for (idx, (key, value)) in pairs.into_iter().enumerate() {
        if idx > 0 {
            out.push('&');
        }
        crate::app::push_encoded_route_query_part(key, &mut out);
        out.push('=');
        crate::app::push_encoded_route_query_part(value, &mut out);
    }
    out
}

fn record_pending_guard_navigation(path: &str, search: &str) {
    PENDING_GUARD_NAVIGATION.with(|slot| {
        *slot.borrow_mut() = Some(route_navigation_key(path, search));
    });
}

fn clear_pending_guard_navigation() {
    PENDING_GUARD_NAVIGATION.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

fn take_pending_guard_navigation(path: &str, search: &str) -> bool {
    let key = route_navigation_key(path, search);
    PENDING_GUARD_NAVIGATION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_deref() == Some(key.as_str()) {
            *slot = None;
            true
        } else {
            false
        }
    })
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
    apply_page_meta(None);
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
    percent_decode(s, true)
}

fn url_decode_path_segment(s: &str) -> String {
    percent_decode(s, false)
}

fn percent_decode(s: &str, plus_as_space: bool) -> String {
    let s = if plus_as_space {
        s.replace('+', " ")
    } else {
        s.to_string()
    };
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
    use crate::app::{RouteMetaKey, RouteRejection, RouteTarget};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn reset_router_for_test() {
        ROUTES.with(|routes| routes.borrow_mut().clear());
        ROUTE_REJECTION_HANDLERS.with(|handlers| handlers.borrow_mut().clear());
        PENDING_LOADER_DATA.with(|slot| slot.borrow_mut().take());
        LOADER_SLOTS.with(|slots| slots.borrow_mut().clear());
        PENDING_GUARD_NAVIGATION.with(|slot| slot.borrow_mut().take());
        clear_prefetch_state();
        ROUTE_TOKEN.with(|token| token.set(0));
        PREFETCH_TOKEN.with(|token| token.set(0));
        BASE_DOCUMENT_TITLE.with(|title| title.borrow_mut().take());
    }

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
    fn param_capture_decodes_percent_encoded_segments() {
        let r = Route::parse("/blog/:id", "blog", RouteRuntimeConfig::default());
        let caps = r.match_path("/blog/user%2042").unwrap();
        assert_eq!(caps.get("id"), Some(&"user 42".to_string()));

        let slash = r.match_path("/blog/user%2F42").unwrap();
        assert_eq!(slash.get("id"), Some(&"user/42".to_string()));
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
    fn named_route_target_replaces_params_and_query() {
        reset_router_for_test();
        const NAME: RouteName = RouteName::new("router.tests.named");

        register_route_with_config(
            "/named/:id",
            "named-route",
            RouteRuntimeConfig {
                name: Some(NAME),
                ..RouteRuntimeConfig::default()
            },
        );

        let target = RouteTarget::named(NAME)
            .param("id", "user 42")
            .query("tab", "a b")
            .build()
            .unwrap();

        assert_eq!(target.into_path(), "/named/user%2042?tab=a%20b");
    }

    #[test]
    fn named_route_target_reports_missing_param() {
        reset_router_for_test();
        const NAME: RouteName = RouteName::new("router.tests.missing-param");

        register_route_with_config(
            "/needs/:id",
            "needs-param",
            RouteRuntimeConfig {
                name: Some(NAME),
                ..RouteRuntimeConfig::default()
            },
        );

        assert_eq!(
            RouteTarget::named(NAME).build(),
            Err(RouteTargetError::MissingParam("id".into()))
        );
    }

    #[test]
    fn named_route_target_reports_empty_param() {
        reset_router_for_test();
        const NAME: RouteName = RouteName::new("router.tests.empty-param");

        register_route_with_config(
            "/needs/:id",
            "needs-param",
            RouteRuntimeConfig {
                name: Some(NAME),
                ..RouteRuntimeConfig::default()
            },
        );

        assert_eq!(
            RouteTarget::named(NAME).param("id", "").build(),
            Err(RouteTargetError::EmptyParam("id".into()))
        );
    }

    #[test]
    fn named_route_target_reports_duplicate_name() {
        reset_router_for_test();
        const NAME: RouteName = RouteName::new("router.tests.duplicate");

        for pattern in ["/one/:id", "/two/:id"] {
            register_route_with_config(
                pattern,
                "duplicate",
                RouteRuntimeConfig {
                    name: Some(NAME),
                    ..RouteRuntimeConfig::default()
                },
            );
        }

        assert_eq!(
            RouteTarget::named(NAME).param("id", "42").build(),
            Err(RouteTargetError::DuplicateRouteName(NAME.as_str()))
        );
    }

    #[test]
    fn route_location_splits_path_query_and_hash() {
        reset_router_for_test();
        let loc = location_for_url("/reports?tab=active#summary");

        assert_eq!(loc.path, "/reports");
        assert_eq!(loc.full_path, "/reports?tab=active#summary");
        assert_eq!(loc.query.get("tab"), Some(&"active".to_string()));
        assert_eq!(loc.hash.as_deref(), Some("summary"));
    }

    #[test]
    fn route_location_exposes_route_meta() {
        reset_router_for_test();
        const SECTION: RouteMetaKey<&'static str> = RouteMetaKey::new("section");
        let mut meta = RouteMeta::new();
        meta.insert(SECTION, "reports");

        register_route_with_config(
            "/reports-meta",
            "reports-meta",
            RouteRuntimeConfig {
                meta,
                ..RouteRuntimeConfig::default()
            },
        );

        let loc = location_for_url("/reports-meta");

        assert_eq!(loc.meta.get(SECTION).copied(), Some("reports"));
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
    fn navigation_key_ignores_fragment() {
        assert_eq!(navigation_key_from_url("/foo#section"), "/foo");
        assert_eq!(navigation_key_from_url("/foo?tab=a#section"), "/foo?tab=a");
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
                ..RouteRuntimeConfig::default()
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
                ..RouteRuntimeConfig::default()
            },
        };

        assert_eq!(
            evaluate_guards(&matched, "/admin", &HashMap::new()),
            Some(RouteGuardDecision::Reject(RouteRejection::Unauthorized))
        );
        assert!(!second_guard_called.get());
    }

    #[test]
    fn guards_stop_at_pending() {
        let second_guard_called = Rc::new(Cell::new(false));
        let first_guard: Rc<dyn RouteGuard> =
            Rc::new(|_: &RouteContext<'_>| RouteGuardDecision::Pending);
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
                ..RouteRuntimeConfig::default()
            },
        };

        assert_eq!(
            evaluate_guards(&matched, "/admin", &HashMap::new()),
            Some(RouteGuardDecision::Pending)
        );
        assert!(!second_guard_called.get());
    }

    #[test]
    fn route_rejection_handlers_run_until_action() {
        reset_router_for_test();
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
        reset_router_for_test();
        let before = RouteToken::current();
        let bumped = bump_route_token();
        assert_ne!(before, bumped);
        assert!(is_token_current(bumped));
        assert!(!is_token_current(before));
    }

    #[test]
    fn route_token_is_current_only_for_latest() {
        reset_router_for_test();
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
        reset_router_for_test();
        let t = bump_route_token();
        let copy = t;
        assert_eq!(t, copy);
        // PartialEq is value-based, not pointer-based.
        let again = RouteToken::current();
        assert_eq!(t, again);
    }
}
