# RFC 076 - App plugin lifecycle

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-05 |
| **Related** | [`rfc-002-app-stores-servers.md`](./rfc-002-app-stores-servers.md), [`rfc-060-component-uses-registry.md`](./rfc-060-component-uses-registry.md), [`rfc-069-observability.md`](./rfc-069-observability.md), [`rfc-071-event-spine-and-live-invalidation.md`](./rfc-071-event-spine-and-live-invalidation.md) |
| **Supersedes** | - |

## 1. Summary

Add a first-class app plugin lifecycle so optional crates can install app-level
wiring without editing `pocopine-core` or asking users to copy internal startup
code.

The initial API is intentionally small:

```rust
pub trait AppPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, app: App) -> App;
}

impl<F> AppPlugin for F
where
    F: FnOnce(App) -> App,
{
    fn install(self, app: App) -> App {
        self(app)
    }
}

impl App {
    pub fn plugin<P: AppPlugin>(self, plugin: P) -> Self;
}
```

The compiled `app!{}` macro also accepts a `plugins: [...]` section:

```rust
pocopine::app! {
    components: [AppShell, Home],
    plugins: [
        observability_plugin(),
        live_query_plugin(),
    ],
    routes: [
        ("/", Home),
    ],
}
```

## 2. Motivation

RFC 069 exposed a concrete flaw in the app surface: observability can emit
framework events, but a reusable observability package cannot install browser
logging, analytics sinks, consent gates, or app startup hooks through a stable
extension point. It either asks applications to copy boilerplate into every
entrypoint or patches `pocopine-core`.

That is the wrong ownership boundary. Core should own lifecycle sequencing and
runtime invariants. Optional crates should own their integrations. Applications
should opt into those integrations declaratively.

This is now a blocking foundation for:

- observability installers;
- live-query / live-invalidation installers;
- auth/session UI installers;
- devtools and profiling installers;
- future deploy-target adapters that need one app-level startup hook.

## 3. Goals

- Let third-party crates provide app-level installers without modifying core.
- Keep plugin installation synchronous, deterministic, and ordered.
- Preserve the existing `App` builder shape.
- Make the `app!{}` macro support plugins without weakening the static
  component registry contract from RFC 060.
- Let plugins install `before_mount` / `after_mount` hooks, stores, devtools,
  logging, analytics, and other app-level setup by composing the existing
  builder.
- Let macro-installed plugins inspect the static component and route manifest
  before mount starts.
- Attach plugin/provider metadata to diagnostics without introducing a plugin
  dependency solver.
- Validate hook/service wiring before the first mount.

## 4. Non-goals

- No dynamic plugin loading.
- No async plugin installation in the first slice.
- No dependency injection container for plugins.
- No plugin priority system beyond explicit app order.
- No unhook/removal API; installed services and hooks live for the active app
  lifetime.
- No new author-authored component lifecycle methods. Components keep their
  existing `#[handlers]` surface (`on_setup` / `on_mount` / `on_ready` /
  `on_unmount`) — RFC 076 does **not** introduce new `on_X` methods that
  components have to implement. The runtime does dispatch typed
  `Component{Setup,Mounted,Ready,Unmounted}` events to plugin observers
  through `Hook<E>`, but those events fire from the runtime *to* plugins,
  not as new lifecycle methods on the component itself; component authors
  see no API change.
- No network exporter or vendor-specific observability package in this RFC.

## 5. Design

### 5.1 Plugin shape

`AppPlugin` is app-to-app:

```rust
pub trait AppPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, app: App) -> App;
}
```

This makes plugins regular builder transforms. They can add hooks, stores,
devtools, and other configuration by returning the modified app:

```rust
pub fn observability_plugin() -> impl pocopine::AppPlugin {
    |app: pocopine::App| {
        app.before_mount(|| {
            let _ = pocopine::logging::init_console_logging(
                pocopine::logging::ConsoleLoggingConfig::json(),
            );
        })
    }
}
```

Closures implement the trait for low-friction app-local plugins. Crates that
need typed configuration can expose a struct:

```rust
pub struct ObservabilityPlugin {
    pub service_name: &'static str,
}

impl pocopine::AppPlugin for ObservabilityPlugin {
    fn name(&self) -> &'static str {
        "pocopine-observability"
    }

    fn install(self, app: pocopine::App) -> pocopine::App {
        app.before_mount(move || install_observability(self.service_name))
    }
}
```

Reusable plugins should override `name()` with a stable crate-level name. Core
uses it for duplicate-service diagnostics, missing-service boot errors, logs,
devtools, and future ordering checks. App-local closures keep the default Rust
type name, and that closure type name is what appears in diagnostics.

### 5.2 Builder order

Direct builder order is exactly the order the application writes:

```rust
App::new()
    .route::<Home>("/")
    .plugin(my_plugin())
    .run();
```

In this example the plugin sees the route in `registered_routes()`. If an app
places `.plugin(...)` before `.route(...)`, the plugin does not see that later
route. This is intentional because the direct builder is explicit Rust code.

### 5.3 `app!{}` order

For macro apps, plugin order is deterministic and manifest-aware:

1. `App::new()`
2. record static component names from `components: [...]`
3. record generated routes from `routes: [...]`
4. install plugins from `plugins: [...]` in list order
5. run the authoritative static registry walk
6. validate plugin hook/service wiring
7. activate plugins and emit `AppBootStarted`
8. verify registry
9. run `before_mount`
10. mount `[pp-app]`
11. initialize router/devtools
12. schedule `after_mount`
13. emit `AppBootCompleted`

This lets plugins inspect `registered_components()` and
`registered_routes()` before mount while preserving RFC 060's rule that
`run_with_registry` is the only component registration path for `app!{}`.

### 5.4 Static component manifest

`app!{}` records component names on the `App` builder before plugin install,
but it does not call component registration. The macro emits hidden
`component_static(name)` calls for metadata only:

```rust
App::new()
    .component_static("app-shell")
    .component_static("home")
    .route_static::<Home>("/")
    .plugin(my_plugin())
    .run_with_registry(&REGISTRY);
```

The actual registration still happens through `run_with_registry(&REGISTRY)`.

### 5.5 Failure semantics and diagnostics

Plugin installation is synchronous and may panic like ordinary app startup
code. The framework does not catch plugin panics in this RFC.

`App::provide_plugin<T>` records the provider name. Installing the same service
type twice is a configuration error and panics immediately with both provider
names.

`App::hook_plugin::<T, E>` and
`App::hook_component_plugin::<T, C, E>` record the service requirement they add.
`App::run()` validates those requirements before activating plugins. A hook may
be registered before or after the corresponding `provide_plugin`, but by boot
time every required service must exist. Missing services render a fixed plugin
boot error and log a diagnostic that names the plugin, service, event, and
component filter when present.

This RFC deliberately does not add declarative plugin dependencies or
topological sorting. The app's plugin order remains the only ordering rule.
Hooks for the same event dispatch in registration order: app plugin install
order first, then each plugin's internal `hook_plugin` /
`hook_component_plugin` call order.

If a plugin installs a hook and that hook panics, it follows the existing hook
behavior. This RFC does not introduce hook isolation. Integrations that need
failure isolation, such as analytics sinks, must provide it inside their own
crate.

### 5.6 Runtime services and lifecycle extractors

`AppPlugin` is the install-time shape. If components need to use the plugin,
the installer provides a runtime service:

```rust
impl AppPlugin for AnalyticsPlugin {
    fn install(self, app: App) -> App {
        app.provide_plugin(Analytics::new(self.config))
    }
}
```

Component lifecycle methods extract that service with `Plugin<T>`:

```rust
fn on_ready(&self, analytics: Plugin<Analytics>) {
    analytics.track("ready");
}
```

`Plugin<T>` is required. If the service is missing, extraction panics with a
message that names the missing type and tells the author to install it through
`App::provide_plugin(...)` or use the optional form.

Reusable components use `Option<Plugin<T>>`:

```rust
fn on_unmount(&mut self, analytics: Option<Plugin<Analytics>>) {
    if let Some(analytics) = analytics {
        analytics.track("closed");
    }
}
```

The optional extractor returns `None` when the service is not installed.

Ordinary component methods and DOM event handlers make the component own the
lookup:

```rust
fn on_click(&self) {
    if let Some(analytics) = self.plugins().get::<Analytics>() {
        analytics.track("clicked");
    }
}
```

`self.plugin::<T>()` is the required form and panics with the same missing
service message as the lifecycle extractor. `self.plugins().get::<T>()` is the
optional form.

Plugin lookups read the active app plugin registry. They are meaningful after
`App::run()` has activated plugins. A subtree mounted before any app has run
observes an empty registry: required lookups panic and optional lookups return
`None`.

Reusable component families should usually own their integration through these
extractors. For example, a CTA tracking plugin provides `CtaTracking` once, and
every CTA button instance can opt in without the app enumerating button
component types:

```rust
pub fn firebase_cta_tracking(config: FirebaseConfig) -> impl AppPlugin {
    move |app: App| app.provide_plugin(CtaTracking::new(config))
}

#[handlers]
impl CtaButton {
    pub fn on_ready(&self, cta: Option<Plugin<CtaTracking>>) {
        if let Some(cta) = cta {
            cta.impression(&self.analytics_id);
        }
    }

    pub fn on_click(&self) {
        if let Some(cta) = self.plugins().get::<CtaTracking>() {
            cta.click(&self.analytics_id);
        }
    }
}
```

Use `Plugin<T>` when the component requires the app capability to function.
Use `Option<Plugin<T>>` when the component remains reusable without it.

### 5.7 Framework event hooks

Runtime services can implement typed hook traits for app-wide framework events.
This is for global/default behavior such as automatic lifecycle telemetry:

```rust
impl Hook<ComponentSetup> for Analytics {
    fn call(&self, event: ComponentSetup) {
        self.track_component_setup(event.component);
    }
}

impl Hook<ComponentMounted> for Analytics {
    fn call(&self, event: ComponentMounted) {
        self.track_component_mount(event.component, event.duration_ms);
    }
}

impl Hook<ComponentReady> for Analytics {
    fn call(&self, event: ComponentReady) {
        self.track_component_ready(event.component);
    }
}

impl Hook<ComponentUnmounted> for Analytics {
    fn call(&self, event: ComponentUnmounted) {
        self.track_component_unmount(event.component);
    }
}
```

Installers opt into those dispatch paths explicitly:

```rust
impl AppPlugin for AnalyticsPlugin {
    fn install(self, app: App) -> App {
        app.provide_plugin(Analytics::new(self.config))
            .hook_plugin::<Analytics, ComponentSetup>()
            .hook_plugin::<Analytics, ComponentMounted>()
            .hook_plugin::<Analytics, ComponentReady>()
            .hook_plugin::<Analytics, ComponentUnmounted>()
    }
}
```

Component-specific hooks use a typed wrapper. This path is for app-specific
overrides or special cases where the plugin intentionally targets one known
component type:

```rust
impl Hook<ForComponent<CheckoutPage, ComponentMounted>> for Analytics {
    fn call(&self, event: ForComponent<CheckoutPage, ComponentMounted>) {
        self.track_checkout_mount(event.duration_ms);
    }
}

impl AppPlugin for AnalyticsPlugin {
    fn install(self, app: App) -> App {
        app.provide_plugin(Analytics::new(self.config))
            .hook_component_plugin::<Analytics, CheckoutPage, ComponentMounted>()
    }
}
```

The component filter is type-level at the plugin boundary. Core still emits a
canonical component name internally, and `hook_component_plugin` performs the
match before invoking `Hook<ForComponent<C, E>>`. Reusable component families
should not depend on this path as their primary integration model; they should
prefer `Option<Plugin<T>>` and opt in from the component hook itself.

The first framework events are:

- `AppBootStarted { component_count, route_count }`;
- `AppBootCompleted { duration_ms }`;
- `AppBootFailed { reason }`;
- `ComponentSetup { component, scope_id }`;
- `ComponentMounted { component, scope_id, duration_ms }`;
- `ComponentReady { component, scope_id }`;
- `ComponentUnmounted { component, scope_id }`;
- `RouteNavigationStarted { path, route_pattern, component }`;
- `RouteNavigationCompleted { path, route_pattern, component, duration_ms }`;
- `RouteNavigationFailed { path, route_pattern, component, reason, duration_ms }`.

Core emits these from the compiled mount/release path. Plugins decide whether
to subscribe.

Route events include the current URL path and, when matching succeeds, the
static route pattern. Observability integrations should prefer the route
pattern for aggregate analytics and treat raw paths as potentially
identifying.

`AppBootCompleted.duration_ms` measures synchronous boot through root mount and
post-mount scheduling. It excludes the deferred execution time of
`after_mount` callbacks.

## 6. Privacy and Reliability

Plugins are powerful by design: they can install logging, analytics, network
clients, and global runtime hooks. Core therefore keeps two rules:

- core does not install vendor exporters by default;
- plugins must be explicitly opted into by app code.

Observability plugins must continue to follow RFC 069's redaction and
allowlist posture. This RFC only creates the installation hook; it does not
weaken the event privacy contract.

## 7. Initial Implementation

The first slice ships:

- `pocopine_core::AppPlugin`;
- `AppPlugin::name`;
- blanket `AppPlugin` implementation for `FnOnce(App) -> App`;
- `App::plugin`;
- `App::provide_plugin`;
- `App::hook_plugin`;
- `App::hook_component_plugin`;
- duplicate-service diagnostics with plugin/provider names;
- boot-time hook/service validation and plugin boot errors;
- cached hook-activity bits so plugin-free component mount/unmount paths avoid
  plugin-only metadata stamps, `Date.now()` calls, and component-name
  allocations;
- `Plugin<T>` and `Option<Plugin<T>>` lifecycle extractors;
- `self.plugin::<T>()` and `self.plugins().get::<T>()` component-owned plugin
  accessors for ordinary component methods and DOM event handlers;
- `Hook<E>` framework-event dispatch;
- `Hook<ForComponent<C, E>>` component-filtered dispatch;
- `ComponentSetup` / `ComponentMounted` / `ComponentReady` /
  `ComponentUnmounted` events;
- `AppBoot*` and `RouteNavigation*` events;
- hidden `App::component_static` for `app!{}` manifest metadata;
- `pocopine::AppPlugin` and prelude re-exports;
- `app! { plugins: [...] }`;
- wasm tests for direct builder plugins, macro plugins, plugin extractors, and
  framework hook dispatch.

Later slices can add convenience plugins in separate crates, beginning with
observability.
