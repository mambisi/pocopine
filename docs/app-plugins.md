# App plugins - architecture

This doc explains the app plugin lifecycle introduced by
[`RFC 076`](../rfcs/rfc-076-app-plugin-lifecycle.md). The goal is to let
optional crates install app-level behavior without editing `pocopine-core`.

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
    fn install(self, app: App) -> App;
}
```

Closures implement `AppPlugin`, so an app-local plugin can be one function:

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

## Framework Hooks

Runtime services can also subscribe to framework lifecycle events by
implementing `Hook<E>`:

```rust
impl Hook<ComponentMounted> for Analytics {
    fn call(&self, event: ComponentMounted) {
        self.track_component_mount(event.component, event.duration_ms);
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
            .hook_plugin::<Analytics, ComponentMounted>()
            .hook_plugin::<Analytics, ComponentUnmounted>()
    }
}
```

This gives plugins two integration paths:

- component authors pull `Plugin<T>` when their hook needs the service;
- the framework dispatches typed events to `Hook<E>` implementations for
  automatic integration.

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

1. verify the component registry;
2. install built-in animation styles;
3. run `before_mount` hooks;
4. mount the `[pp-app]` subtree;
5. initialize router and devtools;
6. schedule `after_mount` hooks on the next microtask.

Global integrations that must observe boot failures should run in plugin
`install`, not in `before_mount`, because registry verification happens before
`before_mount`.

## `app!` Macro Order

Compiled apps can install plugins through `plugins: [...]`:

```rust
pocopine::app! {
    components: [AppShell, Home, Report],
    plugins: [
        ObservabilityPlugin { service_name: "site" },
        live_query_plugin(),
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

## Observability Shape

Observability is the reference use case:

- `install` should initialize the chosen subscriber/exporter and any analytics
  client that must exist before boot errors;
- `provide_plugin` should store the runtime observability service;
- `hook_plugin` should attach automatic component mount/unmount handling;
- component hooks can extract `Plugin<Observability>` for app-authored events;
- event privacy still follows
  [`RFC 069`](../rfcs/rfc-069-observability.md).

Core emits framework events. The app or plugin decides where those events go.
That keeps vendor dependencies out of core while giving application code one
stable place to opt in.

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
- `Plugin<T>` panics clearly when required services are missing;
- `Option<Plugin<T>>` returns `None` when optional services are missing;
- `Hook<ComponentMounted>` and `Hook<ComponentUnmounted>` receive framework
  lifecycle events.
