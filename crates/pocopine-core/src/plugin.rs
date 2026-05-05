//! App plugin runtime services and lifecycle hook dispatch.
//!
//! `AppPlugin` installs configuration on the [`crate::App`] builder.
//! This module stores the runtime services those installers provide,
//! exposes them to component lifecycle hooks through [`Plugin<T>`],
//! and dispatches framework lifecycle events to services that implement
//! [`Hook<E>`].

use std::any::{type_name, Any, TypeId};
use std::cell::RefCell;
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

/// Typed framework event hook implemented by runtime plugin services.
pub trait Hook<E>: 'static {
    fn call(&self, event: E);
}

/// Component-scoped framework event.
///
/// Plugin services use this as the event bound for typed per-component
/// hooks. The runtime keeps the matching private so authors register
/// `Hook<ForComponent<C, E>>` instead of string-comparing component names.
pub trait ComponentEvent: Clone + 'static {
    fn component(&self) -> &str;

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
#[derive(Clone, Debug)]
pub struct ComponentSetup {
    pub component: String,
    pub scope_id: ScopeId,
}

/// Emitted after a component subtree has been mounted and finalized.
#[derive(Clone, Debug)]
pub struct ComponentMounted {
    pub component: String,
    pub scope_id: ScopeId,
    pub duration_ms: f64,
}

/// Emitted on the component ready microtask before the component's
/// `on_ready` hook runs.
#[derive(Clone, Debug)]
pub struct ComponentReady {
    pub component: String,
    pub scope_id: ScopeId,
}

/// Emitted just before a component scope is removed.
#[derive(Clone, Debug)]
pub struct ComponentUnmounted {
    pub component: String,
    pub scope_id: ScopeId,
}

impl ComponentEvent for ComponentSetup {
    fn component(&self) -> &str {
        &self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentMounted {
    fn component(&self) -> &str {
        &self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentReady {
    fn component(&self) -> &str {
        &self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl ComponentEvent for ComponentUnmounted {
    fn component(&self) -> &str {
        &self.component
    }

    fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

#[derive(Default)]
pub(crate) struct PluginRegistry {
    services: HashMap<TypeId, Rc<dyn Any>>,
    hooks: HashMap<TypeId, Vec<HookDispatch>>,
}

impl PluginRegistry {
    pub(crate) fn provide<T: 'static>(&mut self, service: T) {
        let previous = self.services.insert(TypeId::of::<T>(), Rc::new(service));
        assert!(
            previous.is_none(),
            "plugin service `{}` is already installed",
            type_name::<T>(),
        );
    }

    pub(crate) fn hook_plugin<T, E>(&mut self)
    where
        T: Hook<E> + 'static,
        E: Clone + 'static,
    {
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

    pub(crate) fn hook_component_plugin<T, C, E>(&mut self)
    where
        T: Hook<ForComponent<C, E>> + 'static,
        C: Component + 'static,
        E: ComponentEvent,
    {
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

    fn plugin<T: 'static>(&self) -> Option<Plugin<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|service| service.clone().downcast::<T>().ok())
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

    fn has_hooks<E: 'static>(&self) -> bool {
        self.hooks
            .get(&TypeId::of::<E>())
            .map(|hooks| !hooks.is_empty())
            .unwrap_or(false)
    }
}

pub(crate) fn activate(registry: PluginRegistry) {
    ACTIVE_PLUGINS.with(|plugins| {
        *plugins.borrow_mut() = registry;
    });
}

pub(crate) fn emit<E>(event: E)
where
    E: Clone + 'static,
{
    ACTIVE_PLUGINS.with(|plugins| {
        plugins.borrow().emit(event);
    });
}

pub(crate) fn has_hooks<E: 'static>() -> bool {
    ACTIVE_PLUGINS.with(|plugins| plugins.borrow().has_hooks::<E>())
}

/// Return the installed plugin service `T`, if the app provided it.
///
/// Lifecycle methods should usually use the `Option<Plugin<T>>` extractor.
/// Ordinary component methods and DOM event handlers can call this helper
/// directly because they do not receive a [`crate::LifecycleContext`].
pub fn optional_plugin<T: 'static>() -> Option<Plugin<T>> {
    active_plugin::<T>()
}

/// Return the installed plugin service `T`, or panic with install guidance.
///
/// Lifecycle methods should usually use the `Plugin<T>` extractor. Ordinary
/// component methods and DOM event handlers can call this helper directly when
/// the service is required.
pub fn require_plugin<T: 'static>() -> Plugin<T> {
    required_plugin::<T>()
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
