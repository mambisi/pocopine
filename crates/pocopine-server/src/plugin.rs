//! Server plugin runtime services and lifecycle hook dispatch.
//!
//! [`ServerPlugin`] installs configuration on the [`crate::Server`]
//! builder. This module stores the runtime services those installers
//! provide, exposes them through [`ServerPluginHandle<T>`], and
//! dispatches framework lifecycle events to services that implement
//! [`ServerHook<E>`].
//!
//! Mirror of `pocopine-core::plugin` for host code: services are
//! `Arc<T>` and require `T: Send + Sync + 'static`, the active
//! registry lives in a process-global behind an `RwLock`, and an
//! `AtomicU16` hook bitmask gates per-request emit sites so plugin-
//! free servers stay on the same fast path as before.

use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

type HookDispatch = Arc<dyn Fn(&PluginRegistry, &dyn Any) + Send + Sync>;
type HookMask = u16;

const HOOK_SERVER_BOOT_STARTED: HookMask = 1 << 0;
const HOOK_SERVER_LISTENING: HookMask = 1 << 1;
const HOOK_SERVER_BOOT_FAILED: HookMask = 1 << 2;
const HOOK_HTTP_REQUEST_STARTED: HookMask = 1 << 3;
const HOOK_HTTP_REQUEST_COMPLETED: HookMask = 1 << 4;
const HOOK_HTTP_REQUEST_FAILED: HookMask = 1 << 5;
const HOOK_SERVER_FN_STARTED: HookMask = 1 << 6;
const HOOK_SERVER_FN_COMPLETED: HookMask = 1 << 7;
const HOOK_SERVER_FN_REJECTED: HookMask = 1 << 8;
const HOOK_SERVER_FN_FAILED: HookMask = 1 << 9;

const HOOK_HTTP_REQUEST_EVENTS: HookMask =
    HOOK_HTTP_REQUEST_STARTED | HOOK_HTTP_REQUEST_COMPLETED | HOOK_HTTP_REQUEST_FAILED;
const HOOK_SERVER_FN_EVENTS: HookMask = HOOK_SERVER_FN_STARTED
    | HOOK_SERVER_FN_COMPLETED
    | HOOK_SERVER_FN_REJECTED
    | HOOK_SERVER_FN_FAILED;

static ACTIVE_HOOK_MASK: AtomicU16 = AtomicU16::new(0);
static REGISTRY: LazyLock<RwLock<Arc<PluginRegistry>>> =
    LazyLock::new(|| RwLock::new(Arc::new(PluginRegistry::default())));

const APP_PROVIDER: &str = "app";

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Allocate a process-local request id used to correlate paired
/// events (started/completed, started/failed). Wraps at `u64::MAX`,
/// so the value is for correlation only — never for security.
pub fn next_request_id() -> u64 {
    REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Newtype around the framework-allocated correlation id for an
/// in-flight HTTP request. The [`crate::request_event_layer`]
/// inserts this into the request's extensions so downstream
/// `#[server]` route handlers can emit
/// [`ServerFunctionStarted`] / [`ServerFunctionCompleted`] /
/// [`ServerFunctionFailed`] / [`ServerFunctionRejected`] with the
/// same `request_id` as the HTTP-layer events.
#[derive(Copy, Clone, Debug)]
pub struct RequestId(pub u64);

/// Runtime handle for a service installed by a server plugin.
///
/// Backed by an `Arc<T>` so concurrent request handlers and event
/// dispatch closures can each hold a clone without coordinating.
pub struct ServerPluginHandle<T: Send + Sync + 'static> {
    service: Arc<T>,
}

impl<T: Send + Sync + 'static> Clone for ServerPluginHandle<T> {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
        }
    }
}

impl<T: Send + Sync + 'static> Deref for ServerPluginHandle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.service.as_ref()
    }
}

impl<T: Send + Sync + 'static> ServerPluginHandle<T> {
    pub fn get(&self) -> &T {
        self.service.as_ref()
    }

    pub fn arc(&self) -> Arc<T> {
        self.service.clone()
    }
}

/// Typed framework event hook implemented by runtime plugin services.
///
/// `call` takes the event by value: every subscriber receives its
/// own copy via the dispatcher's `event.clone()`. Framework events
/// are designed so the clone is cheap — fields are `&'static str`,
/// primitives, or short owned strings sourced from request data.
pub trait ServerHook<E>: Send + Sync + 'static {
    fn call(&self, event: E);
}

/// Emitted at the top of [`crate::Server::serve`] after plugin
/// validation succeeds and before the listener binds.
#[derive(Clone, Debug)]
pub struct ServerBootStarted {
    pub addr: String,
}

/// Emitted once the listener is bound and ready to accept
/// connections. Fires before `axum::serve` enters its accept loop.
#[derive(Clone, Debug)]
pub struct ServerListening {
    pub addr: String,
}

/// Emitted when boot fails after plugin validation succeeds — bind
/// failure, address parse failure, or another listener-side error.
/// `reason` is a stable identifier (`"address_parse"`, `"bind"`,
/// etc.) for filtering; the full error is on the returned
/// `io::Error` and in the `pocopine.log` tracing record.
#[derive(Clone, Debug)]
pub struct ServerBootFailed {
    pub reason: &'static str,
}

/// Emitted at the top of an HTTP request after axum has matched it
/// to a route.
///
/// `route_pattern` is captured from axum's
/// [`axum::extract::MatchedPath`] when present. For requests that
/// the router cannot match (fallback service, unmatched fall-through)
/// the value is `None` and `path` carries the raw URI path.
///
/// Only the framework's request id, method, and path identifying
/// fields are included. Headers, cookies, query strings, and body
/// payloads never enter framework events — observability plugins
/// derive size/error-class fields if they need them.
#[derive(Clone, Debug)]
pub struct HttpRequestStarted {
    pub method: String,
    pub path: String,
    pub route_pattern: Option<String>,
    pub request_id: u64,
}

/// Emitted after the response status is known. `duration_ms` covers
/// router + middleware + handler time, measured from the
/// [`HttpRequestStarted`] timestamp.
#[derive(Clone, Debug)]
pub struct HttpRequestCompleted {
    pub method: String,
    pub path: String,
    pub route_pattern: Option<String>,
    pub request_id: u64,
    pub status: u16,
    pub duration_ms: f64,
}

/// Emitted when the request layer fails before producing a response
/// (e.g. middleware error, panic). `reason` is a stable identifier.
#[derive(Clone, Debug)]
pub struct HttpRequestFailed {
    pub method: String,
    pub path: String,
    pub route_pattern: Option<String>,
    pub request_id: u64,
    pub reason: &'static str,
    pub duration_ms: f64,
}

/// Emitted at the start of a `#[server]` route handler — after the
/// HTTP layer has matched the route and before guard/body work.
#[derive(Clone, Debug)]
pub struct ServerFunctionStarted {
    pub function: &'static str,
    pub request_id: u64,
}

/// Emitted when a `#[server]` handler returns an `Ok` value.
#[derive(Clone, Debug)]
pub struct ServerFunctionCompleted {
    pub function: &'static str,
    pub request_id: u64,
    pub duration_ms: f64,
}

/// Emitted when a `#[server]` request is rejected before the user
/// handler runs — guard failure, body read failure, body parse
/// failure. `status` is the response status the framework will
/// return; `reason` is a stable identifier
/// (`"unauthorized"`, `"forbidden"`, `"bad_request"`, …).
#[derive(Clone, Debug)]
pub struct ServerFunctionRejected {
    pub function: &'static str,
    pub request_id: u64,
    pub status: u16,
    pub reason: &'static str,
}

/// Emitted when a `#[server]` user handler returns an `Err`.
/// `error_class` mirrors the existing tracing
/// `error_kind` field (`"app"`, `"unauthorized"`, `"forbidden"`,
/// `"bad_request"`, `"network"`).
#[derive(Clone, Debug)]
pub struct ServerFunctionFailed {
    pub function: &'static str,
    pub request_id: u64,
    pub error_class: &'static str,
    pub duration_ms: f64,
}

struct PluginService {
    service: Arc<dyn Any + Send + Sync>,
    provider: &'static str,
}

struct HookRequirement {
    plugin: &'static str,
    service: &'static str,
    service_type: TypeId,
    event: &'static str,
}

/// Boot-time plugin validation error.
///
/// The runtime records which plugin installed each hook. Before
/// binding the listener, every hook's required service is checked
/// against the provided services so misconfigured plugin ordering
/// fails fast with a concrete diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginValidationError {
    pub plugin: &'static str,
    pub service: &'static str,
    pub event: &'static str,
}

impl fmt::Display for PluginValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "plugin `{}` registered a hook for event `{}` requiring service `{}`, \
             but that service was not provided",
            self.plugin, self.event, self.service
        )
    }
}

#[derive(Default)]
pub(crate) struct PluginRegistry {
    services: HashMap<TypeId, PluginService>,
    hooks: HashMap<TypeId, Vec<HookDispatch>>,
    requirements: Vec<HookRequirement>,
}

impl PluginRegistry {
    pub(crate) fn provide<T: Send + Sync + 'static>(
        &mut self,
        service: T,
        provider: Option<&'static str>,
    ) {
        let service_type = TypeId::of::<T>();
        let provider = provider.unwrap_or(APP_PROVIDER);
        if let Some(previous) = self.services.get(&service_type) {
            panic!(
                "plugin service `{}` is already installed (first provider: `{}`, \
                 second provider: `{}`)",
                type_name::<T>(),
                previous.provider,
                provider,
            );
        }
        self.services.insert(
            service_type,
            PluginService {
                service: Arc::new(service),
                provider,
            },
        );
    }

    pub(crate) fn hook_plugin<T, E>(&mut self, plugin: Option<&'static str>)
    where
        T: ServerHook<E> + Send + Sync + 'static,
        E: Clone + Send + Sync + 'static,
    {
        let plugin = plugin.unwrap_or(APP_PROVIDER);
        self.requirements.push(HookRequirement {
            plugin,
            service: type_name::<T>(),
            service_type: TypeId::of::<T>(),
            event: type_name::<E>(),
        });
        self.hooks
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Arc::new(|registry, event| {
                let event = event
                    .downcast_ref::<E>()
                    .expect("plugin hook dispatched with the wrong event type")
                    .clone();
                let service = registry.plugin::<T>().unwrap_or_else(|| {
                    panic!(
                        "plugin hook for event `{}` requires plugin service `{}`, \
                         but that service is not installed. Install it with \
                         `Server::provide_plugin(...)` before \
                         `Server::hook_plugin::<{}, {}>()`.",
                        type_name::<E>(),
                        type_name::<T>(),
                        type_name::<T>(),
                        type_name::<E>(),
                    )
                });
                service.get().call(event);
            }));
    }

    pub(crate) fn validate(&self) -> Result<(), Vec<PluginValidationError>> {
        let errors: Vec<_> = self
            .requirements
            .iter()
            .filter(|requirement| !self.services.contains_key(&requirement.service_type))
            .map(|requirement| PluginValidationError {
                plugin: requirement.plugin,
                service: requirement.service,
                event: requirement.event,
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn plugin<T: Send + Sync + 'static>(&self) -> Option<ServerPluginHandle<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|service| service.service.clone().downcast::<T>().ok())
            .map(|service| ServerPluginHandle { service })
    }

    fn emit<E>(&self, event: E)
    where
        E: Clone + Send + Sync + 'static,
    {
        if let Some(hooks) = self.hooks.get(&TypeId::of::<E>()) {
            for hook in hooks {
                hook(self, &event);
            }
        }
    }

    fn has_stored_hooks<E: 'static>(&self) -> bool {
        self.hooks
            .get(&TypeId::of::<E>())
            .map(|hooks| !hooks.is_empty())
            .unwrap_or(false)
    }

    fn hook_mask(&self) -> HookMask {
        let mut mask = 0;
        if self.has_stored_hooks::<ServerBootStarted>() {
            mask |= HOOK_SERVER_BOOT_STARTED;
        }
        if self.has_stored_hooks::<ServerListening>() {
            mask |= HOOK_SERVER_LISTENING;
        }
        if self.has_stored_hooks::<ServerBootFailed>() {
            mask |= HOOK_SERVER_BOOT_FAILED;
        }
        if self.has_stored_hooks::<HttpRequestStarted>() {
            mask |= HOOK_HTTP_REQUEST_STARTED;
        }
        if self.has_stored_hooks::<HttpRequestCompleted>() {
            mask |= HOOK_HTTP_REQUEST_COMPLETED;
        }
        if self.has_stored_hooks::<HttpRequestFailed>() {
            mask |= HOOK_HTTP_REQUEST_FAILED;
        }
        if self.has_stored_hooks::<ServerFunctionStarted>() {
            mask |= HOOK_SERVER_FN_STARTED;
        }
        if self.has_stored_hooks::<ServerFunctionCompleted>() {
            mask |= HOOK_SERVER_FN_COMPLETED;
        }
        if self.has_stored_hooks::<ServerFunctionRejected>() {
            mask |= HOOK_SERVER_FN_REJECTED;
        }
        if self.has_stored_hooks::<ServerFunctionFailed>() {
            mask |= HOOK_SERVER_FN_FAILED;
        }
        mask
    }
}

/// Install `registry` as the active plugin set and refresh the hook
/// bitmask cache. The mask is sampled here and is **not** recomputed
/// afterwards — the runtime has no public API for installing hooks
/// after `Server::serve`, and the per-request fast paths assume the
/// cache stays in sync with the registry.
pub(crate) fn activate(registry: PluginRegistry) {
    let hook_mask = registry.hook_mask();
    *REGISTRY.write().expect("plugin registry lock poisoned") = Arc::new(registry);
    ACTIVE_HOOK_MASK.store(hook_mask, Ordering::Release);
}

/// Reset the plugin registry to an empty state. Intended for tests
/// that activate the registry repeatedly within a single process.
#[doc(hidden)]
pub fn __reset_for_test() {
    *REGISTRY.write().expect("plugin registry lock poisoned") = Arc::new(PluginRegistry::default());
    ACTIVE_HOOK_MASK.store(0, Ordering::Release);
}

/// Dispatch `event` to every registered hook for `E`.
///
/// Hot-path callers gate on a bitmask predicate (`has_*_hooks`)
/// before calling this, so plugin-free servers pay only an atomic
/// load. Once a hook is known to exist, this function takes one
/// `RwLock::read` and one HashMap lookup — both only when at least
/// one observer is listening.
pub fn emit<E>(event: E)
where
    E: Clone + Send + Sync + 'static,
{
    let registry = REGISTRY
        .read()
        .expect("plugin registry lock poisoned")
        .clone();
    registry.emit(event);
}

#[inline]
pub fn has_server_boot_started_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_BOOT_STARTED)
}

#[inline]
pub fn has_server_listening_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_LISTENING)
}

#[inline]
pub fn has_server_boot_failed_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_BOOT_FAILED)
}

#[inline]
pub fn has_http_request_hooks() -> bool {
    active_hook_mask_contains(HOOK_HTTP_REQUEST_EVENTS)
}

#[inline]
pub fn has_http_request_started_hooks() -> bool {
    active_hook_mask_contains(HOOK_HTTP_REQUEST_STARTED)
}

#[inline]
pub fn has_http_request_completed_hooks() -> bool {
    active_hook_mask_contains(HOOK_HTTP_REQUEST_COMPLETED)
}

#[inline]
pub fn has_http_request_failed_hooks() -> bool {
    active_hook_mask_contains(HOOK_HTTP_REQUEST_FAILED)
}

#[inline]
pub fn has_server_function_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FN_EVENTS)
}

#[inline]
pub fn has_server_function_started_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FN_STARTED)
}

#[inline]
pub fn has_server_function_completed_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FN_COMPLETED)
}

#[inline]
pub fn has_server_function_rejected_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FN_REJECTED)
}

#[inline]
pub fn has_server_function_failed_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FN_FAILED)
}

#[inline]
fn active_hook_mask_contains(mask: HookMask) -> bool {
    ACTIVE_HOOK_MASK.load(Ordering::Acquire) & mask != 0
}

/// Look up the active service of type `T`. Returns `None` if no
/// plugin has provided one.
pub fn active_plugin<T: Send + Sync + 'static>() -> Option<ServerPluginHandle<T>> {
    let registry = REGISTRY
        .read()
        .expect("plugin registry lock poisoned")
        .clone();
    registry.plugin::<T>()
}
