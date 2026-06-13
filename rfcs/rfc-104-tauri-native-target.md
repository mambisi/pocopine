# RFC 104 - Tauri native target: ship the wasm app as a desktop binary

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Author** | pocopine team |
| **Created** | 2026-06-13 |
| **Related** | [`rfc-077-server-plugin-lifecycle.md`](./rfc-077-server-plugin-lifecycle.md) (the `Server` builder this reuses), [`rfc-078-client-route-guards-and-loaders.md`](./rfc-078-client-route-guards-and-loaders.md) (`fetch::call` middleware chain), [`rfc-099-ssr-hydration.md`](./rfc-099-ssr-hydration.md) (host-side render — future native first-paint), [`rfc-080-deploy-contract.md`](./rfc-080-deploy-contract.md) (web deploy is orthogonal; native is a *distribution* target), [`rfc-100-asset-pipeline.md`](./rfc-100-asset-pipeline.md) (`POCOPINE_ASSET_BASE` for native), `docs/internal/roadmap-0.2.x.md` |

## 1. Summary

pocopine apps compile to wasm and run in a browser; `#[server]`
functions run host-side behind an axum HTTP server. This RFC adds a
**native desktop target** without forking the runtime: the *exact same
wasm bundle* runs inside a [Tauri](https://tauri.app) webview, and the
app's `#[server]` functions run **in the same native process** as the
window — reached over a custom URI scheme that drives the existing
axum `Router` in-process. No open TCP port, no IPC layer, and **zero
changes to `pocopine-core`**.

The whole feature is two new host crates — `pocopine-native` (the
backend-neutral transport core) and `pocopine-native-tauri` (the Tauri
webview backend) — two CLI subcommands (`pocopine native dev` /
`pocopine native build`), and an `src-tauri/` scaffold per app. The web
target is untouched.

```text
 ┌──────────────────────── Tauri process (native, one Tokio runtime) ───────────────────────┐
 │                                                                                            │
 │   ┌─ WebView (WKWebView / WebView2 / WebKitGTK) ─┐      ┌─ Rust backend ─────────────────┐ │
 │   │  document  pocopine://localhost/             │      │                                │ │
 │   │  pkg/<app>_bg.<hash>.wasm  (UNCHANGED)       │      │  axum Router                   │ │
 │   │  pocopine-core reactive runtime              │      │   = Server::new(Router::new()) │ │
 │   │                                              │      │     · inventory!(#[server])    │ │
 │   │  window.fetch("/_pocopine/save_a1b2")        │      │     · static_files(pkg)        │ │
 │   └───────────────────────┬──────────────────────┘      └───────────────▲────────────────┘ │
 │                           │  scheme handler: http::Request                │                  │
 │                           └────────────►  bridge::dispatch ──────────────┘                  │
 │                                          Router::oneshot(req).await                          │
 └────────────────────────────────────────────────────────────────────────────────────────────┘
```

## 2. Motivation

- **One codebase, two shells.** Teams that already have a pocopine web
  app want a desktop build (offline-friendly, native menus, file-system
  access, auto-update, dock/tray) without rewriting components or
  re-learning a second framework.
- **Electron is heavy.** Tauri ships a ~3–10 MB binary that reuses the
  OS webview instead of bundling Chromium. The pocopine wasm bundle is
  already the payload; Tauri just gives it a window and a native
  backend.
- **Server functions become local calls.** In a desktop app there is no
  remote server — yet apps still want the `#[server]` programming model
  (typed args, guards, plugins) for local privileged work (SQLite, the
  filesystem, OS keychain). Running the *same* router in-process keeps
  the authoring model identical between web and native.

### 2.1 Non-goals

- **No native renderer.** `pocopine-core` drives a DOM via `web_sys`.
  Rendering the component tree to a *native* widget set (the
  Dioxus-desktop / GPUI approach) is a separate runtime and explicitly
  out of scope. The component tree always lives in a webview.
- **No mobile (iOS/Android) in v1.** Tauri v2 supports mobile; we defer
  it until the desktop path is stable. The backend crate is named for
  the toolkit (`pocopine-native-tauri`), not the platform, so mobile can
  land later without a rename; the backend-neutral core is
  `pocopine-native`.
- **No second authoring mode.** `self.x = 1`, `.poco` templates, and
  `#[server]` stay exactly as they are. Native is a packaging decision,
  not an API.

## 3. Why wasm-in-webview (and not native-render)

`pocopine-core` is a DOM runtime: bindings, structural directives, and
`pp-for` pools all manipulate `web_sys` nodes, and `fetch::perform_fetch`
bottoms out at `window.fetch` (`crates/pocopine-core/src/fetch.rs`).
Three consequences:

1. The runtime needs a **browser engine**. Every desktop OS ships one
   (WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux); Tauri
   wraps them behind one API (`wry`/`tao`). `window`, `js_sys::Date`,
   `fetch`, custom elements — all present. **The wasm runs unmodified.**
2. A native-widget backend would mean re-implementing the entire
   render/hydration path against a non-DOM tree. That is a different
   product, not an incremental target.
3. Because the runtime is unchanged, **the wasm bundle is byte-identical
   between web and native.** The native target is purely additive: a
   window + an in-process backend.

## 4. Transport: custom URI scheme → `Router::oneshot`

The single interesting design decision is how `window.fetch("/_pocopine/…")`
inside the webview reaches the app's `#[server]` handlers.

| Option | Open port? | wasm-side change | Reuses `#[server]` router |
|---|---|---|---|
| **A. Custom URI scheme → `oneshot`** ✅ | No | **none** | yes, verbatim |
| B. Tauri IPC (`invoke`) + fetch middleware | No | new client crate + dispatch shim | needs adapter |
| C. Embed axum on `127.0.0.1:<rand>` | **yes** | none | yes, verbatim |

This RFC adopts **Option A**. Tauri v2's
`register_asynchronous_uri_scheme_protocol` hands the Rust side an
`http::Request<Vec<u8>>` for every request the webview makes under a
registered scheme and lets it answer with an `http::Response`. axum's
`Router` *is* a `tower::Service<http::Request<Body>>`, so the request is
fed straight into the same router the web server would `serve()`:

```text
 window.fetch("/_pocopine/save_a1b2", {POST, body})
        │  scheme = pocopine  (document origin)
        ▼
 register_asynchronous_uri_scheme_protocol("pocopine", handler)
        │  http::Request<Vec<u8>>
        ▼
 bridge::dispatch(router.clone(), req):
        ├─ Request<Vec<u8>>  → Request<axum::body::Body>
        ├─ router.oneshot(req).await        (Error = Infallible)
        └─ Response<Body>    → Response<Vec<u8>>   (collect bytes)
        ▼
 responder.respond(http::Response<Cow<[u8]>>)
```

**Why A over B/C.**

- **vs C (localhost):** an open `127.0.0.1` port is reachable by any
  other process on the machine (and any web page via DNS-rebinding to
  localhost). The custom scheme is in-process only — there is no socket.
  C is fine as a throwaway spike; it is not the shipping design.
- **vs B (IPC):** Tauri's `invoke()` would require a *new wasm-side
  crate* (a `FetchMiddleware` that serialises calls over the JS IPC
  bridge) plus a host dispatch shim that re-derives routing from the
  `#[server]` inventory. Option A reuses the HTTP semantics the whole
  stack already speaks (status codes, headers, the auth middleware, the
  `pocopine-server` plugin chain) and needs **no** wasm-side code.

The document itself is served the same way: the window points at
`pocopine://localhost/`, and the scheme handler answers `/`, `/index.html`,
`pkg/*.wasm`, and CSS from the same router's `static_files` fallback. One
handler, one router, every request.

### 4.1 Same-origin by construction

Every native request originates from the app's own webview hitting the
in-process router; there is no network listener and no cross-origin or
CSRF surface. The bridge makes that explicit by stamping
`Sec-Fetch-Site: same-origin` on each dispatched request before it reaches
the router. This is factually accurate for native, and it lets
server-side origin/CSRF guards written for the web accept native calls
unchanged — e.g. `pocopine-storage`'s mutation-origin check, which
otherwise rejects WebKitGTK custom-scheme `fetch`es because they carry
neither `Origin` nor `Sec-Fetch-Site` ("storage mutation origin could not
be validated"). Guards that inspect `Origin`/`Referer` specifically may
need the same treatment extended (synthesizing a matching `Origin`); the
`Sec-Fetch-Site` stamp covers the common pattern. This is the one place
the native transport rewrites a request — everything else is passed
through verbatim.

## 5. The crates: `pocopine-native` + `pocopine-native-tauri`

The native target is split into a backend-neutral core and a Tauri
backend, so the *interesting, testable* part carries no webview
dependency and the toolkit-specific part is isolated behind one crate
boundary. A future non-Tauri backend would be `pocopine-native-<x>`
against the same core.

```text
crates/pocopine-native/                 (NO tauri dep — always compiles, holds the tests)
  src/
    lib.rs            NativeApp builder + NativeApp::into_parts for backends
    bridge.rs         http::Request ⇄ axum::Router ⇄ http::Response   (unit-tested)
    assets.rs         dev_dir(): POCOPINE_NATIVE_DEV_DIR resolution

crates/pocopine-native-tauri/           (tauri OPTIONAL, behind feature "tauri", default OFF)
  src/
    lib.rs            re-exports NativeApp/dev_dir; run!/__run_with_context entry
    shell.rs          #[cfg(feature = "tauri")] — the ONLY file that imports `tauri`
```

- **`pocopine-native::bridge`** is pure `axum`/`tower`/`http`. It
  contains the `dispatch` adapter (§4) and `build_router(dir, configure)`
  which composes `Router::new().fallback_service(static_files(dir))`,
  runs the app's `configure: FnOnce(Server) -> Server` hook (for
  `.with_auth` / `.plugin`), and calls `Server::try_finalize()` to
  install `#[server]` routes and activate the plugin registry. **This
  crate compiles and is unit-tested on any host** (no webview libraries).
- **`pocopine-native-tauri::shell`** is the thin Tauri wiring: register
  the scheme, spawn the dispatch future on `tauri::async_runtime`, build
  the `WebviewWindow` pointed at `pocopine://localhost/`. It consumes
  the core via `pocopine_native::{bridge, NativeApp, dev_dir}` and is
  gated behind `feature = "tauri"`.

### 5.1 Why the `tauri` dep is optional and off by default

The webview backend links system libraries (`webkit2gtk-4.1`,
`libsoup-3.0`, `gtk` on Linux) that are **not** present on a stock CI
runner. If `tauri` were a default dependency, every `cargo build` of the
workspace — and CI — would require those libraries. Making it an opt-in
feature keeps **both** crates safe workspace members (`pocopine-native`
has no webview dep at all; `pocopine-native-tauri`'s default build is the
re-exports + `run!` macro only) and pushes the system-library
requirement to exactly the moment a developer runs `pocopine native` on
a desktop machine. Apps' `src-tauri` crates depend on
`pocopine-native-tauri = { features = ["tauri"] }`.

### 5.2 App-facing API

The app's `src-tauri/src/main.rs` is ~10 lines and never names a
pocopine-internal type beyond `NativeApp`:

```rust
// src-tauri/src/main.rs — host binary, links the app rlib for #[server] inventory
use my_app as _;

fn main() {
    pocopine_native_tauri::run!(
        pocopine_native_tauri::NativeApp::new()
            .title("My App")
            .inner_size(1100.0, 720.0)
        // .configure(|s| s.with_auth(LocalKeyringProvider))  // optional
    );
}
```

`run!` is a macro (not a fn) because Tauri's `generate_context!()` must
expand in the app's own crate to read its `tauri.conf.json`. It expands
to `pocopine_native_tauri::__run_with_context(::tauri::generate_context!(), app)`.
`__run_with_context` is `#[cfg(feature = "tauri")]`.

## 6. CLI: `pocopine native dev` / `pocopine native build`

Both reuse the existing wasm + CSS pipeline (`build::wasm`,
`client_modules::build`, `tailwind`, `stylekit`) verbatim, then drive
the `src-tauri` host crate.

- **`pocopine native dev`** — build the wasm bundle (debug) + CSS, then
  `cargo run` the `src-tauri` bin with `--features tauri` and
  `POCOPINE_NATIVE_DEV_DIR=<project>` in the environment. The shell
  resolves that env var as the static root, so the window serves the
  live on-disk `pkg/` + `index.html` and a rebuild is picked up on
  reload — no asset copying in the dev loop.
- **`pocopine native build`** — build the wasm bundle (release) + CSS,
  then bundle. If the Tauri CLI is available it shells `cargo tauri
  build` (icons, installers, signing); otherwise it falls back to
  `cargo build --release --features tauri` and prints how to produce
  installers. Bundled apps resolve the static root from the Tauri
  resource directory (`pkg/` copied via `tauri.conf.json`
  `bundle.resources`), so `POCOPINE_NATIVE_DEV_DIR` is unset in
  production.

`pocopine native` (no sub-verb, or against a project without
`src-tauri/`) scaffolds the `src-tauri/` directory from the same string
templates the example ships, then prints next steps.

### 6.1 Config — `[package.metadata.pocopine.native]`

Mirrors the existing `tailwind` / `stylekit` / `assets` blocks. All
fields optional:

```toml
[package.metadata.pocopine.native]
# Directory holding the Tauri host crate (default: "src-tauri").
src-tauri = "src-tauri"
# Host bin to run/build (default: the src-tauri crate's bin).
bin = "app-native"
# Extra cargo features to enable on the host bin (default: none — the
# scaffolded src-tauri crate already enables pocopine-native-tauri/tauri).
features = []
# Window title (default: the crate name).
title = "My App"
```

## 7. Interaction with RFC-099 (SSR) — future, not a dependency

In a Tauri app the "server" and the webview host are the **same native
process**. Once RFC-099 phases 3–4 land host-side plan-stamping and
two-tier templates, the scheme handler for `/` can render the first
paint **in-process** (no network, no spinner) and hand fully-formed HTML
to the webview, which then hydrates the wasm — native SSR with zero
round-trips. This is a strong future payoff but **not** a dependency:
RFC-099 phase 1 is only the number formatter + expr host backend, so the
native target ships **client-rendered (identical to the browser)** today
and inherits SSR for free when it lands. The shared seam is the same
`static_files`/router the SSR work already targets.

`POCOPINE_ASSET_BASE` (RFC-100 §native/SSR) applies unchanged: a native
app with a public CDN base loads media from the edge; otherwise the
in-process router proxies `/assets/<hash>/…` exactly like the web
service.

## 8. Build & verification constraints

The webview backend cannot be compiled on a runner without the system
webview libraries. This shapes what is verifiable in CI versus on a
developer desktop:

| Surface | Verified by | Needs webview libs |
|---|---|---|
| `bridge.rs` (dispatch, router composition, static fallback) | `cargo test -p pocopine-native` (default features) | no |
| CLI `native` command logic (paths, env, cargo invocation) | `cargo check -p pocopine-cli` | no |
| `shell.rs`, `src-tauri` example, end-to-end window | `pocopine native dev` on a desktop host | **yes** |
| existing web/wasm targets | unchanged batteries | no |

The `src-tauri` host crate is **excluded from the workspace members** so
a stock `cargo build`/CI never attempts to link the webview. The example
documents the host prerequisites (`libwebkit2gtk-4.1-dev` + friends on
Linux; nothing extra on macOS/Windows), and `pocopine doctor` checks them
per-OS — on Linux it probes the GTK/WebKitGTK `.pc` files and prints the
distro-specific install command when any are missing.

**Linux runtime note.** WebKitGTK's DMABUF renderer SIGSEGVs on many
Linux setups with NVIDIA / hybrid GPUs under Wayland (a WebKit/driver
bug, not framework code). The shell defaults
`WEBKIT_DISABLE_DMABUF_RENDERER=1` when the user hasn't set it, so the
native app runs out of the box there; `WEBKIT_DISABLE_DMABUF_RENDERER=0`
re-enables it.

## 9. Implementation plan

1. **`pocopine-native` + `pocopine-native-tauri` crates** — the
   backend-neutral bridge/builder (with the unit tests) and the
   feature-gated Tauri shell. Both are workspace members (safe: neither
   default build links `tauri`).
2. **CLI** — `native` subcommands, `[package.metadata.pocopine.native]`
   config, `src-tauri` scaffolder.
3. **Example** — add `examples/file-browser/src-tauri` (excluded from
   the workspace): the existing Cloud File Explorer app, packaged as a
   desktop binary. The native `main` mirrors the example's server bin —
   installing the same `storage_server_plugin` — so its storage
   `#[server]` functions run in-process. Demonstrates "add a `src-tauri`
   to an existing pocopine app."
4. **Docs** — a `docs/` guide and a note in the 0.2.x roadmap.

## 10. Open questions

- **Bundled asset source:** copy `pkg/` into Tauri resources (current
  plan, zero extra deps) vs. `include_dir!`-embed into the host binary
  (single-file distribution, larger binary). Start with resources; embed
  is a later opt-in.
- **`Server::try_finalize` is `#[doc(hidden)]`.** The native path is a
  legitimate non-listener consumer of the finalized router. Either
  promote a stable `Server::finalize()` for embedded/native/test use, or
  keep depending on the doc-hidden seam within the workspace. Leaning
  toward promoting it.
- **Auto-update & signing:** Tauri's updater is available but deferred to
  a follow-up; v1 produces unsigned local bundles.
- **Mobile:** Tauri v2 mobile reuses the same crate; revisit after
  desktop is stable.
