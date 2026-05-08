# Tutorial — phone OTP auth with Twilio + Postgres

This is the **build-it-yourself-today** guide for phone OTP
authentication. The official crate (`pocopine-auth-otp`) is
future work; until it ships, you get the same functionality by
composing the primitives that already exist:

- `pocopine_auth_jwt::JwtIssuer` mints session tokens.
- `pocopine_auth_jwt::JwtVerifier` validates them in middleware.
- `pocopine_server::Server` mounts your routes as a `ServerPlugin`.
- Postgres + `sqlx` stores users and OTP codes.
- Twilio Messages API delivers the SMS.

When `pocopine-auth-otp` lands, swapping is a small refactor:
delete your hand-rolled routes, install the plugin, keep the
schema. Everything downstream (`Principal`, `#[server(guard = …)]`,
`auth_plugin()` on the wasm side) is unchanged because the
session JWT shape stays the same.

## What we're building

Two routes:

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `/_pocopine/auth/otp/request` | `{ "phone": "+15551234567" }` | `204 No Content` |
| POST | `/_pocopine/auth/otp/verify` | `{ "phone": "+15551234567", "code": "123456" }` | `{ "token", "user" }` |

The flow:

```
   user                           server                            Twilio
   │  type phone, click "Send"
   │  → POST /otp/request
   │                                │
   │                                ├─ validate +E.164
   │                                ├─ check rate limit
   │                                ├─ generate 6-digit code
   │                                ├─ hash + store in otp_codes
   │                                │  (5-min TTL, attempt counter = 0)
   │                                ├─ POST Messages API
   │                                │  ──────────────────────────►  send SMS
   │                                │
   │  ← 204 No Content              │
   │                                │
   │  receive SMS, type code
   │  → POST /otp/verify
   │                                │
   │                                ├─ validate +E.164
   │                                ├─ load otp_codes row, check TTL
   │                                ├─ check attempt counter < 3
   │                                ├─ constant-time compare hashes
   │                                ├─ delete otp_codes row (single-use)
   │                                ├─ find_or_create user by phone
   │                                ├─ JwtIssuer::sign → session JWT
   │  ← 200 { token, user }         │
```

## Schema

```sql
-- The user table. Pure-OTP apps may have only `phone` as the
-- credential; multi-credential apps add `password_hash`,
-- `google_oauth_sub`, etc. alongside.
CREATE TABLE app_users (
    id              TEXT PRIMARY KEY,
    phone           TEXT NOT NULL UNIQUE,    -- +E.164
    phone_verified  BOOLEAN NOT NULL DEFAULT FALSE,
    display_name    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One row per outstanding OTP. Single-use: deleted on verify.
-- Indexed on phone for the rate-limit lookup.
CREATE TABLE otp_codes (
    phone            TEXT PRIMARY KEY,        -- +E.164, one outstanding code per phone
    code_hash        BYTEA NOT NULL,          -- sha256(code)
    expires_at_ms    BIGINT NOT NULL,
    attempt_count    INT NOT NULL DEFAULT 0,
    requested_at_ms  BIGINT NOT NULL          -- for rate-limit windows
);

-- Per-phone request rate limit (deny if more than N requests in M
-- minutes). Stored as a sliding window of recent request times.
CREATE TABLE otp_request_log (
    phone            TEXT NOT NULL,
    requested_at_ms  BIGINT NOT NULL
);
CREATE INDEX otp_request_log_phone_idx ON otp_request_log (phone, requested_at_ms);
```

## Cargo.toml

```toml
[dependencies]
pocopine-auth = { workspace = true }
pocopine-auth-jwt = { workspace = true }
pocopine-core = { workspace = true, default-features = false }
pocopine-server = { workspace = true }
serde = { workspace = true }
serde_json = "1"
axum = "0.7"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync"] }
tracing = { workspace = true }
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "macros"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls-webpki-roots"] }
sha2 = "0.10"
subtle = "2"
rand = "0.8"
async-trait = "0.1"
```

## Twilio sender

A tiny async helper that POSTs to the Messages API. Twilio's
auth is HTTP Basic with `account_sid` as the username and
`auth_token` as the password. From-numbers, Messaging Service
SIDs, and other niceties are app-specific.

```rust
use std::sync::Arc;

use reqwest::Client;
use serde::Serialize;

#[derive(Clone)]
pub struct TwilioSender {
    client: Client,
    account_sid: Arc<String>,
    auth_token: Arc<String>,
    from_number: Arc<String>,    // E.164 of the Twilio number sending
}

impl TwilioSender {
    pub fn from_env() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("http client"),
            account_sid: Arc::new(
                std::env::var("TWILIO_ACCOUNT_SID")
                    .expect("set TWILIO_ACCOUNT_SID"),
            ),
            auth_token: Arc::new(
                std::env::var("TWILIO_AUTH_TOKEN")
                    .expect("set TWILIO_AUTH_TOKEN"),
            ),
            from_number: Arc::new(
                std::env::var("TWILIO_FROM_NUMBER")
                    .expect("set TWILIO_FROM_NUMBER"),
            ),
        }
    }

    pub async fn send(&self, to: &str, body: &str) -> Result<(), TwilioError> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(rename = "To")]
            to: &'a str,
            #[serde(rename = "From")]
            from: &'a str,
            #[serde(rename = "Body")]
            body: &'a str,
        }

        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.account_sid
        );
        let response = self
            .client
            .post(&url)
            .basic_auth(&*self.account_sid, Some(&*self.auth_token))
            .form(&Body { to, from: &self.from_number, body })
            .send()
            .await
            .map_err(TwilioError::Network)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::error!(
                target: "pocopine.log",
                %status,
                body = %body,
                "twilio send failed"
            );
            return Err(TwilioError::Provider(status.as_u16()));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum TwilioError {
    Network(reqwest::Error),
    Provider(u16),
}

impl std::fmt::Display for TwilioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "twilio network error: {e}"),
            Self::Provider(s) => write!(f, "twilio provider returned {s}"),
        }
    }
}

impl std::error::Error for TwilioError {}
```

The 5-second client timeout matters. Twilio's API is usually
sub-second, but a hung request blocks the user for that long
on the `/otp/request` route. Don't let it hang.

## OTP service

The handler glue plus the rate-limit / attempt-limit checks:

```rust
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pocopine_auth::AuthUser;
use pocopine_auth_jwt::JwtIssuer;
use pocopine_server::{Server, ServerPlugin};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;

const CODE_TTL_MS: u64 = 5 * 60 * 1000;          // 5 minutes
const RATE_LIMIT_WINDOW_MS: u64 = 15 * 60 * 1000; // 15 minutes
const RATE_LIMIT_PER_PHONE: i64 = 5;              // codes per window per phone
const MAX_ATTEMPTS: i32 = 3;                      // verifications per code

pub struct PhoneOtp {
    pool: PgPool,
    twilio: TwilioSender,
    issuer: JwtIssuer,
}

impl PhoneOtp {
    pub fn new(pool: PgPool, twilio: TwilioSender, issuer: JwtIssuer) -> Self {
        Self { pool, twilio, issuer }
    }
}

impl ServerPlugin for PhoneOtp {
    fn name(&self) -> &'static str {
        "app-phone-otp"
    }

    fn install(self, server: Server) -> Server {
        let state = Arc::new(self);
        server.router_mut(|r| {
            r.nest(
                "/_pocopine/auth/otp",
                Router::new()
                    .route("/request", post(request_handler))
                    .route("/verify", post(verify_handler))
                    .with_state(state),
            )
        })
    }
}

#[derive(Deserialize)]
struct RequestBody {
    phone: String,
}

#[derive(Deserialize)]
struct VerifyBody {
    phone: String,
    code: String,
}

async fn request_handler(
    State(state): State<Arc<PhoneOtp>>,
    Json(body): Json<RequestBody>,
) -> Result<StatusCode, OtpError> {
    let phone = validate_e164(&body.phone)?;
    let now_ms = unix_ms();

    // Per-phone rate limit: count requests in the last window.
    let window_start = now_ms.saturating_sub(RATE_LIMIT_WINDOW_MS);
    let recent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM otp_request_log
            WHERE phone = $1 AND requested_at_ms > $2",
    )
    .bind(&phone)
    .bind(window_start as i64)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| OtpError::Storage(Box::new(e)))?;
    if recent_count >= RATE_LIMIT_PER_PHONE {
        return Err(OtpError::RateLimited);
    }

    // Generate, hash, store.
    let code = generate_code();
    let code_hash = sha256(&code);
    sqlx::query(
        "INSERT INTO otp_codes (phone, code_hash, expires_at_ms, attempt_count, requested_at_ms)
         VALUES ($1, $2, $3, 0, $4)
         ON CONFLICT (phone) DO UPDATE SET
           code_hash       = EXCLUDED.code_hash,
           expires_at_ms   = EXCLUDED.expires_at_ms,
           attempt_count   = 0,
           requested_at_ms = EXCLUDED.requested_at_ms",
    )
    .bind(&phone)
    .bind(&code_hash[..])
    .bind((now_ms + CODE_TTL_MS) as i64)
    .bind(now_ms as i64)
    .execute(&state.pool)
    .await
    .map_err(|e| OtpError::Storage(Box::new(e)))?;

    sqlx::query(
        "INSERT INTO otp_request_log (phone, requested_at_ms) VALUES ($1, $2)",
    )
    .bind(&phone)
    .bind(now_ms as i64)
    .execute(&state.pool)
    .await
    .ok();    // best-effort logging — don't fail the request

    // Send SMS. If Twilio fails, log + return 503; the user
    // hits "Resend" and we burn one rate-limit slot.
    let message = format!("Your verification code is {code}. Expires in 5 minutes.");
    state
        .twilio
        .send(&phone, &message)
        .await
        .map_err(|err| {
            tracing::error!(
                target: "pocopine.log",
                error = %err,
                "twilio sms send failed"
            );
            OtpError::SmsSend
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn verify_handler(
    State(state): State<Arc<PhoneOtp>>,
    Json(body): Json<VerifyBody>,
) -> Result<Json<Value>, OtpError> {
    let phone = validate_e164(&body.phone)?;
    let submitted = body.code.trim();
    if submitted.len() != 6 || !submitted.chars().all(|c| c.is_ascii_digit()) {
        return Err(OtpError::InvalidCode);
    }
    let now_ms = unix_ms();

    // Single transaction: load row, bump attempt counter, verify
    // the hash, delete on success.
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| OtpError::Storage(Box::new(e)))?;

    let row: Option<(Vec<u8>, i64, i32)> = sqlx::query_as(
        "SELECT code_hash, expires_at_ms, attempt_count FROM otp_codes
            WHERE phone = $1
            FOR UPDATE",
    )
    .bind(&phone)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| OtpError::Storage(Box::new(e)))?;

    let (stored_hash, expires_at_ms, attempts) = match row {
        Some(r) => r,
        None => {
            tx.commit().await.ok();
            return Err(OtpError::InvalidCode);
        }
    };

    if (expires_at_ms as u64) <= now_ms {
        // Expired — delete row, treat as invalid.
        sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
            .bind(&phone)
            .execute(&mut *tx)
            .await
            .ok();
        tx.commit().await.ok();
        return Err(OtpError::InvalidCode);
    }

    if attempts >= MAX_ATTEMPTS {
        // Too many attempts — delete row, force resend.
        sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
            .bind(&phone)
            .execute(&mut *tx)
            .await
            .ok();
        tx.commit().await.ok();
        return Err(OtpError::TooManyAttempts);
    }

    // Bump the attempt counter BEFORE comparing — even an
    // honest user with a typo burns one attempt.
    sqlx::query(
        "UPDATE otp_codes SET attempt_count = attempt_count + 1 WHERE phone = $1",
    )
    .bind(&phone)
    .execute(&mut *tx)
    .await
    .map_err(|e| OtpError::Storage(Box::new(e)))?;

    let submitted_hash = sha256(submitted);
    let matches: bool = stored_hash.ct_eq(&submitted_hash[..]).into();
    if !matches {
        tx.commit().await.ok();
        return Err(OtpError::InvalidCode);
    }

    // Success: delete the OTP row (single-use).
    sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
        .bind(&phone)
        .execute(&mut *tx)
        .await
        .map_err(|e| OtpError::Storage(Box::new(e)))?;

    // Find-or-create the user.
    let user: AppUser = sqlx::query_as::<_, AppUser>(
        "INSERT INTO app_users (id, phone, phone_verified)
         VALUES ($1, $2, TRUE)
         ON CONFLICT (phone) DO UPDATE SET phone_verified = TRUE
         RETURNING *",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| OtpError::Storage(Box::new(e)))?;

    tx.commit()
        .await
        .map_err(|e| OtpError::Storage(Box::new(e)))?;

    // Mint the session token. Same `JwtIssuer` the credentials
    // crate uses — the token shape is identical, so the
    // verifier middleware on the rest of the app accepts both
    // password-issued and OTP-issued tokens.
    let auth_user = user.to_auth_user();
    let extra = json!({
        "phone": user.phone,
        "phone_verified": true,
        "name": auth_user.name,
        "claims": auth_user.claims,
    });
    let token = state
        .issuer
        .sign(&user.id, extra)
        .map_err(|e| OtpError::SessionIssue(e.to_string()))?;

    Ok(Json(json!({
        "token": token,
        "user": auth_user,
    })))
}
```

## The user record

For an OTP-only app:

```rust
#[derive(sqlx::FromRow, Clone)]
pub struct AppUser {
    pub id: String,
    pub phone: String,
    pub phone_verified: bool,
    pub display_name: Option<String>,
}

impl AppUser {
    fn to_auth_user(&self) -> AuthUser {
        let mut user = AuthUser::new(&self.id);
        if let Some(name) = &self.display_name {
            user = user.with_name(name);
        }
        user.with_claim("phone", json!(self.phone))
            .with_claim("phone_verified", json!(self.phone_verified))
    }
}
```

For a **multi-credential** app (phone OTP + email password +
Google OAuth), the same `AppUser` struct also implements
`PasswordCredentials` from the credentials crate; rows have
both `phone` and `password_hash` columns and a user can sign in
either way. The OTP route doesn't care about the password
column; the password route's `find_by_login_id` uses the email
column. Both routes mint the same JWT shape.

## Validation + helpers

```rust
fn validate_e164(raw: &str) -> Result<String, OtpError> {
    if !raw.starts_with('+') {
        return Err(OtpError::InvalidPhone);
    }
    let digits = &raw[1..];
    let len = digits.len();
    if len < 8 || len > 15 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(OtpError::InvalidPhone);
    }
    Ok(raw.to_string())
}

fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen_range(0..1_000_000);
    format!("{n:06}")
}

fn sha256(s: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher.finalize().into()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

`generate_code` uses `rand::thread_rng()` which is seeded from
the OS RNG — fine for OTP codes. Don't substitute a deterministic
RNG.

## Errors

```rust
#[derive(Debug)]
pub enum OtpError {
    InvalidPhone,
    InvalidCode,
    RateLimited,
    TooManyAttempts,
    SmsSend,
    Storage(Box<dyn std::error::Error + Send + Sync + 'static>),
    SessionIssue(String),
}

impl OtpError {
    fn reason(&self) -> &'static str {
        match self {
            Self::InvalidPhone => "invalid_phone",
            Self::InvalidCode => "invalid_code",
            Self::RateLimited => "rate_limited",
            Self::TooManyAttempts => "too_many_attempts",
            Self::SmsSend => "sms_unavailable",
            Self::Storage(_) => "storage_error",
            Self::SessionIssue(_) => "session_issue_error",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::InvalidPhone | Self::InvalidCode => StatusCode::UNPROCESSABLE_ENTITY,
            Self::RateLimited | Self::TooManyAttempts => StatusCode::TOO_MANY_REQUESTS,
            Self::SmsSend => StatusCode::SERVICE_UNAVAILABLE,
            Self::Storage(_) | Self::SessionIssue(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for OtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason())
    }
}

impl std::error::Error for OtpError {}

impl axum::response::IntoResponse for OtpError {
    fn into_response(self) -> axum::response::Response {
        tracing::warn!(
            target: "pocopine.log",
            error = %self,
            reason = self.reason(),
            "otp route rejected"
        );
        let status = self.status();
        let body = Json(json!({ "error": self.reason() }));
        (status, body).into_response()
    }
}
```

The body uses the same closed-set reason shape the credentials
crate does (`{"error": "rate_limited"}`, etc.), so the wasm
client can do one error-display path for both auth surfaces.

## Wiring it up

```rust
use pocopine_auth_jwt::{JwtIssuer, JwtVerifier, SecretBytes};
use pocopine_server::{axum::Router, RouterAuthExt, Server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .expect("postgres pool");
    let secret = SecretBytes::new(
        std::env::var("POCOPINE_AUTH_SECRET")
            .expect("set POCOPINE_AUTH_SECRET to >= 32 random bytes"),
    );

    // The JwtIssuer the OTP routes use to mint session tokens.
    // The exact same value (issuer, audience, secret) drives the
    // verifier on the request middleware below.
    let issuer = JwtIssuer::hs256(secret.clone(), "pocopine", "pocopine");

    // The verifier middleware accepts whatever any first-party
    // route (OTP, credentials, future OAuth) issued.
    let verifier_config = pocopine_auth_jwt::JwtConfig {
        keys: pocopine_auth_jwt::KeySource::Hmac { secret: secret.clone() },
        issuer: Some("pocopine".to_string()),
        audience: Some(vec!["pocopine".to_string()]),
        algorithms: vec![pocopine_auth_jwt::Algorithm::Hs256],
        leeway: std::time::Duration::from_secs(60),
        sources: vec![pocopine_auth_jwt::TokenSource::Bearer],
        revocation: None,
        claim_map: pocopine_auth_jwt::ClaimMap::oidc(),
        required_scopes: vec![],
    };
    let verifier = JwtVerifier::custom(verifier_config).expect("verifier");

    let twilio = TwilioSender::from_env();
    let otp = PhoneOtp::new(pool.clone(), twilio, issuer);

    // App routes you want to protect. `with_auth` reads the
    // Authorization header → JwtVerifier → Principal in extensions
    // → #[server(guard = ...)] sees a real user regardless of
    // whether they signed in via OTP, password, or OAuth.
    let router = my_app::__routes(Router::new()).with_auth(verifier);

    Server::new(router)
        .plugin(otp)
        .serve("0.0.0.0:3000")
        .await
}
```

## Security checklist

| Concern | Owned by |
|---|---|
| Code stored only as SHA-256 hash | **Tutorial** — `code_hash BYTEA`. Even a database leak doesn't expose live codes. |
| Single-use code | **Tutorial** — `DELETE` on success in the verify transaction. Replay protection. |
| Code TTL | **Tutorial** — 5 minutes hard-coded. Tune for your audience. |
| Constant-time comparison | **Tutorial** — `subtle::ConstantTimeEq`. The hashes are 32 bytes so `==` would also be constant-time in practice, but `ct_eq` documents intent. |
| Per-phone rate limit | **Tutorial** — `otp_request_log` sliding window. 5 codes per 15 minutes per phone. |
| Per-IP rate limit | **You** — install `tower-governor` or a CDN-side limiter on `/_pocopine/auth/otp/*`. SMS is expensive and the per-phone limit alone doesn't stop a botnet hitting one phone with thousands of IPs. |
| Attempt limit | **Tutorial** — `attempt_count` bumped per verify. 3 strikes deletes the row, forces resend. |
| Twilio failure visibility | **Tutorial** — `503 sms_unavailable` to the client + structured `tracing::error!` to operators. |
| HTTPS | **You** — `Authorization: Bearer …` is the session credential. |
| Phone-number enumeration | **Tutorial** — invalid phone returns `422 invalid_phone` regardless of whether the number has an account; the `/request` route doesn't reveal whether the phone is registered. |
| Code-not-leaked-via-side-channel | **Tutorial** — the code never appears in the response. Twilio sees the plaintext code (it has to, to deliver the SMS); your server logs only the SHA-256 hash and the failure class. |
| Secret rotation | **You** — `POCOPINE_AUTH_SECRET` rotation requires coordinating with the verifier. Plan it.

## What changes when `pocopine-auth-otp` ships

Likely shape (sketch — not committed):

```rust
use pocopine_auth_otp::{PhoneOtp, OtpStore, SmsSender};

let otp = PhoneOtp::new(secret, store, sms_sender)
    .with_code_ttl(Duration::from_secs(300))
    .with_per_phone_rate_limit(5, Duration::from_secs(900))
    .with_max_attempts(3);

Server::new(router)
    .plugin(otp)
    .serve("0.0.0.0:3000")
    .await
```

What survives the migration without changes:

- The `app_users.phone` column. The official crate's
  `OtpStore::find_by_phone` / `create_by_phone` matches the
  pattern the `Credentials` crate's `UserStore` already uses.
- The session JWT shape. The verifier middleware doesn't care.
- The `AppUser` struct. It implements the new
  `PhoneOtpCredentials` trait alongside any other credential
  types it already implements (`PasswordCredentials`, etc.).
- The wasm client (typed sign-in helper). Same JSON wire shape.

What changes:

- `otp_codes` and `otp_request_log` tables become whatever the
  official crate's `OtpStore` adapter expects. A migration script
  copies any in-flight rows.
- The hand-rolled routes and `OtpError` go away. Replaced by
  `pocopine-auth-otp`'s opinionated routes and error mapping.
- Twilio integration becomes `impl SmsSender for TwilioSender`
  and the official trait's signature; today's `TwilioSender`
  surface is already close.

Plan the migration but don't block on it — what's documented
above is production-shippable. Multiple apps are likely to ship
with this exact pattern before the official crate stabilizes.

## Why we ship this as a tutorial, not a half-finished crate

A bundled OTP crate without a committed `SmsSender` abstraction
is a footgun: apps would build against an interim trait shape,
the trait would change when we figure out the right gateway
abstraction (Twilio vs Messagebird vs AWS SNS vs WhatsApp
Business API have meaningfully different concepts), and every
consumer would need to refactor. Documenting the build-it-yourself
pattern keeps apps unblocked while the trait shape stabilizes
and gives us real-world implementations to learn from before
freezing the public API.

If you build something useful on this pattern, file an issue
with what worked and what didn't — that's how the official
crate's contract gets shaped.

## See also

- [`auth-credentials.md`](./auth-credentials.md) — email + password
  routes from the same primitives, plus the multi-credential
  model that lets phone OTP and password coexist on one user.
- [`auth-client.md`](./auth-client.md) — wasm-side wiring; the
  bearer middleware and `auth_plugin()` are credential-agnostic
  and accept whatever JWT this tutorial mints.
- [`auth-jwt-providers.md`](./auth-jwt-providers.md) — the
  Provider trait that lets you also accept Firebase / Auth0 /
  Clerk tokens through the same verifier middleware.
