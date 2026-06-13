use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::firebase::{FirebaseAuthUser, publish_keep_auth_user, restore_keep_auth_snapshot};
use crate::{KeepFirebaseAuth, KeepLogin};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "KeepAuthGate.poco",
    style = "KeepAuthGate.css",
    role = "panel",
    uses = [PineIcon, KeepLogin]
)]
pub struct KeepAuthGate {
    pub error: String,
}

#[handlers]
impl KeepAuthGate {
    pub fn on_ready(&self, handle: Handle<Self>) {
        let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
            mark_auth_unavailable(
                handle,
                "Firebase auth extension is not installed".to_string(),
            );
            return;
        };

        restore_keep_auth_snapshot();

        let firebase = firebase.get().clone();
        let scope = handle.scope_id();
        pocopine::spawn_for_scope(scope, async move {
            let initial = firebase.initial_user().await;
            let subscribe_error = if initial.is_ok() {
                let handle_for_auth = handle.clone();
                firebase
                    .subscribe(scope, move |result| {
                        let handle = handle_for_auth.clone();
                        update_gate_from_auth_result_deferred(handle, result);
                    })
                    .err()
            } else {
                None
            };

            let (error, auth_user) = prepare_auth_result(initial);
            update_gate_error(handle, error.or(subscribe_error));
            publish_keep_auth_user(auth_user);
        });
    }
}

fn prepare_auth_result(
    result: Result<Option<FirebaseAuthUser>, String>,
) -> (Option<String>, Option<FirebaseAuthUser>) {
    match result {
        Ok(user) => (None, user),
        Err(err) => (Some(err), None),
    }
}

fn update_gate_from_auth_result_deferred(
    handle: Handle<KeepAuthGate>,
    result: Result<Option<FirebaseAuthUser>, String>,
) {
    let scope = handle.scope_id();
    pocopine::spawn_for_scope(scope, async move {
        let (error, auth_user) = prepare_auth_result(result);
        update_gate_error(handle, error);
        publish_keep_auth_user(auth_user);
    });
}

fn update_gate_error(handle: Handle<KeepAuthGate>, error: Option<String>) {
    if let Some(err) = error {
        handle.update(move |gate| {
            gate.error = err;
        });
    } else {
        handle.update(|gate| {
            gate.error.clear();
        });
    }
}

fn mark_auth_unavailable(handle: Handle<KeepAuthGate>, message: String) {
    publish_keep_auth_user(None);
    let scope = handle.scope_id();
    pocopine::spawn_for_scope(scope, async move {
        handle.update(move |gate| {
            gate.error = message;
        });
    });
}
