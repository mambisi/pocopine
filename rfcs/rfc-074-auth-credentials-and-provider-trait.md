# RFC 074 - `pocopine-auth-credentials` and the `Provider` trait

| Field | Value |
|---|---|
| **Status** | Accepted (PR-1 Provider trait + PR-2 credentials data plane — signup/login/logout — landed; PR-3 email/reset flows & PR-4 example adoption pending) |
| **Author** | pocopine team |
| **Created** | 2026-05-04 |
| **Related** | [`rfc-066-server-function-auth.md`](./rfc-066-server-function-auth.md), [`rfc-069-observability.md`](./rfc-069-observability.md), [`rfc-070-jwt-auth-verification.md`](./rfc-070-jwt-auth-verification.md) |
| **Supersedes** | RFC-070 §5.10 ("`pocopine-auth-credentials` builds on top, separate RFC") |

## 1. Summary

Two pieces, shipped together because they share an audience (the
vibe-coder building a first app):

1. **A built-in email + password flow** in a new
   `pocopine-auth-credentials` crate. The Django-shaped path: drop
   `Credentials::new(secret, MemoryUserStore::default())` into your
   server bin, get signup/login/logout/password-reset/email-verify
   for free, paired with `JwtIssuer::hs256` for first-party
   session tokens.

2. **An extensible `Provider` trait** in `pocopine-auth-jwt` so
   apps, tutorials, or third-party crates can ship a typed config
   struct that materializes a `JwtConfig`. Pocopine keeps the
   verifier engine vendor-neutral; provider examples can live in
   docs/tutorials without becoming maintained framework crates.

```rust
// First-party email + password — no third-party signup needed.
let secret = SecretBytes::from_env("POCOPINE_AUTH_SECRET")?;
let auth = Credentials::new(secret, MemoryUserStore::default())
    .with_email_sender(my_email_sender);

let router = Router::new();
let router = my_app::__server_routes(router);
let router = auth.install_routes(router);
let router = router.with_auth(JwtVerifier::custom(auth.verifier_config())?);

// Or: external identity provider via the Provider trait.
let verifier = JwtVerifier::from_provider(Firebase::new("my-project"))?;
let router = router.with_auth(verifier);
```

## 2. Motivation

RFC-070 shipped the JWT verifier engine and (deliberately) skipped
provider presets. Two gaps remain that block the "vibe-coder
builds an app from zero" UX:

- **No first-party way to get users.** Today an app must already
  have Firebase / Clerk / a custom IdP just to get a logged-in
  user. The Django-equivalent first-app path is "sign up with
  email and password," and pocopine has no story for it.
- **No extension shape for third-party providers.** RFC-070 §5.9's
  preset functions were dropped in PR #34 because they were
  aspirational without integration tests. The replacement —
  third-party crates publishing typed configs — needs a small
  trait so the integration is consistent, IDE-discoverable, and
  swappable.

## 3. Goals

- A drop-in email + password provider that compiles in <1 minute,
  works with an in-memory store for local dev, and swaps to a
  durable store via one line.
- A `Provider` trait that lets apps, tutorials, or third-party
  crates ship typed config structs (`Firebase`, `Okta`, `Cognito`,
  …) and integrate with `JwtVerifier::from_provider` without a PR
  to pocopine.
- Argon2id with OWASP-aligned defaults for the credentials path.
  No password-cleartext-on-disk surface anywhere.
- Constant-time login (no user-enumeration timing oracle).
- Email verification + password reset flows, both with hashed
  ephemeral tokens.
- Provider examples should stay in docs/tutorials or external
  crates; the framework should not maintain vendor-specific JWT
  preset crates by default.

## 4. Non-goals

- **Not an OAuth client.** Pocopine doesn't do redirect flows /
  code exchange / refresh handling; apps that want Google /
  GitHub / etc. without Firebase use the existing `oauth2` Rust
  crate and feed the resulting ID token to `JwtVerifier`.
- **Not a UI library.** Sign-in / sign-up component templates are
  RFC-075 (separate slice). This RFC only covers the data plane.
- **Not a multi-tenant user system.** `User.id` is opaque; tenant
  scoping (per-org users, per-workspace roles) is app-level
  concern.
- **Not a session-replay store.** Tokens are stateless JWTs by
  default; revocation (if needed) goes through RFC-070's
  `RevocationCheck` hook.
- **Not a CSRF defense.** Host middleware's job
  (`tower_http::cors`, `SameSite=Lax` cookies, custom
  `Content-Type`).
- **Not a rate-limiter.** Same — host middleware. We document
  recommended rate limits per-route but don't ship them.

## 5. Design

### 5.1 Crate layout

```
crates/pocopine-auth-credentials/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── user.rs          // User struct + Role/Permission projections
    ├── store.rs         // UserStore trait + MemoryUserStore
    ├── tokens.rs        // TokenStore trait + MemoryTokenStore
    ├── email.rs         // EmailSender trait + EmailError
    ├── argon.rs         // Argon2id wrapper + parameter knobs
    ├── credentials.rs   // Credentials<S, T> builder
    ├── routes.rs        // install_routes + the seven endpoints
    └── error.rs         // CredentialsError + From impls
```

`pocopine-auth-jwt` gains:

```
crates/pocopine-auth-jwt/src/
├── ...
└── provider.rs          // Provider trait + Firebase struct (example)
```

### 5.2 The `Provider` trait

Lives in `pocopine-auth-jwt`:

```rust
/// Provider-specific config that knows how to materialize itself
/// into a verifier-ready [`JwtConfig`]. Third-party crates
/// implement this for their identity provider; users pass the
/// typed config to [`JwtVerifier::from_provider`].
///
/// The trait consumes `self` because providers are one-shot
/// descriptions — after `jwt_config()`, the owned strings
/// (project_id, domain, …) move into the resulting config.
pub trait Provider {
    /// Build the verifier config. May fail if the provider
    /// performs construction-time checks (e.g. parsing a static
    /// JWKS document). Most return `Ok(...)` directly.
    fn jwt_config(self) -> Result<JwtConfig, JwtAuthError>;
}

impl JwtVerifier {
    /// Construct from any [`Provider`] implementation.
    pub fn from_provider<P: Provider>(provider: P) -> Result<Self, JwtAuthError> {
        Self::custom(provider.jwt_config()?)
    }
}
```

### 5.3 Example tutorial provider — `Firebase`

```rust
/// Firebase identity verifier configuration.
///
/// Drop in your Firebase Console project ID. Tokens are accepted
/// from the `Authorization: Bearer <id-token>` header by default;
/// set `session_cookie = true` to also accept the SSR `__session`
/// cookie (note: cookies use a separate JWKS endpoint and are
/// gated behind a deliberate opt-in).
#[non_exhaustive]
pub struct Firebase {
    pub project_id: String,
    pub session_cookie: bool,
    pub cache_ttl: Duration,
    pub refresh_cooldown: Duration,
    pub leeway: Duration,
}

impl Firebase {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            session_cookie: false,
            cache_ttl: Duration::from_secs(3600),
            refresh_cooldown: Duration::from_secs(30),
            leeway: Duration::from_secs(60),
        }
    }
}

impl Provider for Firebase {
    fn jwt_config(self) -> Result<JwtConfig, JwtAuthError> {
        let mut sources = vec![TokenSource::Bearer];
        if self.session_cookie {
            sources.push(TokenSource::Cookie(Cow::Borrowed("__session")));
        }
        Ok(JwtConfig {
            keys: KeySource::Jwks {
                url:
                    "https://www.googleapis.com/service_accounts/\
                     v1/jwk/securetoken@system.gserviceaccount.com"
                        .into(),
                cache_ttl: self.cache_ttl,
                refresh_cooldown: self.refresh_cooldown,
            },
            issuer: Some(format!(
                "https://securetoken.google.com/{}",
                self.project_id
            )),
            audience: Some(vec![self.project_id]),
            algorithms: vec![Algorithm::Rs256],
            leeway: self.leeway,
            sources,
            revocation: None,
            claim_map: ClaimMap::oidc(),
            required_scopes: vec![],
        })
    }
}
```

### 5.3.1 Integration test contract for providers

**Every published reusable provider should ship with a recorded-token
integration test.** No exceptions for third-party crates or copied
tutorial providers. The test:

1. Generates a test RSA keypair.
2. Synthesizes a JWKS document containing the test public key.
3. Builds a `JwtVerifier` from the provider's config but
   substitutes `KeySource::StaticJwks { document: synthetic_jwks }`
   and the test issuer/audience.
4. Issues a token with the provider's expected claim shape using
   the test private key.
5. Verifies the token round-trips and projects to the correct
   `AuthUser`.

This keeps the provider's claim-map / audience / issuer / source
configuration honest without any network calls. The CI suite runs
in <1 second per provider.

### 5.3.2 Third-party providers

Naming convention: `pocopine-auth-jwt-<vendor>`. Each crate
exports a config struct + `Provider` impl. The README shows a
one-line install:

```rust
let verifier = JwtVerifier::from_provider(
    pocopine_auth_jwt_okta::Okta::new("example.okta.com", "my-api"),
)?;
```

`docs/auth-jwt-providers.md` (new, this RFC) maintains a
community-maintained list of provider crates. Adding a row to
that table is the same drive-by PR third-party authors already do
for `serde-*` adapters.

### 5.4 The `User` record

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub email_verified: bool,
    /// Argon2id PHC string. Never logged.
    pub password_hash: String,
    pub roles: Vec<Role>,
    pub permissions: Vec<Permission>,
    /// Created at, Unix milliseconds.
    pub created_at_ms: u64,
    /// Updated at, Unix milliseconds.
    pub updated_at_ms: u64,
    /// App-defined metadata. Pocopine doesn't read these fields;
    /// use them for display name, avatar URL, tenant id, etc.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}
```

`User.id` is generated by the framework on signup
(`format!("{ms:x}-{uuid_v7}")` — sortable by creation time, no
collision risk). Apps that want their own id strategy can
override via `Credentials::with_id_generator(...)`.

### 5.5 The `UserStore` trait

```rust
/// Storage backend for credential users. Apps pick:
/// `MemoryUserStore` for dev, a third-party crate's
/// `RedisUserStore` / `PostgresUserStore` / etc. for production.
///
/// The associated `Error` lets storage-specific errors flow
/// through (Redis connection drop, Postgres unique-violation)
/// while still implementing `Into<AuthError>` for the auth
/// contract. Use `impl Future` returns (RPITIT) so call sites
/// don't pay for boxed-future allocation per call.
pub trait UserStore: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + Into<CredentialsError> + 'static;

    fn find_by_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<User>, Self::Error>> + Send;

    fn find_by_id(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<User>, Self::Error>> + Send;

    fn create(
        &self,
        user: User,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn update(
        &self,
        user: User,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn delete(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
```

In-tree implementations:

- **`MemoryUserStore`** — `Arc<RwLock<HashMap<String, User>>>` plus
  an email index. Tryout default; data lost on restart.
- (Future) `RedisUserStore` ships in `pocopine-auth-credentials-redis`
  to reuse the Redis already in the workspace via
  `pocopine-jobs`. Out of scope for this RFC's first slice.

### 5.6 The `TokenStore` trait

Separate from `UserStore` because ephemeral tokens (password
reset, email verification) have different semantics — TTL,
single-use, not tied to user identity at storage time.

```rust
/// Ephemeral, hashed token store. Used for password-reset and
/// email-verification flows. Tokens are stored as their hash —
/// even if the storage is compromised, in-flight tokens can't be
/// reused.
pub trait TokenStore: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + Into<CredentialsError> + 'static;

    fn put(
        &self,
        token_hash: &[u8; 32],
        record: TokenRecord,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn take(
        &self,
        token_hash: &[u8; 32],
    ) -> impl Future<Output = Result<Option<TokenRecord>, Self::Error>> + Send;

    fn purge_expired(
        &self,
        now_ms: u64,
    ) -> impl Future<Output = Result<usize, Self::Error>> + Send;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenRecord {
    pub user_id: String,
    pub kind: TokenKind,
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    PasswordReset,
    EmailVerification,
}
```

`take` is single-use semantics: the implementor must remove the
record before returning it (or atomically mark consumed) so a
replay attack on the token can't re-confirm.

In-tree:
- **`MemoryTokenStore`** — `Arc<RwLock<HashMap<[u8; 32], TokenRecord>>>`
  with a periodic purge task (or lazy purge inside `take`).

### 5.7 The `EmailSender` trait

Apps implement this against their email vendor (SendGrid,
Postmark, AWS SES, Resend, …). The framework only generates the
verification/reset tokens and the message text; delivery is the
app's concern.

```rust
pub trait EmailSender: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn send(
        &self,
        to: &str,
        subject: &str,
        body: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
```

Apps that don't ship an email sender skip
`Credentials::with_email_sender`; the password-reset and
email-verification routes return `503 Service Unavailable` (or
similar) so unconfigured flows fail loudly instead of silently
no-oping.

### 5.8 The `Credentials<S, T>` builder

```rust
pub struct Credentials<S: UserStore, T: TokenStore> {
    secret: SecretBytes,
    store: S,
    tokens: T,
    email: Option<Arc<dyn DynEmailSender>>,  // type-erased so the
                                              // generic surface stays
                                              // small for the common
                                              // path
    issuer: String,
    audience: String,
    session_ttl: Duration,
    reset_ttl: Duration,
    verify_ttl: Duration,
    argon: Argon2Params,
    id_generator: Arc<dyn Fn() -> String + Send + Sync>,
}

impl<S: UserStore, T: TokenStore> Credentials<S, T> {
    pub fn new(secret: SecretBytes, store: S, tokens: T) -> Self;

    pub fn with_email_sender<E: EmailSender>(self, sender: E) -> Self;
    pub fn with_session_ttl(self, ttl: Duration) -> Self;
    pub fn with_argon_params(self, params: Argon2Params) -> Self;
    pub fn with_issuer(self, name: impl Into<String>) -> Self;
    pub fn with_audience(self, name: impl Into<String>) -> Self;
    pub fn with_id_generator(self, f: impl Fn() -> String + Send + Sync + 'static) -> Self;

    /// Verifier config that pairs with this credential issuer.
    /// App passes this to `JwtVerifier::custom(...)` then to
    /// `RouterAuthExt::with_auth`.
    pub fn verifier_config(&self) -> JwtConfig;

    /// Install the credential routes on an axum Router.
    pub fn install_routes(&self, router: Router) -> Router;
}
```

Convenience constructor for the simplest "tryout" case:

```rust
impl Credentials<MemoryUserStore, MemoryTokenStore> {
    /// All-in-memory tryout: dev-grade defaults, no persistence.
    /// Generates the secret from `POCOPINE_AUTH_SECRET` env or
    /// (if unset) a per-process ephemeral secret with a warning
    /// banner via `tracing` so vibe-coders see something works
    /// out of the box.
    pub fn tryout() -> Self;
}
```

### 5.9 The seven routes

`install_routes` mounts these under `/_pocopine/auth/`:

| Method | Path | Body | Returns | Notes |
|---|---|---|---|---|
| POST | `/signup` | `{email, password}` | `{token, user}` | 8-char min, configurable |
| POST | `/login` | `{email, password}` | `{token, user}` | Constant-time on user-not-found |
| POST | `/logout` | — | `204 No Content` | No-op for stateless tokens; clears `pocopine_session` cookie if set |
| POST | `/password/reset/request` | `{email}` | `{}` | Always succeeds (no user enumeration); sends email if registered |
| POST | `/password/reset/confirm` | `{token, new_password}` | `{}` | Single-use token, hashed lookup |
| POST | `/email/verify/request` | — (auth required) | `{}` | Sends verification email |
| POST | `/email/verify/confirm` | `{token}` | `{}` | Single-use token, sets `email_verified = true` |

Optional eighth route:

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/me` | — (auth required) | `{user}` |

`/me` is useful for client-side state hydration — the wasm app
calls it on bootstrap to learn who the cookie/bearer token
identifies. Off by default; opt in via `with_me_route(true)`.

### 5.10 Constant-time login

```rust
async fn login(state: State<...>, Json(req): Json<LoginRequest>) -> Result<...> {
    let user = state.store.find_by_email(&req.email).await?;

    // Always perform argon2 verify, even if the user doesn't
    // exist, against a known-bad hash. This makes the timing of
    // the negative path indistinguishable from a wrong-password
    // hit on a real user, defeating user-enumeration timing
    // attacks.
    let dummy = "$argon2id$v=19$m=65536,t=3,p=4$YWJjZGVmZ2hpag$..."; // pre-baked
    let (hash, real) = match user {
        Some(u) => (u.password_hash.clone(), Some(u)),
        None => (dummy.to_string(), None),
    };

    let ok = argon2_verify(&hash, &req.password)?;
    if !ok || real.is_none() {
        return Err(CredentialsError::InvalidCredentials);
    }
    /* ...issue session token... */
}
```

The "dummy" hash is generated once at builder construction so
even the dummy verify cost matches the configured argon params.

### 5.11 Password complexity

Default: minimum 8 characters, no other restrictions.

`Credentials::with_password_validator(impl Fn(&str) -> Result<(), &str>)`
lets apps inject their own (NIST SP 800-63B-aligned, breach-list
checks via HIBP, …). The framework deliberately doesn't ship
"must contain a number / special / uppercase" — those rules
inflate breach risk by encouraging weak-but-compliant patterns.
Length is the only universal default.

### 5.12 Session token shape

Issued via `JwtIssuer::hs256(secret, issuer, audience)`.
Verified via `JwtVerifier::custom(...)` paired through
`Credentials::verifier_config()`.

Default claim set:

```json
{
    "sub": "01J5T6...",
    "iss": "pocopine",
    "aud": "pocopine",
    "iat": 1714750000,
    "exp": 1714753600,
    "email": "alice@example.com",
    "email_verified": true,
    "roles": ["admin"]
}
```

`exp - iat` defaults to 1 hour. Apps that want different lifetimes
override via `with_session_ttl`. Refresh is the app's job (call
`/login` again or implement an app-level refresh route);
pocopine doesn't issue refresh tokens out of the box.

### 5.13 Re-export shape

```rust
// pocopine umbrella, host-only:
pub mod auth {
    pub use pocopine_auth::*;
    pub mod client { pub use pocopine_core::auth_client::*; }

    #[cfg(not(target_arch = "wasm32"))]
    pub mod jwt {
        pub use pocopine_auth_jwt::*;
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub mod credentials {
        pub use pocopine_auth_credentials::*;
    }
}
```

Both crates are host-only — wasm bundles never carry argon2 or
the verifier engine.

## 6. Security

### 6.1 Threats addressed by construction

- **Plaintext passwords on disk.** The `User.password_hash` field
  is the only storage path; signup/login take `password: String`,
  hash it, then drop. No code path stores plaintext.
- **User enumeration via response timing.** Constant-time login
  (always run argon2 verify, even on non-existent users — §5.10).
- **User enumeration via response shape.** Password-reset request
  always returns `{}` regardless of whether the email matched.
- **Token replay.** Reset/verify tokens are stored hashed; even a
  full database compromise doesn't yield reusable tokens. Tokens
  are single-use (`take` semantics on `TokenStore`).
- **Token theft post-leak.** Default 1-hour reset/verify TTL;
  defenders can shorten further via `with_reset_ttl` /
  `with_verify_ttl`.
- **Argon2 misuse.** OWASP-aligned defaults (m=64MiB, t=3, p=4).
  Apps that opt into custom params via `with_argon_params` must
  pass an `Argon2Params` struct that validates min memory cost.
- **Session token forgery.** `JwtIssuer::hs256` + `JwtVerifier::custom`
  pinned to HS256 with the same secret (RFC-070 §5.8 algorithm
  pinning applies).

### 6.2 Threats deferred

- **CSRF on credential routes.** Host middleware concern; the
  doc page recommends `tower_http::cors` + `SameSite=Lax` on any
  cookie-shaped session, plus the existing custom `Content-Type`
  requirement on `/_pocopine/*` routes.
- **Rate limiting.** Host middleware. Doc recommends e.g. 5
  req/min per IP on `/login` and `/password/reset/request`.
- **Account lockout after N failed logins.** App concern; the
  framework exposes a hook (`with_failed_login_observer`) so apps
  can implement IP-based or account-based lockout against their
  own metric store.
- **Breached-password checks (HIBP-k-anon API).** Out of scope; can
  ship as `pocopine-auth-credentials-hibp` later.
- **Email-injection via the user's email field.** `EmailSender::send`
  takes `to: &str` directly; the framework doesn't sanitize.
  Implementations that hit SMTP directly (vs. a SaaS API) must
  reject CRLF in addresses themselves. Doc note required.

### 6.3 Operator obligations

Documented in the credentials module rustdoc and
`docs/auth-credentials.md`:

1. Set `POCOPINE_AUTH_SECRET` in production (≥ 32 random bytes).
   `Credentials::tryout` warns at startup if the secret is
   ephemeral.
2. Configure rate limiting on `/login`, `/password/reset/request`,
   `/email/verify/request`.
3. Use HTTPS in production.
4. Pin `session_ttl` short enough for your security posture (≤ 1
   hour without revocation; longer if revocation is configured
   via RFC-070's `RevocationCheck`).
5. Treat Argon2 parameter overrides as security-critical config;
   never lower them below `Argon2Params::owasp_minimum()`.

## 7. Migration

This RFC is additive. No breaking changes to existing crates:

- `pocopine-auth-jwt` gains a new `Provider` trait. No existing
  API changes and no in-tree vendor provider crate.
- `pocopine-auth-credentials` is a new crate. Apps opt in.
- The `pocopine` umbrella gains
  `pocopine::auth::credentials` (host-only) and
  `pocopine::auth::jwt::Provider` (host-only).

Recommended ship sequence (each its own PR):

1. **PR-1: `Provider` trait + provider docs.** Add the trait and
   document the app/tutorial pattern for Firebase-style providers.
   ~1 day.
2. **PR-2: `pocopine-auth-credentials` core (no email).**
   `User`, `UserStore`, `TokenStore`, `MemoryUserStore`,
   `MemoryTokenStore`, `Credentials::new` /
   `verifier_config`, signup + login + logout routes. argon2
   wiring, constant-time login. ~1500 LOC + tests. ~1 week.
3. **PR-3: Email flows.** `EmailSender` trait,
   password-reset + email-verification routes. ~600 LOC + tests.
   ~3 days.
4. **PR-4: Examples + docs.**
   `examples/blog` adopts the credentials crate;
   `docs/auth-credentials.md` walks through tryout → durable
   store → email sender;
   `docs/auth-jwt-providers.md` documents the `Provider`
   convention and lists in-tree + community providers. ~3 days.

## 8. Open questions

1. **Should `MemoryTokenStore` be the same struct as
   `MemoryUserStore` (single in-memory backend)** or two
   independent ones? The two-trait split is cleaner but for the
   tryout case it's six lines of boilerplate to construct both.
   **Recommend:** keep them separate at the trait layer, but ship
   a `MemoryStore` convenience wrapper that implements both with
   shared backing state.

2. **`Credentials::tryout()` ephemeral secret.** Should the
   ephemeral case generate a per-process random secret (so tokens
   are valid until restart) or refuse to start without
   `POCOPINE_AUTH_SECRET`? Ergonomic argument for ephemeral with
   loud warnings; security argument for fail-closed.
   **Recommend:** ephemeral with a `tracing::warn!` banner naming
   the env var. `cargo run` for vibe-coders should "just work."

3. **Where do `roles` and `permissions` come from on signup?**
   Currently the User record carries them but signup doesn't set
   them. Apps that need an "admin signs up first" pattern call a
   custom server function that creates a user with `Role::admin()`.
   Should `install_routes` accept a closure
   `signup_role_assigner: Fn(&User) -> Vec<Role>` so the first
   signup can be elevated?
   **Recommend:** ship an `assign_roles_on_create` builder hook
   that takes `Fn(&User) -> (Vec<Role>, Vec<Permission>)` and
   defaults to empty vectors. Apps wire whatever logic they want.

4. **Refresh tokens.** RFC-070 explicitly skipped them. For the
   credentials path, should we ship an opt-in refresh-token flow
   to avoid forcing apps to redo `/login` every hour?
   **Recommend:** defer to RFC-076. The single-shot session token
   is sufficient for vibe-coders; production apps with tighter
   security posture can layer their own refresh on top via the
   `RevocationCheck` hook.

5. **Should the `Provider` trait be `async`?** Some hypothetical
   providers might want to fetch dynamic config (e.g. tenant
   discovery) at construction. Current design is sync. If we ever
   need async, the migration is `fn jwt_config(self) -> impl
   Future<...>` which is a breaking change.
   **Recommend:** keep it sync. The `JwksResolver` already
   handles runtime fetches; provider construction shouldn't
   block on network.

6. **Naming: `Credentials` vs `EmailPasswordAuth` vs
   `LocalAuth`.** `Credentials` is shortest and matches the crate
   name. Other names (`PasswordAuth`, `LocalIdentity`) are more
   specific but longer.
   **Recommend:** `Credentials`.
