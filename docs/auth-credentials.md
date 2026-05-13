# First-party credentials — `pocopine-auth-credentials`

Email + password sign-up / login / logout for pocopine apps —
the Django-shaped path, plain and stable.

The `pocopine-auth-credentials` crate ships:

- `PasswordCredentials` trait — apps implement it on **their own**
  user/account record (Postgres row, custom struct, anything). The
  crate doesn't define a `User` type; it reads `id()`, `email()`,
  `password_hash()`, and a `to_auth_user()` projection off
  whatever the app gives it.
- `UserStore` / `TokenStore` traits — implement against your database.
- `Argon2Params` (OWASP defaults + min-validation) and the
  argon2id wrapper.
- `Credentials<S, T>` builder that mounts
  `/_pocopine/auth/{signup,login,logout}` as a
  [`ServerPlugin`](./server-plugins.md).
- `verifier_config()` — pairs the issuer with
  `JwtVerifier::custom(...)` so the auth middleware on the same
  Router accepts the tokens this builder issues.

What the crate **does not** ship:

- A `User` struct. The framework doesn't own user identity — apps
  do. Many apps need one user table that spans password + OAuth +
  passkey + magic-link auth, with the password hash being one
  column among many. Bundling our own `User` would force a
  parallel record they'd have to keep in sync.
- A default in-memory store. The in-memory shape is a footgun
  (data lost every restart, no shared state across processes),
  and we'd rather you spend ten minutes on the real thing than
  ship a tryout backend you'll forget to swap.
- Phone-OTP / username-based / passkey / OAuth flows. Those are
  **different credential types** and ship as **sibling crates**
  (future `pocopine-auth-otp`, etc.). They produce the same
  session-JWT shape, so the verifier middleware accepts tokens
  from any of them and `Principal` / route guards / `#[server]`
  guards work identically. See [`auth-phone-otp-tutorial.md`](./auth-phone-otp-tutorial.md)
  for the build-it-yourself-today pattern.

This page walks Postgres + [`sqlx`](https://docs.rs/sqlx) end to
end. SQLite, MySQL, Redis, or any other backend works the same way
— implement the same trait + storage contract.

## At a glance

```rust
use pocopine_auth_credentials::Credentials;
use pocopine_auth_jwt::{JwtVerifier, SecretBytes};
use pocopine_server::{axum::Router, RouterAuthExt, Server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let secret = SecretBytes::new(
        std::env::var("POCOPINE_AUTH_SECRET")
            .expect("set POCOPINE_AUTH_SECRET to >= 32 random bytes"),
    );
    let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();

    let store = my_app::PgUserStore { pool: pool.clone() };
    let tokens = my_app::PgTokenStore { pool };
    let creds = Credentials::new(secret, store, tokens);

    let verifier = JwtVerifier::custom(creds.verifier_config()).unwrap();

    Server::new(Router::new())
        .with_auth(verifier)
        .plugin(creds)
        .serve("0.0.0.0:3000")
        .await
}
```

## What's wired

```
client → POST /_pocopine/auth/signup        ─┐
client → POST /_pocopine/auth/login         ─┼─► Credentials handler
client → POST /_pocopine/auth/logout        ─┘     │
                                                   │
                                                   ▼
                                       UserStore::find_by_email
                                       UserStore::create(email, hash) → MyUser
                                       PasswordCredentials::password_hash()
                                       PasswordCredentials::to_auth_user() → AuthUser
                                       argon2id hash / verify
                                       JwtIssuer::sign  →  HS256 session token
                                                   │
                                                   ▼
                                       Returns {token, user: AuthUser}
                                       Frontend calls
                                       AuthSession::sign_in(token, principal)
                                       (BearerMiddleware reads it on subsequent calls)

every other request → axum middleware → JwtVerifier::custom(creds.verifier_config())
                                          → AuthUser → Principal in extensions
                                          → #[server(guard = ...)] sees a real user
```

## Step 1 — define your user type

Whatever your app's `User` row looks like, add a `PasswordCredentials`
impl. The trait has four methods, all reads:

```rust
use pocopine_auth::{AuthUser, Role};
use pocopine_auth_credentials::PasswordCredentials;
use serde_json::json;

pub struct AppUser {
    pub id: String,
    pub email: String,            // for an email-keyed app
    pub email_verified: bool,
    pub password_hash: String,
    pub roles: Vec<String>,
    pub display_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    // ... whatever else lives on your real `users` table.
}

impl PasswordCredentials for AppUser {
    fn id(&self) -> &str {
        &self.id
    }
    fn email(&self) -> &str {
        &self.email
    }
    fn password_hash(&self) -> Option<&str> {
        // `Option<&str>` so the same trait works for users who
        // have **no** password (OAuth-only / passkey-only). See
        // the "Multiple credential types per user" section
        // below. Pure password-only apps just wrap their column
        // in `Some(...)`.
        Some(&self.password_hash)
    }

    /// Project to the `AuthUser` shape the framework hands to
    /// `JwtIssuer` (for the JWT claim set) and to the
    /// signup/login response body.
    fn to_auth_user(&self) -> AuthUser {
        let mut user = AuthUser::new(&self.id).with_email(&self.email);
        if let Some(name) = &self.display_name {
            user = user.with_name(name);
        }
        for role in &self.roles {
            user = user.with_role(Role::named(role));
        }
        // Surface anything else through the `claims` map. The
        // verifier on the server side reads them back via
        // `ClaimMap::oidc()` extended with the paths you care
        // about; on the wasm side they round-trip through
        // `AuthUser::claim("...")`.
        user.with_claim("email_verified", json!(self.email_verified))
    }
}
```

## Multiple credential types per user

Real apps usually link more than one auth method to a single
account: email + password **and** Google OAuth **and** phone OTP,
all → the same `users.id`. Firebase Auth, Auth0, Clerk all model
this as "one user, many identities."

`pocopine-auth-credentials` is **one credential type** in that
mix — specifically the password one. Future sibling crates ship
parallel traits and routes for the others:

| Future crate | Trait | Mints / verifies |
|---|---|---|
| `pocopine-auth-oauth-google` | `GoogleOAuthCredentials` | OAuth2 with Google |
| `pocopine-auth-oauth-firebase` | `FirebaseCredentials` | Firebase ID tokens |
| `pocopine-auth-passkey` | `PasskeyCredentials` | WebAuthn |
| `pocopine-auth-otp` | `PhoneOtpCredentials` | SMS OTP |

Apps that link multiple methods implement multiple traits on the
**same** `AppUser` struct (or composing struct). Their `users`
table grows columns / link tables for each provider:

```sql
CREATE TABLE app_users (
    id                TEXT PRIMARY KEY,
    email             TEXT NOT NULL UNIQUE,
    password_hash     TEXT,                 -- nullable: NULL for OAuth-only users
    google_oauth_sub  TEXT UNIQUE,          -- non-null if Google linked
    firebase_uid      TEXT UNIQUE,          -- non-null if Firebase linked
    -- ... etc
);
```

The `PasswordCredentials::password_hash()` method returns
`Option<&str>` for exactly this reason. A user who signed up via
Google OAuth has `password_hash = NULL` in their row; their
`password_hash()` returns `None`. The login handler treats that
the same as "user not found":

```
login("alice@example.com", "anything")
    ↓
find_by_email("alice@example.com") → Some(user), but password_hash() → None
    ↓
fall through to the dummy hash, run argon2 verify
    ↓
return InvalidCredentials (401)
```

A timing/CPU/shape probe sees the same response regardless of
whether the input fell into "no such user," "OAuth-only account,"
or "wrong password." All three branches hit argon2 against the
dummy hash before returning the closed-set
`invalid_credentials`. Defense against
"is-this-email-a-google-account?" enumeration is implicit in the
trait shape — apps don't have to remember to filter
`password_hash IS NOT NULL` in their SQL; the framework does the
right thing if they just return the row.

When the OAuth / passkey / OTP crates ship, an account-linking
flow will let users add a password to a previously-passwordless
account (and vice versa). That's a separate plugin — out of
scope for the current PR, but the trait shape is ready.

The framework reads `password_hash()` exactly twice per login
(once to verify, once nowhere — the value never escapes); it
reads the others on signup and on login to build the response and
the JWT. Anything you don't surface in `to_auth_user()` simply
isn't visible to the framework — the rest of your row stays your
business.

## Step 2 — implement `UserStore` against Postgres

Schema:

```sql
CREATE TABLE app_users (
    id                TEXT PRIMARY KEY,
    email             TEXT NOT NULL UNIQUE,
    email_verified    BOOLEAN NOT NULL DEFAULT FALSE,
    password_hash     TEXT,                    -- NULL for OAuth-only / passkey-only users
    display_name      TEXT,
    roles             JSONB NOT NULL DEFAULT '[]',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

The framework lowercases `email` before calling the store, so a
plain `UNIQUE` constraint suffices — no `LOWER()`-indexed
expression needed.

The trait — three methods to implement (`find_by_email`,
`find_by_id`, `create`):

```rust
use async_trait::async_trait;
use pocopine_auth_credentials::{StoreError, UserStore};
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgUserStore {
    pub pool: PgPool,
}

#[async_trait]
impl UserStore for PgUserStore {
    type User = AppUser;

    async fn find_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AppUser>, StoreError> {
        sqlx::query_as::<_, AppUser>(
            "SELECT * FROM app_users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Box::new(e) as StoreError)
    }

    async fn find_by_id(&self, id: &str) -> Result<Option<AppUser>, StoreError> {
        sqlx::query_as::<_, AppUser>("SELECT * FROM app_users WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Box::new(e) as StoreError)
    }

    async fn create(
        &self,
        email: &str,
        password_hash: String,
    ) -> Result<AppUser, StoreError> {
        // The store decides id format. UUIDv7 is the recommended
        // default — sortable by creation time, collision-resistant.
        let id = Uuid::now_v7().to_string();
        let user = sqlx::query_as::<_, AppUser>(
            "INSERT INTO app_users (id, email, email_verified, password_hash)
             VALUES ($1, $2, FALSE, $3)
             RETURNING *",
        )
        .bind(&id)
        .bind(email)
        .bind(&password_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Box::new(e) as StoreError)?;
        Ok(user)
    }
}

// You'll typically derive sqlx::FromRow on AppUser, or hand-roll
// a `FromRow` impl that handles the JSONB columns. See sqlx docs.
```

A few non-obvious things to keep in mind:

- **No collation tricks needed.** The framework lowercases the
  email before calling the store, so a plain `UNIQUE` constraint
  is enough.
- **Unique violation on signup.** The crate translates *any* error
  from `create` into `CredentialsError::EmailTaken → 409`. The
  original error is logged via `tracing` at `pocopine.log` so you
  can tell duplicate-email apart from a connection drop in your
  logs even though both surface the same closed-set HTTP reason.
- **Nothing in the error message reaches the wire.** The body is
  `{"error": "email_taken"}` — closed-set identifier — and the
  detail is in the log line. RFC-077 §6 invariant.
- **`to_auth_user` runs once per authenticated request.** The JWT
  carries the projected fields; the verifier on the other side
  rebuilds an `AuthUser` from the claims, so anything you forget
  to project doesn't get to the request handler. Keep this method
  cheap — it's per-request after sign-in too if the session
  cookie path is used.

## Step 3 — implement `TokenStore` against Postgres

The `TokenStore` is for ephemeral password-reset / email-verification
tokens. It's not used by the routes that ship in this slice
(signup/login/logout), but the `Credentials<S, T>` generic carries
both stores so the email-flow PR (PR-3) doesn't churn the API.

```sql
CREATE TABLE auth_tokens (
    token_hash      BYTEA PRIMARY KEY,
    user_id         TEXT NOT NULL,
    kind            TEXT NOT NULL,  -- 'password_reset' | 'email_verification'
    expires_at_ms   BIGINT NOT NULL
);
CREATE INDEX auth_tokens_expires_idx ON auth_tokens (expires_at_ms);
```

```rust
use async_trait::async_trait;
use pocopine_auth_credentials::{StoreError, TokenKind, TokenRecord, TokenStore};
use sqlx::PgPool;

pub struct PgTokenStore {
    pub pool: PgPool,
}

#[async_trait]
impl TokenStore for PgTokenStore {
    async fn put(&self, token_hash: [u8; 32], record: TokenRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO auth_tokens (token_hash, user_id, kind, expires_at_ms)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (token_hash) DO UPDATE
               SET user_id = EXCLUDED.user_id,
                   kind = EXCLUDED.kind,
                   expires_at_ms = EXCLUDED.expires_at_ms",
        )
        .bind(&token_hash[..])
        .bind(&record.user_id)
        .bind(kind_to_str(record.kind))
        .bind(record.expires_at_ms as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| Box::new(e) as StoreError)?;
        Ok(())
    }

    async fn take(
        &self,
        token_hash: [u8; 32],
        now_ms: u64,
    ) -> Result<Option<TokenRecord>, StoreError> {
        // Single-use semantics: DELETE ... RETURNING is atomic so
        // a replay against the same token hash returns nothing
        // even under concurrent calls.
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "DELETE FROM auth_tokens
              WHERE token_hash = $1
              RETURNING user_id, kind, expires_at_ms",
        )
        .bind(&token_hash[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Box::new(e) as StoreError)?;

        let Some((user_id, kind_str, exp_ms)) = row else {
            return Ok(None);
        };
        if (exp_ms as u64) <= now_ms {
            return Ok(None);
        }
        Ok(Some(TokenRecord::new(
            user_id,
            kind_from_str(&kind_str)?,
            exp_ms as u64,
        )))
    }

    async fn purge_expired(&self, now_ms: u64) -> Result<usize, StoreError> {
        let result = sqlx::query("DELETE FROM auth_tokens WHERE expires_at_ms <= $1")
            .bind(now_ms as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(result.rows_affected() as usize)
    }
}

fn kind_to_str(k: TokenKind) -> &'static str {
    match k {
        TokenKind::PasswordReset => "password_reset",
        TokenKind::EmailVerification => "email_verification",
    }
}

fn kind_from_str(s: &str) -> Result<TokenKind, StoreError> {
    match s {
        "password_reset" => Ok(TokenKind::PasswordReset),
        "email_verification" => Ok(TokenKind::EmailVerification),
        other => Err(format!("unknown token kind: {other}").into()),
    }
}
```

The single-use contract is non-negotiable: implementations
**must** remove-then-return atomically. With Postgres, `DELETE ...
RETURNING` is the idiomatic shape; with Redis, `WATCH/MULTI/EXEC`
or a Lua script. Don't read-then-delete in two round trips — that's
the replay vector RFC-074 §6.1 calls out by name.

## Step 4 — wire it up

```rust
use pocopine_auth_credentials::Credentials;
use pocopine_auth_jwt::{JwtVerifier, SecretBytes};
use pocopine_server::{axum::Router, RouterAuthExt, Server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    pocopine_logging::init();

    let secret = SecretBytes::new(
        std::env::var("POCOPINE_AUTH_SECRET")
            .expect("set POCOPINE_AUTH_SECRET to >= 32 random bytes"),
    );
    let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .expect("postgres pool");

    let creds = Credentials::new(
        secret,
        my_app::PgUserStore { pool: pool.clone() },
        my_app::PgTokenStore { pool },
    );

    let verifier =
        JwtVerifier::custom(creds.verifier_config()).expect("verifier from credentials config");

    // Important: install the verifier middleware on the router
    // BEFORE installing the credentials plugin, otherwise the
    // signup/login routes added by the plugin would be wrapped in
    // the auth middleware and reject anonymous requests.
    Server::new(Router::new())
        .with_auth(verifier)
        .plugin(creds)
        .serve("0.0.0.0:3000")
        .await
}
```

## Builder options

`Credentials::new(secret, store, tokens)` takes the only three
required values; everything else is a builder method:

| Method | Default | Purpose |
|---|---|---|
| `.with_session_ttl(Duration)` | 1 hour | Session JWT lifetime. |
| `.with_argon_params(Argon2Params)` | OWASP m=64MiB / t=3 / p=4 | Argon2id cost. Validated against `owasp_minimum()` (m=19MiB / t=2 / p=1). |
| `.with_issuer(name)` | `"pocopine"` | `iss` claim. |
| `.with_audience(name)` | `"pocopine"` | `aud` claim. |
| `.with_password_validator(closure)` | min 8 chars | NIST SP 800-63B-style checks, HIBP, anything you want. |

The user-id scheme is the **store's** decision — `UserStore::create`
returns the constructed user, so the store picks the format
(UUIDv7, snowflake, ULID, sequential, …) that matches its database.

The `.verifier_config()` method always returns a fresh `JwtConfig`
matching the current builder state. Calling it twice is fine — it
doesn't lock builder state.

## Security model — what's enforced and what isn't

| Concern | Owned by |
|---|---|
| Plaintext-password storage | **Framework** — only the argon2id PHC string ever touches the store; the framework never logs or serializes it. |
| User-enumeration via response timing | **Framework** — constant-time login (always runs `verify_password` against a pre-baked dummy hash on miss). |
| User-enumeration via response shape | **You (PR-3)** — the email-flow `/password/reset/request` route returns `200 {}` regardless of whether the address matches. |
| Token replay | **Framework** — reset/verify tokens are stored as their SHA-256 hash, single-use via `take`. |
| `password_hash` leak via logs | **Framework** — the response body builds an `AuthUser` projection that excludes the hash. **You** make sure your `AppUser`'s `Debug` impl redacts the hash too. |
| Argon2 misuse | **Framework** — `Argon2Params::validate` rejects below OWASP minimum at builder time. |
| Session-token forgery | **Framework** — HS256 with the same `SecretBytes` on both sides; algorithm is pinned in `verifier_config`. |
| CSRF on `/login` etc. | **You** — install your CSRF middleware on the router. |
| Rate limiting | **You** — install `tower-governor` or a CDN-side limiter on `/_pocopine/auth/*`. |
| Account lockout after N failed logins | **You** — track via your own metrics store and short-circuit `login` ahead of the credentials handler. |

Per RFC-074 §6 these are firm boundaries. Production checklist:

1. `POCOPINE_AUTH_SECRET` set to ≥ 32 random bytes; rotated only in
   coordination with verifier rollout.
2. Rate limits on `/_pocopine/auth/login` and (when PR-3 lands)
   `/password/reset/request`, `/email/verify/request`.
3. HTTPS — full stop. The bearer token in `Authorization` is the
   session credential.
4. Session TTL ≤ 1 hour without a revocation hook (`JwtConfig::revocation`
   is the seam if you need denylists).
5. Treat `with_argon_params` overrides as security-critical config;
   never lower below `Argon2Params::owasp_minimum()`.
6. Custom `Debug` on your user record that redacts `password_hash`.
   The framework does this for the types it owns; your row type is
   yours to harden.

## Phone OTP without waiting for the official crate

The future `pocopine-auth-otp` crate will ship phone OTP as a
`ServerPlugin` alongside this credentials plugin. Until it lands,
the `auth-phone-otp-tutorial.md` walks through the full DIY
pattern using the same primitives this crate uses (Twilio +
Postgres + `JwtIssuer`), with the same JWT shape so your
verifier middleware accepts both password-issued and OTP-issued
tokens. Apps that need phone OTP today don't need to wait.

## Pairs with `pocopine-auth-client`

The wasm-side companion (`pocopine-auth-client`) is what calls the
routes you just mounted, stores the returned token, and propagates
it as `Authorization: Bearer …` on subsequent `#[server]` calls.
See [`auth-client.md`](./auth-client.md) for the full client-side
walkthrough — you'll usually wire both sides in the same change.

## Out of scope (deferred, per RFC-074 PR sequence)

- Email flows: `EmailSender` trait, `/password/reset/{request,confirm}`,
  `/email/verify/{request,confirm}` ship in PR-3. PR-3 will add
  `update_password_hash` and `set_email_verified` methods to
  `UserStore` (default-no-op for apps that don't need them).
- HIBP / breach checks ship in `pocopine-auth-credentials-hibp`.
- A `/me` route is under consideration for PR-4 (apps frequently
  add their own).
- Bundled Postgres / SQLite / Redis adapter crates. Open to
  contributions — the test fixture in
  `crates/pocopine-auth-credentials/tests/common/mod.rs` shows the
  exact trait shape an adapter would implement.
