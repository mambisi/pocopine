//! Axum handlers for `/_pocopine/auth/{signup,login,logout}`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pocopine_auth::AuthUser;
use serde_json::{json, Map, Value};

use crate::builder::CredentialsHandle;
use crate::error::CredentialsError;
use crate::password::{hash_password, verify_password};
use crate::store::{PasswordCredentials, TokenStore, UserStore};

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

/// Pull the login identifier and password out of an incoming JSON
/// body. The field carrying the identifier is configurable per
/// builder; the password field is always `"password"`. Both must
/// be strings.
fn extract_credentials(
    body: &Map<String, Value>,
    login_field: &str,
) -> Result<(String, String), CredentialsError> {
    let login_id = body
        .get(login_field)
        .and_then(|v| v.as_str())
        .ok_or(CredentialsError::InvalidLoginId("missing"))?
        .to_string();
    let password = body
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or(CredentialsError::WeakPassword("missing"))?
        .to_string();
    Ok((login_id, password))
}

async fn signup_handler<S: UserStore, T: TokenStore>(
    State(state): State<Arc<CredentialsHandle<S, T>>>,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    let (raw_login_id, password) = extract_credentials(&body, &state.login_id_field)?;

    state
        .login_id_validator
        .validate(&raw_login_id)
        .map_err(CredentialsError::InvalidLoginId)?;
    let normalized = state.login_id_validator.normalize(&raw_login_id);

    (state.password_validator)(&password).map_err(CredentialsError::WeakPassword)?;

    if state
        .store
        .find_by_login_id(&normalized)
        .await
        .map_err(CredentialsError::Storage)?
        .is_some()
    {
        return Err(CredentialsError::LoginIdTaken);
    }

    let password_hash = hash_password(password, state.argon.clone()).await?;
    // The store generates the id and returns the freshly-created
    // user. Any failure inside `create` (DB issue, race-induced
    // duplicate, …) maps to `LoginIdTaken` for the response;
    // the `tracing` log carries the original error class.
    let user = state
        .store
        .create(&normalized, password_hash)
        .await
        .map_err(|err| {
            tracing::warn!(
                target: "pocopine.log",
                error = %err,
                "credentials signup: store rejected create"
            );
            CredentialsError::LoginIdTaken
        })?;

    let auth_user = user.to_auth_user();
    let token = issue_session_token(&state, user.id(), &auth_user)?;
    Ok(Json(json!({
        "token": token,
        "user": auth_user,
    })))
}

async fn login_handler<S: UserStore, T: TokenStore>(
    State(state): State<Arc<CredentialsHandle<S, T>>>,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    let (raw_login_id, password) = extract_credentials(&body, &state.login_id_field)?;

    // Validation runs even on login: a malformed identifier never
    // matches a stored one, but doing the check at the route layer
    // gives the client a `422 invalid_login_id` error rather than a
    // `401 invalid_credentials` after the argon2 cycle (which would
    // run anyway). The constant-time-login property is preserved
    // because the validation+normalization does no
    // crypto / DB work.
    state
        .login_id_validator
        .validate(&raw_login_id)
        .map_err(CredentialsError::InvalidLoginId)?;
    let normalized = state.login_id_validator.normalize(&raw_login_id);

    let user = state
        .store
        .find_by_login_id(&normalized)
        .await
        .map_err(CredentialsError::Storage)?;

    // Constant-time login: always run argon2 verify, even on
    // not-found, so a timing/CPU probe can't distinguish the
    // unknown-identifier branch from the wrong-password branch.
    let (hash, real_user) = match user {
        Some(u) => (u.password_hash().to_string(), Some(u)),
        None => (state.dummy_hash.clone(), None),
    };

    let ok = verify_password(password, hash).await?;
    let user = match (ok, real_user) {
        (true, Some(u)) => u,
        _ => return Err(CredentialsError::InvalidCredentials),
    };

    let auth_user = user.to_auth_user();
    let token = issue_session_token(&state, user.id(), &auth_user)?;
    Ok(Json(json!({
        "token": token,
        "user": auth_user,
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
    subject: &str,
    auth_user: &AuthUser,
) -> Result<String, CredentialsError> {
    // Mirror the JWT claim shape RFC-074 §5.12 specifies. The app's
    // `to_auth_user()` projection decides which fields are visible:
    // emails, phones, roles, custom claims (incl. `email_verified`)
    // all flow through here.
    let extra = json!({
        "email": auth_user.email,
        "name": auth_user.name,
        "roles": auth_user.roles,
        "permissions": auth_user.permissions,
        "claims": auth_user.claims,
    });
    state
        .issuer
        .sign(subject, extra)
        .map_err(|err| CredentialsError::SessionIssue(err.to_string()))
}
