pub mod auth;
pub mod bindings;

#[pocopine::client_module("Firebase.client.ts")]
pub mod client {
    use super::bindings::FirebaseAuthUser;
}

pub use auth::{
    keep_auth_fields_from_principal, keep_firebase_auth_plugin, publish_keep_auth_user,
    restore_keep_auth_snapshot, KeepFirebaseAuth, KEEP_AUTH_SNAPSHOT_KEY, KEEP_FIREBASE_TOKEN_KEY,
};
pub use bindings::FirebaseAuthUser;
