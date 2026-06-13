//! Axum handlers for `/_pocopine/auth/{signup,login,logout}`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pocopine_auth::AuthUser;
use serde::Deserialize;
use serde_json::{Map, Value, json};

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

async fn signup_handler<S: UserStore, T: TokenStore>(
    State(state): State<Arc<CredentialsHandle<S, T>>>,
    Json(req): Json<SignupRequest>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    validate_email_syntax(&req.email)?;
    (state.password_validator)(&req.password).map_err(CredentialsError::WeakPassword)?;
    let email = req.email.trim().to_ascii_lowercase();

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
    // The store generates the id and returns the freshly-created
    // user. Any failure inside `create` (DB issue, race-induced
    // duplicate-email, …) maps to `EmailTaken` for the response;
    // the `tracing` log carries the original error class.
    let user = state
        .store
        .create(&email, password_hash)
        .await
        .map_err(|err| {
            tracing::warn!(
                target: "pocopine.log",
                error = %err,
                "credentials signup: store rejected create"
            );
            CredentialsError::EmailTaken
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
    Json(req): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, CredentialsError> {
    let email = req.email.trim().to_ascii_lowercase();
    let user = state
        .store
        .find_by_email(&email)
        .await
        .map_err(CredentialsError::Storage)?;

    // Constant-time login: always run argon2 verify, even on
    // not-found OR no-password-set (multi-credential users with
    // OAuth-only / passkey-only auth). A timing/CPU probe sees
    // the same shape regardless of which bucket the input fell
    // into: unknown email, OAuth-only account, wrong password —
    // all run argon2 against the dummy hash before returning
    // `InvalidCredentials`.
    let (hash, real_user) = match user {
        Some(u) => match u.password_hash() {
            Some(h) => (h.to_string(), Some(u)),
            // User exists but has no password (OAuth-only / passkey-only).
            // Fall through to the dummy hash so timing matches.
            None => (state.dummy_hash.clone(), None),
        },
        None => (state.dummy_hash.clone(), None),
    };

    let ok = verify_password(req.password, hash).await?;
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

/// JWT-reserved + framework-recognized claim keys that custom
/// claims must not shadow. The framework writes the authoritative
/// values for these; an app-supplied `with_claim("sub", "evil")`
/// would otherwise overwrite the JWT subject and let the user
/// impersonate anyone.
const RESERVED_CLAIM_KEYS: &[&str] = &[
    // Standard JWT (RFC 7519) — the issuer sets these.
    "sub",
    "iss",
    "aud",
    "iat",
    "exp",
    "nbf",
    "jti",
    // Framework-recognized projections from `AuthUser` typed fields.
    "email",
    "name",
    "roles",
    "permissions",
];

fn issue_session_token<S: UserStore, T: TokenStore>(
    state: &CredentialsHandle<S, T>,
    subject: &str,
    auth_user: &AuthUser,
) -> Result<String, CredentialsError> {
    // Build the JWT claim payload. Framework-recognized fields go at
    // the top level; the app's custom claims (from
    // `to_auth_user().with_claim(...)`) are flattened in alongside
    // so they round-trip through `JwtVerifier` and reach
    // `principal.user().claim("...")` at the documented top-level
    // path. RFC-074 §5.12 specifies this shape.
    //
    // Reserved-key collision policy: framework writes the
    // authoritative values for `sub` / `iss` / `aud` / `iat` /
    // `exp` / `nbf` / `jti` / `email` / `name` / `roles` /
    // `permissions`. App custom claims with the same key are
    // dropped with a `tracing::warn!` — never silently overwritten,
    // never silently shadowed.
    let mut extra = Map::new();
    if let Some(email) = &auth_user.email {
        extra.insert("email".to_string(), json!(email));
    }
    if let Some(name) = &auth_user.name {
        extra.insert("name".to_string(), json!(name));
    }
    if !auth_user.roles.is_empty() {
        extra.insert("roles".to_string(), json!(auth_user.roles));
    }
    if !auth_user.permissions.is_empty() {
        extra.insert("permissions".to_string(), json!(auth_user.permissions));
    }
    for (key, value) in &auth_user.claims {
        if RESERVED_CLAIM_KEYS.contains(&key.as_str()) {
            tracing::warn!(
                target: "pocopine.log",
                claim = %key,
                "credentials: custom claim collides with reserved JWT or framework key; dropping"
            );
            continue;
        }
        extra.insert(key.clone(), value.clone());
    }

    state
        .issuer
        .sign(subject, Value::Object(extra))
        .map_err(|err| CredentialsError::SessionIssue(err.to_string()))
}

/// Minimal email-address validator. Catches the structural shape
/// (missing `@`, no domain dot, control chars, whitespace,
/// excessive length); apps that need stricter rules layer their
/// own checks at the route or store level. Not a full RFC 5322
/// implementation — good RFC 5322 takes thousands of lines and
/// breaks against most user-typed real addresses anyway.
fn validate_email_syntax(email: &str) -> Result<(), CredentialsError> {
    let trimmed = email.trim();
    if trimmed.is_empty() || trimmed.len() > 254 {
        return Err(CredentialsError::InvalidEmail);
    }
    if trimmed.contains(char::is_whitespace) {
        return Err(CredentialsError::InvalidEmail);
    }
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(CredentialsError::InvalidEmail);
    }
    let Some(at) = trimmed.find('@') else {
        return Err(CredentialsError::InvalidEmail);
    };
    let (local, rest) = trimmed.split_at(at);
    let domain = &rest[1..];
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(CredentialsError::InvalidEmail);
    }
    Ok(())
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
