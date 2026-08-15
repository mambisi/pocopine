<p align="center">
  <img src="./docs/assets/mascot.svg" alt="pocopine mascot" width="220">
</p>

<h1 align="center">pocopine</h1>

<p align="center">
  <em>A full-stack Rust application framework — reactive Rust/WASM UI,
  type-safe server functions, a local-first data layer, auth, storage,
  background jobs, and one-command deploy. One language, end to end.</em>
</p>

---

pocopine is a full-stack application framework written in Rust. The
front end is a directive-driven Rust/WASM UI layer: a Vue-3-style
reactive core (real `Proxy` traps, auto dep-tracking) wired into
compiled `.poco` template plans, with tag-based components and a
built-in SPA router. The back end is reached through a type-safe
server-function bridge — write an `async fn`, call it from the client
as a typed stub. Around that core sits a set of opt-in crates for the
rest of an application: a query-centric data layer, auth, object
storage, live updates, background jobs, observability, and deploy
adapters.

Templates are plain HTML, styles plain CSS, logic plain Rust. No
mixed-language SFCs, no virtual DOM, and no JavaScript toolchain unless
you opt into Pocopine-managed typed `.client.ts` modules. One canonical
way per decision — the framework is opinionated so application code
stays small.

> Status: **pre-1.0 / experimental.** The API is still moving; every
> breaking change lands in an RFC under [`rfcs/`](./rfcs/).

```rust
// examples/counter/src/lib.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! {
    <div>
      <p><strong pp-text="count"></strong> <span pp-text="label"></span></p>
      <button pp-on:click="decrement">-</button>
      <button pp-on:click="increment">+</button>
    </div>
})]
pub struct Counter { pub count: i32, pub label: String }

#[handlers]
impl Counter {
    pub fn increment(&mut self) { self.count += 1; }
    pub fn decrement(&mut self) { self.count -= 1; }
}

#[wasm_bindgen(start)]
pub fn main() { App::new().register::<Counter>().run(); }
```

The template is ordinary HTML, parsed and checked at compile time — a
typo in `pp-text="cuont"` fails the build, not the page.

```html
<!-- examples/counter/index.html -->
<body>
  <counter label="clicks"></counter>
  <script type="module">
    import init from "/pkg/counter.js";
    init();
  </script>
</body>
```

That's the whole counter. No virtual DOM, no build step beyond
`pocopine dev`, no `Rc<RefCell<_>>` in the author's code.

## Get started in 60 seconds

### 1. Install the CLI

The `pocopine` CLI handles building, serving, and hot-reload — one
install covers all three.

```bash
curl -fsSL https://pocopine.dev/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://github.com/mambisi/pocopine/releases/latest/download/pocopine-cli-installer.ps1 | iex
```

This installs a prebuilt binary — no Rust toolchain required. (Prefer
Cargo? `cargo install pocopine-cli` once it's published, or `./install.sh`
from a source checkout.) Then check your setup:

```bash
pocopine doctor
```

### 2. Scaffold an app

```bash
pocopine new hello-pine
cd hello-pine
just dev
```

`pocopine new` clones the starter — a small welcome app showing props,
slots, events, reactive state, and Pine Stylekit — and `just dev` builds
and serves it with live reload.

### 3. Write your first component

A component is a Rust struct plus a template:

```rust
// src/lib.rs
use pocopine::prelude::*;

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! {
    <button @click="bump">
      clicked <strong pp-text="n"></strong> times
    </button>
})]
pub struct Counter { pub n: u32 }

#[handlers]
impl Counter {
    pub fn bump(&mut self) { self.n += 1; }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<Counter>().run();
}
```

### 4. Run it

`pocopine dev` builds the wasm bundle, serves it on a local port, and
rebuilds on save.

```bash
pocopine dev
# → listening on http://127.0.0.1:5243
```

Ship with `pocopine build --release`, then `pocopine deploy`.

## The stack

pocopine is a Cargo workspace. Apps depend on the `pocopine` façade
crate (runtime + prelude) and add only the modules they need. Each
module is documented under [`docs/`](./docs).

### Core & rendering

| Crate | What it does |
|---|---|
| [`pocopine`](./crates/pocopine) | The façade crate apps depend on: runtime re-exports + `prelude`. |
| [`pocopine-core`](./crates/pocopine-core) | Reactive runtime — signals, effects, component scopes, directives, router. A Rust/WASM port of Alpine.js. |
| [`pocopine-macros`](./crates/pocopine-macros) | `#[component]`, `#[handlers]`, `#[store]`, `#[server]`. |
| [`pocopine-template-parser`](./crates/pocopine-template-parser) | Host-only `.poco` parser (html5ever); shared by the macros and Stylekit. Never linked into wasm. |
| [`pocopine-expr`](./crates/pocopine-expr) | Pure-Rust template-expression grammar (RFC 012), shared by the runtime evaluator and compile-time validation. |
| [`pocopine-stylekit`](./crates/pocopine-stylekit) | Pine Stylekit — a Pocopine-native, Tailwind-shaped utility-CSS compiler. Build-time only, no browser runtime. |

### Pine — UI primitives

| Crate | What it does |
|---|---|
| [`pine`](./crates/pine) | Unstyled, accessible UI primitives (Button, Dialog, Popover, …) ready to style. |
| [`pine-icons`](./crates/pine-icons) | Tabler Icons as a tree-shaken Pine component. |
| [`pine-charts`](./crates/pine-charts) | SVG-first chart primitives. |
| [`pine-motion`](./crates/pine-motion) | Motion.dev-style animation — springs, gestures, drag, scroll, shared-layout (FLIP). |
| [`pine-richtext`](./crates/pine-richtext) | Rust-native rich-text document model + editor state, with an optional browser view. |

### Server & types

| Crate | What it does |
|---|---|
| [`pocopine-server`](./crates/pocopine-server) | Host-side helpers for `#[server]` functions: axum integration + static-file serving. |
| [`pocopine-client-codegen`](./crates/pocopine-client-codegen) | Discovery + typed-facade generation for managed `.client.ts` modules. |
| [`pocopine-ts-rs`](./crates/pocopine-ts-rs) | Rust → TypeScript DTO generation (a Pocopine-owned fork of `ts-rs`). |

### Data & sync

| Crate | What it does |
|---|---|
| [`pocopine-sync-query`](./crates/pocopine-sync-query) | Query-centric, local-first data layer: filtered subscriptions, predicate-routed mutations, reactive selectors, typed writes. |
| [`pocopine-sync`](./crates/pocopine-sync) | Sync protocol + server plugin the query layer rides on. |
| [`pocopine-sync-sqlite`](./crates/pocopine-sync-sqlite) / [`-indexdb`](./crates/pocopine-sync-indexdb) | Local-store backends (server/native and browser). |
| [`pocopine-storage`](./crates/pocopine-storage) | Object-storage protocol + server-mediated uploads, with [`-s3`](./crates/pocopine-storage-s3), [`-gcs`](./crates/pocopine-storage-gcs), and [`-azure`](./crates/pocopine-storage-azure) backends. |

### Auth

| Crate | What it does |
|---|---|
| [`pocopine-auth`](./crates/pocopine-auth) | Auth contracts + server-function guards. |
| [`pocopine-auth-credentials`](./crates/pocopine-auth-credentials) | First-party email + password (argon2id, signup/login/logout as a `ServerPlugin`). |
| [`pocopine-auth-jwt`](./crates/pocopine-auth-jwt) | JWT verification for Firebase, Clerk, Auth0, Supabase, custom OIDC, and pocopine-issued tokens. |
| [`pocopine-auth-client`](./crates/pocopine-auth-client) | Wasm-side bearer-token bridge + fetch middleware. |

### Realtime & background work

| Crate | What it does |
|---|---|
| [`pocopine-live`](./crates/pocopine-live) | Browser live-invalidation streams (SSE) for collection/query refresh. |
| [`pocopine-events`](./crates/pocopine-events) | Event envelopes, cursors, and backends for live features. |
| [`pocopine-jobs`](./crates/pocopine-jobs) | Background jobs — Redis Streams + scheduler, periodic firings, reclaim, in-memory backend. |

### Observability

| Crate | What it does |
|---|---|
| [`pocopine-observe`](./crates/pocopine-observe) | Shared observability event contract for logging, tracing, and analytics. |
| [`pocopine-logging`](./crates/pocopine-logging) | Logging adapters for server and browser. |
| [`pocopine-analytics`](./crates/pocopine-analytics) | Analytics + telemetry adapters. |

### Deploy & ops

| Crate | What it does |
|---|---|
| [`pocopine-cli`](./crates/pocopine-cli) | `pocopine build \| run \| dev \| deploy`. |
| [`pocopine-deploy`](./crates/pocopine-deploy) | Deploy contract + adapters (RFC 080), with [`-railway`](./crates/pocopine-deploy-railway) and [`-render`](./crates/pocopine-deploy-render). Host-API-direct; no host CLIs. |
| [`pocopine-launcher`](./crates/pocopine-launcher) | Procfile-style entrypoint for the production OCI image. |

### Shared utilities

| Crate | What it does |
|---|---|
| [`pocopine-crypto`](./crates/pocopine-crypto) | Centralized hashing + checksum primitives (sha2, hmac, crc32c). |
| [`pocopine-codec`](./crates/pocopine-codec) | Shared encoding (base64, percent-encoding, serde adapters). |

## Documentation & tutorials

Full guides and tutorials live under [`docs/`](./docs). Start here:

**Concepts & guides**

- [Components](./docs/guides/components/) — structure, state, and composition. Read first.
- [Reactivity](./docs/guides/reactivity/) — effects, dep tracking, signals, and the `Proxy` bridge.
- [`.poco` templates](./docs/guides/poco/) — the template format, compilation, scoped styles, and expressions.
- [Pine Stylekit](./docs/guides/styling/stylekit.md) — the utility-CSS compiler and `@theme` tokens.
- [Animation](./docs/guides/styling/animation.md) — presets, FLIP, and the WAAPI escape hatch.
- [Routing](./docs/guides/routing/route-guards-and-loaders.md) — route guards, async loaders, and fetch middleware.
- [Server functions](./docs/guides/server/server-functions.md) — `#[server]` policies, guards, request context, and middleware extractors.
- [App plugins](./docs/guides/plugins/app-plugins.md) / [Server plugins](./docs/guides/server/server-plugins.md) — install-time setup and lifecycle ordering.
- [Sync (client)](./docs/guides/data/sync-client.md) / [Sync (server)](./docs/guides/data/sync-server.md) — the query data layer, end to end.
- [Object-storage uploads and reads](./docs/guides/data/storage-uploads.md) — server-mediated upload and short-lived read paths.
- [Logging, tracing & observers](./docs/guides/observability/logging-tracing.md) — structured events, sinks, and privacy labels.
- [Charts](./docs/guides/styling/charts/) · [Icons](./docs/guides/styling/icons.md) · [Client modules](./docs/guides/server/client-modules.md) · [Browser storage](./docs/guides/data/browser-storage.md)

**Tutorials (build something end to end)**

- [Sync: a workspace issue tracker](./docs/tutorials/issue-tracker-sync.md) — `#[query_resource]`, a `Source` impl, reactive views, typed writes.
- [Live invalidation](./docs/tutorials/live-invalidation.md) — SSE streams + collection/query refresh callbacks.
- [Email + password auth](./docs/guides/auth/credentials.md) — implement `UserStore`/`TokenStore` (Postgres + `sqlx`), plug in `Credentials`.
- [Phone OTP auth](./docs/tutorials/phone-otp-auth.md) — Twilio + Postgres on top of the credentials primitives.
- [Firebase Auth](./docs/tutorials/firebase-auth.md) — client modules + server-side token verification.
- [Background jobs](./docs/guides/jobs/jobs.md) — the job runtime, scheduling, and a live-deployment validation recipe.

## Examples

Drop into any one with `pocopine dev --path examples/<name>`:

| Example | What it shows |
|---|---|
| [`counter`](./examples/counter) | Single component, basic directives |
| [`todo`](./examples/todo) | Multi-component, slots, stores |
| [`blog`](./examples/blog) | `App` + `#[server]` + axum server bin |
| [`spa`](./examples/spa) | Router + `<pp-outlet>` + `pp-route` |
| [`hn`](./examples/hn) | Full SPA — routing, server fns, transitions, `pp-for` |
| [`live`](./examples/live) | SSE live invalidation + collection/query refresh |
| [`charts`](./examples/charts) | `pine-charts` primitives |
| [`richtext`](./examples/richtext) | `pine-richtext` editor |
| [`file-browser`](./examples/file-browser) | Storage browser shell for S3/MinIO |
| [`website`](./examples/website) | Pine UI — every primitive, side-by-side |
| [`site`](./examples/site) | The marketing page, dogfooded |
| [`tailwind`](./examples/tailwind) | Tailwind v4 + `.poco` scanning (fallback styling) |

## Architecture

Three layers you can reach for independently, with the application
modules layered on top:

1. **Runtime** — reactive engine, component scopes, directives, and the
   adopted-DOM bridge for dynamic HTML. No virtual DOM; mutations
   happen in place against real DOM nodes.
2. **Templates** — pure HTML with `pp-*` directives, written inline or
   in a `.poco` file. The `#[component]` macro wires them to Rust
   structs, emits static template metadata, and specializes eligible
   binding/listener installs at compile time.
3. **Server functions** — `#[server] async fn` on the backend; the
   client gets a typed stub that POSTs to a generated `/_pocopine/...`
   route and deserializes the response. Bodies can accept host-only
   `RequestContext` / `Extension<T>` parameters for middleware context,
   while normal owned parameters remain JSON payload fields.

On top of those sit the opt-in application modules — data/sync, auth,
storage, live, jobs, observability — most of which install as **app
plugins** (browser) or **server plugins** (host) through a single
lifecycle boundary. See [`docs/guides/plugins/app-plugins.md`](./docs/guides/plugins/app-plugins.md).

Authoritative design decisions live in [`rfcs/`](./rfcs); narrative
design notes live in [`docs/`](./docs).

### Directives

`pp-text`, `pp-html`, `pp-bind:<attr>`, `pp-on:<event>`, `pp-show`,
`pp-model`, `pp-init`, `pp-for`, `pp-if`, `pp-cloak`, `pp-transition:*`,
`pp-teleport`, `pp-ref`, `pp-route`. Component templates and lifted
`pp-if` / `pp-for` / `pp-teleport` bodies install through
macro-generated closures rather than a generic runtime applier.

## Performance

The `js-framework-benchmark` keyed-table action plan, run locally under
headless Firefox against pinned Rust/WASM and JS competitors. Numbers
are wall-clock geometric means (lower is better); vanilla is the
control because browser timing drifts between runs.

| framework  | geomean (ms) | vs vanilla |
|------------|-------------:|-----------:|
| vanilla JS |       185.41 |       1.00× |
| Vue 3      |       202.17 |       1.09× |
| **pocopine** |   **215.92** |   **1.16×** |
| Yew        |       225.07 |       1.21× |
| Leptos     |       281.45 |       1.52× |

No virtual-DOM diff runs in the hot path; generated template code and
fine-grained `Proxy` reactivity mutate real DOM nodes in place.
Reproduce locally with the harness under [`jsbench/`](./jsbench/):

```bash
./jsbench/benchmark.sh pocopine --browser firefox --no-build
./jsbench/benchmark.sh --all --browser firefox
```

## Styling

**Pine Stylekit is the default way to style pocopine apps** — a native
utility-CSS compiler with Tailwind-shaped classes, compiled in-process
at build time (no external watcher, no Node). It runs by default: write
utility classes in `.poco` templates, declare colours in an `@theme`
block, link `/pkg/stylekit.css`, and `pocopine build`/`dev` does the
rest. It parses `.poco` with the real compiler (not text scanning) and
fails loud on typos with source spans. See
[`docs/guides/styling/stylekit.md`](./docs/guides/styling/stylekit.md).

```html
<link rel="stylesheet" href="/pkg/stylekit.css" />
```

Prefer Tailwind? It stays a first-class fallback — add a
`[package.metadata.pocopine.tailwind]` block (with no `[stylekit]`
block) and Stylekit defers to it; the CLI downloads the standalone
binary and runs it alongside the build. DaisyUI works as a plugin. See
[`docs/guides/styling/stylekit.md`](./docs/guides/styling/stylekit.md) for both paths.

## Development

```bash
# cross-target checks (apps build for wasm32)
cargo check --workspace --target wasm32-unknown-unknown
cargo clippy --workspace --all-targets -- -D warnings

# core unit tests
cargo test -p pocopine-core --lib
```

PRs welcome — non-trivial features should open an RFC first (or be
paired with one in the same PR). See [`rfcs/README.md`](./rfcs/README.md)
for the convention.

## Inspiration

* [**Alpine.js**][alpine] — the directive model and author ergonomics.
* [**Vue 3**](https://github.com/vuejs/core) — the `Proxy`-based reactive core.
* [**Headless UI**](https://headlessui.com) — the `<Transition>` API that `pp-transition:*` mirrors.
* [**Solid**](https://solidjs.com) / [**Leptos**](https://leptos.dev) — fine-grained reactivity references.

## License

Dual-licensed under either of

* **Apache License, Version 2.0** ([`LICENSE-APACHE`](./LICENSE-APACHE))
* **MIT License** ([`LICENSE-MIT`](./LICENSE-MIT))

at your option.

[alpine]: https://alpinejs.dev
