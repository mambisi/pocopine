use pine::{
    PineAvatarFallback, PineAvatarImage, PineAvatarRoot, PinePopoverContent, PinePopoverPortal,
    PinePopoverRoot, PinePopoverTrigger,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::firebase::{publish_keep_auth_user, FirebaseAuthUser, KeepFirebaseAuth};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "KeepLogin.poco",
    style = "KeepLogin.css",
    role = "panel",
    uses = [
        PineIcon,
        PineAvatarRoot,
        PineAvatarImage,
        PineAvatarFallback,
        PinePopoverRoot,
        PinePopoverTrigger,
        PinePopoverPortal,
        PinePopoverContent,
    ]
)]
pub struct KeepLogin {
    pub loading: bool,
    pub status: String,
    pub error: String,
}

#[handlers]
impl KeepLogin {
    pub fn sign_in(&mut self) {
        let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
            self.error = "Firebase auth extension is not installed".to_string();
            return;
        };

        self.loading = true;
        self.error.clear();
        self.status = "Opening Google sign-in".to_string();

        let firebase = firebase.get().clone();
        let handle = pocopine::this::<Self>();
        pocopine::spawn_for_scope(handle.scope_id(), async move {
            let result = firebase.sign_in().await;
            let user = result.as_ref().ok().and_then(|user| user.clone());
            handle.update(|login| login.apply_action_result(result));
            if let Some(user) = user {
                publish_keep_auth_user(Some(user));
            }
        });
    }

    pub fn sign_out(&mut self) {
        let Some(firebase) = self.plugins().get::<KeepFirebaseAuth>() else {
            self.error = "Firebase auth extension is not installed".to_string();
            return;
        };

        self.loading = true;
        self.error.clear();
        self.status = "Signing out".to_string();

        let firebase = firebase.get().clone();
        let handle = pocopine::this::<Self>();
        pocopine::spawn_for_scope(handle.scope_id(), async move {
            let result = firebase.sign_out().await;
            let user = result.as_ref().ok().cloned();
            handle.update(|login| login.apply_action_result(result));
            if let Some(user) = user {
                publish_keep_auth_user(user);
            }
        });
    }
}

impl KeepLogin {
    fn apply_action_result(&mut self, result: Result<Option<FirebaseAuthUser>, String>) {
        self.loading = false;
        self.error.clear();

        match result {
            Ok(user) => {
                self.status = if user.is_some() {
                    "Signed in with Google".to_string()
                } else {
                    "Sign in to open your notes".to_string()
                };
            }
            Err(err) => {
                self.error = err;
                self.status = "Google sign-in is unavailable".to_string();
            }
        }
    }
}
