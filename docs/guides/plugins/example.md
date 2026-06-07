---
title: "Example: a Firebase auth extension"
description: "A complete worked extension — npm SDK → client module → app plugin → consumed from a component's lifecycle."
---

# Example: a Firebase auth extension

This walks one real extension end to end: add Google sign-in via the Firebase
JS SDK, wrap it as an app-wide service, and consume it from a component. It
touches all three browser-side surfaces in order — [client module](../server/client-modules.md)
→ [app plugin](./app-plugins.md) → [consumption](./consuming.md). The code is from
`examples/keep`.

```text
Firebase JS SDK ─▶ Firebase.client.ts ─▶ #[client_module] facade
                                                │ wrapped by
                                                ▼
                                  KeepFirebaseAuth  (AppPlugin service)
                                                │ extracted in on_ready / a handler
                                                ▼
                                  AuthGate · Login components
```

## Step 1 — the client module

The `.client.ts` file imports the npm SDK and exports the methods Rust will
call. Return and callback types are explicit so the macro can generate a typed
facade.

```ts
// src/firebase/Firebase.client.ts (trimmed)
import { getAuth, signInWithPopup, onAuthStateChanged, /* … */ } from "firebase/auth";

export default {
  async signIn(): Promise<FirebaseAuthUser | null> {
    const credential = await signInWithPopup(auth, provider);
    return userPayload(credential.user);
  },
  async initialUser(): Promise<FirebaseAuthUser | null> {
    await auth.authStateReady();
    return userPayload(auth.currentUser);
  },
  onAuthStateChanged(callback: AuthStateCallback): Unsubscribe {
    return onAuthStateChanged(auth, async (user) => callback(await userPayload(user)));
  },
};
```

Declare the Rust facade with `#[client_module]`. The inline `pub mod` body holds
the imports its generated signatures need; payload types are Rust DTOs shared via
the vendored `ts-rs` fork.

```rust
// src/firebase/mod.rs
#[pocopine::client_module("Firebase.client.ts")]
pub mod client {
    use super::bindings::FirebaseAuthUser;
}
```

`async signIn(): Promise<FirebaseAuthUser | null>` becomes `client::…sign_in()`;
`onAuthStateChanged(cb): Unsubscribe` becomes `on_auth_state_changed(scope, handler)`.
(Full mechanics: [Client modules](../server/client-modules.md).)

## Step 2 — wrap it in an app plugin

The client module is a raw SDK adapter. Wrap it in a **service** so components
don't poke at the facade directly, and install that service with an `AppPlugin`.

```rust
// src/firebase/auth.rs
#[derive(Clone, Default)]
pub struct KeepFirebaseAuth;

impl KeepFirebaseAuth {
    pub async fn sign_in(&self) -> Result<Option<FirebaseAuthUser>, String> {
        module()?.sign_in().await.map_err(|e| e.to_string())
    }
    pub async fn initial_user(&self) -> Result<Option<FirebaseAuthUser>, String> {
        module()?.initial_user().await.map_err(|e| e.to_string())
    }
    pub fn subscribe(
        &self,
        scope: ScopeId,
        mut handler: impl FnMut(Result<Option<FirebaseAuthUser>, String>) + 'static,
    ) -> Result<(), String> {
        module()?
            .on_auth_state_changed(scope, move |r| handler(r.map_err(|e| e.to_string())))
            .map_err(|e| e.to_string())
    }
}

// The installer: provide the service onto the app.
struct KeepFirebaseAuthPlugin;

impl AppPlugin for KeepFirebaseAuthPlugin {
    fn name(&self) -> &'static str { "keep-firebase-auth" }
    fn install(self, app: App) -> App {
        app.provide_plugin(KeepFirebaseAuth)
    }
}

// Reusable crates expose a typed constructor.
pub fn keep_firebase_auth_plugin() -> impl AppPlugin {
    KeepFirebaseAuthPlugin
}
```

## Step 3 — install it

Add the plugin at the entrypoint. The service is now live for the app's lifetime.

```rust
pocopine::app! {
    components: [AppShell, AuthGate, Login, /* … */],
    plugins: [keep_firebase_auth_plugin()],
    routes: [("/", AuthGate)],
}
```

## Step 4 — consume it from a component

A component reaches the service from its lifecycle. The auth gate resolves it in
`on_ready`, kicks off the initial check, and subscribes to auth-state changes —
all on a scope-bound task so it's cancelled at unmount.

```rust
// src/components/auth_gate/mod.rs
pub fn on_ready(&self, handle: Handle<Self>) {
    let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
        mark_auth_unavailable(handle, "Firebase auth extension is not installed".into());
        return;
    };
    let firebase = firebase.get().clone();
    let scope = handle.scope_id();
    pocopine::spawn_for_scope(scope, async move {
        let initial = firebase.initial_user().await;
        if initial.is_ok() {
            let h = handle.clone();
            let _ = firebase.subscribe(scope, move |r| {
                update_gate_from_auth_result_deferred(h.clone(), r);
            });
        }
        update_gate_error(handle, prepare_auth_result(initial).0);
    });
}
```

A handler drives the interactive sign-in the same way — optional lookup, clone
the handle out, run the async call on the scope:

```rust
// src/components/login/mod.rs
pub fn sign_in(&mut self) {
    let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
        self.error = "Firebase auth extension is not installed".into();
        return;
    };
    let firebase = firebase.get().clone();
    let handle = pocopine::this::<Self>();
    self.loading = true;
    pocopine::spawn_for_scope(handle.scope_id(), async move {
        let result = firebase.sign_in().await;
        handle.update(|login| login.apply_action_result(result));
    });
}
```

Both use the **optional** form (`self.plugins().get`) so the components stay
portable; an app that doesn't install Firebase still compiles and renders a clear
"not installed" state instead of panicking. See
[Consuming plugins](./consuming.md) for the required vs optional trade-off.

## The shape

```text
Firebase.client.ts   ──▶  #[client_module] facade   (typed Rust over the SDK)
                                  │
KeepFirebaseAuth     ──▶  wraps the facade as a service
                                  │  app.provide_plugin(KeepFirebaseAuth)
keep_firebase_auth_plugin()  ──▶  installs it in app! { plugins: [...] }
                                  │
AuthGate / Login     ──▶  self.plugins().get::<KeepFirebaseAuth>()  in on_ready / handlers
```

That's the whole pattern: a JS SDK becomes a typed facade, the facade becomes an
app-wide service, and components extract the service from their lifecycle. The
host side mirrors this with [server plugins](../server/server-plugins.md) — same
`provide_plugin` / `hook_plugin` shape on the `Server` builder.
