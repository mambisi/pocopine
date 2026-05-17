//! Firebase Auth bridge exposed as a Pocopine app extension.
//!
//! The JavaScript side only owns the provider SDK calls. Keep's UI,
//! session state, and app gating stay in Pocopine components and
//! `pocopine-auth-client`.

use pocopine::{App, AppPlugin, Plugins, Principal};

use super::bindings::FirebaseAuthUser;
use crate::KeepStore;

pub const KEEP_AUTH_SNAPSHOT_KEY: &str = "pocopine_keep_auth_snapshot";
pub const KEEP_FIREBASE_TOKEN_KEY: &str = "pocopine_keep_firebase_id_token";

pub fn restore_keep_auth_snapshot() -> bool {
    let Some(session) = Plugins.get::<pocopine_auth_client::AuthSession>() else {
        return false;
    };
    if !session.is_restoring() {
        return false;
    }
    publish_keep_principal(session.principal())
}

pub fn keep_auth_fields_from_principal(principal: &Principal) -> Option<(String, String, String)> {
    let user = principal.user()?;
    let display_name = user
        .name
        .clone()
        .or_else(|| user.email.clone())
        .unwrap_or_default();
    let email = user.email.clone().unwrap_or_default();
    let photo_url = user
        .claim("photo_url")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    Some((display_name, email, photo_url))
}

pub fn publish_keep_auth_user(user: Option<FirebaseAuthUser>) {
    match user {
        Some(user) => {
            let principal = user.principal();

            if let Some(session) = Plugins.get::<pocopine_auth_client::AuthSession>() {
                // Firebase fires onAuthStateChanged on every ~1h token rotation
                // with the same identity. Without this guard each rotation bumps
                // the session epoch, persists the snapshot, re-broadcasts to peer
                // tabs, and re-runs the router guard — all wasted.
                if session.is_ready() && !session.is_restoring() && session.principal() == principal
                {
                    pocopine_auth_client::set_token(user.token);
                    return;
                }
                session.sign_in(user.token.clone(), principal);
            }

            let display_name = user.display_name();
            let email = user.email.unwrap_or_default();
            let photo_url = user.photo_url.unwrap_or_default();
            pocopine::store::<KeepStore>().update(move |store| {
                store.set_auth_user(true, display_name, email, photo_url);
            });
        }
        None => {
            if let Some(session) = Plugins.get::<pocopine_auth_client::AuthSession>() {
                if !session.is_authenticated() && session.is_ready() && !session.is_restoring() {
                    return;
                }
                session.sign_out();
            }

            pocopine::store::<KeepStore>().update(|store| {
                store.set_auth_user(false, String::new(), String::new(), String::new());
            });
        }
    }
}

fn publish_keep_principal(principal: Principal) -> bool {
    let Some((display_name, email, photo_url)) = keep_auth_fields_from_principal(&principal) else {
        return false;
    };

    pocopine::store::<KeepStore>().update(move |store| {
        store.set_auth_user(true, display_name, email, photo_url);
    });
    true
}

#[derive(Clone, Default)]
pub struct KeepFirebaseAuth;

pub fn keep_firebase_auth_plugin() -> impl AppPlugin {
    KeepFirebaseAuthPlugin
}

struct KeepFirebaseAuthPlugin;

impl AppPlugin for KeepFirebaseAuthPlugin {
    fn name(&self) -> &'static str {
        "keep-firebase-auth"
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(KeepFirebaseAuth)
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use pocopine::ScopeId;

    use super::{FirebaseAuthUser, KeepFirebaseAuth};

    impl KeepFirebaseAuth {
        pub async fn sign_in(&self) -> Result<Option<FirebaseAuthUser>, String> {
            module()?.sign_in().await.map_err(|err| err.to_string())
        }

        pub async fn initial_user(&self) -> Result<Option<FirebaseAuthUser>, String> {
            module()?
                .initial_user()
                .await
                .map_err(|err| err.to_string())
        }

        pub async fn sign_out(&self) -> Result<Option<FirebaseAuthUser>, String> {
            module()?.sign_out().await.map_err(|err| err.to_string())
        }

        pub fn subscribe(
            &self,
            scope: ScopeId,
            mut handler: impl FnMut(Result<Option<FirebaseAuthUser>, String>) + 'static,
        ) -> Result<(), String> {
            module()?
                .on_auth_state_changed(scope, move |result| {
                    handler(result.map_err(|err| err.to_string()));
                })
                .map_err(|err| err.to_string())
        }
    }

    fn module() -> Result<crate::firebase::client::Module, String> {
        crate::firebase::client::required().map_err(|err| err.to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl KeepFirebaseAuth {
    pub async fn sign_in(&self) -> Result<Option<FirebaseAuthUser>, String> {
        Err("Firebase auth bridge is only available in the browser".to_string())
    }

    pub async fn initial_user(&self) -> Result<Option<FirebaseAuthUser>, String> {
        Err("Firebase auth bridge is only available in the browser".to_string())
    }

    pub async fn sign_out(&self) -> Result<Option<FirebaseAuthUser>, String> {
        Err("Firebase auth bridge is only available in the browser".to_string())
    }

    pub fn subscribe(
        &self,
        _scope: pocopine::ScopeId,
        _handler: impl FnMut(Result<Option<FirebaseAuthUser>, String>) + 'static,
    ) -> Result<(), String> {
        Ok(())
    }
}
