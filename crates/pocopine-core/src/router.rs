//! Client-side SPA router.
//!
//! Shape of the runtime:
//!
//! * User-declared route records live in a parent-linked `Vec<Route>`
//!   behind a thread-local. Flat `App::route` records have no parent;
//!   `App::layout` builds relative/index children.
//! * Compiled `<pp-outlet>` sentinels register by owning route scope and
//!   depth. A nested outlet therefore cannot replace the app's root outlet.
//! * Navigation matches the deepest route chain and renders it in depth
//!   order through the same dynamic-component region as `<pp-component>`.
//!   Sibling navigation preserves the common mounted prefix.
//! * A synthetic `RouteState` scope drives the `$route` magic. The
//!   router calls [`trigger_scope`] on its id so any template binding
//!   reading `$route.path` / `$route.params.<name>` / `$route.query.<name>`
//!   re-evaluates when the URL changes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect};
use once_cell::unsync::OnceCell;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{Element, Event, MouseEvent, Node};

use crate::app::{
    IntoRouteTarget, Loader, LoaderContext, PageMeta, PageMetaContext, PageMetaFactory,
    PageMetaTag, Prefetch, RejectionSource, RouteContext, RouteErrorSurface, RouteGuard,
    RouteGuardDecision, RouteLoader, RouteMeta, RouteName, RouteQuery, RouteRejection,
    RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler, RouteTarget,
    RouteTargetError, push_encoded_route_path_segment,
};
use crate::mount;
use crate::reactive::{ScopeId, trigger_scope};
use crate::scope::{ComponentState, Scope};

pub(crate) mod locale;
mod return_to;

pub use return_to::ReturnTo;

// ─── route parsing ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Segment {
    Literal(String),
    Param(String),
    RestParam(String),
    Wildcard,
}

/// Stable identity of one registered route record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RouteRecordId(usize);

impl RouteRecordId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone)]
pub struct Route {
    pub id: RouteRecordId,
    pub parent: Option<RouteRecordId>,
    pub outlet_depth: usize,
    pub pattern: &'static str,
    full_pattern: String,
    segments: Vec<Segment>,
    own_params: Vec<String>,
    pub component_name: &'static str,
    config: RouteRuntimeConfig,
}

impl Route {
    fn parse_root(
        id: RouteRecordId,
        pattern: &'static str,
        component_name: &'static str,
        config: RouteRuntimeConfig,
    ) -> Self {
        Self::parse_record(
            id,
            None,
            0,
            pattern,
            pattern.to_string(),
            component_name,
            config,
        )
    }

    fn parse_child(
        id: RouteRecordId,
        parent: &Route,
        pattern: &'static str,
        component_name: &'static str,
        config: RouteRuntimeConfig,
    ) -> Self {
        let full_pattern = join_route_pattern(&parent.full_pattern, pattern);
        Self::parse_record(
            id,
            Some(parent.id),
            parent.outlet_depth + 1,
            pattern,
            full_pattern,
            component_name,
            config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_record(
        id: RouteRecordId,
        parent: Option<RouteRecordId>,
        outlet_depth: usize,
        pattern: &'static str,
        full_pattern: String,
        component_name: &'static str,
        config: RouteRuntimeConfig,
    ) -> Self {
        let segments = if full_pattern == "*" {
            vec![Segment::Wildcard]
        } else {
            full_pattern
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    if s == "*" {
                        return Segment::Wildcard;
                    }
                    if let Some(name) = s.strip_prefix('*') {
                        return Segment::RestParam(name.to_string());
                    }
                    if let Some(name) = s.strip_prefix(':') {
                        Segment::Param(name.to_string())
                    } else {
                        Segment::Literal(s.to_string())
                    }
                })
                .collect()
        };
        let own_params = pattern
            .split('/')
            .filter_map(|segment| {
                segment
                    .strip_prefix(':')
                    .or_else(|| segment.strip_prefix('*'))
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .collect();
        Route {
            id,
            parent,
            outlet_depth,
            pattern,
            full_pattern,
            segments,
            own_params,
            component_name,
            config,
        }
    }

    #[cfg(test)]
    fn parse(
        pattern: &'static str,
        component_name: &'static str,
        config: RouteRuntimeConfig,
    ) -> Self {
        Self::parse_root(RouteRecordId(0), pattern, component_name, config)
    }

    fn is_catch_all(&self) -> bool {
        self.segments
            .last()
            .is_some_and(|segment| matches!(segment, Segment::Wildcard | Segment::RestParam(_)))
    }

    /// Try to match `path` against this route. Returns captured params
    /// on a successful match, `None` otherwise.
    fn match_path(&self, path: &str) -> Option<HashMap<String, String>> {
        if matches!(self.segments.as_slice(), [Segment::Wildcard]) {
            return Some(HashMap::new());
        }
        let input: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let has_tail = self
            .segments
            .last()
            .is_some_and(|segment| matches!(segment, Segment::RestParam(_) | Segment::Wildcard));
        if (!has_tail && input.len() != self.segments.len())
            || (has_tail && input.len() + 1 < self.segments.len())
        {
            return None;
        }
        let mut params = HashMap::new();
        for (idx, seg) in self.segments.iter().enumerate() {
            match seg {
                Segment::RestParam(name) if idx + 1 == self.segments.len() => {
                    let value = input[idx..]
                        .iter()
                        .map(|part| url_decode_path_segment(part))
                        .collect::<Vec<_>>()
                        .join("/");
                    params.insert(name.clone(), value);
                    break;
                }
                Segment::RestParam(_) => return None,
                Segment::Wildcard if idx + 1 == self.segments.len() => break,
                Segment::Wildcard => return None,
                _ if idx >= input.len() => return None,
                Segment::Literal(s) if s == input[idx] => {}
                Segment::Literal(_) => return None,
                Segment::Param(name) => {
                    let got = input[idx];
                    params.insert(name.clone(), url_decode_path_segment(got));
                }
            }
        }
        Some(params)
    }
}

fn join_route_pattern(parent: &str, child: &str) -> String {
    if child.starts_with('/') {
        return child.to_string();
    }
    if child.is_empty() {
        return parent.to_string();
    }
    if parent == "/" {
        format!("/{}", child.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            parent.trim_end_matches('/'),
            child.trim_start_matches('/'),
        )
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

/// One record in a normalized matched route chain.
#[derive(Clone, Debug)]
pub struct MatchedRoute {
    pub record_id: RouteRecordId,
    pub component_name: &'static str,
    pub route_pattern: &'static str,
    pub params: HashMap<String, String>,
    pub outlet_depth: usize,
}

/// Parent-to-child route records matched for one location.
#[derive(Clone, Debug, Default)]
pub struct MatchedRouteChain(Vec<MatchedRoute>);

impl MatchedRouteChain {
    pub fn as_slice(&self) -> &[MatchedRoute] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &MatchedRoute> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn last(&self) -> Option<&MatchedRoute> {
        self.0.last()
    }
}

#[derive(Clone)]
struct MatchedEntry {
    route: MatchedRoute,
    config: RouteRuntimeConfig,
}

#[derive(Clone)]
struct RouteMatch {
    entries: Vec<MatchedEntry>,
    params: HashMap<String, String>,
    meta: RouteMeta,
    include_pattern: bool,
}

impl RouteMatch {
    fn deepest(&self) -> &MatchedEntry {
        self.entries
            .last()
            .expect("matched route chain is never empty")
    }

    fn component_name(&self) -> &'static str {
        self.deepest().route.component_name
    }

    fn route_pattern(&self) -> Option<&'static str> {
        self.include_pattern
            .then_some(self.deepest().route.route_pattern)
    }

    fn config(&self) -> &RouteRuntimeConfig {
        &self.deepest().config
    }

    fn chain(&self) -> MatchedRouteChain {
        MatchedRouteChain(
            self.entries
                .iter()
                .map(|entry| entry.route.clone())
                .collect(),
        )
    }
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
    static ROOT_OUTLET: RefCell<Option<Element>> = const { RefCell::new(None) };
    static NESTED_OUTLETS: RefCell<Vec<OutletRegistration>> = const { RefCell::new(Vec::new()) };
    static ROUTE_MOUNT_CONTEXT: RefCell<Option<RouteMountContext>> = const { RefCell::new(None) };
    static ACTIVE_ROUTE_MOUNTS: RefCell<Vec<ActiveRouteMount>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_ROUTE_QUERY: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
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

#[derive(Clone)]
struct OutletRegistration {
    element: Element,
    parent_scope: ScopeId,
    depth: usize,
}

#[derive(Clone, Copy)]
struct RouteMountContext {
    depth: usize,
}

#[derive(Clone)]
struct ActiveRouteMount {
    record_id: RouteRecordId,
    params: HashMap<String, String>,
    host: Element,
    scope_id: Option<ScopeId>,
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
    pub matched: MatchedRouteChain,
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
    /// Cross-locale loader prefetch waits until that language is committed.
    LocaleNotCurrent,
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
    if let Some((_, Some(controller))) =
        PREFETCH_IN_FLIGHT.with(|cache| cache.borrow_mut().remove(key))
    {
        controller.abort();
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
    let _ = register_route_with_config(pattern, component_name, RouteRuntimeConfig::default());
}

pub(crate) fn register_route_with_config(
    pattern: &'static str,
    component_name: &'static str,
    config: RouteRuntimeConfig,
) -> RouteRecordId {
    clear_prefetch_state();
    ROUTES.with(|routes| {
        validate_root_pattern(pattern);
        let mut routes = routes.borrow_mut();
        let id = RouteRecordId(routes.len());
        let route = Route::parse_root(id, pattern, component_name, config);
        validate_record(&routes, &route);
        routes.push(route);
        id
    })
}

pub(crate) fn register_child_route_with_config(
    parent: RouteRecordId,
    pattern: &'static str,
    component_name: &'static str,
    config: RouteRuntimeConfig,
) -> RouteRecordId {
    clear_prefetch_state();
    ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        let parent_route = routes
            .get(parent.0)
            .unwrap_or_else(|| panic!("pocopine router: unknown parent route id {}", parent.0))
            .clone();
        let id = RouteRecordId(routes.len());
        let route = Route::parse_child(id, &parent_route, pattern, component_name, config);
        validate_record(&routes, &route);
        routes.push(route);
        id
    })
}

fn validate_root_pattern(pattern: &str) {
    if pattern != "*" && !pattern.starts_with('/') {
        panic!("pocopine router: root route pattern `{pattern}` must start with `/` or be `*`");
    }
}

fn validate_record(routes: &[Route], route: &Route) {
    if route.full_pattern == "/_pocopine" || route.full_pattern.starts_with("/_pocopine/") {
        panic!(
            "pocopine router: route pattern `{}` uses the reserved `/_pocopine` namespace",
            route.full_pattern,
        );
    }
    for (index, segment) in route.segments.iter().enumerate() {
        if matches!(segment, Segment::Wildcard | Segment::RestParam(_))
            && index + 1 != route.segments.len()
        {
            panic!(
                "pocopine router: wildcard/rest segment in `{}` must be last",
                route.full_pattern,
            );
        }
    }
    if routes
        .iter()
        .any(|sibling| sibling.parent == route.parent && catch_all_prefix_overlaps(sibling, route))
    {
        panic!(
            "pocopine router: route `{}` appears after a wildcard child; wildcard routes must be last among siblings",
            route.pattern,
        );
    }

    let mut ancestor_params = std::collections::HashSet::new();
    let mut parent = route.parent;
    while let Some(parent_id) = parent {
        let ancestor = &routes[parent_id.0];
        ancestor_params.extend(ancestor.own_params.iter().cloned());
        parent = ancestor.parent;
    }
    for param in &route.own_params {
        if !ancestor_params.insert(param.clone()) {
            panic!(
                "pocopine router: duplicate route param `{param}` in nested pattern `{}`",
                route.full_pattern,
            );
        }
    }
}

/// Whether an existing catch-all can match paths in a candidate route's
/// branch. Catch-alls under disjoint literal prefixes (for example,
/// `/docs/*slug` and `/blogs/*slug`) are independent root branches and may be
/// registered in either order.
fn catch_all_prefix_overlaps(catch_all: &Route, candidate: &Route) -> bool {
    if !catch_all.is_catch_all() {
        return false;
    }

    let prefix = &catch_all.segments[..catch_all.segments.len() - 1];
    if candidate.segments.len() < prefix.len() {
        return false;
    }

    prefix
        .iter()
        .zip(&candidate.segments)
        .all(|(left, right)| match (left, right) {
            (Segment::Literal(left), Segment::Literal(right)) => left == right,
            // A parameter can take the literal or parameter value represented
            // by the other route, so these branches overlap.
            (Segment::Param(_), Segment::Literal(_) | Segment::Param(_))
            | (Segment::Literal(_), Segment::Param(_)) => true,
            // Wildcard/rest segments are already required to be final, and
            // the catch-all's final segment was removed from `prefix`.
            _ => false,
        })
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

/// Tell the router where to mount the depth-0 route. Calls made while a
/// route component itself is mounting are handled by [`register_outlet`]
/// instead and never replace this root.
pub fn set_outlet(el: Element) {
    let previous = ROOT_OUTLET.with(|root| {
        let mut root = root.borrow_mut();
        let changed = root
            .as_ref()
            .is_some_and(|current| !same_element(current, &el));
        let previous = changed.then(|| root.take()).flatten();
        *root = Some(el);
        previous
    });
    if let Some(previous) = previous {
        ACTIVE_ROUTE_MOUNTS.with(|active| active.borrow_mut().clear());
        ACTIVE_ROUTE_QUERY.with(|query| query.borrow_mut().clear());
        NESTED_OUTLETS.with(|outlets| outlets.borrow_mut().clear());
        crate::dynamic_component::clear(&previous);
    }
}

/// Register a compiled `<pp-outlet>` sentinel. Outside a route mount this is
/// the app root outlet; during a route mount it belongs to the mounting
/// component's scope at the next depth.
pub(crate) fn register_outlet(el: Element) {
    let context = ROUTE_MOUNT_CONTEXT.with(|context| *context.borrow());
    let Some(context) = context else {
        set_outlet(el);
        return;
    };
    let Some(parent_scope) = mount::enclosing_scope_id(&el) else {
        web_sys::console::error_1(&JsValue::from_str(
            "pocopine router: nested <pp-outlet> has no owning route scope",
        ));
        return;
    };
    let depth = context.depth + 1;
    NESTED_OUTLETS.with(|outlets| {
        let mut outlets = outlets.borrow_mut();
        outlets.retain(|entry| {
            !(same_element(&entry.element, &el)
                || (entry.parent_scope == parent_scope && entry.depth == depth))
        });
        outlets.push(OutletRegistration {
            element: el,
            parent_scope,
            depth,
        });
    });
}

pub(crate) fn release_outlet(el: &Element) {
    NESTED_OUTLETS.with(|outlets| {
        outlets
            .borrow_mut()
            .retain(|entry| !same_element(&entry.element, el));
    });
    let released_root = ROOT_OUTLET.with(|root| {
        let should_clear = root
            .borrow()
            .as_ref()
            .is_some_and(|current| same_element(current, el));
        if should_clear {
            root.borrow_mut().take();
        }
        should_clear
    });
    if released_root {
        ACTIVE_ROUTE_MOUNTS.with(|active| active.borrow_mut().clear());
        ACTIVE_ROUTE_QUERY.with(|query| query.borrow_mut().clear());
        NESTED_OUTLETS.with(|outlets| outlets.borrow_mut().clear());
        return;
    }

    ACTIVE_ROUTE_MOUNTS.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(index) = active
            .iter()
            .position(|mounted| same_element(&mounted.host, el))
        {
            active.truncate(index);
        }
        if active.is_empty() {
            ACTIVE_ROUTE_QUERY.with(|query| query.borrow_mut().clear());
        }
    });
}

fn same_element(left: &Element, right: &Element) -> bool {
    left.is_same_node(Some(right.unchecked_ref::<Node>()))
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
    if let Some(failure) = mount_current_or_defer() {
        return Err(failure);
    }
    Ok(location)
}

/// Paint the current URL now unless a component callback still owns state.
///
/// Programmatic navigation is commonly initiated by a `&mut self` handler.
/// Mounting synchronously from that handler can tear down the route component
/// and attempt its `on_unmount` borrow before the handler's `RefMut` has been
/// released. The callback FIFO drains synchronously when the outermost frame
/// unwinds, so the deferred branch stays in the same browser turn.
fn mount_current_or_defer() -> Option<NavigationFailure> {
    if crate::component_callback_active() {
        crate::defer_component_callback(|| {
            let _ = mount_current();
        });
        None
    } else {
        mount_current()
    }
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
    if !locale::can_prefetch(target.as_str()) {
        return PrefetchResult::Skipped(PrefetchSkip::LocaleNotCurrent);
    }
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
    if !matched.config().prefetch.includes_loader() {
        return PrefetchResult::Skipped(PrefetchSkip::LoaderDisabled);
    }
    let Some(loader) = matched.config().loader.clone() else {
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
        if route.is_catch_all() {
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
                    Segment::RestParam(param) => {
                        let Some(value) = params.get(param) else {
                            found = Some(Err(RouteTargetError::MissingParam(param.clone())));
                            return;
                        };
                        let mut parts =
                            value.trim_matches('/').split('/').filter(|s| !s.is_empty());
                        let Some(first) = parts.next() else {
                            if path.len() > 1 {
                                path.pop();
                            }
                            continue;
                        };
                        push_encoded_route_path_segment(first, &mut path);
                        for part in parts {
                            path.push('/');
                            push_encoded_route_path_segment(part, &mut path);
                        }
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
    RouteTarget::new(locale::href(path))
}

fn location_for_url(url: &str) -> RouteLocation {
    let (path, search, hash) = split_route_url(url);
    let query = parse_query(&search);
    let matched = match_route(&path, true);
    let meta = matched
        .as_ref()
        .map(|matched| matched.meta.clone())
        .unwrap_or_default();
    let chain = matched.as_ref().map(RouteMatch::chain).unwrap_or_default();
    RouteLocation {
        full_path: full_path_from_parts(&path, &search, hash.as_deref()),
        path,
        query,
        hash,
        params: matched
            .as_ref()
            .map(|matched| matched.params.clone())
            .unwrap_or_default(),
        route_pattern: matched.as_ref().and_then(RouteMatch::route_pattern),
        component: matched.as_ref().map(RouteMatch::component_name),
        meta,
        matched: chain,
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

    // Delegated client-side navigation: intercept plain left-clicks on
    // same-origin `<a pp-route>` links so internal navigation never triggers
    // a full page reload (which would re-download + recompile the wasm).
    // Attribute-based, so it also covers links inside `pp-for` clones. Modified
    // clicks, `target`, `download`, external/scheme hrefs, and unmarked links
    // fall through to the browser.
    let on_click = Closure::wrap(Box::new(move |ev: Event| {
        let Some(me) = ev.dyn_ref::<MouseEvent>() else {
            return;
        };
        if me.default_prevented()
            || me.button() != 0
            || me.meta_key()
            || me.ctrl_key()
            || me.shift_key()
            || me.alt_key()
        {
            return;
        }
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        let Some(anchor) = target.closest("a[pp-route]").ok().flatten() else {
            return;
        };
        if anchor
            .get_attribute("target")
            .is_some_and(|t| !t.is_empty() && t != "_self")
            || anchor.has_attribute("download")
        {
            return;
        }
        let Some(href) = anchor.get_attribute("href") else {
            return;
        };
        // Only intercept absolute internal paths; leave external URLs,
        // schemes (`mailto:`, `http:`), and protocol-relative `//` to the
        // browser.
        if !href.starts_with('/') || href.starts_with("//") {
            return;
        }
        ev.prevent_default();
        navigate(&href);
    }) as Box<dyn FnMut(Event)>);
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let _ = doc.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
    }
    on_click.forget();

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

/// RFC-096 S2 — the `$route` scope's id, for proxy-free reads
/// through the scoped access.
pub(crate) fn route_scope_id() -> Option<crate::reactive::ScopeId> {
    ensure_route_scope();
    ROUTE_SCOPE.with(|cell| cell.get().map(|s| s.id))
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
/// - **`Redirect`** / **`Reject`** → the outlet is cleared at the current
///   component-callback safe point so any PII in the now-rejected component
///   leaves the DOM before control returns to the browser event loop. The full
///   mount flow then re-runs through [`mount_current`] so handlers, error
///   surface, and `RouteNavigationFailed` events fire exactly as they would on
///   a fresh navigation.
///
/// No-op when the current path doesn't match any registered route
/// (no guards to re-evaluate) or when the platform is unavailable.
/// See RFC-078 §5.10.6 for the contract.
pub fn reevaluate_current() {
    if crate::component_callback_active() {
        crate::defer_component_callback(reevaluate_current_now);
    } else {
        reevaluate_current_now();
    }
}

fn reevaluate_current_now() {
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
    clear_active_route_mounts(0);
    if let Some(outlet) = ROOT_OUTLET.with(|outlet| outlet.borrow().clone()) {
        crate::dynamic_component::clear_immediate(&outlet);
    }
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
    if !locale::ready() {
        return None;
    }

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
    let matched = match_route(&path, true);
    let component_name = matched.as_ref().map(RouteMatch::component_name);
    let route_pattern = matched.as_ref().and_then(RouteMatch::route_pattern);
    let params = matched
        .as_ref()
        .map(|m| m.params.clone())
        .unwrap_or_default();
    // RFC-123 §5.5: a page view is a span and the root of its own trace;
    // opened whether or not hooks are installed.
    let navigation_span =
        crate::client_trace::navigation_started(&path, route_pattern, component_name);
    let _mounting = navigation_span.enter();
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
                    crate::client_trace::navigation_failed("guard_pending");
                    if has_route_hooks {
                        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                            path: path.clone(),
                            route_pattern,
                            component: Some(matched.component_name()),
                            reason: "guard_pending",
                            duration_ms: elapsed_since(start_ms),
                        });
                    }
                    apply_page_meta(None);
                    return Some(NavigationFailure::GuardPending);
                }
                RouteGuardDecision::Redirect(target) => {
                    clear_pending_guard_navigation();
                    crate::client_trace::navigation_failed("guard_redirected");
                    if has_route_hooks {
                        crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                            path: path.clone(),
                            route_pattern,
                            component: Some(matched.component_name()),
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

        let preserved_prefix = preserved_prefix_len(matched, &query);
        let loader_key = route_navigation_key(&path, &search);
        cancel_prefetch(&loader_key);
        let mut loader_data: HashMap<RouteRecordId, Rc<dyn std::any::Any>> = HashMap::new();
        if let Some(data) = take_prefetched_loader_data(&loader_key) {
            loader_data.insert(matched.deepest().route.record_id, data);
        }
        let has_loader = matched.entries.iter().skip(preserved_prefix).any(|entry| {
            entry.config.loader.is_some() && !loader_data.contains_key(&entry.route.record_id)
        });
        if has_loader {
            let abort_signal = begin_loader_abort(nav_token);
            update_route_state(&path, &params, query.clone());
            let matched_for_async = matched.clone();
            let path_for_async = path.clone();
            let query_for_async = query.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let mut loader_data = loader_data;
                for entry in matched_for_async.entries.iter().skip(preserved_prefix) {
                    if loader_data.contains_key(&entry.route.record_id) {
                        continue;
                    }
                    let Some(loader) = entry.config.loader.clone() else {
                        continue;
                    };
                    let loader_ctx = LoaderContext {
                        path: path_for_async.clone(),
                        params: entry.route.params.clone(),
                        query: query_for_async.clone(),
                        matched_pattern: Some(entry.route.route_pattern),
                        navigation_token: nav_token,
                        abort_signal: abort_signal.clone(),
                    };
                    let result = loader.run(loader_ctx).await;
                    if !is_token_current(nav_token) {
                        clear_pending_loader_data();
                        return;
                    }
                    match result {
                        Ok(data) => {
                            loader_data.insert(entry.route.record_id, Rc::from(data));
                        }
                        Err(err) => {
                            clear_active_loader_abort(nav_token);
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
                            return;
                        }
                    }
                }
                clear_active_loader_abort(nav_token);
                let _ = finish_route_mount(
                    Some(&matched_for_async),
                    preserved_prefix,
                    &path_for_async,
                    &query_for_async,
                    &loader_data,
                    has_route_hooks,
                    start_ms,
                );
            });
            return None;
        }

        update_route_state(&path, &params, query.clone());
        return finish_route_mount(
            Some(matched),
            preserved_prefix,
            &path,
            &query,
            &loader_data,
            has_route_hooks,
            start_ms,
        );
    }

    update_route_state(&path, &params, query.clone());
    finish_route_mount(
        None,
        0,
        &path,
        &query,
        &HashMap::new(),
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
            crate::scope::invalidate_field_cache(scope.id);
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
    matched: Option<&RouteMatch>,
    preserved_prefix: usize,
    path: &str,
    query: &HashMap<String, String>,
    loader_data: &HashMap<RouteRecordId, Rc<dyn std::any::Any>>,
    has_route_hooks: bool,
    start_ms: Option<f64>,
) -> Option<NavigationFailure> {
    clear_pending_guard_navigation();

    // Devtools hook — fires on every resolved route change, even
    // when there's no matching component (404). The router panel
    // uses this to build its recent-history view.
    #[cfg(feature = "devtools")]
    {
        let empty = HashMap::new();
        let params = matched.map(|matched| &matched.params).unwrap_or(&empty);
        crate::devtools::hooks::fire_route_change(path, params);
    }

    let Some(matched) = matched else {
        clear_active_route_mounts(0);
        // No registered route matched. If the app configured a
        // dedicated 404 component (the lower-friction alternative
        // to a `*` wildcard route), mount it here. Otherwise the
        // now-unmatched route subtree stays cleared.
        if let Some(fallback) = NOT_FOUND_COMPONENT.with(|cell| cell.get())
            && mount_component_into_outlet(fallback)
            && has_route_hooks
        {
            apply_page_meta(None);
            crate::client_trace::navigation_completed();
            crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                path: path.to_string(),
                route_pattern: None,
                component: Some(fallback),
                duration_ms: elapsed_since(start_ms),
            });
            return None;
        }
        apply_page_meta(None);
        crate::client_trace::navigation_completed();
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationCompleted {
                path: path.to_string(),
                route_pattern: None,
                component: None,
                duration_ms: elapsed_since(start_ms),
            });
        }
        return None;
    };
    let name = matched.component_name();
    let route_pattern = matched.route_pattern();
    if ROOT_OUTLET.with(|outlet| outlet.borrow().is_none()) {
        crate::client_trace::navigation_failed("missing_outlet");
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
    }

    if let Err(reason) = mount_route_chain(matched, preserved_prefix, query, loader_data) {
        crate::client_trace::navigation_failed(reason);
        if has_route_hooks {
            crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                path: path.to_string(),
                route_pattern,
                component: Some(name),
                reason,
                duration_ms: elapsed_since(start_ms),
            });
        }
        clear_pending_loader_data();
        apply_page_meta(None);
        return Some(NavigationFailure::MountFailed(reason));
    }
    apply_page_meta(matched.config().page_meta.clone());

    // The component's `Loader<T>` extractor (if any) consumed the
    // pending slot during setup; for routes without a loader the
    // slot was never populated. Either way, drop any leftover so
    // the next navigation starts fresh — defensive against
    // `Option<Loader<T>>` extractors that opt out of consuming.
    clear_pending_loader_data();

    crate::client_trace::navigation_completed();

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

fn preserved_prefix_len(matched: &RouteMatch, query: &HashMap<String, String>) -> usize {
    let active = ACTIVE_ROUTE_MOUNTS.with(|active| active.borrow().clone());
    let mut prefix = active
        .iter()
        .zip(matched.entries.iter())
        .take_while(|(active, entry)| {
            active.record_id == entry.route.record_id && active.params == entry.route.params
        })
        .count();
    let query_changed = ACTIVE_ROUTE_QUERY.with(|active| *active.borrow() != *query);
    if query_changed && prefix > 0 {
        prefix = prefix.min(matched.entries.len().saturating_sub(1));
    }
    prefix
}

fn mount_route_chain(
    matched: &RouteMatch,
    preserved_prefix: usize,
    query: &HashMap<String, String>,
    loader_data: &HashMap<RouteRecordId, Rc<dyn std::any::Any>>,
) -> Result<(), &'static str> {
    let prefix = preserved_prefix.min(matched.entries.len());
    clear_active_route_mounts(prefix);

    for entry in matched.entries.iter().skip(prefix) {
        let host = if entry.route.outlet_depth == 0 {
            ROOT_OUTLET.with(|outlet| outlet.borrow().clone())
        } else {
            let parent_scope = ACTIVE_ROUTE_MOUNTS
                .with(|active| active.borrow().last().and_then(|active| active.scope_id));
            parent_scope
                .and_then(|parent_scope| find_nested_outlet(parent_scope, entry.route.outlet_depth))
        };
        let Some(host) = host else {
            clear_active_route_mounts(prefix);
            return Err(if entry.route.outlet_depth == 0 {
                "missing_outlet"
            } else if ACTIVE_ROUTE_MOUNTS.with(|active| {
                active
                    .borrow()
                    .last()
                    .and_then(|active| active.scope_id)
                    .is_none()
            }) {
                "missing_parent_scope"
            } else {
                "missing_nested_outlet"
            });
        };

        if let Some(data) = loader_data.get(&entry.route.record_id) {
            put_pending_loader_rc(data.clone());
        }
        let props: HashMap<String, JsValue> = entry
            .route
            .params
            .iter()
            .map(|(key, value)| (key.clone(), JsValue::from_str(value)))
            .collect();
        let previous = ROUTE_MOUNT_CONTEXT.with(|context| {
            context.replace(Some(RouteMountContext {
                depth: entry.route.outlet_depth,
            }))
        });
        let mounted =
            crate::dynamic_component::render(&host, Some(entry.route.component_name), &props);
        ROUTE_MOUNT_CONTEXT.with(|context| {
            context.replace(previous);
        });
        clear_pending_loader_data();
        let Some(mounted) = mounted else {
            clear_active_route_mounts(prefix);
            return Err("component_not_registered");
        };
        ACTIVE_ROUTE_MOUNTS.with(|active| {
            active.borrow_mut().push(ActiveRouteMount {
                record_id: entry.route.record_id,
                params: entry.route.params.clone(),
                host,
                scope_id: mounted.scope_id,
            });
        });
    }
    ACTIVE_ROUTE_QUERY.with(|active| {
        *active.borrow_mut() = query.clone();
    });
    Ok(())
}

fn find_nested_outlet(parent_scope: ScopeId, depth: usize) -> Option<Element> {
    NESTED_OUTLETS.with(|outlets| {
        outlets
            .borrow()
            .iter()
            .rev()
            .find(|entry| entry.parent_scope == parent_scope && entry.depth == depth)
            .map(|entry| entry.element.clone())
    })
}

fn clear_active_route_mounts(prefix: usize) {
    let removed = ACTIVE_ROUTE_MOUNTS.with(|active| {
        let mut active = active.borrow_mut();
        if prefix >= active.len() {
            Vec::new()
        } else {
            active.split_off(prefix)
        }
    });
    for active in removed.into_iter().rev() {
        crate::dynamic_component::clear(&active.host);
    }
    if prefix == 0 {
        ACTIVE_ROUTE_QUERY.with(|query| query.borrow_mut().clear());
    }
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
    crate::client_trace::navigation_failed(rejection.reason(source));
    if has_route_hooks {
        // The asynchronous loader path runs outside the mount; keep the
        // event under the page view's span (RFC-123 §5.5).
        crate::client_trace::in_view(|| {
            crate::plugin::emit(crate::plugin::RouteNavigationFailed {
                path: path.to_string(),
                route_pattern,
                component: Some(matched.component_name()),
                reason: rejection.reason(source),
                duration_ms: elapsed_since(start_ms),
            });
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

/// Find the first matching route tree, then reconstruct its parent-to-child
/// chain. Registration order remains authoritative between unrelated records,
/// catch-alls remain fallbacks, and a matching descendant outranks its own
/// layout parent (which is how an index child wins at the same URL).
fn match_route(path: &str, include_pattern: bool) -> Option<RouteMatch> {
    let path = locale::app_path(path);
    ROUTES.with(|r| {
        let routes = r.borrow();
        let mut best: Option<(&Route, HashMap<String, String>)> = None;
        for route in routes.iter() {
            let Some(params) = route.match_path(&path) else {
                continue;
            };
            let replace = match best.as_ref() {
                None => true,
                Some((current, _)) => {
                    is_descendant_of(&routes, route.id, current.id)
                        || (current.is_catch_all() && !route.is_catch_all())
                }
            };
            if replace {
                best = Some((route, params));
            }
        }
        let (deepest, captured) = best?;

        let mut ids = Vec::new();
        let mut current = Some(deepest.id);
        while let Some(id) = current {
            let route = &routes[id.0];
            ids.push(id);
            current = route.parent;
        }
        ids.reverse();

        let mut merged_params = HashMap::new();
        let mut merged_meta = RouteMeta::new();
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            let route = &routes[id.0];
            for param in &route.own_params {
                if let Some(value) = captured.get(param) {
                    merged_params.insert(param.clone(), value.clone());
                }
            }
            merged_meta.merge_from(&route.config.meta);
            entries.push(MatchedEntry {
                route: MatchedRoute {
                    record_id: route.id,
                    component_name: route.component_name,
                    route_pattern: route.pattern,
                    params: merged_params.clone(),
                    outlet_depth: route.outlet_depth,
                },
                config: route.config.clone(),
            });
        }
        Some(RouteMatch {
            entries,
            params: merged_params,
            meta: merged_meta,
            include_pattern,
        })
    })
}

fn is_descendant_of(routes: &[Route], candidate: RouteRecordId, ancestor: RouteRecordId) -> bool {
    let mut parent = routes[candidate.0].parent;
    while let Some(parent_id) = parent {
        if parent_id == ancestor {
            return true;
        }
        parent = routes[parent_id.0].parent;
    }
    false
}

fn evaluate_guards(
    matched: &RouteMatch,
    path: &str,
    query: &HashMap<String, String>,
) -> Option<RouteGuardDecision> {
    if matched
        .entries
        .iter()
        .all(|entry| entry.config.guards.is_empty())
    {
        return None;
    }
    for entry in &matched.entries {
        let ctx = RouteContext {
            path,
            params: &entry.route.params,
            query,
            matched_pattern: matched.include_pattern.then_some(entry.route.route_pattern),
        };
        for guard in &entry.config.guards {
            match guard.decide(&ctx) {
                RouteGuardDecision::Allow => {}
                other => return Some(other),
            }
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
    pairs.sort_by_key(|(left, _)| *left);
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
        matched_pattern: matched.route_pattern(),
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
    if let Some(name) = ROUTE_ERROR_COMPONENT.with(|cell| cell.get())
        && mount_component_into_outlet(name)
    {
        return;
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
    let Some(outlet) = ROOT_OUTLET.with(|outlet| outlet.borrow().clone()) else {
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
    clear_active_route_mounts(0);
    crate::dynamic_component::clear_immediate(&outlet);
    outlet.replace_children_with_node_1(root.as_ref());
}

/// Mount a registered component by name into the current outlet,
/// replacing whatever was there. Returns `true` when the mount
/// succeeded; `false` means the platform/document/outlet wasn't
/// available or the element couldn't be created — the caller
/// should fall back to whatever its non-override path is.
fn mount_component_into_outlet(name: &'static str) -> bool {
    let Some(outlet) = ROOT_OUTLET.with(|outlet| outlet.borrow().clone()) else {
        return false;
    };
    clear_active_route_mounts(0);
    crate::dynamic_component::render(&outlet, Some(name), &HashMap::new()).is_some()
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
    // Query semantics: percent-decode + `+` → ` `. Lossy on invalid UTF-8;
    // a stray `%` is passed through. (Avoids pulling in js_sys URLSearchParams
    // so the same path works on the host-compat side.)
    pocopine_codec::percent_decode(s, true)
}

fn url_decode_path_segment(s: &str) -> String {
    // Path semantics: percent-decode, but `+` is a literal plus.
    pocopine_codec::percent_decode(s, false)
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
        ROOT_OUTLET.with(|outlet| outlet.borrow_mut().take());
        NESTED_OUTLETS.with(|outlets| outlets.borrow_mut().clear());
        ROUTE_MOUNT_CONTEXT.with(|context| context.borrow_mut().take());
        ACTIVE_ROUTE_MOUNTS.with(|active| active.borrow_mut().clear());
        ACTIVE_ROUTE_QUERY.with(|query| query.borrow_mut().clear());
        ROUTE_REJECTION_HANDLERS.with(|handlers| handlers.borrow_mut().clear());
        PENDING_LOADER_DATA.with(|slot| slot.borrow_mut().take());
        LOADER_SLOTS.with(|slots| slots.borrow_mut().clear());
        PENDING_GUARD_NAVIGATION.with(|slot| slot.borrow_mut().take());
        clear_prefetch_state();
        ROUTE_TOKEN.with(|token| token.set(0));
        PREFETCH_TOKEN.with(|token| token.set(0));
        BASE_DOCUMENT_TITLE.with(|title| title.borrow_mut().take());
    }

    fn single_match(
        component_name: &'static str,
        route_pattern: &'static str,
        params: HashMap<String, String>,
        config: RouteRuntimeConfig,
    ) -> RouteMatch {
        RouteMatch {
            entries: vec![MatchedEntry {
                route: MatchedRoute {
                    record_id: RouteRecordId(0),
                    component_name,
                    route_pattern,
                    params: params.clone(),
                    outlet_depth: 0,
                },
                config: config.clone(),
            }],
            params,
            meta: config.meta,
            include_pattern: true,
        }
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
    fn named_rest_param_captures_remaining_path_segments() {
        let r = Route::parse(
            "/connection/:connection_id/*prefix",
            "storage",
            RouteRuntimeConfig::default(),
        );
        let caps = r
            .match_path("/connection/abc/Videos/Recent%20Files")
            .unwrap();
        assert_eq!(caps.get("connection_id"), Some(&"abc".to_string()));
        assert_eq!(caps.get("prefix"), Some(&"Videos/Recent Files".to_string()));
    }

    #[test]
    fn wildcard_matches_anything() {
        let r = Route::parse("*", "not-found", RouteRuntimeConfig::default());
        assert!(r.match_path("/").is_some());
        assert!(r.match_path("/nope/anywhere").is_some());
    }

    #[test]
    fn disjoint_prefixed_rest_routes_are_independent_siblings() {
        reset_router_for_test();
        let docs = register_route_with_config("/docs/*slug", "docs", RouteRuntimeConfig::default());
        let blogs_index =
            register_route_with_config("/blogs", "blogs-index", RouteRuntimeConfig::default());
        let blogs =
            register_route_with_config("/blogs/*slug", "blog", RouteRuntimeConfig::default());

        assert_eq!(
            match_route("/docs/getting-started", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            docs,
        );
        assert_eq!(
            match_route("/blogs", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            blogs_index,
        );
        assert_eq!(
            match_route("/blogs/launch", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            blogs,
        );
    }

    #[test]
    #[should_panic(expected = "appears after a wildcard child")]
    fn overlapping_prefixed_rest_route_must_stay_last() {
        reset_router_for_test();
        register_route_with_config(
            "/docs/*slug",
            "docs-fallback",
            RouteRuntimeConfig::default(),
        );
        register_route_with_config(
            "/docs/reference",
            "docs-reference",
            RouteRuntimeConfig::default(),
        );
    }

    #[test]
    fn nested_route_matches_parent_to_child_chain_and_merges_params() {
        reset_router_for_test();
        let parent = register_route_with_config(
            "/teams/:team_id",
            "team-layout",
            RouteRuntimeConfig::default(),
        );
        let child = register_child_route_with_config(
            parent,
            "members/:member_id",
            "team-member",
            RouteRuntimeConfig::default(),
        );

        let matched = match_route("/teams/acme/members/user%2042", true).unwrap();
        assert_eq!(matched.entries.len(), 2);
        assert_eq!(matched.entries[0].route.record_id, parent);
        assert_eq!(matched.entries[1].route.record_id, child);
        assert_eq!(matched.entries[0].route.outlet_depth, 0);
        assert_eq!(matched.entries[1].route.outlet_depth, 1);
        assert_eq!(
            matched.entries[0].route.params.get("team_id"),
            Some(&"acme".to_string()),
        );
        assert_eq!(
            matched.entries[1].route.params.get("member_id"),
            Some(&"user 42".to_string()),
        );
        assert_eq!(matched.component_name(), "team-member");
    }

    #[test]
    fn nested_index_child_wins_at_parent_url() {
        reset_router_for_test();
        let parent = register_route_with_config(
            "/admin-index-test",
            "admin-layout",
            RouteRuntimeConfig::default(),
        );
        let index = register_child_route_with_config(
            parent,
            "",
            "admin-index",
            RouteRuntimeConfig::default(),
        );

        let matched = match_route("/admin-index-test", true).unwrap();
        assert_eq!(matched.entries.len(), 2);
        assert_eq!(matched.deepest().route.record_id, index);
    }

    #[test]
    fn unrelated_route_trees_keep_registration_order() {
        reset_router_for_test();
        let first = register_route_with_config(
            "/route-priority/:page",
            "first-flat-route",
            RouteRuntimeConfig::default(),
        );
        let layout = register_route_with_config(
            "/route-priority",
            "later-layout",
            RouteRuntimeConfig::default(),
        );
        let _ = register_child_route_with_config(
            layout,
            "settings",
            "later-nested-route",
            RouteRuntimeConfig::default(),
        );

        let matched = match_route("/route-priority/settings", true).unwrap();
        assert_eq!(matched.deepest().route.record_id, first);
        assert_eq!(matched.entries.len(), 1);
    }

    #[test]
    fn nested_wildcard_is_a_child_fallback_not_a_specific_route_override() {
        reset_router_for_test();
        let parent = register_route_with_config(
            "/admin-wild-test",
            "admin-layout",
            RouteRuntimeConfig::default(),
        );
        let users = register_child_route_with_config(
            parent,
            "users",
            "admin-users",
            RouteRuntimeConfig::default(),
        );
        let fallback = register_child_route_with_config(
            parent,
            "*",
            "admin-not-found",
            RouteRuntimeConfig::default(),
        );

        assert_eq!(
            match_route("/admin-wild-test/users", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            users,
        );
        assert_eq!(
            match_route("/admin-wild-test/missing/deep", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            fallback,
        );
        assert_eq!(
            match_route("/admin-wild-test", true)
                .unwrap()
                .deepest()
                .route
                .record_id,
            fallback,
        );
    }

    #[test]
    #[should_panic(expected = "duplicate route param `id`")]
    fn nested_duplicate_param_names_are_rejected() {
        reset_router_for_test();
        let parent =
            register_route_with_config("/orgs/:id", "org-layout", RouteRuntimeConfig::default());
        let _ = register_child_route_with_config(
            parent,
            "members/:id",
            "org-member",
            RouteRuntimeConfig::default(),
        );
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
        let matched = single_match(
            "user-page",
            "/users/:uid",
            params,
            RouteRuntimeConfig {
                guards: vec![guard],
                ..RouteRuntimeConfig::default()
            },
        );

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
        let matched = single_match(
            "admin",
            "/admin",
            HashMap::new(),
            RouteRuntimeConfig {
                guards: vec![first_guard, second_guard],
                ..RouteRuntimeConfig::default()
            },
        );

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
        let matched = single_match(
            "admin",
            "/admin",
            HashMap::new(),
            RouteRuntimeConfig {
                guards: vec![first_guard, second_guard],
                ..RouteRuntimeConfig::default()
            },
        );

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
        let matched = single_match(
            "admin",
            "/admin/:section",
            params,
            RouteRuntimeConfig::default(),
        );

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
