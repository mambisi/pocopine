---
title: "Consuming plugins"
description: "Reach an installed app-plugin service from a component — Plugin<T> in lifecycle hooks, self.plugin::<T>() in handlers, required vs optional."
---

# Consuming plugins from the lifecycle

An [app plugin](./app-plugins.md) installs a service once with
`app.provide_plugin(Service)`. A component reaches that service two ways,
depending on **which kind of method** needs it. This is the client-side face of
the [lifecycle extractor](../components/05-extractors.md) system — `Plugin<T>` is
one extractor alongside `Refs`, `Inject`, and `Handle`.

| Method kind | How to reach the service |
|---|---|
| Lifecycle hook (`on_setup` / `on_mount` / `on_ready` / `on_unmount`) | a `Plugin<T>` / `Option<Plugin<T>>` **parameter**, or a body lookup |
| Event handler (`@event="…"`) | a **body lookup**: `self.plugin::<T>()` / `self.plugins().get::<T>()` |

Event handlers can't take lifecycle extractors as parameters, so they always use
the body lookup. Lifecycle hooks can use either.

## Lifecycle hooks — the `Plugin<T>` extractor

Declare the service as a parameter after the receiver and the framework injects
it:

```rust
fn on_ready(&self, analytics: Plugin<Analytics>) {
    analytics.track("home_ready");
}
```

`Plugin<T>` is the **required** form: if the app never installed `T`, extraction
panics with a message naming the missing service. Reusable components that work
*with or without* the integration take the **optional** form and get `None` when
it isn't installed:

```rust
fn on_unmount(&mut self, analytics: Option<Plugin<Analytics>>) {
    if let Some(analytics) = analytics {
        analytics.track("closed");
    }
}
```

A hook that already takes other extractors can do the lookup in its body instead
— common when the service is optional and the hook also needs a `Handle`:

```rust
// examples/keep — auth gate resolves the Firebase service in on_ready
pub fn on_ready(&self, handle: Handle<Self>) {
    let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
        mark_auth_unavailable(handle, "Firebase auth extension is not installed".into());
        return;
    };
    let firebase = firebase.get().clone();
    let scope = handle.scope_id();
    pocopine::spawn_for_scope(scope, async move {
        let initial = firebase.initial_user().await;
        // …subscribe + update the gate
    });
}
```

`Plugin<T>` is always valid in every lifecycle phase (it isn't element-dependent),
unlike `Refs` / `El`, which are `on_mount` / `on_ready` only — see
[Extractors → Phase validity](../components/05-extractors.md#phase-validity).

## Event handlers — body lookup

Handlers reach the service through the component, not the signature:

```rust
// Required: panics if the service isn't installed.
pub fn track_feature(&mut self) {
    self.plugin::<FrontendObservability>().emit(
        ObservedEvent::analytics("feature_used"),
    );
}

// Optional: None when not installed — the portable form for reusable components.
pub fn sign_in(&mut self) {
    let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
        self.error = "Firebase auth extension is not installed".into();
        return;
    };
    let firebase = firebase.get().clone();
    let handle = pocopine::this::<Self>();
    pocopine::spawn_for_scope(handle.scope_id(), async move {
        let result = firebase.sign_in().await;
        handle.update(|s| s.apply_action_result(result));
    });
}
```

`self.plugin::<T>()` is the required form (same panic message as `Plugin<T>`);
`self.plugins().get::<T>()` is the optional form. A handle returned by either is
cloned out (`.get().clone()`) before moving it into an async task.

## Required vs optional — which to use

| Use | When |
|---|---|
| `Plugin<T>` / `self.plugin::<T>()` | the component **cannot work** without that app capability |
| `Option<Plugin<T>>` / `self.plugins().get::<T>()` | the integration is **optional** — the component stays portable across apps that don't install it |

Reusable component families (a CTA button that *may* report to analytics) should
default to the optional form and own their opt-in locally, so the app doesn't
have to enumerate every consumer. See
[App plugins → Component-owned capability opt-in](./app-plugins.md#component-owned-capability-opt-in).

These lookups read the active app plugin registry, so they're meaningful after
`App::run()` has activated plugins — inside lifecycle hooks, DOM handlers, and
subtree mounts created after boot. Before any app has run (e.g. a bare
`mount_subtree` in a test), required lookups panic and optional lookups return
`None`.

## Related

- [App plugins](./app-plugins.md) — providing the service these lookups resolve.
- [Extractors](../components/05-extractors.md) — the full lifecycle-extractor surface (`Refs`, `Inject`, `Handle`, `Plugin`, …) and how to author your own.
- [Example: a Firebase auth extension](./example.md) — a service consumed end-to-end.
