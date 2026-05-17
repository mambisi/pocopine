# Integration Firebase

This guide shows the app-level Firebase Auth pattern Pocopine wants:
the Firebase web SDK stays in a tiny `.client.js` adapter, while
Pocopine components, stores, route guards, and `pocopine-auth-client`
own the application state.

The Keep example is the reference implementation:

- [`examples/keep/src/Firebase.client.js`](../examples/keep/src/Firebase.client.js)
- [`examples/keep/src/firebase_auth.rs`](../examples/keep/src/firebase_auth.rs)
- [`examples/keep/src/components/login/`](../examples/keep/src/components/login/)
- [`examples/keep/src/components/auth_gate/`](../examples/keep/src/components/auth_gate/)

## Shape

```
Firebase web SDK
  -> src/Firebase.client.js
  -> ClientModule::required("firebase")
  -> app-owned auth extension
  -> pocopine-auth-client::AuthSession
  -> store fields and Pocopine components
```

JavaScript should stay at the SDK boundary. It initializes Firebase,
opens the popup, returns plain JSON, and subscribes to provider state.
Rust owns everything else.

## Step 1 - add the Firebase npm package

From a Pocopine app directory:

```bash
pocopine js init
pocopine js add firebase
```

From the repository root for the Keep example:

```bash
cargo run -p pocopine-cli -- js --path examples/keep init
cargo run -p pocopine-cli -- js --path examples/keep add firebase
```

The CLI owns the JavaScript toolkit path. Use `pocopine js ...`,
`pocopine build`, and `pocopine dev` instead of adding separate npm
scripts for Pocopine-managed client modules.

## Step 2 - create `src/Firebase.client.js`

The filename matters. `src/Firebase.client.js` registers as the
client module named `firebase`, so Rust can call
`ClientModule::required("firebase")`.

```js
import { initializeApp } from "firebase/app";
import {
  getAuth,
  GoogleAuthProvider,
  onAuthStateChanged,
  signInWithPopup,
  signOut as firebaseSignOut,
} from "firebase/auth";

const firebaseConfig = {
  apiKey: "YOUR_WEB_API_KEY",
  authDomain: "YOUR_PROJECT.firebaseapp.com",
  projectId: "YOUR_PROJECT",
  appId: "YOUR_APP_ID",
};

const app = initializeApp(firebaseConfig);
const auth = getAuth(app);
const provider = new GoogleAuthProvider();

provider.setCustomParameters({ prompt: "select_account" });

async function userPayload(user) {
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
  async signIn() {
    const credential = await signInWithPopup(auth, provider);
    return userPayload(credential.user);
  },

  async signOut() {
    await firebaseSignOut(auth);
    return null;
  },

  async initialUser() {
    await auth.authStateReady();
    return userPayload(auth.currentUser);
  },

  onAuthStateChanged(callback) {
    return onAuthStateChanged(auth, async (user) => {
      callback(await userPayload(user));
    });
  },
};
```

Keep this file boring. It should not render UI, mutate Pocopine stores,
or know about routes. It is only a typed bridge from Firebase SDK calls
to plain JSON values.

## Step 3 - adapt the client module in Rust

Create an app-owned service that wraps `ClientModule`. This keeps
components away from raw JavaScript reflection and makes the rest of
the app read like Pocopine code.

```rust
use pocopine::{App, AppPlugin, ClientModule, ScopeId};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct FirebaseAuthUser {
    pub token: String,
    pub uid: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "photoUrl")]
    pub photo_url: String,
}

#[derive(Clone, Default)]
pub struct FirebaseAuth;

pub fn firebase_auth_plugin() -> impl AppPlugin {
    struct Plugin;

    impl AppPlugin for Plugin {
        fn name(&self) -> &'static str {
            "firebase-auth"
        }

        fn install(self, app: App) -> App {
            app.provide_plugin(FirebaseAuth)
        }
    }

    Plugin
}

#[cfg(target_arch = "wasm32")]
impl FirebaseAuth {
    pub async fn sign_in(&self) -> Result<Option<FirebaseAuthUser>, String> {
        module()?.call_async("signIn").await.map_err(|err| err.to_string())
    }

    pub async fn initial_user(&self) -> Result<Option<FirebaseAuthUser>, String> {
        module()?.call_async("initialUser").await.map_err(|err| err.to_string())
    }

    pub async fn sign_out(&self) -> Result<Option<FirebaseAuthUser>, String> {
        module()?.call_async("signOut").await.map_err(|err| err.to_string())
    }

    pub fn subscribe(
        &self,
        scope: ScopeId,
        handler: impl FnMut(Result<Option<FirebaseAuthUser>, pocopine::ClientModuleError>)
            + 'static,
    ) -> Result<(), String> {
        module()?
            .subscribe(scope, "onAuthStateChanged", handler)
            .map_err(|err| err.to_string())
    }
}

#[cfg(target_arch = "wasm32")]
fn module() -> Result<ClientModule, String> {
    ClientModule::required("firebase").map_err(|err| err.to_string())
}
```

The non-wasm implementation can return a clear error for `sign_in`,
`initial_user`, and `sign_out`, and a no-op `Ok(())` for `subscribe`.
That lets server/native builds type-check without pretending Firebase
exists outside the browser.

## Step 4 - install `pocopine-auth-client`

Firebase owns provider session discovery. Pocopine owns the app's
`AuthSession`, fetch middleware token, cross-tab sync, and optimistic
refresh snapshot.

```rust
use pocopine::prelude::*;

const TOKEN_KEY: &str = "my_app_firebase_id_token";
const SNAPSHOT_KEY: &str = "my_app_auth_snapshot";

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(
            pocopine_auth_client::auth_plugin()
                .with_token_storage(pocopine_auth_client::storage::LocalStorage::new(
                    TOKEN_KEY,
                ))
                .with_session_snapshot(pocopine_auth_client::storage::LocalStorage::new(
                    SNAPSHOT_KEY,
                ))
                .wait_for_initial_auth_check(true)
                .with_cross_tab_sync(true),
        )
        .plugin(firebase_auth_plugin())
        .run();
}
```

`wait_for_initial_auth_check(true)` prevents a signed-out route flash
while Firebase checks its local IndexedDB session. The session snapshot
lets the UI render the last known user immediately on refresh while
Firebase confirms the real current user in the background.

## Step 5 - publish Firebase users into Pocopine auth

Convert the Firebase payload into a Pocopine `Principal`, then call
`AuthSession::sign_in` or `AuthSession::sign_out`.

```rust
use pocopine::{AuthUser, Plugins, Principal};

impl FirebaseAuthUser {
    fn principal(&self) -> Principal {
        let mut user = AuthUser::new(self.uid.clone());
        if !self.email.is_empty() {
            user = user.with_email(self.email.clone());
        }
        if !self.name.is_empty() {
            user = user.with_name(self.name.clone());
        }
        if !self.photo_url.is_empty() {
            user = user.with_claim(
                "photo_url",
                serde_json::Value::String(self.photo_url.clone()),
            );
        }
        Principal::from_user(user)
    }
}

fn publish_firebase_user(user: Option<FirebaseAuthUser>) {
    let Some(session) = Plugins.get::<pocopine_auth_client::AuthSession>() else {
        return;
    };

    match user {
        Some(user) => {
            session.sign_in(user.token.clone(), user.principal());
            // Also update your app store here: display name, email,
            // avatar URL, and any signed-in UI flags.
        }
        None => {
            session.sign_out();
            // Also clear your app store's signed-in UI fields.
        }
    }
}
```

Keep's implementation also avoids bumping the session when Firebase
rotates the same user's ID token. If the principal is unchanged, it
updates the token storage and skips another full sign-in publish.

## Step 6 - build login and auth gate components

The login component should call the Rust extension service, not the
JavaScript module directly:

```rust
#[handlers]
impl LoginButton {
    pub fn sign_in(&mut self) {
        let Some(firebase) = self.plugins().get::<FirebaseAuth>() else {
            self.error = "Firebase auth extension is not installed".to_string();
            return;
        };

        self.loading = true;
        let firebase = firebase.get().clone();
        let handle = pocopine::this::<Self>();

        pocopine::spawn_for_scope(handle.scope_id(), async move {
            let result = firebase.sign_in().await;
            let user = result.clone().ok().flatten();
            handle.update(|login| {
                login.loading = false;
                login.error = result.err().unwrap_or_default();
            });
            publish_firebase_user(user);
        });
    }
}
```

The auth gate should:

1. Restore the session snapshot into your store if one exists.
2. Await `firebase.initial_user()`.
3. Subscribe to `firebase.subscribe(scope, ...)`.
4. Publish every provider change into `AuthSession` and your store.

Do not put app rendering in JavaScript. Use `.poco` templates with
store fields:

```html
<template pp-if="!$store.app.auth_ready">
  <section class="auth-gate">Checking your Google session.</section>
</template>

<template pp-if="$store.app.auth_ready && !$store.app.auth_signed_in">
  <section class="auth-gate">
    <login-button></login-button>
  </section>
</template>

<template pp-if="$store.app.auth_signed_in">
  <slot></slot>
</template>
```

For app-bar controls, bind visibility to the store:

```html
<button pp-show="$store.app.auth_signed_in" @click="toggle_sidebar">...</button>
<account-menu pp-show="$store.app.auth_signed_in"></account-menu>
```

Use Pine primitives such as `pine-avatar-root`, `pine-popover-root`,
and `pine-dropdown-menu-root` for the actual UI. The JavaScript module
should never be responsible for showing an avatar, menu, or route.

## Production server verification

The Firebase web config is not a server secret, but the ID token must
still be verified on the server before reading or writing private data.
Client-side guards and `AuthSession` are UX. Server authorization is
the security boundary.

Pocopine intentionally does not maintain bundled vendor verifier
crates. For production, wire Firebase token verification in app code,
tutorial code, or an external provider crate using
[`pocopine-auth-jwt`](./auth-jwt-providers.md). Then scope server
streams, database rows, and `#[server]` functions by the verified
`Principal`.

The Keep example demonstrates the browser integration and local app
UX. Its example sync stream is not multi-user authorization.

## Local development notes

- Add `localhost` and your production host to the Firebase Auth
  authorized domains for Google sign-in.
- Firebase popup auth can be blocked by cross-origin isolation
  headers because the hosted helper iframe must load from Firebase's
  domain. The Keep example leaves those headers off by default and
  uses the IndexedDB sync cache in that mode.
- If an app needs OPFS SQLite in the browser, test that mode
  separately from Firebase popup auth. Some browsers require
  cross-origin isolation for OPFS SQLite, while Firebase popup auth
  prefers the non-isolated setup.

