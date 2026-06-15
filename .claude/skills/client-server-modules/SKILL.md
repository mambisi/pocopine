---
name: client-server-modules
description: >-
  Use when creating or restructuring a pocopine library crate that has
  host-only and/or wasm-only code (anything depending on axum/tokio for the
  server, or wasm-bindgen/web-sys for the browser). Put platform code in
  `server` and/or `client` modules gated ONCE at the crate root, instead of
  sprinkling `#[cfg(target_arch = "wasm32")]` on every item and every `pub
  use`. Covers the lib.rs gating pattern, when to use a `server.rs` file vs a
  `server/` folder, sibling-import paths, and the Cargo.toml target tables.
---

# Client / server module split

In pocopine, most library crates have code that only compiles on one target:
the **server/host** side (axum, tokio, `pocopine_auth::RequestContext`, DB
drivers) and the **client/browser** side (`wasm-bindgen`, `web-sys`,
`pocopine-core`). Do **not** gate each item and each `pub use` with its own
`#[cfg(...)]`. Gate **once**, at the module boundary.

## The rule

1. Host-only code lives under a `server` module. Browser-only code lives under
   a `client` module. Cross-target code (pure data types, codecs) can stay at
   the crate root or in plain modules.
2. The crate root applies the platform `#[cfg(...)]` to the **module
   declaration and its re-export** — and nowhere else.
3. Inside `server`/`client`, **no `cfg` attributes**. Submodules reference
   siblings with `super::` (or `crate::server::…` from nested test modules).
4. One file (`server.rs`) if small; a folder (`server/mod.rs` + submodules) if
   it will hold lots of code.

## Canonical `lib.rs`

Host-only crate (e.g. a server gateway). On wasm32 the crate is empty:

```rust
//! Crate docs …

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

// Re-export the API at the crate root so `mycrate::Thing` and
// `mycrate::server::Thing` both resolve (mirrors pocopine-live).
#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
```

Crate with both sides:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
#[cfg(not(target_arch = "wasm32"))]
pub use server::*;

#[cfg(target_arch = "wasm32")]
pub mod client;
#[cfg(target_arch = "wasm32")]
pub use client::*;

// Anything that compiles everywhere stays in a normal module:
pub mod protocol; // shared wire types, codecs, errors
```

That is the **only** place `#[cfg(target_arch = …)]` appears. Two lines per
side, not one per export.

## Folder layout (lots of code → `server/`)

```
src/
  lib.rs            # crate docs + the cfg-gated `pub mod server;` + re-export
  server/
    mod.rs          # `mod a; mod b; …  pub use a::*; pub use b::*;`  (NO cfg)
    frame.rs
    gateway.rs
    route.rs
```

`server/mod.rs` aggregates and re-exports — it carries no `cfg`:

```rust
//! Host-side implementation. One cfg gate at the crate root (see the
//! client-server-modules skill); submodules below carry no cfg.
mod frame;
mod gateway;
mod route;

pub use frame::Frame;
pub use gateway::Gateway;
pub use route::{routes, STREAM_PATH};
```

Small crate → a single `src/server.rs` file instead of the folder. Same
gating in `lib.rs`.

## Sibling imports inside the module

From a submodule, reach a sibling via `super::`:

```rust
// in server/frame.rs
use super::error::WsError;
use super::varint::read_var_uint;
```

From a **nested** module (e.g. a `#[cfg(test)] mod tests`), the absolute path
is clearest:

```rust
// in server/control.rs  ->  mod tests
use crate::server::frame::FrameKind;
```

Intra-doc links that point at re-exported items keep using `crate::Thing`
(they resolve through the root re-export). Links to a private submodule path
use the full `crate::server::frame` path.

## Cargo.toml: target dependency tables

Put host deps under the host target table and wasm deps under the wasm table,
so a default `cargo build` / wasm clippy does not try to compile axum/tokio
for wasm:

```toml
[dependencies]                       # cross-target only
serde = { workspace = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
axum = { version = "0.7", features = ["ws"] }   # ws is OFF by default — enable it
tokio = { version = "1", features = ["rt", "macros", "sync", "time"] }
pocopine-auth = { workspace = true }

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen = { workspace = true }
web-sys = { version = "0.3", features = ["…"] }
```

If a crate is purely host-only, move **all** deps (serde included) under the
`cfg(not(target_arch = "wasm32"))` table so the wasm build truly has nothing.

## Why

- One gate, not N. Adding a new `pub fn`/`pub struct` doesn't mean remembering
  yet another `#[cfg(...)]` — the module boundary already covers it.
- `cargo build` (default target is wasm32 in this workspace) and the CI wasm
  clippy gate stay green, because host-only deps never reach the wasm target.
- The public API reads cleanly: `mycrate::routes` on the server,
  `mycrate::Client` in the browser, both backed by one cfg line.

## Reference implementations

- **`pocopine-ws`** — host-only gateway; everything under `src/server/`, one
  `pub mod server;` gate. The canonical example of this skill.
- **`pocopine-live`** — uses the older `mod host` / `mod browser` names with
  root re-exports (`pub use host::{LiveHub, routes};`). Same idea; new crates
  use `server` / `client` naming.

## Anti-patterns

```rust
// DON'T: a cfg on every item and every re-export
#[cfg(not(target_arch = "wasm32"))] mod frame;
#[cfg(not(target_arch = "wasm32"))] mod gateway;
#[cfg(not(target_arch = "wasm32"))] mod route;
#[cfg(not(target_arch = "wasm32"))] pub use frame::Frame;
#[cfg(not(target_arch = "wasm32"))] pub use gateway::Gateway;
#[cfg(not(target_arch = "wasm32"))] pub use route::routes;
// … this is what the `server` module exists to collapse into two lines.
```
