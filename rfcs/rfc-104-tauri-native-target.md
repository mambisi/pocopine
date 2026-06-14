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
window — served over an ephemeral `127.0.0.1` loopback listener that
drives the existing axum `Router`. No remote backend, no IPC layer, and
**zero changes to `pocopine-core`**.

The whole feature is two new host crates — `pocopine-native` (the
backend-neutral transport core) and `pocopine-native-tauri` (the Tauri
webview backend) — two CLI subcommands (`pocopine native dev` /
`pocopine native build`), and an `src-tauri/` scaffold per app. The web
target is untouched.

```text
 ┌──────────────────────── Tauri process (native, one Tokio runtime) ───────────────────────┐
 │                                                                                            │
 │   ┌─ WebView (WKWebView / WebView2 / WebKitGTK) ─┐      ┌─ Rust backend ─────────────────┐ │
 │   │  document  http://127.0.0.1:<ephemeral>/     │      │  axum Router                   │ │
 │   │  pkg/<app>_bg.<hash>.wasm  (UNCHANGED)       │      │   = Server::new(Router::new()) │ │
 │   │  pocopine-core reactive runtime              │      │     · inventory!(#[server])    │ │
 │   │                                              │      │     · static_files(pkg)        │ │
 │   │  fetch("/_pocopine/save_a1b2")  +  uploads   │      │                                │ │
 │   └───────────────────────┬──────────────────────┘      └───────────────▲────────────────┘ │
 │                           │  real HTTP over loopback (127.0.0.1)          │                  │
 │                           └────────────►  axum::serve(listener, router) ──┘                  │
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

## 4. Transport: an ephemeral `127.0.0.1` loopback listener

The one interesting design decision is how the webview reaches the app's
`#[server]` handlers. Three options were considered:

| Option | Open port? | wasm-side change | Reuses `#[server]` router | Binary uploads |
|---|---|---|---|---|
| A. Custom URI scheme → `oneshot` | No | none | yes | **broken (WebKitGTK)** |
| B. Tauri IPC (`invoke`) + fetch middleware | No | new client crate + shim | needs adapter | awkward |
| **C. Loopback `127.0.0.1:<ephemeral>`** ✅ | yes (loopback) | none | yes, verbatim | works |

This RFC ships **Option C**, after starting on A. The history matters:

**A was the original design** — `register_asynchronous_uri_scheme_protocol`
serves the app from a `pocopine://localhost` custom scheme with no open
port, feeding each request into the router via `Router::oneshot`. It
works for the document, wasm, CSS, and JSON `#[server]` calls. But
**WebKitGTK SIGSEGVs reading a binary `Blob` request body over a custom
scheme** (file uploads — `pocopine-storage`'s resumable `PATCH` chunks).
That is a hard platform bug, not something the framework can work around
on the custom-scheme path; small JSON string bodies survive, binary
bodies crash the webview process.

**C avoids it by being real HTTP.** The shell binds an ephemeral
`127.0.0.1:0` listener and `axum::serve`s the same router the web server
would; the window loads `http://127.0.0.1:<port>/`. Every request — the
document, wasm, CSS, `#[server]` calls, and uploads — is an ordinary HTTP
request the OS webview handles natively. No custom scheme, no
per-request adapter; `axum::serve(listener, router)` is the entire
transport, identical to production serving.

```text
 fetch("/_pocopine/save_a1b2")  +  binary uploads
        │  origin = http://127.0.0.1:<port>  (real HTTP)
        ▼
 ephemeral 127.0.0.1:0 listener  ──►  axum::serve(listener, router)
        │
        └─ standalone: in-process #[server] router  |  server: forward to backend (§6.2)
```

- **vs A (custom scheme):** A's "no open port" is nicer, but it cannot
  carry binary upload bodies on WebKitGTK. C trades the port for working
  uploads. The port is **loopback only** (`127.0.0.1`, never `0.0.0.0`)
  and **ephemeral** (`:0`, a fresh random port per launch). This is the
  same model Tauri's own `tauri-plugin-localhost` uses. (`bridge::dispatch`
  — the custom-scheme `http::Request → oneshot` adapter — remains in
  `pocopine-native` as a tested utility for backends that can use it.)
- **vs B (IPC):** would require a *new wasm-side crate* (a
  `FetchMiddleware` over the JS IPC bridge) plus a host shim re-deriving
  routing from the `#[server]` inventory, and still wouldn't cleanly
  carry the storage client's uploads (it bypasses the fetch middleware).
  C reuses the HTTP semantics the whole stack already speaks (status
  codes, headers, auth middleware, the `pocopine-server` plugin chain)
  with **no** wasm-side code.

### 4.1 Origin/CSRF — natural, not stamped

Because the page now has a real `http://127.0.0.1:<port>` origin, the
webview sends proper `Origin` / `Sec-Fetch-Site` headers on every
request, so server-side origin/CSRF guards written for the web accept
native calls **as designed** — e.g. `pocopine-storage`'s mutation-origin
check sees `Origin` matching the (localhost) `Host` and passes. The
loopback transport needs no header rewriting; it serves the router
verbatim. (Only the "server"-mode proxy in §6.2 stamps
`Sec-Fetch-Site: same-origin`, because it forwards to a *different*
origin on the app's behalf.)

### 4.2 Loopback hardening

The listener binds `127.0.0.1` (loopback only — unreachable off-box) on
an ephemeral port. The residual risks are other local processes and
DNS-rebinding from a browser tab; a follow-up can add a `Host`-header
allowlist (reject requests whose `Host` isn't the bound `127.0.0.1:port`)
and/or a per-launch bearer token to close those. v1 ships the loopback +
ephemeral-port baseline.

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
- **`pocopine-native-tauri::shell`** is the thin Tauri wiring: build the
  router (`bridge::build_router` for standalone, or a forwarding router
  for server mode), `axum::serve` it on an ephemeral `127.0.0.1` listener
  via `tauri::async_runtime`, and open the `WebviewWindow` at
  `http://127.0.0.1:<port>/`. It consumes the core via
  `pocopine_native::{bridge, NativeApp, dev_dir}` and is gated behind
  `feature = "tauri"`.

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
  `cargo run` the `src-tauri` bin with `POCOPINE_NATIVE_DEV_DIR=<project>`
  in the environment. The shell resolves that env var as the static root,
  so the window serves the live on-disk `pkg/` + `index.html` and a
  rebuild is picked up on reload — no asset copying in the dev loop.
- **`pocopine native build`** — build the wasm bundle (release) + CSS,
  then bundle. If the Tauri CLI is available it shells `cargo tauri
  build` (icons, installers, signing); otherwise it falls back to `cargo
  build --release` and prints how to produce installers. Bundled apps
  resolve the static root from the Tauri resource directory (`pkg/`
  copied via `tauri.conf.json` `bundle.resources`), so
  `POCOPINE_NATIVE_DEV_DIR` is unset in production.

`pocopine native init` (and `dev`/`build` against a project without
`src-tauri/`) scaffolds the `src-tauri/` directory from the same string
templates the example ships, then prints next steps.

### 6.1 No config block — convention + flags

The native target has **no `[package.metadata.pocopine.native]` block**.
The host crate is `src-tauri/` by convention with a single binary that
already enables `pocopine-native-tauri/tauri`, so `cargo run`/`cargo
build` need no `--bin` or `--features`. Everything else is a flag —
nothing about where the app talks to is a source fact baked into
`Cargo.toml`.

### 6.2 Backend selection — standalone vs server

Where the app's `#[server]` calls run is a **build-invocation choice**, a
single flag — not a code change (the wasm UI is identical either way):

```sh
pocopine native build                              # standalone — #[server] runs in-process
pocopine native build --backend https://myapp.up.railway.app   # server — desktop client of the deploy
pocopine native dev   --backend http://localhost:3024          # dev against a local `pocopine run`
```

- **standalone** (no `--backend`) → the functions run in-process; nothing
  to deploy.
- **server** (`--backend <url>`) → the CLI passes the URL to the shell via
  `POCOPINE_NATIVE_BACKEND`; the shell forwards the `#[server]`/storage
  routes (`/_pocopine/*`, `/__pocopine/*`) to that URL.

The URL is a build argument, so CI passes it from a secret/env and the
prod URL never lives in the repo. It comes straight from `pocopine deploy
status`:

```text
pocopine deploy                                  # ship the server
pocopine deploy status                           # read the URL
pocopine native build --backend "<that url>"     # desktop client points at it
```

**How "server" works (host-side forward).** The loopback listener serves
the document + wasm + CSS locally, but its `/_pocopine/*` and
`/__pocopine/*` routes forward to the backend with an HTTP client
(`reqwest`). Because the forward is **host-to-host**, not a browser
request:

- there is **no browser CORS** to configure on the server;
- the webview only ever talks to `http://127.0.0.1:<port>` (same-origin);
- auth headers the app already sets flow through unchanged;
- the proxy stamps `Sec-Fetch-Site: same-origin` (it forwards to a
  different origin on the app's behalf) and drops the loopback
  `Host`/`Origin`.

## 7. Interaction with RFC-099 (SSR) — future, not a dependency

In a Tauri app the "server" and the webview host are the **same native
process**. Once RFC-099 phases 3–4 land host-side plan-stamping and
two-tier templates, the loopback `/` route can render the first paint
**in-process** (no network, no spinner) and hand fully-formed HTML to the
webview, which then hydrates the wasm — native SSR with zero
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
