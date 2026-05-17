//! App plugin runtime services and lifecycle hook dispatch.
//!
//! `AppPlugin` installs configuration on the [`crate::App`] builder.
//! This module stores the runtime services those installers provide,
//! exposes them to component lifecycle hooks through [`Plugin<T>`],
//! and dispatches framework lifecycle events to services that implement
//! [`Hook<E>`].

use std::any::{type_name, Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;

use crate::app::Component;
use crate::reactive::ScopeId;

type HookDispatch = Rc<dyn Fn(&PluginRegistry, &dyn Any)>;

thread_local! {
    static ACTIVE_PLUGINS: RefCell<PluginRegistry> = RefCell::new(PluginRegistry::default());
    static ACTIVE_HOOK_MASK: Cell<HookMask> = const { Cell::new(0) };
}

const APP_PROVIDER: &str = "app";
type HookMask = u16;

const HOOK_APP_BOOT_STARTED: HookMask = 1 << 0;
const HOOK_APP_BOOT_COMPLETED: HookMask = 1 << 1;
const HOOK_APP_BOOT_FAILED: HookMask = 1 << 2;
const HOOK_ROUTE_NAVIGATION_STARTED: HookMask = 1 << 3;
const HOOK_ROUTE_NAVIGATION_COMPLETED: HookMask = 1 << 4;
const HOOK_ROUTE_NAVIGATION_FAILED: HookMask = 1 << 5;
const HOOK_COMPONENT_SETUP: HookMask = 1 << 6;
const HOOK_COMPONENT_MOUNTED: HookMask = 1 << 7;
const HOOK_COMPONENT_READY: HookMask = 1 << 8;
const HOOK_COMPONENT_UNMOUNTED: HookMask = 1 << 9;
const HOOK_SERVER_FUNCTION_CLIENT_STARTED: HookMask = 1 << 10;
const HOOK_SERVER_FUNCTION_CLIENT_COMPLETED: HookMask = 1 << 11;
const HOOK_SERVER_FUNCTION_CLIENT_FAILED: HookMask = 1 << 12;
const HOOK_COMPONENT_NAME_EVENTS: HookMask =
    HOOK_COMPONENT_MOUNTED | HOOK_COMPONENT_READY | HOOK_COMPONENT_UNMOUNTED;
const HOOK_ROUTE_NAVIGATION_EVENTS: HookMask =
    HOOK_ROUTE_NAVIGATION_STARTED | HOOK_ROUTE_NAVIGATION_COMPLETED | HOOK_ROUTE_NAVIGATION_FAILED;
const HOOK_SERVER_FUNCTION_CLIENT_EVENTS: HookMask = HOOK_SERVER_FUNCTION_CLIENT_STARTED
    | HOOK_SERVER_FUNCTION_CLIENT_COMPLETED
    | HOOK_SERVER_FUNCTION_CLIENT_FAILED;

/// Per-mount snapshot of which plugin-side stamps the runtime needs to
/// produce. Sampled once from `ACTIVE_HOOK_MASK` at the top of a mount so
/// `mount_component` and `try_mount_component_as` skip both the JS-FFI
/// `Date.now()` call and the `COMPONENT_NAME_KEY` allocation when no
/// observer is listening. `needs_component_name` covers any event that
/// reads the stamp later (mounted, ready, unmounted); `needs_mount_start`
/// covers only `ComponentMounted`'s `duration_ms`.
#[derive(Clone, Copy)]
pub(crate) struct ComponentHookActivity {
    pub(crate) needs_component_name: bool,
    pub(crate) needs_mount_start: bool,
}

/// Runtime handle for a service installed by an app plugin.
///
/// Component lifecycle hooks receive this through the standard extractor
/// pipeline:
///
/// ```ignore
/// fn on_ready(&self, analytics: Plugin<Analytics>) {
///     analytics.track("ready");
/// }
/// ```
///
/// Use `Option<Plugin<T>>` for reusable components where the plugin is
/// optional.
pub struct Plugin<T: 'static> {
    service: Rc<T>,
}

impl<T: 'static> Clone for Plugin<T> {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
        }
    }
}

impl<T: 'static> Deref for Plugin<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.service.as_ref()
    }
}

impl<T: 'static> Plugin<T> {
    pub fn get(&self) -> &T {
        self.service.as_ref()
    }
}

/// App plugin service lookup handle for component methods.
#[derive(Clone, Copy, Debug, Default)]
pub struct Plugins;

impl Plugins {
    pub fn get<T: 'static>(&self) -> Option<Plugin<T>> {
        active_plugin::<T>()
    }
}

/// Convenience methods available on every pocopine component.
///
/// The blanket implementation keeps these methods out of macro-generated
/// inherent impls, so a component can still define its own method with the same
/// name if it needs to. In normal app code, importing the prelude lets handler
/// methods call `self.plugin::<T>()` or `self.plugins().get::<T>()`.
pub trait ComponentPluginExt: Component {
    fn plugins(&self) -> Plugins {
        Plugins
    }

    fn plugin<T: 'static>(&self) -> Plugin<T> {
        required_plugin::<T>()
    }
}

impl<C: Component + ?Sized> ComponentPluginExt for C {}

/// Typed framework event hook implemented by runtime plugin services.
///
/// `call` takes the event by value: every subscriber receives its own
/// copy via the dispatcher's `event.clone()`. Framework events are
/// designed so the clone is cheap — fields are `&'static str` (registry
/// names, route patterns, failure reasons) or primitives wherever
/// possible, so `Clone` collapses to a memcpy. Author-defined event
/// types should follow the same rule when high-frequency dispatch
/// matters.
pub trait Hook<E>: 'static {
    fn call(&self, event: E);
}

/// Emitted once app boot starts after plugin validation succeeds.
#[derive(Copy, Clone, Debug)]
pub struct AppBootStarted {
    pub component_count: usize,
    pub route_count: usize,
}

/// Emitted once initial app boot has mounted the root and scheduled
/// post-mount work. `duration_ms` covers synchronous boot only; deferred
/// `after_mount` callbacks are not included.
#[derive(Copy, Clone, Debug)]
pub struct AppBootCompleted {
    pub duration_ms: f64,
}

/// Emitted when app boot fails after plugin validation succeeds.
///
/// `reason` is a stable identifier from a closed set of `&'static str`
/// values produced by the runtime: `"component_registry"`,
/// `"missing_window"`, `"missing_document"`, `"missing_pp_app_root"`.
#[derive(Copy, Clone, Debug)]
pub struct AppBootFailed {
    pub reason: &'static str,
}

/// Emitted before the router tries to paint the current URL.
///
/// `path` is read from `window.location.pathname` and is therefore
/// owned. `route_pattern` and `component` reference the static route
/// registry.
#[derive(Clone, Debug)]
pub struct RouteNavigationStarted {
    pub path: String,
    pub route_pattern: Option<&'static str>,
    pub component: Option<&'static str>,
}

/// Emitted after the router paints a matched route or resolves an
/// unmatched path.
#[derive(Clone, Debug)]
pub struct RouteNavigationCompleted {
    pub path: String,
    pub route_pattern: Option<&'static str>,
    pub component: Option<&'static str>,
    pub duration_ms: f64,
}

/// Emitted when route matching succeeds but the router cannot paint
/// the route. `reason` is a stable identifier: `"missing_window"`,
/// `"missing_outlet"`, `"missing_document"`, `"create_element_failed"`,
/// `"guard_pending"`, `"guard_redirected"`.
#[derive(Clone, Debug)]
pub struct RouteNavigationFailed {
    pub path: String,
    pub route_pattern: Option<&'static str>,
    pub component: Option<&'static str>,
    pub reason: &'static str,
    pub duration_ms: f64,
}

/// Emitted before the browser client sends a generated `#[server]`
/// function request.
///
/// `route` is the public request path with any query string or fragment
/// stripped. Request arguments and response bodies are never included.
#[derive(Clone, Debug)]
pub struct ServerFunctionClientStarted {
    pub route: String,
}

/// Emitted after a generated `#[server]` client request succeeds.
#[derive(Clone, Debug)]
pub struct ServerFunctionClientCompleted {
    pub route: String,
    pub duration_ms: f64,
    pub status_code: u16,
}

/// Emitted after a generated `#[server]` client request fails before
/// returning an application value.
///
/// `error_kind` is a stable coarse reason such as `"serialize"`,
/// `"fetch"`, `"http_status"`, or `"unauthorized"`.
#[derive(Clone, Debug)]
pub struct ServerFunctionClientFailed {
    pub route: String,
    pub duration_ms: f64,
    pub error_kind: &'static str,
}

/// Component-scoped framework event.
///
/// Plugin services use this as the event bound for typed per-component
/// hooks. The runtime keeps the matching private so authors register
/// `Hook<ForComponent<C, E>>` instead of string-comparing component names.
pub trait ComponentEvent: Clone + 'static {
    fn component(&self) -> &'static str;

    fn scope_id(&self) -> ScopeId;
}

/// Typed wrapper for a component event filtered to one component type.
///
/// A service implements `Hook<ForComponent<MyComponent, ComponentMounted>>`
/// and installs it with `App::hook_component_plugin::<Service,
/// MyComponent, ComponentMounted>()`. This is for app-specific overrides where
/// the plugin intentionally targets a known component. Reusable component
/// behavior should normally be owned by the component through `Plugin<T>` or
/// `Option<Plugin<T>>` extraction.
pub struct ForComponent<C, E> {
    event: E,
    _component: PhantomData<fn() -> C>,
}

impl<C, E> ForComponent<C, E> {
    pub(crate) fn new(event: E) -> Self {
        Self {
            event,
            _component: PhantomData,
        }
    }

    pub fn event(&self) -> &E {
        &self.event
    }

    pub fn into_event(self) -> E {
        self.event
    }
}

impl<C, E: Clone> Clone for ForComponent<C, E> {
    fn clone(&self) -> Self {
        Self::new(self.event.clone())
    }
}

impl<C, E> Deref for ForComponent<C, E> {
    type Target = E;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

impl<C, E: fmt::Debug> fmt::Debug for ForComponent<C, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ForComponent").field(&self.event).finish()
    }
}

/// Emitted after a component scope has been created and before the
/// component's `on_setup` hook runs.
#[derive(Copy, Clone, Debug)]
pub struct ComponentSetup {
    pub component: &'static str,
    pub scope_id: ScopeId,
}

/// Emitted after a component subtree has been mounted and finalized.
#[derive(Copy, Clone, Debug)]
pub struct ComponentMounted {
    pub component: &'static str,
    pub scope_id: ScopeId,
    pub duration_ms: f64,
}

/// Emitted on the component ready microtask before the component's
/// `on_ready` hook runs.
#[derive(Copy, Clone, Debug)]
pub struct ComponentReady {
    pub component: &'static str,
    pub scope_id: ScopeId,
}

/// Emitted just before a component scope is removed.
#[derive(Copy, Clone, Debug)]
pub struct ComponentUnmounted {
    pub component: &'static str,
    pub scope_id: ScopeId,
}

impl ComponentEvent for ComponentSetup {
    fn component(&self) -> &'static str {
        self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentMounted {
    fn component(&self) -> &'static str {
        self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentReady {
    fn component(&self) -> &'static str {
        self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentUnmounted {
    fn component(&self) -> &'static str {
        self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

struct PluginService {
    service: Rc<dyn Any>,
    provider: &'static str,
}

struct HookRequirement {
    plugin: &'static str,
    service: &'static str,
    service_type: TypeId,
    event: &'static str,
    component: Option<&'static str>,
}

/// Boot-time plugin validation error.
///
/// The runtime records which plugin installed each hook. Before mounting the
/// app it verifies that every hook's required service has also been provided,
/// so misconfigured plugin ordering fails at boot with a concrete diagnostic
/// instead of failing later on the first matching event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginValidationError {
    pub plugin: &'static str,
    pub service: &'static str,
    pub event: &'static str,
    pub component: Option<&'static str>,
}

impl fmt::Display for PluginValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.component {
            Some(component) => write!(
                f,
                "plugin `{}` registered a hook for component `{}` and event `{}` \
                 requiring service `{}`, but that service was not provided",
                self.plugin, component, self.event, self.service
            ),
            None => write!(
                f,
                "plugin `{}` registered a hook for event `{}` requiring service `{}`, \
                 but that service was not provided",
                self.plugin, self.event, self.service
            ),
        }
    }
}

#[derive(Default)]
pub(crate) struct PluginRegistry {
    services: HashMap<TypeId, PluginService>,
    hooks: HashMap<TypeId, Vec<HookDispatch>>,
    requirements: Vec<HookRequirement>,
}

impl PluginRegistry {
    pub(crate) fn provide<T: 'static>(&mut self, service: T, provider: Option<&'static str>) {
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
                service: Rc::new(service),
                provider,
            },
        );
    }

    pub(crate) fn hook_plugin<T, E>(&mut self, plugin: Option<&'static str>)
    where
        T: Hook<E> + 'static,
        E: Clone + 'static,
    {
        let plugin = plugin.unwrap_or(APP_PROVIDER);
        self.requirements.push(HookRequirement {
            plugin,
            service: type_name::<T>(),
            service_type: TypeId::of::<T>(),
            event: type_name::<E>(),
            component: None,
        });
        self.hooks
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Rc::new(|registry, event| {
                let event = event
                    .downcast_ref::<E>()
                    .expect("plugin hook dispatched with the wrong event type")
                    .clone();
                let service = registry.plugin::<T>().unwrap_or_else(|| {
                    panic!(
                        "plugin hook for event `{}` requires plugin service `{}`, \
                         but that service is not installed. Install it with \
                         `App::provide_plugin(...)` before `App::hook_plugin::<{}, {}>()`.",
                        type_name::<E>(),
                        type_name::<T>(),
                        type_name::<T>(),
                        type_name::<E>(),
                    )
                });
                service.get().call(event);
            }));
    }

    pub(crate) fn hook_component_plugin<T, C, E>(&mut self, plugin: Option<&'static str>)
    where
        T: Hook<ForComponent<C, E>> + 'static,
        C: Component + 'static,
        E: ComponentEvent,
    {
        let plugin = plugin.unwrap_or(APP_PROVIDER);
        self.requirements.push(HookRequirement {
            plugin,
            service: type_name::<T>(),
            service_type: TypeId::of::<T>(),
            event: type_name::<E>(),
            component: Some(C::NAME),
        });
        self.hooks
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Rc::new(|registry, event| {
                let event = event
                    .downcast_ref::<E>()
                    .expect("plugin hook dispatched with the wrong event type")
                    .clone();
                if event.component() != C::NAME {
                    return;
                }
                let service = registry.plugin::<T>().unwrap_or_else(|| {
                    panic!(
                        "plugin hook for component `{}` and event `{}` requires \
                         plugin service `{}`, but that service is not installed. \
                         Install it with `App::provide_plugin(...)` before \
                         `App::hook_component_plugin::<{}, {}, {}>()`.",
                        C::NAME,
                        type_name::<E>(),
                        type_name::<T>(),
                        type_name::<T>(),
                        type_name::<C>(),
                        type_name::<E>(),
                    )
                });
                service.get().call(ForComponent::new(event));
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
                component: requirement.component,
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn plugin<T: 'static>(&self) -> Option<Plugin<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|service| service.service.clone().downcast::<T>().ok())
            .map(|service| Plugin { service })
    }

    fn emit<E>(&self, event: E)
    where
        E: Clone + 'static,
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
        if self.has_stored_hooks::<AppBootStarted>() {
            mask |= HOOK_APP_BOOT_STARTED;
        }
        if self.has_stored_hooks::<AppBootCompleted>() {
            mask |= HOOK_APP_BOOT_COMPLETED;
        }
        if self.has_stored_hooks::<AppBootFailed>() {
            mask |= HOOK_APP_BOOT_FAILED;
        }
        if self.has_stored_hooks::<RouteNavigationStarted>() {
            mask |= HOOK_ROUTE_NAVIGATION_STARTED;
        }
        if self.has_stored_hooks::<RouteNavigationCompleted>() {
            mask |= HOOK_ROUTE_NAVIGATION_COMPLETED;
        }
        if self.has_stored_hooks::<RouteNavigationFailed>() {
            mask |= HOOK_ROUTE_NAVIGATION_FAILED;
        }
        if self.has_stored_hooks::<ComponentSetup>() {
            mask |= HOOK_COMPONENT_SETUP;
        }
        if self.has_stored_hooks::<ComponentMounted>() {
            mask |= HOOK_COMPONENT_MOUNTED;
        }
        if self.has_stored_hooks::<ComponentReady>() {
            mask |= HOOK_COMPONENT_READY;
        }
        if self.has_stored_hooks::<ComponentUnmounted>() {
            mask |= HOOK_COMPONENT_UNMOUNTED;
        }
        if self.has_stored_hooks::<ServerFunctionClientStarted>() {
            mask |= HOOK_SERVER_FUNCTION_CLIENT_STARTED;
        }
        if self.has_stored_hooks::<ServerFunctionClientCompleted>() {
            mask |= HOOK_SERVER_FUNCTION_CLIENT_COMPLETED;
        }
        if self.has_stored_hooks::<ServerFunctionClientFailed>() {
            mask |= HOOK_SERVER_FUNCTION_CLIENT_FAILED;
        }
        mask
    }
}

/// Install `registry` as the active plugin set and refresh the hook
/// bitmask cache. The mask is sampled once here and is **not**
/// recomputed afterwards — the runtime has no public API for
/// installing hooks after `App::run`, and the mount/unmount fast paths
/// assume the cache stays in sync with `ACTIVE_PLUGINS`.
pub(crate) fn activate(registry: PluginRegistry) {
    let hook_mask = registry.hook_mask();
    ACTIVE_PLUGINS.with(|plugins| {
        *plugins.borrow_mut() = registry;
    });
    ACTIVE_HOOK_MASK.with(|mask| mask.set(hook_mask));
}

/// Dispatch `event` to every registered hook for `E`.
///
/// Hot-path callers gate on a bitmask predicate (`has_*_hooks`) before
/// calling this so that plugin-free apps pay only a `Cell::get`. Once a
/// hook is known to exist, this function does one HashMap lookup to
/// find the dispatch vec — the lookup is redundant with the bitmask
/// check (the bitmask is computed from the same map at activation
/// time), but eliminating it would require a parallel `[Vec<…>; N]`
/// array indexed by event id, duplicating the registration data and
/// adding a sealed `FrameworkEvent` trait. The single hash lookup is
/// only paid when at least one plugin is installed, so the duplication
/// isn't worth it.
pub(crate) fn emit<E>(event: E)
where
    E: Clone + 'static,
{
    ACTIVE_PLUGINS.with(|plugins| {
        plugins.borrow().emit(event);
    });
}

#[inline]
pub(crate) fn component_hook_activity() -> ComponentHookActivity {
    ACTIVE_HOOK_MASK.with(|active| {
        let active = active.get();
        ComponentHookActivity {
            needs_component_name: active & HOOK_COMPONENT_NAME_EVENTS != 0,
            needs_mount_start: active & HOOK_COMPONENT_MOUNTED != 0,
        }
    })
}

#[inline]
pub(crate) fn has_component_setup_hooks() -> bool {
    active_hook_mask_contains(HOOK_COMPONENT_SETUP)
}

#[inline]
pub(crate) fn has_component_mounted_hooks() -> bool {
    active_hook_mask_contains(HOOK_COMPONENT_MOUNTED)
}

#[inline]
pub(crate) fn has_component_ready_hooks() -> bool {
    active_hook_mask_contains(HOOK_COMPONENT_READY)
}

#[inline]
pub(crate) fn has_component_unmounted_hooks() -> bool {
    active_hook_mask_contains(HOOK_COMPONENT_UNMOUNTED)
}

#[inline]
pub(crate) fn has_route_navigation_hooks() -> bool {
    active_hook_mask_contains(HOOK_ROUTE_NAVIGATION_EVENTS)
}

#[inline]
pub(crate) fn has_server_function_client_hooks() -> bool {
    active_hook_mask_contains(HOOK_SERVER_FUNCTION_CLIENT_EVENTS)
}

#[inline]
fn active_hook_mask_contains(mask: HookMask) -> bool {
    ACTIVE_HOOK_MASK.with(|active| active.get() & mask != 0)
}

pub(crate) fn active_plugin<T: 'static>() -> Option<Plugin<T>> {
    ACTIVE_PLUGINS.with(|plugins| plugins.borrow().plugin::<T>())
}

pub(crate) fn required_plugin<T: 'static>() -> Plugin<T> {
    active_plugin::<T>().unwrap_or_else(|| {
        panic!(
            "plugin service `{}` is not installed. Install it from an app \
             plugin with `App::provide_plugin(...)`, or use \
             `Option<Plugin<{}>>` for reusable components where the plugin is optional.",
            type_name::<T>(),
            type_name::<T>(),
        )
    })
}

pub(crate) fn render_plugin_boot_error(errors: &[PluginValidationError]) {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(body) = doc.body() else { return };
    if let Ok(Some(existing)) = body.query_selector("[data-pocopine-boot-error=\"plugin\"]") {
        existing.remove();
    }
    let Ok(banner) = doc.create_element("div") else {
        return;
    };
    let _ = banner.set_attribute("data-pocopine-boot-error", "plugin");
    let _ = banner.set_attribute(
        "style",
        "position:fixed;inset:0;background:#1b1b1f;color:#f5f5f7;\
         font-family:ui-monospace,monospace;padding:24px;overflow:auto;\
         z-index:2147483647;",
    );
    let mut html = String::from(
        "<h2 style=\"margin:0 0 12px 0;color:#ff6b6b;\">pocopine: \
         app plugin configuration is invalid</h2>\
         <p style=\"margin:0 0 16px 0;\">The runtime refused to mount \
         because one or more plugin hooks require services that were not \
         installed.</p><ul style=\"margin:0;padding-left:20px;\">",
    );
    for err in errors {
        html.push_str("<li style=\"margin-bottom:8px;\">");
        html.push_str(&html_escape(&err.to_string()));
        html.push_str("</li>");
    }
    html.push_str("</ul>");
    banner.set_inner_html(&html);
    let _ = body.append_child(&banner);
    web_sys::console::error_1(
        &format!(
            "pocopine: app plugin configuration has {} error(s); refusing to mount",
            errors.len()
        )
        .into(),
    );
    for err in errors {
        web_sys::console::error_1(&err.to_string().into());
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
