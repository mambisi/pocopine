# Client Modules And Node Packages

Pocopine apps do not need JavaScript tooling by default. When a project opts
into `.client.js` or `.client.ts` files, the CLI owns the small JavaScript
toolkit path so authors still use Pocopine commands as the front door.

## Contract

- Put optional client modules under `src/` as `*.client.js` or
  `*.client.ts`.
- Use one project-root `package.json` and one project-root `node_modules/`.
- Import npm packages normally from client modules:

  ```js
  import { initializeApp } from "firebase/app";

  const app = initializeApp({ projectId: "my-project" });

  export default {
    appName() {
      return app.name;
    },
  };
  ```

- Do not use `.client.jsx`, `.client.tsx`, React/Vue/Svelte/Solid/Preact
  islands, or framework-owned hydration. Pocopine accepts imperative JS/TS SDK
  integration only.

The CLI scans `src/**/*.client.js` and `src/**/*.client.ts`, writes a generated
entry file under `target/pocopine/client-modules/`, bundles it with esbuild, and
serves `/pkg/pocopine-client.js` beside the wasm package.

## Rust Access

Application code should treat `.client.js` / `.client.ts` as a small SDK adapter.
Pocopine exposes the bundled default export through `ClientModule` so Rust
components and app plugins do not need to write raw `wasm-bindgen` reflection
code:

```rust
use pocopine::{ClientModule, ScopeId};
use serde::Deserialize;

#[derive(Deserialize)]
struct FirebaseUser {
    token: String,
    uid: String,
}

async fn sign_in() -> Result<Option<FirebaseUser>, pocopine::ClientModuleError> {
    ClientModule::required("firebase")?
        .call_async("signIn")
        .await
}

fn subscribe(
    scope: ScopeId,
    handler: impl FnMut(Result<Option<FirebaseUser>, pocopine::ClientModuleError>) + 'static,
) -> Result<(), pocopine::ClientModuleError> {
    ClientModule::required("firebase")?
        .subscribe(scope, "onAuthStateChanged", handler)
}
```

The module name comes from the filename. `src/Firebase.client.js` registers as
`firebase`, and `src/FirebaseAuth.client.ts` registers as `firebase-auth`.
Names are normalized to kebab case, so `FirebaseAuth.client.ts` and
`firebase-auth.client.ts` collide; the CLI reports that as a build error.

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
dependency. `pocopine js install` installs through the detected package
manager. `pocopine build`, `run`, and `dev` install missing client-toolkit
dependencies before bundling when client modules are present.

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
- `.client.js` / `.client.ts` files and rebundles only
  `/pkg/pocopine-client.js`.
- `package.json` and supported lockfiles, then reruns install and rebundles the
  client bundle.

This keeps the Rust build and node package flow separate while sharing one CLI
entrypoint.
