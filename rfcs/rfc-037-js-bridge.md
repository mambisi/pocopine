# RFC 037 — JS client module bridge

| Field | Value |
|---|---|
| **Status** | Accepted, Phase 1 shipped |
| **Author** | pocopine team |
| **Created** | 2026-04-21 |
| **Related** | [RFC 001 — Components](./rfc-001-components.md), [RFC 027 — Provide/Inject](./rfc-027-provide-inject.md) |

## 1. Summary

Give authors a way to pull npm packages (Firebase, Stripe,
PostHog, a WebSocket SDK, a canvas library…) into a Pocopine
app without breaking the "`.poco` is templates, `.rs` is
logic" rule. Phase 1 shipped a **client module adapter** model:

- **`.poco`** — template, unchanged.
- **`.rs`** — component state, handlers, stores, and app plugins.
- **`.client.ts`** — optional browser-only module that imports npm
  packages normally and default-exports a typed plain object of
  functions/subscriptions. The Pocopine CLI owns package
  installation, type extraction, generated Rust bindings, and
  esbuild bundling.

The earlier per-component `ctx` factory/island design is deferred.
The shipped model is deliberately smaller: SDK adapters are module
singletons. Rust reaches them through `ClientModule` today and through
generated facades as the typed extractor lands. That is enough for
Firebase, analytics, and other imperative SDKs without introducing a
second reactive ownership surface.

## 2. Motivation

Real apps need things pocopine shouldn't re-implement:

- Firebase Auth / Firestore client.
- Stripe Elements.
- Analytics SDKs (PostHog, Segment).
- Canvas / charting (Chart.js, D3, Three.js).
- Client-side Markdown / Monaco editor.
- WebRTC / WebSocket client libraries.

Authors today have two bad options:

1. **`wasm-bindgen` everything.** Hand-write JS imports + Rust
   bindings per SDK. Punishingly verbose for Firebase-shaped
   SDKs with many entry points.
2. **Smuggle globals.** Load Firebase via `<script>` in the
   page, poke at `window.firebase` from Rust. Breaks teardown,
   typing, SSR-ability, and any serious build pipeline.

The client module gives a disciplined third path: a JS file that
imports from npm normally, exposes plain async functions and
subscriptions, and lets Rust/Pocopine own UI state, teardown, route
guards, and stores.

## 3. File layout

```
src/
  Firebase.client.ts         // typed browser SDK adapter
  firebase_auth.rs           // Rust wrapper around ClientModule
  components/Login.poco      // normal Pocopine UI
```

The CLI scans `src/**/*.client.ts`. No component macro argument is
required. No `.client.ts` files means zero JavaScript bundling cost.
When files exist, the generated bundle registers their default exports
by filename-derived names: `Firebase.client.ts` becomes `firebase`,
and `FirebaseAuth.client.ts` becomes `firebase-auth`.

## 4. Author surface (JS side)

Phase 1 uses a plain module object. Functions are callable from
Rust via `ClientModule::call_async`; subscription functions are
adapted with `ClientModule::subscribe`.

```ts
// Firebase.client.ts
import { initializeApp } from "firebase/app";
import type { User } from "firebase/auth";
import {
  getAuth,
  GoogleAuthProvider,
  signInWithPopup,
  signOut as firebaseSignOut,
  onAuthStateChanged,
} from "firebase/auth";

const app = initializeApp({ projectId: "my-project" });
const auth = getAuth(app);
const provider = new GoogleAuthProvider();

type FirebaseUser = {
  token: string;
  uid: string;
  email: string | null;
  name: string | null;
  photoUrl: string | null;
};

async function userPayload(user: User | null): Promise<FirebaseUser | null> {
  if (!user) {
    return null;
  }
  return {
    token: await user.getIdToken(),
    uid: user.uid,
    email: user.email,
    name: user.displayName,
    photoUrl: user.photoURL,
  };
}

export default {
  async signIn(): Promise<FirebaseUser | null> {
    const credential = await signInWithPopup(auth, provider);
    return userPayload(credential.user);
  },

  async signOut(): Promise<null> {
    await firebaseSignOut(auth);
    return null;
  },

  async initialUser(): Promise<FirebaseUser | null> {
    await auth.authStateReady();
    return userPayload(auth.currentUser);
  },

  onAuthStateChanged(callback: (user: FirebaseUser | null) => void): () => void {
    return onAuthStateChanged(auth, async (user) => {
      callback(await userPayload(user));
    });
  },
};
```

The module should not mutate Pocopine stores or render UI. It returns
plain JSON and unsubscribe functions. Rust owns the application model.
Managed modules are typed TypeScript by design; untyped `.client.js`
is not part of the stable managed-module contract.

## 5. Author surface (Rust side)

```rust
#[handlers]
impl FirebaseAuth {
    pub fn click_sign_in(&mut self) {
        let module = pocopine::ClientModule::required("firebase")?;
        let user = module.call_async::<Option<FirebaseUser>>("signIn").await?;
        // Convert plain JSON into a Pocopine Principal and store state.
    }
}
```

Error cases:
- Client bundle is missing ⇒ `ClientModule::required` returns an error naming the module.
- Function name is missing ⇒ `call_async` returns a module error.
- Promise rejects or return value cannot deserialize ⇒ `call_async` returns a module error.

For reusable app code, wrap `ClientModule` in an app-owned plugin:

```rust
#[derive(Clone, Default)]
pub struct FirebaseAuth;

impl FirebaseAuth {
    pub async fn sign_in(&self) -> Result<Option<FirebaseUser>, pocopine::ClientModuleError> {
        pocopine::ClientModule::required("firebase")?
            .call_async("signIn")
            .await
    }
}
```

Uses `serde-wasm-bindgen` for the return-type round-trip, same
as `#[server]`.

### Generated facades

`pocopine-client-build` lets `build.rs` generate Rust module facades
into `OUT_DIR`, so rust-analyzer and `cargo check` can see the module
names without running the full dev server:

```rust
// build.rs
fn main() {
    pocopine_client_build::generate().unwrap();
}

// src/lib.rs
pub mod client_modules {
    pocopine::include_client_modules!();
}
```

The first generated layer is intentionally thin:

```rust
let firebase = crate::client_modules::firebase::required()?;
let user = firebase.call_async::<Option<FirebaseUser>>("signIn").await?;
```

The next extractor phase will read explicit TypeScript signatures and
generate typed methods (`firebase.sign_in().await?`) on top of that
same facade.

## 6. Deferred scoped-island design

The original sketch below describes a richer per-component factory
with `ctx.state`, lifecycle hooks, refs, and `ctx.watch`. That is not
part of the Phase 1 shipped API. Keep it here as design background for
a possible later RFC; do not treat it as current documentation.

## 7. Lifecycle + state-sync contract

The island sits inside the existing walker mount pass. Its
factory runs synchronously between Rust `on_setup` and the
template clone; its `mounted()` hook fires deferred (microtask)
after children bind, same tick as Rust `on_ready`.

### Mount timeline

```
1. Scope::new(state)                       Rust mints scope
2. context::set_parent(child, parent)      RFC-027 chain wired
3. Rust on_setup()                         provide(...) lands here
4. JS factory(ctx) runs once, sync         ★ new
     · ctx.state, ctx.inject, ctx.refs,
       ctx.el all live (el = host;
       refs populate during step 5)
     · ctx.watch(...) subscriptions
       register BEFORE children bind, so
       they catch mutations from step 5
     · returned object is split:
         exposed = non-lifecycle fn props
         mounted, unmounted = lifecycle
5. Template clone + children walk          pp-bind, pp-on, etc
6. Rust on_mount(ctx) (if declared)
7. JS mounted() via tick::next             ★ new (microtask)
8. trigger_scope(id) sweep                 existing
9. Rust on_ready via tick::next            existing
```

Why the factory slots between 3 and 5:
- Rust `on_setup` is where `provide(...)` lands — a factory in
  step 4 can `ctx.inject(...)` immediately.
- Watches register before children bind, so effects fire for
  any mutation the child walk triggers.
- The DOM isn't fully bound yet, so DOM-touching work belongs
  in `mounted()` (step 7), not in the factory body.

### Teardown timeline

```
1. MutationObserver delivers removal
2. release_subtree descends leaves-first
3. Release tracked effects                 existing
4. JS ctx.onUnmount LIFO stack fires       ★ new
5. JS unmounted() fires                    ★ new
6. Release tracked listeners               existing (PR1)
7. Rust on_unmount() (if declared)
8. Scope::remove evicts refs/slots/ctx/reactive
```

JS teardown runs *before* Rust `on_unmount` so Rust observes
the island's final state writes (e.g. `state.user = ""` on
sign-out). Reversing the order would let Rust tear down the
scope while JS still holds it.

### State sync

`ctx.state` IS the scope's existing `js_sys::Proxy` — no second
reactive system, no mirror, no diff. So:

- **Reads**: `ctx.state.x` → Reflect::get → proxy `get` trap →
  `track(scope_id, "x")` → subscribes the current effect.
  Inside `ctx.watch(() => state.x, cb)` the JS closure runs
  inside an `effect` body, so CURRENT_EFFECT is bound across
  the JS→Rust call boundary (wasm-bindgen reentrance preserves
  thread-locals) — `track` sees it and subscribes.
- **Writes**: `ctx.state.x = v` → proxy `set` trap →
  `ComponentState::set("x", v)` + `trigger(scope_id, "x")` →
  queue subscribers → flush on microtask. Same timing model as
  a `Scope::invoke` mutation — synchronous write, deferred
  rerun.

#### What flows through `state`

The proxy round-trips values through `serde-wasm-bindgen`.
Survives:

- `bool`, `f64`/`i32`/`u32`/etc, `String`
- `Vec<T>`, `Option<T>`, serde-derivable structs
- `HashMap<String, T>` (JS object form)

**Doesn't survive**: DOM `Element` handles, class instances
(Firebase `User`, Stripe `Elements`), Function values,
Map/Set/Date instances.

Rule: keep plain data in Rust state; keep complex JS handles in
the factory's closure scope. Example:

```js
export default (ctx) => {
  // Complex objects stay in closure — never touch ctx.state.
  const auth = getAuth();
  let currentUser = null;

  return {
    mounted() {
      onAuthStateChanged(auth, (u) => {
        currentUser = u;                          // closure
        ctx.state.user_name = u?.displayName ?? "";  // state (String)
        ctx.state.signed_in = !!u;                    // state (bool)
      });
    },
    async sign_out() {
      await auth.signOut();
    },
  };
};
```

### Auto-cleanup

Every effect registered via `ctx.watch` is tracked in the
scope's `JsIsland` side-table. `release_subtree` releases them
the same way Rust-registered effects already auto-release via
`track_effect_on`. The unsubscribe fn `ctx.watch` returns is
for manual mid-life teardown; forgetting to call it doesn't
leak.

Same rule for `ctx.onUnmount(fn)` registrations — they fire on
teardown whether the author tracked them or not.

### Re-mount idempotence

`pp-if="open"` toggling true → false → true rebuilds the
subtree: fresh `Scope::new`, fresh factory invocation, fresh
closure state. Islands must tolerate being instantiated
multiple times in a single session. Firebase / Stripe / etc.
clients end up created once per mount, which is usually what
you want — but authors who need a cross-mount singleton (one
Firebase app per *page*, shared by many islands) hoist the
`initializeApp` call out of the factory into a module-scope
const.

### Edge cases

| Scenario | Resolution |
|---|---|
| Factory throws synchronously | Mount aborts; `console.error`; Rust `js::call` on this scope returns `NoIsland`. The scope still mounts on the Rust side — the island is the opt-in part. |
| `mounted()` is async + throws | Unhandled promise rejection; logged; mount continues. Authors wrap in try/catch as usual. |
| Write to `state.x` during Rust `on_setup` | Runs before the factory exists; can't happen via JS. Rust-side writes in `on_setup` happen pre-step-4, which is fine — factory steps 4 reads `ctx.state` and sees Rust's already-set values. |
| Write to `state.x` during JS `mounted()` | Same as any proxy write — queues subscribers, flushes next microtask. Rust effects see the new value on the next flush. |
| Component unmounts before `mounted()` has fired | `mounted()` is skipped; `unmounted()` still runs. `ctx.onUnmount(fn)` stacks still run. (Teardown catches up whether or not mount completed.) |
| Factory calls `ctx.inject("X")` for a key no ancestor provided | Returns `undefined`. Matches Rust `inject` behaviour. |
| Two islands in the same app register the same RPC name on different components | Fine — names are per-scope, not global. Two `sign_out`s on different components don't collide. |

## 8. Runtime mechanics

```
┌──────────────────────────────────────────────────────────┐
│ Build-time                                               │
│                                                          │
│  FirebaseAuth.client.ts  ─┐                              │
│  PostHog.client.ts        ├─ esbuild bundle ─► app.js    │
│  Other.client.ts          │                              │
│                          (bare-import resolution via     │
│                           npm; tree-shaking; source      │
│                           maps; minification)            │
└──────────────────────────────────────────────────────────┘
```

```
┌──────────────────────────────────────────────────────────┐
│ Bundled app.js (single <script type="module">)           │
│                                                          │
│  const R = window.__pp_client_modules ??= {};            │
│  R["firebase-auth"] = (scope) => { … };                  │
│  R["pine-post-hog"] = (scope) => { … };                  │
└──────────────────────────────────────────────────────────┘
```

```
┌──────────────────────────────────────────────────────────┐
│ Walker mount path                                        │
│                                                          │
│  mount_component(el, name):                              │
│    scope = Scope::new(...)                               │
│    ... template clone + walk + Rust on_mount ...         │
│    if let Some(_) = component_has_client(name):          │
│        ctx = build_ctx(scope.id, proxy)                  │
│        factory = window.__pp_client_modules[name]        │
│        returned = factory(ctx)   // { mounted, ...rpcs } │
│        split_into(scope.id, returned):                   │
│          - exposed: object of function values            │
│          - mounted: optional 0-arg fn                    │
│          - unmounted: optional 0-arg fn                  │
│        if mounted: queue_microtask(mounted)              │
│                                                          │
│  release_subtree(el):                                    │
│    ... existing cleanups ...                             │
│    if has_island(scope.id):                              │
│        run all ctx.onUnmount(fn) stacked fns (LIFO)      │
│        run `unmounted()` if returned                     │
│        drop per-scope JsIsland entry                     │
└──────────────────────────────────────────────────────────┘
```

### `ctx` JS wrapper

Built in Rust, handed to the factory as a plain JS object with
the scope's proxy as `ctx.state`. The proxy is the existing one
`Scope::into_proxy` already builds — authors get the same
reactive semantics directives rely on, no separate wrapper.

```rust
// crates/pocopine-core/src/js_bridge.rs (new)

#[wasm_bindgen]
pub struct Ctx {
    scope_id: ScopeId,
    proxy: JsValue,
    root_el: Element,
}

#[wasm_bindgen]
impl Ctx {
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> JsValue { self.proxy.clone() }

    #[wasm_bindgen(getter)]
    pub fn el(&self) -> Element { self.root_el.clone() }

    /// `ctx.watch(() => state.x, (next, prev) => {...})` — Vue
    /// signature. Also accepts `(fieldName: string, cb)` as a
    /// quick-path; the impl sniffs arg types.
    #[wasm_bindgen]
    pub fn watch(&self, source: JsValue, cb: Function) -> Function {
        /* If source is a string: watch_scope_field.
         * If source is a Function: build a synthetic effect that
         *   invokes source() inside a `track` context and fires
         *   cb on distinct values. Returns an unsubscribe fn. */
    }

    #[wasm_bindgen(js_name = onUnmount)]
    pub fn on_unmount(&self, cb: Function) { /* push to LIFO stack */ }

    #[wasm_bindgen]
    pub fn refs(&self, name: &str) -> Option<Element> {
        /* refs::get_on(scope_id, name) */
    }

    #[wasm_bindgen]
    pub fn emit(&self, name: &str, detail: JsValue) {
        /* emit::emit_from(root_el, name, detail) */
    }

    #[wasm_bindgen]
    pub fn inject(&self, name: &str) -> JsValue {
        /* string-keyed inject — resolves the InjectKey by debug name
         * via a name → id table populated at provide time. */
    }
}
```

The factory's returned object is then split into
`(exposed_map, lifecycle)` by a small JS-side splitter
registered in the runtime bootstrap — no `wasm-bindgen` plumbing
per component.

### Per-scope JS state

One `thread_local! HashMap<ScopeId, JsIsland>` side-table:

```rust
struct JsIsland {
    exposed: JsValue,             // Object from scope.expose(...)
    onmount: Vec<Function>,       // stack
    onunmount: Vec<Function>,     // stack
}
```

Cleared by `walker::release_subtree` alongside refs/slots/effects.

## 9. CLI integration

### 8.1 Package management

Authors keep `Cargo.toml` + a root `package.json` only when the
project has client modules. Pocopine owns the front door:
authors use `pocopine js ...`, `pocopine build`, and
`pocopine dev`, not ad-hoc esbuild/Vite/npm scripts. Under the
hood, the CLI consumes `target/` from cargo and `node_modules/`
from the detected JS package manager.

#### Canonical JS package manager: **pnpm**

`pocopine new` scaffolds every project with pnpm — no flag, no
prompt. Docs, starters, examples all speak pnpm. Reasoning (and
why it specifically fits a Rust-adjacent audience):

- **Content-addressed global store** (`~/.local/share/pnpm`)
  mirrors cargo's `~/.cargo/registry/` — one copy per dep-version
  machine-wide, symlinked into each project's `node_modules/`.
  Same install-once-link-everywhere model Rust users already
  live in.
- **No phantom dependencies** — you can only import what's in
  your `package.json`. Identical "explicit deps or compile
  error" discipline to cargo.
- **YAML lockfile** (`pnpm-lock.yaml`) diffs readably, reviews
  like `Cargo.lock`. Bun's binary lockfile doesn't.
- **Workspaces** (`pnpm-workspace.yaml`) coexist cleanly with
  Cargo workspaces — each tool ignores the other's root file.
- **wasm-pack / wasm-bindgen** emit to `node_modules/` fine under
  pnpm's symlinked layout; every modern bundler (esbuild, Vite,
  Rollup, Turbopack) explicitly supports it.
- Avoid: yarn berry PnP (virtual-path rewriting has caused real
  breakage with `wasm-pack`-generated packages).

Teams that inherit a different lockfile are not blocked, but the
normal command remains `pocopine js ...`. The package manager is
an implementation detail the CLI invokes.

Teams that do not want Pocopine to infer global tools can add a
repo-local `.pocopine.toml`:

```toml
[tools]
cargo = { command = "cargo", args = ["+stable"] }
rustc = { command = "rustc", args = ["+stable"] }
wasm-pack = "/opt/tools/wasm-pack"
package-manager = "pnpm"
node = "node"
tailwindcss = "tailwindcss"
```

The value may be a plain binary/path string or `{ command, args }`.
The CLI uses these commands directly instead of going through npm
scripts or shell aliases. This keeps Pocopine as the front door while
still letting teams pin wrappers, toolchains, or package-manager
choices.

#### Managed JS subcommands

```
pocopine js init                    # create/update package.json toolkit
pocopine js install                 # install through detected manager
pocopine js add firebase            # add runtime dependency
pocopine js add -D some-dev-tool    # add dev dependency
```

`pocopine js add` auto-detects via lockfile (`pnpm-lock.yaml` →
pnpm, `yarn.lock` → yarn, `bun.lockb` → bun, default → pnpm).
The CLI also ensures the project has the small client toolkit
dependency it owns (`esbuild`) before bundling.

On `pocopine build`, `pocopine run`, and `pocopine dev`, if
client modules exist and `node_modules/` is absent, the CLI runs
the detected install command once before invoking the bundler.
`cargo build` / `wasm-pack build` stays on the existing path.

### 8.2 Bundling

The CLI grows a tiny "client bundler" step:

1. Scan `src/**/*.client.ts`. Reject `.client.js`, `.client.jsx`,
   and `.client.tsx`; Pocopine managed modules are typed SDK adapters,
   not untyped globals or other UI frameworks.
2. Emit a thin entry file:
   ```js
   import firebaseAuth from "./FirebaseAuth.client.ts";
   import postHog      from "./PostHog.client.ts";
   const R = (window.__pp_client_modules ??= {});
   R["firebase-auth"] = firebaseAuth;
   R["pine-post-hog"] = postHog;
   ```
3. Run esbuild:
   ```
   esbuild _generated/client-entry.js \
     --bundle --format=esm --outfile=pkg/pocopine-client.js
   ```
4. In static-server mode, inject
   `<script type="module" src="/pkg/pocopine-client.js">` into
   served HTML when the bundle exists.

Dev server watches `*.client.ts` and rebundles on save. No
separate `package.json` in each component dir — one root
`package.json` with the SDKs the app uses.

Package-file changes (`package.json`, `pnpm-lock.yaml`,
`package-lock.json`, `yarn.lock`, `bun.lockb`, `bun.lock`) trigger a
client dependency install followed by a client rebundle in dev mode.
Rust/template changes still rebuild wasm; client-module changes do not
force a wasm rebuild.

## 10. Error / edge cases

| Scenario | Behaviour |
|---|---|
| `.client.js` present | Build error. Managed modules must be typed `.client.ts` files. |
| `.client.jsx` / `.client.tsx` present | Build error. Pocopine supports imperative TypeScript SDK interop, not JSX/TSX or alternate UI framework islands. |
| Factory throws | Logged as `console.error`; mount continues; Rust `js::call` returns `NoIsland`. |
| Factory returns non-object | Dev warning; treated as `{}` (no RPCs, no hooks). |
| Factory is async | Supported — the `await` lands in `mounted()` (top-level imports stay synchronous — use dynamic `import()` inside `mounted` for late-loaded SDKs). |
| `state.x = v` on a state-only field | Writes land and trigger; island has full access to its own state, same as Rust handlers do. RFC-031 gates inter-component writes (`pp-bind` / `pp-model`), not intra-component state the component itself owns. |
| Same component mounted twice | Each mount gets a fresh `ScopeWrap` + island invocation. |
| Teleport / `pp-if` re-mount | `onUnmount` → `onMount` sequence fires per mount. Island must be idempotent. |
| SSR hydration | Out of scope — SSR doesn't run the island; first client mount initialises it. |
| Build without JS tooling installed | CLI emits a tool-specific error with the `pocopine js install` path. The normal workflow stays inside Pocopine commands. |

## 11. Implementation plan

Three PRs, independently mergeable:

**PR 1 — CLI toolkit + bundling.**
- `pocopine-cli` — `pocopine js init/install/add`, scan
  typed `.client.ts`, reject JS/TSX/JSX, generate the entry
  file, drive esbuild through the managed package
  manager, and inject `/pkg/pocopine-client.js` in static mode.
- Dev-server reload path mirrors the Rust hot-reload.

**PR 2 — generated facade foundation.**
- `pocopine-client-codegen` — shared module discovery, schema IR,
  generated Rust facade code, and runtime-entry writing.
- `pocopine-client-build` — `build.rs` helper that writes
  `pocopine_client_modules.rs` to `OUT_DIR` and emits
  `cargo:rerun-if-changed` lines for rust-analyzer.
- `pocopine::include_client_modules!()` — app-side include helper.

**PR 3 — TypeScript API extraction.**
- Use the TypeScript compiler API, not a hand-rolled parser.
- Extract only the public `defineClientModule` / default-export facade:
  async JSON-returning methods and callback subscriptions.
- Generate typed Rust methods over the existing `ClientModule` runtime.
- Reject `any`, DOM/class instances, functions except subscription
  callbacks, and imported SDK handle types such as `FirebaseApp`.

## 12. Out of scope

- **TSX/JSX and UI-framework islands.** `.client.ts` is accepted
  as typed JavaScript input to the Pocopine-managed bundler.
  `.client.tsx`, `.client.jsx`, React/Vue/Svelte/Solid/Preact
  mounting, and framework hydration inside Pocopine are out of
  scope.
- **Server-side rendering of islands.** Islands are
  client-only; SSR emits an empty shell, hydration runs the
  factory.
- **Full SDK binding generation.** Only DTOs and methods that cross the
  Pocopine bridge are generated. Firebase/Stripe/etc handles stay
  private in the `.client.ts` closure.

## 13. Open questions

1. **Naming.** `.client.ts` is the stable managed-module suffix.
   `.client.js` was useful during the initial runtime bridge spike,
   but generated Rust bindings need a typed boundary.

2. **`onMount` ordering relative to Rust `on_mount`.** Both fire
   post-walk. Proposal: **Rust `on_mount` first**, then JS
   `onMount`. Rationale: Rust wires `provide` etc. before the
   island reads them. If authors want JS-first, they can
   `setTimeout(fn, 0)` from Rust.

3. **Should `scope.set` go through `is_prop` gate?** Proposal:
   **no**, because the island belongs to the component same as
   its Rust handlers do. Parents still can't reach in —
   nothing about the bridge changes the inter-component
   write-boundary.

4. **String-keyed inject.** RFC-030 went hard on `InjectKey<T>`
   being a typed Symbol-ish. The JS bridge can't carry those
   types, so `scope.inject("ROOT")` needs a string lookup
   table. Propose: when the Rust side `provide`s, it also
   registers `name → key_id` into a devtools-visible table
   (same one RFC-036's inject-chain view uses). Island injects
   by name, gets back a wrapped proxy if the stored value is a
   `Handle<T>`. Typed-in-Rust, stringly-typed-in-JS — the
   trade is inherent, document it clearly.

## 14. Alternatives considered

- **Svelte-style `<script>` in `.poco`.** Rejected: breaks the
  templates-only invariant; forces a `.poco` compiler; blurs
  where logic lives. Authors would eventually put control-flow
  JS in templates and the boundary collapses.
- **Rust-only via `wasm-bindgen`.** Possible but verbose per
  SDK. Fine for a handful of cross-cutting needs (the router,
  `fetch`), punishing for Firebase-shaped libraries with
  hundreds of entry points.
- **Web Components as the boundary.** Author ships a Web
  Component (`<firebase-auth>`), uses pocopine's `pp-bind` to
  pass props. Works today with no runtime change, but the
  author writes the whole integration including lifecycle by
  hand, and the reactive bridge is lost — Firebase writes
  don't trigger pocopine effects unless the WC emits a custom
  event and the host listens. The proposed bridge does that
  plumbing once.
