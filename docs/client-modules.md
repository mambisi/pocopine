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

The CLI scans `src/**/*.client.ts`, writes a generated entry file under
`target/pocopine/client-modules/`, bundles it with esbuild, and serves
`/pkg/pocopine-client.js` beside the wasm package.

## Rust Access

Application code should treat `.client.ts` as a small SDK adapter.
Declare a Rust facade with `#[pocopine::client_module]` so components and
app plugins do not need raw `wasm-bindgen` reflection code or stringly module
lookups:

```rust
#[pocopine::client_module("Firebase.client.ts")]
pub mod firebase {}
```

Use an empty inline module body. That keeps rustfmt from looking for a
separate `src/firebase.rs` file before the macro expands.

The module name comes from the filename. `src/Firebase.client.ts` registers as
`firebase`, and `src/FirebaseAuth.client.ts` registers as `firebase-auth`.
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

async fn sign_in() -> Result<Option<FirebaseUser>, firebase::Error> {
    firebase::required()?.call_async("signIn").await
}

fn subscribe(
    scope: ScopeId,
    handler: impl FnMut(Result<Option<FirebaseUser>, firebase::Error>) + 'static,
) -> Result<(), firebase::Error> {
    firebase::required()?
        .subscribe(scope, "onAuthStateChanged", handler)
}
```

`#[client_module]` also accepts `file = "..."` and `name = "..."` for explicit
cases:

```rust
#[pocopine::client_module(file = "Firebase.client.ts", name = "firebase")]
pub mod firebase {}
```

The generated facade is intentionally thin today:

```rust
let user = firebase::required()?
    .call_async::<Option<FirebaseUser>>("signIn")
    .await?;
```

The next codegen layer will extract the explicit TypeScript API from the
`.client.ts` default export and generate typed method wrappers on top of the
same facade, for example `firebase::required()?.sign_in().await?`.

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
client-toolkit dependencies before bundling when client modules are present.

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
- `.client.ts` files and rebundles only `/pkg/pocopine-client.js`.
- `package.json` and supported lockfiles, then reruns install and rebundles the
  client bundle.

This keeps the Rust build and node package flow separate while sharing one CLI
entrypoint.
