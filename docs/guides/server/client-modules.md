---
title: "Client Modules And Node Packages"
description: "Optional typed .client.ts modules, npm package imports, and dev-watch behavior."
---

# Client Modules And Node Packages

Pocopine apps do not need JavaScript tooling by default. When a project opts
into typed `.client.ts` files, the CLI owns the small JavaScript toolkit path
so authors still use Pocopine commands as the front door.

## Contract

- Put optional managed client modules under `src/` as `*.client.ts`.
- Use one project-root `package.json` and one project-root `node_modules/`.
- Import npm packages normally from client modules:

  ```ts
  import { initializeApp } from "firebase/app";

  const app = initializeApp({ projectId: "my-project" });

  export default {
    appName() {
      return app.name;
    },
  };
  ```

- Do not use `.client.js`, `.client.jsx`, `.client.tsx`,
  React/Vue/Svelte/Solid/Preact islands, or framework-owned hydration.
  Pocopine managed modules are typed TypeScript SDK adapters.

The CLI scans `src/**/*.client.ts`, writes generated toolkit files under
`target/pocopine/client-modules/`, type-checks the managed modules with
TypeScript, bundles them with esbuild, and serves `/pkg/pocopine-client.js`
beside the wasm package.

## Compilation Pipeline

Managed client modules go through three Pocopine-owned steps:

1. `#[pocopine::client_module("...")]` reads the `.client.ts` file during
   Rust compilation. It extracts explicit async return types and subscription
   callback types, then emits the typed Rust facade methods.
2. `pocopine build`, `pocopine run`, and `pocopine dev` generate
   `target/pocopine/client-modules/tsconfig.json` and run `tsc --noEmit`
   before bundling. This catches broken npm imports, mismatched generated
   `./bindings` types, and normal TypeScript errors. Pocopine owns this
   generated config; apps do not need to add npm scripts or a hand-written
   `tsconfig.json` for managed modules.
3. After type-checking passes, the CLI writes `entry.js` in the same generated
   directory and asks esbuild to bundle it into `pkg/pocopine-client.js`.

`tsc` is the type checker. esbuild is only the transpiler/bundler. This split is
intentional because esbuild intentionally does not type-check TypeScript.

## Rust Access

Application code should treat `.client.ts` as a small SDK adapter.
Declare a Rust facade with `#[pocopine::client_module]` so components and
app plugins do not need raw `wasm-bindgen` reflection code or stringly module
lookups. Add Pocopine's vendored `ts-rs` fork as a dev dependency when Rust
DTOs should generate matching TypeScript payload types:

```toml
[dev-dependencies]
pocopine-ts-rs = "0.1"
```

```text
src/firebase/
  mod.rs
  auth.rs
  bindings.rs
  bindings.ts
  Firebase.client.ts
```

```rust
// src/firebase/mod.rs
pub mod auth;
pub mod bindings;

#[pocopine::client_module("Firebase.client.ts")]
pub mod client {
    use super::bindings::FirebaseUser;
}
```

Use an inline module body for imports used by generated method signatures. That
keeps rustfmt from looking for a separate `src/firebase/client.rs` file before
the macro expands.

Keep TypeScript payload types colocated behind a simple local import:

```rust
// src/firebase/bindings.rs
#[derive(serde::Deserialize)]
#[cfg_attr(test, derive(pocopine_ts_rs::TS))]
#[cfg_attr(test, ts(export, export_to = "../src/firebase/bindings.ts"))]
#[serde(rename_all = "camelCase")]
struct FirebaseUser {
    token: String,
    uid: String,
    #[serde(default)]
    photo_url: Option<String>,
}
```

```ts
import type { FirebaseUser } from "./bindings";
```

Run `cargo test -p your-app-crate export_bindings` to refresh the checked-in
TypeScript file. This keeps Rust DTOs as the source of truth while leaving the
`.client.ts` file focused on SDK calls.

The module name comes from the filename. `src/firebase/Firebase.client.ts`
registers as `firebase`, and `src/FirebaseAuth.client.ts` registers as
`firebase-auth`.
Names are normalized to kebab case, so `FirebaseAuth.client.ts` and
`firebase-auth.client.ts` collide; the CLI reports that as a build error.

Use the facade from app-owned services or plugins:

```rust
use pocopine::ScopeId;
use serde::Deserialize;

#[derive(Deserialize)]
struct FirebaseUser {
    token: String,
    uid: String,
}

async fn sign_in() -> Result<Option<FirebaseUser>, firebase::client::Error> {
    firebase::client::required()?.sign_in().await
}

fn subscribe(
    scope: ScopeId,
    handler: impl FnMut(Result<Option<FirebaseUser>, firebase::client::Error>) + 'static,
) -> Result<(), firebase::client::Error> {
    firebase::client::required()?.on_auth_state_changed(scope, handler)
}
```

`#[client_module]` reads explicit `.client.ts` return and callback types for
supported bridge shapes:

- `async signIn(): Promise<FirebaseUser | null>` becomes `sign_in()`.
- `onAuthStateChanged(callback: AuthStateCallback): Unsubscribe` becomes
  `on_auth_state_changed(scope, handler)`.

It also accepts `file = "..."` and `name = "..."` for explicit
cases:

```rust
#[pocopine::client_module(file = "Firebase.client.ts", name = "firebase")]
pub mod client {
    use super::bindings::FirebaseUser;
}
```

The generated facade is intentionally thin today:

```rust
let module = firebase::client::required()?;
let user = module.sign_in().await?;
```

The next codegen layer will broaden this extractor beyond the small bridge
shapes above.

## Commands

```bash
pocopine js init
pocopine js install
pocopine js add firebase
pocopine js add -D some-dev-tool
pocopine build
pocopine dev
```

`pocopine js init` creates or updates `package.json` with the managed esbuild
and TypeScript dependencies. `pocopine js install` installs through the
detected package manager. `pocopine build`, `run`, and `dev` install missing
client-toolkit dependencies before type-checking and bundling when client
modules are present.

## Package Manager Selection

Pocopine prefers pnpm when there is no existing lockfile. Existing projects can
keep their current manager:

| Lockfile | Manager |
|---|---|
| `pnpm-lock.yaml` | pnpm |
| `package-lock.json` | npm |
| `yarn.lock` | yarn |
| `bun.lockb` / `bun.lock` | bun |

If more than one lockfile is present, `pocopine doctor` warns because installs
are no longer deterministic.

Teams that need pinned wrappers or tool paths can add `.pocopine.toml`:

```toml
[tools]
package-manager = { command = "corepack", args = ["pnpm"] }
node = "node"
wasm-pack = "/opt/tools/wasm-pack"
```

The value may be a plain command/path string or `{ command, args }`. Pocopine
uses the configured command directly; it does not require npm scripts.

## Dev Mode

`pocopine dev` watches:

- Rust/template files under `src/` and rebuilds wasm.
- `.client.ts` files and rebuilds wasm plus `/pkg/pocopine-client.js`, because
  Rust facade signatures may have changed.
- `package.json` and supported lockfiles, then reruns install and rebundles the
  client bundle.

This keeps the Rust build and node package flow separate while sharing one CLI
entrypoint.
