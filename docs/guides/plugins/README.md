---
title: "Plugins & extensions"
description: "The four extension surfaces — client modules, app plugins, server plugins, and how components consume them — and when to reach for each."
---

# Plugins & extensions

pocopine keeps the core small and pushes optional capability — observability,
auth, live queries, object storage, third-party JS SDKs — into **extensions** an
app opts into explicitly. Nothing here patches `pocopine-core`; the app composes
the pieces it wants from its entrypoint.

There are four surfaces, split by **where the code runs** and **who consumes it**:

| Surface | Runs | What it is | Reach for it to… |
|---|---|---|---|
| [Client modules](../server/client-modules.md) | browser (JS) | a typed Rust facade over a `.client.ts` SDK adapter (`#[client_module]`) | call an npm SDK (Firebase, Stripe.js, a map widget) from Rust |
| [App plugins](./app-plugins.md) | browser (wasm) | a client-side service + framework lifecycle hooks (`AppPlugin`) | install an app-wide capability (auth client, analytics, live queries) |
| [Consuming plugins](./consuming.md) | browser (wasm) | lifecycle / handler extraction (`Plugin<T>`, `self.plugin::<T>()`) | reach an installed service from a component |
| [Server plugins](../server/server-plugins.md) | host (native) | tower middleware + services + request hooks (`ServerPlugin`) | add host-side behavior (observability, request logging, auth layers) |

## How they fit together

The surfaces compose top to bottom: a client module gives typed access to a JS
SDK; an app plugin wraps that SDK into an app-wide service; components extract
the service from their lifecycle. Server plugins are the host-side mirror.

```text
BROWSER (wasm)
  npm SDK ─▶ Foo.client.ts ─▶ #[client_module] facade   (typed Rust over JS)
                                     │  wrapped into a service by
                                     ▼
                              App plugin ── app.provide_plugin(Service)
                                     │  extracted from a component
                                     ▼
                Plugin<T>  ·  self.plugin::<T>()  ·  self.plugins().get::<T>()

HOST (native)
  axum Router ─▶ Server plugin ── server.provide_plugin(Service)
                                  .layer(…) + hook_plugin::<T, E>()
```

The [end-to-end example](./example.md) walks a Firebase auth extension through
all three browser-side stages — client module → app plugin → consumption.

## Pick the right surface

- **Calling an npm package from Rust** → [Client modules](../server/client-modules.md). The
  CLI owns a small TypeScript toolkit; you write a typed facade and call it from Rust.
- **An app-wide client capability other components use** → [App plugins](./app-plugins.md).
  Install once at the entrypoint; the service lives for the app's lifetime.
- **Reaching an installed service from a component** → [Consuming plugins](./consuming.md).
  `Plugin<T>` in a lifecycle hook, `self.plugin::<T>()` in a handler.
- **Host-side middleware or request telemetry** → [Server plugins](../server/server-plugins.md).
  Installed on the axum `Router`; mirrors the `App` plugin shape.

Plugins are install-time builder transforms and consumption is just the
[lifecycle extractor](../components/05-extractors.md) system — `Plugin<T>` is one
extractor among `Refs`, `Inject`, `Handle`, and the rest.

## In this section

- [Client modules](../server/client-modules.md) — typed `.client.ts` SDK adapters and the npm toolchain.
- [App plugins](./app-plugins.md) — the `AppPlugin` contract, runtime services, and framework hooks.
- [Consuming plugins](./consuming.md) — extracting plugin services from lifecycle hooks and handlers.
- [Server plugins](../server/server-plugins.md) — the host-side `Server` builder, request events, and hooks.
- [Example: a Firebase auth extension](./example.md) — the full client-module → plugin → consumption chain.
