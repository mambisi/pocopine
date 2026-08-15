---
name: run-with-pocopine-cli
description: >-
  Use whenever you are about to run, serve, preview, or build a pocopine
  application or example (anything with a wasm client + a server bin, or a
  `[package.metadata.pocopine]` block) — including writing the run instructions
  in a README. ALWAYS drive it through the pocopine CLI (`pocopine run` /
  `pocopine dev` / `pocopine build`), never a hand-rolled `wasm-pack build`
  followed by `cargo run --bin`. Covers the subcommands + flags, what the CLI
  does that manual steps silently miss, and the contract a runnable crate must
  satisfy.
---

# Run pocopine apps with the pocopine CLI

A pocopine app is a wasm client **plus** a host server bin. Building and serving
it correctly means: build the wasm with wasm-pack, content-hash the bundle,
generate the hashed entry HTML, compile the server bin(s), wire their ports and
env, serve js/wasm with the right MIME, and run the CSS stage. The CLI does all
of that. A hand-rolled `wasm-pack build … && cargo run --bin server` does not —
so **always use the CLI.**

## The rule

To run / serve / preview / build a pocopine app or example, use:

| Command | What it does |
|---|---|
| `pocopine run --path <dir>` | One-shot: build wasm (wasm-pack) → hash the bundle → generate `pkg/index.html` → build the configured server bin(s) → spawn them with `PORT` set → serve. |
| `pocopine dev --path <dir>` | Same as `run`, plus watch the sources and rebuild wasm / server bin / CSS on change. Use while editing. |
| `pocopine build --path <dir>` | Production build only (no serve). Add `--release` for an optimized, content-hashed bundle. |

`--path` defaults to the current dir, so `cd <dir> && pocopine run` works too.
Other flags: `--port <n>` (override the bin's `PORT`), `--release`,
`--no-stylekit` (the Pine Stylekit CSS stage runs by default, RFC 092).

```sh
# Run an example:
pocopine run --path examples/collab-canvas      # → http://127.0.0.1:<metadata port>
# Iterate on it:
pocopine dev --path examples/collab-canvas
```

## Why the CLI, not manual steps

`wasm-pack build && cargo run --bin server` silently skips:

- **Bundle content-hashing.** The CLI hashes the js+wasm pair together
  (`<name>.<hash>.js` / `<name>_bg.<hash>.wasm`) and writes a hash-rewritten
  `pkg/index.html`. Without it, a CDN can pair stale cached glue with fresh wasm
  → `WebAssembly.instantiate` fails with `LinkError`.
- **Correct MIME.** It serves `.js` as `text/javascript` and `.wasm` as
  `application/wasm` (a plain static server often mis-serves wasm → instantiate
  fails) and applies the cache-control policy.
- **`PORT` wiring.** It launches the server bin with `PORT=<metadata or --port>`;
  the bin reads `PORT`. A hardcoded `cargo run` ignores `--port`.
- **Multi-process orchestration.** It builds and spawns BOTH the web bin and a
  worker bin when configured, sharing env (RFC 080 deploy contract).
- **The CSS stage** (Pine Stylekit) and, in `dev`, **file-watch rebuilds**.

## The contract a runnable crate must satisfy

For the CLI to drive a crate, it needs (see `examples/collab-canvas`,
`examples/live`):

1. **Manifest metadata + targets:**
   ```toml
   [package.metadata.pocopine]
   bin = "server"     # the web server bin name
   port = 3030        # its default port (overridable with --port)

   [lib]
   crate-type = ["cdylib", "rlib"]   # the wasm client

   [[bin]]
   name = "server"
   path = "src/bin/server.rs"
   ```
2. **A `build.rs`** aliasing the platform cfgs to the target arch:
   ```rust
   fn main() {
       println!("cargo:rustc-check-cfg=cfg(pocopine_browser)");
       println!("cargo:rustc-check-cfg=cfg(pocopine_host)");
       match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
           Ok("wasm32") => println!("cargo:rustc-cfg=pocopine_browser"),
           _ => println!("cargo:rustc-cfg=pocopine_host"),
       }
   }
   ```
3. **A server bin** gated on those cfgs, reading `PORT`, serving via
   `pocopine-server`:
   ```rust
   #[cfg(pocopine_host)]
   #[tokio::main]
   async fn main() -> std::io::Result<()> {
       use pocopine_server::{axum::Router, serve, static_files};
       let router = Router::new()
           // .merge(your routes — e.g. pocopine_realtime::routes(gateway))
           .fallback_service(static_files(env!("CARGO_MANIFEST_DIR")));
       let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3030);
       serve(router, &format!("127.0.0.1:{port}")).await
   }
   #[cfg(pocopine_browser)]
   fn main() {}
   ```
4. **`index.html` is the bundle-loading page.** `pocopine build` content-hashes
   the bundle and rewrites the `/pkg/<name>.js` reference **in `index.html`**
   into the generated `pkg/index.html`. So the page that does
   `import init from "/pkg/<name>.js"` must be `index.html`, not a secondary
   page. A side-by-side/multi-session view is a separate static file that
   `<iframe src="/">`s the app (see `collab-canvas/dual.html`).

## Anti-pattern

```sh
# DON'T document or run a pocopine app like this:
wasm-pack build examples/foo --target web --out-dir pkg
cargo run -p foo --bin server
# Misses hashing, the generated entry HTML, PORT wiring, the worker bin, CSS,
# and correct wasm MIME. Use `pocopine run` / `pocopine dev` instead.
```

## Reference

- **`examples/collab-canvas`** — a wasm client (cdylib) + a `server` bin that
  mounts the realtime gateway and serves static files; runs with
  `pocopine run --path examples/collab-canvas`. The canonical example of this skill.
- **`examples/live`** — the same shape with `pocopine-server` + `pocopine::live`.
