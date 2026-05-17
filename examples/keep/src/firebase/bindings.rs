use pocopine::{AuthUser, Principal};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(pocopine_ts_rs::TS))]
#[cfg_attr(test, ts(export, export_to = "../src/firebase/bindings.ts"))]
#[serde(rename_all = "camelCase")]
pub struct FirebaseAuthUser {
    pub token: String,
    pub uid: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub photo_url: Option<String>,
}

impl FirebaseAuthUser {
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or_else(|| self.email.as_deref().filter(|email| !email.is_empty()))
            .unwrap_or_default()
            .to_string()
    }

    pub fn initial(&self) -> String {
        self.display_name()
            .chars()
            .find(|ch| !ch.is_whitespace())
            .map(|ch| ch.to_uppercase().collect())
            .unwrap_or_else(|| "G".to_string())
    }

    pub fn principal(&self) -> Principal {
        let mut user = AuthUser::new(self.uid.clone());
        if let Some(email) = self.email.as_deref().filter(|email| !email.is_empty()) {
            user = user.with_email(email.to_string());
        }
        if let Some(name) = self.name.as_deref().filter(|name| !name.is_empty()) {
            user = user.with_name(name.to_string());
        }
        if let Some(photo_url) = self
            .photo_url
            .as_deref()
            .filter(|photo_url| !photo_url.is_empty())
        {
            user = user.with_claim(
                "photo_url",
                serde_json::Value::String(photo_url.to_string()),
            );
        }
        Principal::from_user(user)
    }
}
