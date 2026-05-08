//! Axum handlers for `/_pocopine/auth/{signup,login,logout}`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pocopine_auth::Role;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::builder::CredentialsHandle;
use crate::error::CredentialsError;
use crate::password::{hash_password, verify_password};
use crate::store::{TokenStore, UserStore};
use crate::user::User;

/// Build the axum sub-router under `/_pocopine/auth`. Mounted by
/// the [`crate::Credentials`] [`pocopine_server::ServerPlugin`]
/// install path.
pub(crate) fn router<S: UserStore, T: TokenStore>(handle: Arc<CredentialsHandle<S, T>>) -> Router {
    Router::new()
        .route("/signup", post(signup_handler::<S, T>))
        .route("/login", post(login_handler::<S, T>))
        .route("/logout", post(logout_handler::<S, T>))
        .with_state(handle)
}

#[derive(Deserialize)]
struct SignupRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

/// Public user shape returned to the wasm client. Excludes the
/// argon2 hash; preserves only the fields the client typically
/// projects through `Principal::user()`.
#[derive(Serialize)]
struct PublicUser {
    id: String,
    email: String,
    email_verified: bool,
    roles: Vec<Role>,
}

impl From<&User> for PublicUser {
    fn from(user: &User) -> Self {
        Self {
            id: user.id.clone(),
            email: user.email.clone(),
            email_verified: user.email_verified,
            roles: user.roles.clone(),
        }
    }
}

async fn signup_handler<S: UserStore, T: TokenStore>(
    State(state): State<Arc<CredentialsHandle<S, T>>>,
    Json(req): Json<SignupRequest>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    validate_email_syntax(&req.email)?;
    (state.password_validator)(&req.password).map_err(CredentialsError::WeakPassword)?;
    let email = req.email.to_ascii_lowercase();

    if state
        .store
        .find_by_email(&email)
        .await
        .map_err(CredentialsError::Storage)?
        .is_some()
    {
        return Err(CredentialsError::EmailTaken);
    }

    let password_hash = hash_password(req.password, state.argon.clone()).await?;
    let id = (state.id_generator)();
    let now = now_ms();
    let user = User::new(id, &email, password_hash, now);

    state
        .store
        .create(user.clone())
        .await
        .map_err(CredentialsError::Storage)?;

    let token = issue_session_token(&state, &user)?;
    let public = PublicUser::from(&user);
    Ok(Json(json!({
        "token": token,
        "user": public,
    })))
}

async fn login_handler<S: UserStore, T: TokenStore>(
    State(state): State<Arc<CredentialsHandle<S, T>>>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    let email = req.email.to_ascii_lowercase();
    let user = state
        .store
        .find_by_email(&email)
        .await
        .map_err(CredentialsError::Storage)?;

    // Constant-time login: always run argon2 verify, even on
    // not-found, so a timing/CPU probe can't distinguish the
    // unknown-email branch from the wrong-password branch.
    let (hash, real_user) = match user {
        Some(u) => (u.password_hash.clone(), Some(u)),
        None => (state.dummy_hash.clone(), None),
    };

    let ok = verify_password(req.password, hash).await?;
    let user = match (ok, real_user) {
        (true, Some(u)) => u,
        _ => return Err(CredentialsError::InvalidCredentials),
    };

    let token = issue_session_token(&state, &user)?;
    let public = PublicUser::from(&user);
    Ok(Json(json!({
        "token": token,
        "user": public,
    })))
}

async fn logout_handler<S: UserStore, T: TokenStore>(
    State(_): State<Arc<CredentialsHandle<S, T>>>,
) -> StatusCode {
    // Stateless tokens have no server-side state to invalidate; the
    // client clears its bearer slot on this response. Future:
    // session-cookie clearing via `Set-Cookie: <name>=; Max-Age=0`
    // when the cookie source is in use.
    StatusCode::NO_CONTENT
}

fn issue_session_token<S: UserStore, T: TokenStore>(
    state: &CredentialsHandle<S, T>,
    user: &User,
) -> Result<String, CredentialsError> {
    let extra = json!({
        "email": user.email,
        "email_verified": user.email_verified,
        "roles": user.roles,
        "permissions": user.permissions,
    });
    state
        .issuer
        .sign(&user.id, extra)
        .map_err(|err| CredentialsError::SessionIssue(err.to_string()))
}

/// Minimal email-address validator. Apps that need RFC 5322 strictness
/// should override the validator on the [`crate::Credentials`] builder
/// (a future builder addition); for the tryout case this catches the
/// structural shape and the empty-string footgun.
fn validate_email_syntax(email: &str) -> Result<(), CredentialsError> {
    let trimmed = email.trim();
    if trimmed.is_empty() || trimmed.len() > 254 {
        return Err(CredentialsError::InvalidEmail);
    }
    let Some(at) = trimmed.find('@') else {
        return Err(CredentialsError::InvalidEmail);
    };
    let (local, domain) = trimmed.split_at(at);
    let domain = &domain[1..];
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(CredentialsError::InvalidEmail);
    }
    if trimmed.contains(char::is_whitespace) {
        return Err(CredentialsError::InvalidEmail);
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_syntax_acceptance() {
        validate_email_syntax("alice@example.com").unwrap();
        validate_email_syntax("a.b+tag@sub.example.co.uk").unwrap();
    }

    #[test]
    fn email_syntax_rejection() {
        assert!(matches!(
            validate_email_syntax(""),
            Err(CredentialsError::InvalidEmail)
        ));
        assert!(matches!(
            validate_email_syntax("not-an-email"),
            Err(CredentialsError::InvalidEmail)
        ));
        assert!(matches!(
            validate_email_syntax("@example.com"),
            Err(CredentialsError::InvalidEmail)
        ));
        assert!(matches!(
            validate_email_syntax("alice@"),
            Err(CredentialsError::InvalidEmail)
        ));
        assert!(matches!(
            validate_email_syntax("alice@nodot"),
            Err(CredentialsError::InvalidEmail)
        ));
        assert!(matches!(
            validate_email_syntax("alice @example.com"),
            Err(CredentialsError::InvalidEmail)
        ));
    }
}
