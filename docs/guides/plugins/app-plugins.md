---
title: "App plugins"
description: "The app-plugin architecture: install-time setup, lifecycle ordering, and the ownership boundary."
---

# App plugins

The app plugin system, introduced by
[RFC 076](../../../rfcs/rfc-076-app-plugin-lifecycle.md), lets optional crates
install app-level behavior without editing `pocopine-core`.

Plugins are the boundary between framework-owned startup and integration-owned
setup:

- core owns registry verification, route mounting, compiled-template runtime,
  component lifecycle, and app boot ordering;
- plugin crates own optional integrations such as observability, live queries,
  auth/session UI, devtools, profiling, and deploy-target adapters;
- applications opt into plugins explicitly from their entrypoint.

## The Contract

A plugin is a builder transform:

```rust
pub trait AppPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, app: App) -> App;
}
```

`FnOnce(App) -> App` implements `AppPlugin`, so an app-local plugin can be a
plain function or closure:

```rust
fn local_plugin(app: pocopine::App) -> pocopine::App {
    app.before_mount(|| {
        tracing::info!(target: "pocopine.log", "mounting app");
    })
}

pocopine::App::new()
    .route::<Home>("/")
    .plugin(local_plugin)
    .run();
```

Reusable crates should expose a typed constructor:

```rust
pub struct ObservabilityPlugin {
    pub service_name: &'static str,
}

pub struct Observability {
    pub service_name: &'static str,
}

impl pocopine::AppPlugin for ObservabilityPlugin {
    fn name(&self) -> &'static str {
        "pocopine-observability"
    }

    fn install(self, app: pocopine::App) -> pocopine::App {
        let _ = pocopine::logging::init_console_logging(
            pocopine::logging::ConsoleLoggingConfig::json(),
        );

        app.provide_plugin(Observability {
            service_name: self.service_name,
        })
        .hook_plugin::<Observability, pocopine::ComponentMounted>()
        .hook_plugin::<Observability, pocopine::ComponentUnmounted>()
    }
}
```

The important point: `install` runs while the app builder is still being
assembled. It can do immediate global setup and can also attach later lifecycle
hooks.

`name()` is optional, but reusable plugin crates should override it with a
stable string. Core uses the name in duplicate-service panics, boot diagnostics,
devtools, and future ordering checks. Without an override, `name()` returns
`std::any::type_name::<Self>()` — opaque enough for app-local plugins but noisy
for published integrations.

## Runtime Services

`AppPlugin` is the install-time object. Components should not receive the
installer. They receive the runtime service the installer provides:

```rust
impl pocopine::AppPlugin for AnalyticsPlugin {
    fn install(self, app: pocopine::App) -> pocopine::App {
        app.provide_plugin(Analytics::new(self.config))
    }
}
```

Component lifecycle hooks extract provided services through `Plugin<T>`:

```rust
fn on_ready(&self, analytics: Plugin<Analytics>) {
    analytics.track("home_ready");
}
```

`Plugin<T>` is the required form. If the app did not install `T`, extraction
panics with a clear message naming the missing service and telling the author
to either install it with `App::provide_plugin(...)` or use the optional form.

Reusable components should prefer the optional form when the integration is not
mandatory:

```rust
fn on_unmount(&mut self, analytics: Option<Plugin<Analytics>>) {
    if let Some(analytics) = analytics {
        analytics.track("closed");
    }
}
```

`Option<Plugin<T>>` returns `None` when the service is not installed.

Ordinary component methods and DOM event handlers should make the component own
the lookup:

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

These lookups read the active app plugin registry. They are meaningful after
`App::run()` has activated plugins, including inside lifecycle hooks, DOM
event handlers, and subtree mounts created after app boot. If a test or host
page calls `App::mount_subtree::<C>()` before any app has run, required plugin
lookups panic and optional lookups return `None`.

### Component-Owned Capability Opt-In

Reusable components should usually extend plugin capabilities from their own
hooks. The plugin installs the capability once; every component instance that
knows how to use it opts in locally.

```rust
pub struct CtaTracking {
    sink: FirebaseSink,
}

impl CtaTracking {
    pub fn new(config: FirebaseConfig) -> Self {
        Self {
            sink: FirebaseSink::new(config),
        }
    }

    pub fn impression(&self, id: &str) {
        self.sink.track("cta_impression", id);
    }

    pub fn click(&self, id: &str) {
        self.sink.track("cta_click", id);
    }
}

pub fn firebase_cta_tracking(config: FirebaseConfig) -> impl AppPlugin {
    move |app: App| app.provide_plugin(CtaTracking::new(config))
}
```

The button component owns the opt-in:

```rust
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

The app does not enumerate every button:

```rust
pocopine::app! {
    components: [AppShell, PricingPage, CtaButton],
    plugins: [firebase_cta_tracking(firebase_config)],
    routes: [("/", PricingPage)],
}
```

Use `Plugin<T>` instead of `Option<Plugin<T>>` only when the component cannot
work without that app capability.

## Framework Hooks And Diagnostics

Runtime services can also subscribe to app-wide framework lifecycle events by
implementing `Hook<E>`. This is for global/default behavior such as automatic
mount telemetry:

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

The installer wires those implementations into the app:

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

Plugins can also attach an override or special case to one known component type
without string filters in the hook body. The service implements
`Hook<ForComponent<C, E>>` and the installer uses `hook_component_plugin`:

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

The runtime still emits one canonical component name. `ForComponent<C, E>` is
the typed filter: the hook only fires when the emitted component is `C`. This
is not the primary extension path for reusable component families like CTA
buttons; those components should prefer `Option<Plugin<T>>` and own their
plugin opt-in locally.

This gives plugins four integration paths:

- component authors pull `Plugin<T>` when their hook needs the service;
- reusable components pull `Option<Plugin<T>>` when an integration is optional;
- the framework dispatches typed events to `Hook<E>` implementations for
  global/default behavior;
- component-specific integrations use `Hook<ForComponent<C, E>>` and
  `hook_component_plugin` for app-specific overrides.

Plugin hook services are validated before the first mount. A hook registered
with `hook_plugin::<Analytics, ComponentMounted>()` may be declared before or
after `provide_plugin(Analytics::new(...))`, but by `App::run()` every required
service must be installed. If not, core renders a fixed boot error and logs a
message naming:

- the plugin that installed the hook;
- the missing service type;
- the event type;
- the component type for `hook_component_plugin`.

Duplicate services also fail immediately and name both providers:

```text
plugin service `Analytics` is already installed
(first provider: `pocopine-observability`, second provider: `app`)
```

This keeps plugin ordering explicit. The framework does not infer dependency
graphs yet; install plugins in the order the app wants:

```rust
App::new()
    .plugin(logging_plugin())
    .plugin(analytics_plugin())
    .run();
```

Hooks for the same event run in registration order. For direct builder code,
that is the order `hook_plugin` / `hook_component_plugin` calls are made. For
multiple installed plugins, it follows app plugin installation order and then
each plugin's internal hook registration order.

## Lifecycle Order

Direct `App` builder calls run in the order the app writes them:

```rust
App::new()
    .route::<Home>("/")
    .plugin(my_plugin())
    .run();
```

Here the plugin can inspect `registered_routes()` and see `"/"`. If the app
places `.plugin(...)` before `.route(...)`, the plugin does not see the later
route. This is deliberate: direct builder code is ordinary Rust builder order.

After `run()` starts, the runtime order is:

1. validate plugin hook/service wiring;
2. activate plugin services and emit `AppBootStarted`;
3. verify the component registry;
4. check that every `$store.X` reference in compiled templates has a
   corresponding `App::store::<T>()` registration;
5. install built-in animation styles;
6. run `before_mount` hooks;
7. mount the `[pp-app]` subtree;
8. initialize router and devtools;
9. schedule `after_mount` hooks on the next microtask;
10. emit `AppBootCompleted`.

Core emits `AppBootFailed` for boot failures that happen after plugins are
valid and active. These include component registry conflicts
(`"component_registry"`), missing `$store` registrations
(`"missing_store_registration"`), missing `[pp-app]` root
(`"missing_pp_app_root"`), and missing browser globals (`"missing_window"`,
`"missing_document"`). Plugin validation failures happen before activation, so
invalid plugin wiring renders the plugin boot error instead of dispatching hook
events through a known-bad plugin graph.

## Framework Event Surface

The first frontend framework events are:

```rust
AppBootStarted {
    component_count,
    route_count,
}

AppBootCompleted {
    duration_ms,
}

AppBootFailed {
    reason,  // "component_registry" | "missing_store_registration"
             // | "missing_pp_app_root" | "missing_window" | "missing_document"
}

ComponentSetup {
    component,
    scope_id,
}

ComponentMounted {
    component,
    scope_id,
    duration_ms,
}

ComponentReady {
    component,
    scope_id,
}

ComponentUnmounted {
    component,
    scope_id,
}

RouteNavigationStarted {
    path,
    route_pattern,
    component,
}

RouteNavigationCompleted {
    path,
    route_pattern,
    component,
    duration_ms,
}

RouteNavigationFailed {
    path,
    route_pattern,
    component,
    reason,
    duration_ms,
}

ServerFunctionClientStarted {
    route,
}

ServerFunctionClientCompleted {
    route,
    duration_ms,
    status_code,
}

ServerFunctionClientFailed {
    route,
    duration_ms,
    error_kind,
}
```

Route events include both the current URL path and the matched route pattern
when one exists. Observability plugins should prefer `route_pattern` for
aggregate analytics and treat `path` as potentially identifying, because apps
often encode IDs in route segments.

Server-function client events include the public request path with query
strings and fragments stripped. They never include serialized arguments,
response bodies, headers, or cookies.

`AppBootCompleted.duration_ms` measures synchronous boot through root mount and
post-mount scheduling. It does not include execution time for deferred
`after_mount` callbacks.

## `app!` Macro Order

Compiled apps can install plugins through `plugins: [...]`:

```rust
pocopine::app! {
    components: [AppShell, Home, Report],
    plugins: [
        ObservabilityPlugin { service_name: "site" },
        query_client_plugin(),
    ],
    routes: [
        ("/", Home),
        ("/report/:id", Report),
    ],
}
```

For macro apps, the generated order is intentionally manifest-aware:

1. create `App::new()`;
2. record static component names from `components: [...]`;
3. record generated routes from `routes: [...]`;
4. install plugins from `plugins: [...]` in list order;
5. run the authoritative static registry walk;
6. enter the normal `run()` sequence.

This means plugins can inspect `registered_components()` and
`registered_routes()` before mount starts. Component registration still happens
only in `run_with_registry(&REGISTRY)`, preserving the RFC 060 static registry
contract.

## Ownership Rules

Plugins may:

- initialize global browser/server logging chosen by the application;
- install analytics or live-query clients;
- add `before_mount` and `after_mount` hooks;
- enable devtools or profiling;
- inspect component and route metadata already recorded on the builder;
- register stores when that is part of the integration.

Plugins should not:

- patch `pocopine-core` startup to install themselves;
- call hidden macro-only APIs such as `component_static`;
- register route components behind the macro's static registry path;
- scrape raw DOM text, route params, request bodies, cookies, or tokens into
  analytics/logging fields;
- rely on async work completing during `install`.

Plugin installation is synchronous. If a plugin needs async setup, it should
spawn its own task or install a lifecycle hook that starts the work at the right
time.

There is no unhook/removal API. Installed plugin services and hooks live for
the active app lifetime; dynamic plugin loading and unloading are out of scope.

## Observability Shape

Observability is the reference use case:

- `install` should initialize the chosen subscriber/exporter and any analytics
  client that must exist before boot errors;
- `provide_plugin` should store the runtime observability service;
- `hook_plugin` should attach automatic app-wide component handling;
- `hook_component_plugin` should attach page- or component-specific telemetry
  overrides without string filters in the hook implementation;
- `hook_plugin` should subscribe to `AppBoot*` and `RouteNavigation*` events
  for boot and navigation telemetry;
- `hook_plugin` should subscribe to `ServerFunctionClient*` events for
  browser-side generated `#[server]` request telemetry;
- component hooks can extract `Plugin<Observability>` for app-authored events;
- reusable components can extract `Option<Plugin<Observability>>` to
  participate when observability is installed and remain portable when it is
  not;
- event privacy still follows
  [`RFC 069`](../../../rfcs/rfc-069-observability.md).

Core emits framework events. The app or plugin decides where those events go.
That keeps vendor dependencies out of core while giving application code one
stable place to opt in.

The runtime caches installed framework hooks into a small bitmask at
`App::run()` activation. Component mount/unmount hot paths use that bitmask to
avoid plugin-only DOM metadata stamps, `Date.now()` calls, and component-name
allocations when no matching hook is installed.

## Testing Contract

Any plugin-facing change should keep these tests green:

```sh
wasm-pack test --firefox --headless crates/pocopine --test app_plugins
wasm-pack test --firefox --headless crates/pocopine
cargo clippy --workspace --all-targets -- -D warnings
```

The dedicated app-plugin tests assert:

- direct builder plugins can install lifecycle hooks;
- `app!` plugins see static component and route metadata before mount;
- plugin-installed `before_mount` and `after_mount` hooks run in runtime order;
- plugin names appear in duplicate-service diagnostics;
- missing hook services render a plugin boot error before mount;
- app boot hooks fire on successful and failed boot;
- route navigation hooks fire for matched routes;
- plugin-free component mounts do not stamp plugin-only component-name or
  mount-timing metadata;
- `Plugin<T>` panics clearly when required services are missing;
- `Option<Plugin<T>>` returns `None` when optional services are missing;
- reusable components can use `self.plugins().get::<T>()` from ordinary event
  handlers;
- `Hook<ComponentSetup>`, `Hook<ComponentMounted>`, `Hook<ComponentReady>`,
  and `Hook<ComponentUnmounted>` receive framework lifecycle events;
- `Hook<ForComponent<C, E>>` only fires for the selected component type.
