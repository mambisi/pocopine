//! [`auth_plugin`] — the [`AppPlugin`] that wires the wasm-side auth
//! surface together.
//!
//! Builder-style configuration: the app picks where to send users on
//! `RouteRejection::Unauthorized` (login route, optional `ReturnTo`
//! query param), and the plugin handles the rest:
//!
//! 1. `provide_plugin(AuthSession::new())` so any component can
//!    extract the session via `Plugin<AuthSession>` /
//!    `Option<Plugin<AuthSession>>` per RFC-076. Apps with an async
//!    client-side provider can opt into a pending initial session with
//!    [`AuthPluginBuilder::wait_for_initial_auth_check`].
//! 2. `route_rejection_handler` mapping `RouteRejection::Unauthorized`
//!    to a redirect to the configured login route, optionally
//!    appending the validated [`ReturnTo`] under a configurable query
//!    param.
//! 3. Optionally installs the [`crate::BearerMiddleware`] so the app
//!    doesn't have to call [`crate::install`] separately.
//!
//! Predicate-as-route-guard ergonomics ship through
//! [`predicate_guard`] (orphan rule prevents a blanket
//! `impl<P: Predicate> RouteGuard for P` from this crate; the helper
//! function is the next-best ergonomic shape).

use std::rc::Rc;

use pocopine_auth::{Decision, Predicate, Principal};
use pocopine_core::{
    App, AppPlugin, ReturnTo, RouteContext, RouteGuard, RouteGuardDecision, RouteRejection,
    RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler, RouteTarget,
};

use crate::refresh::{set_refresh, TokenRefresh};
use crate::session::{active_principal, AuthSession};
use crate::storage::{
    current_session_snapshot_storage, install_session_snapshot_storage, install_storage,
    SessionSnapshotStorage, TokenStorage,
};

/// Default query parameter name used for the post-login redirect
/// intent. Configurable per-app via
/// [`AuthPluginBuilder::return_to_query_param`].
pub const DEFAULT_RETURN_TO_PARAM: &str = "redirect";

/// Auth plugin entry point. The returned builder is itself an
/// [`AppPlugin`]; chain configuration on it then call `App::plugin`:
///
/// ```ignore
/// use pocopine::App;
/// use pocopine_auth_client::auth_plugin;
///
/// App::new()
///     .plugin(
///         auth_plugin()
///             .login_route("/login")
///             .return_to_query_param("redirect")
///             .with_bearer_middleware(true),
///     )
///     .run();
/// ```
pub fn auth_plugin() -> AuthPluginBuilder {
    AuthPluginBuilder::default()
}

/// Configuration for the auth plugin. See [`auth_plugin`].
pub struct AuthPluginBuilder {
    login_route: Option<String>,
    return_to_query_param: &'static str,
    install_bearer_middleware: bool,
    wait_for_initial_auth_check: bool,
    token_refresh: Option<Rc<dyn TokenRefresh>>,
    token_storage: Option<Rc<dyn TokenStorage>>,
    session_snapshot_storage: Option<Rc<dyn SessionSnapshotStorage>>,
    cross_tab_sync: bool,
}

impl Default for AuthPluginBuilder {
    fn default() -> Self {
        Self {
            login_route: None,
            return_to_query_param: DEFAULT_RETURN_TO_PARAM,
            install_bearer_middleware: false,
            wait_for_initial_auth_check: false,
            token_refresh: None,
            token_storage: None,
            session_snapshot_storage: None,
            cross_tab_sync: false,
        }
    }
}

impl AuthPluginBuilder {
    /// Path the rejection handler redirects to on
    /// [`RouteRejection::Unauthorized`]. Without a login route, the
    /// rejection falls through to the next handler — typically the
    /// app's [`pocopine_core::App::route_error_component`]. Path is
    /// validated by [`RouteTarget::new`]; invalid values fall through
    /// to a `RouteTarget::path` panic at install time so config
    /// errors fail loudly.
    pub fn login_route(mut self, path: impl Into<String>) -> Self {
        self.login_route = Some(path.into());
        self
    }

    /// Query parameter name to use when appending the captured
    /// [`ReturnTo`] intent to the login URL. Default: `"redirect"`.
    pub fn return_to_query_param(mut self, name: &'static str) -> Self {
        self.return_to_query_param = name;
        self
    }

    /// Install the [`crate::BearerMiddleware`] on the fetch chain
    /// from inside the plugin — saves the app from a separate
    /// [`crate::install`] call. Off by default so apps that don't
    /// authenticate outgoing `#[server]` calls don't pay for the
    /// middleware.
    ///
    /// Safe to mix with calling [`crate::install`] directly: both
    /// call paths are idempotent. The chain ends up with exactly one
    /// `BearerMiddleware` regardless of how many install paths fired.
    pub fn with_bearer_middleware(mut self, install: bool) -> Self {
        self.install_bearer_middleware = install;
        self
    }

    /// Start the provided [`AuthSession`] in a pending state so
    /// `predicate_guard(...)` pauses route navigation until the app's
    /// browser-side auth provider has checked persisted state at least
    /// once.
    ///
    /// Use this for client-only providers such as Firebase where the
    /// provider has its own local session store. Once the provider
    /// finishes its first check, call [`AuthSession::sign_in`],
    /// [`AuthSession::sign_out`], [`AuthSession::set_principal`], or
    /// [`AuthSession::mark_ready`]. Any of those wakes the router and
    /// re-runs the paused guard.
    ///
    /// Off by default so apps without an async client provider do not
    /// accidentally leave guarded routes pending forever.
    pub fn wait_for_initial_auth_check(mut self, wait: bool) -> Self {
        self.wait_for_initial_auth_check = wait;
        self
    }

    /// Configure how the bearer middleware obtains a fresh token when
    /// an outgoing replay-safe `#[server]` call returns
    /// `Unauthorized`. The closure is invoked at most once per
    /// originating request; on success the middleware calls
    /// [`crate::set_token`] with the result and replays the request
    /// once. On failure the original `Unauthorized` propagates.
    ///
    /// Without a configured refresh the middleware fails closed:
    /// `Unauthorized` propagates directly. This honors RFC-078
    /// §5.10.4's fail-closed default for the no-`#[server(idempotent)]`
    /// and no-refresh cases.
    ///
    /// ```ignore
    /// auth_plugin()
    ///     .with_bearer_middleware(true)
    ///     .with_token_refresh(|| async {
    ///         my_app::refresh_session_token().await
    ///     })
    /// ```
    ///
    /// Concurrent in-flight requests that 401 may each trigger an
    /// independent refresh — single-flight coalescing is a follow-up,
    /// see [`crate::refresh`] module docs.
    pub fn with_token_refresh<R: TokenRefresh>(mut self, refresh: R) -> Self {
        self.token_refresh = Some(Rc::new(refresh));
        self
    }

    /// Persist the bearer token through the supplied [`TokenStorage`]
    /// implementation. Without this, the token slot is in-memory only
    /// and a page reload signs the user out.
    ///
    /// At plugin install time the token slot is hydrated from the
    /// storage's [`TokenStorage::load`] so a returning user picks up
    /// where they left off. Subsequent [`crate::set_token`] /
    /// [`crate::clear_token`] calls write through automatically.
    ///
    /// ```ignore
    /// auth_plugin()
    ///     .with_token_storage(
    ///         pocopine_auth_client::storage::LocalStorage::new("auth_token"),
    ///     )
    /// ```
    ///
    /// **Security tradeoff.** Persisting access tokens in
    /// JavaScript-readable storage means an XSS vulnerability can
    /// steal them. For high-value applications, prefer an `httpOnly`
    /// cookie issued by the server (the bearer middleware then doesn't
    /// need a token slot at all). See [`crate::storage`] for the full
    /// rundown.
    pub fn with_token_storage<S: TokenStorage>(mut self, storage: S) -> Self {
        self.token_storage = Some(Rc::new(storage));
        self
    }

    /// Persist an optimistic identity snapshot for instant reload
    /// continuity. A restored snapshot lets guarded route shells render
    /// while an async provider/server check confirms the real session.
    ///
    /// This does not make the snapshot authoritative. Server functions
    /// and provider checks still own authorization.
    pub fn with_session_snapshot<S: SessionSnapshotStorage>(mut self, storage: S) -> Self {
        self.session_snapshot_storage = Some(Rc::new(storage));
        self
    }

    /// Sync sign-in / sign-out across tabs of the same origin via a
    /// `BroadcastChannel`. Off by default.
    ///
    /// When enabled, calls to [`AuthSession::set_principal`],
    /// [`AuthSession::sign_in`], [`AuthSession::sign_out`] (and any
    /// app code that goes through `set_principal`) post a message to
    /// the channel. Other tabs of the same origin running this app
    /// receive it and:
    /// 1. Re-load the token from the configured [`TokenStorage`].
    /// 2. Bump the local [`AuthSession`]'s epoch so the bearer
    ///    middleware fences any in-flight responses captured under
    ///    the previous identity.
    ///
    /// **Requires [`with_token_storage`](Self::with_token_storage)** for
    /// the peer tab to read the new credential — otherwise the peer
    /// just bumps its epoch with no way to learn the new token.
    /// Combine the two for end-to-end coordination.
    ///
    /// See [`crate::cross_tab`] for the full description and security
    /// considerations.
    pub fn with_cross_tab_sync(mut self, enabled: bool) -> Self {
        self.cross_tab_sync = enabled;
        self
    }
}

impl AppPlugin for AuthPluginBuilder {
    fn name(&self) -> &'static str {
        "pocopine-auth-client"
    }

    fn install(self, app: App) -> App {
        if self.cross_tab_sync && self.token_storage.is_none() {
            panic!(
                "auth_plugin: with_cross_tab_sync(true) requires \
                 with_token_storage(...) — peer tabs read the new \
                 credential from shared storage. Without storage, \
                 they bump epoch but keep using the stale token."
            );
        }

        // Hydrate before middleware so the first outgoing request
        // carries a persisted credential.
        if let Some(storage) = self.token_storage {
            install_storage(storage);
            crate::hydrate_from_storage();
        }
        if let Some(storage) = self.session_snapshot_storage {
            install_session_snapshot_storage(storage);
        }
        if self.install_bearer_middleware {
            crate::install();
        }
        if let Some(refresh) = self.token_refresh {
            set_refresh(refresh);
        }

        let session = match current_session_snapshot_storage().and_then(|storage| {
            storage
                .load_snapshot()
                .filter(|snapshot| snapshot.principal.is_authenticated())
        }) {
            Some(snapshot) => AuthSession::restoring(snapshot),
            None if self.wait_for_initial_auth_check || crate::active_token().is_some() => {
                AuthSession::pending()
            }
            None => AuthSession::new(),
        };
        if self.cross_tab_sync {
            crate::cross_tab::install(session.clone());
        }
        let login_route = self.login_route;
        let param = self.return_to_query_param;

        app.provide_plugin(session)
            .route_rejection_handler(UnauthorizedRedirect { login_route, param })
    }
}

/// Map [`RouteRejection::Unauthorized`] to a redirect to the
/// configured login route, with optional `ReturnTo` intent
/// appended under [`AuthPluginBuilder::return_to_query_param`].
struct UnauthorizedRedirect {
    login_route: Option<String>,
    param: &'static str,
}

impl RouteRejectionHandler for UnauthorizedRedirect {
    fn handle(
        &self,
        ctx: &RouteRejectionContext<'_>,
        rejection: &RouteRejection,
    ) -> Option<RouteRejectionAction> {
        if !matches!(rejection, RouteRejection::Unauthorized) {
            return None;
        }
        let path = self.login_route.as_deref()?;
        let target = RouteTarget::new(path).ok()?;
        let intent = build_return_to(ctx);
        let final_target = intent.append_to(target, self.param);
        Some(RouteRejectionAction::Redirect(final_target))
    }
}

fn build_return_to(ctx: &RouteRejectionContext<'_>) -> ReturnTo {
    // The router-supplied `ctx.path` is the just-canceled
    // navigation URL (no scheme/host). Feed it through
    // [`ReturnTo::from_path`] so the path-only validation in
    // RFC-078 §5.10.2 runs: protocol-relative, javascript:, etc.
    // are silently dropped to `ReturnTo::none`.
    if ctx.query.is_empty() {
        return ReturnTo::from_path(ctx.path);
    }

    // The router decoded query params into a `HashMap<String,
    // String>`, so we can't recover the original wire ordering or
    // character-for-character representation. We MUST re-encode
    // reserved characters when reconstructing the query string;
    // a value that arrived as `q=a%26b` (percent-encoded `&`)
    // decodes to `q=a&b` and would otherwise reconstruct as two
    // params on the way out. We also sort keys so HashMap
    // iteration order doesn't make the same input produce
    // different return-to URLs.
    let mut combined = ctx.path.to_string();
    combined.push('?');
    let mut params: Vec<(&String, &String)> = ctx.query.iter().collect();
    params.sort_by(|a, b| a.0.cmp(b.0));
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            combined.push('&');
        }
        percent_encode_into(&mut combined, k);
        combined.push('=');
        percent_encode_into(&mut combined, v);
    }
    ReturnTo::from_path(&combined)
}

/// Percent-encode `s` into `out` per RFC 3986 "unreserved" set
/// (`A-Z` / `a-z` / `0-9` / `-` / `.` / `_` / `~`). Any other byte
/// becomes `%XX`. Conservative on purpose: covers the
/// `application/x-www-form-urlencoded` reserved characters (`&`,
/// `=`, `+`, `?`, `#`, …) plus the rest of the URI reserved set.
fn percent_encode_into(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
}

/// Adapt a [`Predicate`] into a [`RouteGuard`].
///
/// Rust's orphan rule prevents a blanket
/// `impl<P: Predicate> RouteGuard for P` shipping from this crate
/// (both traits are foreign). This helper threads the predicate
/// through a closure that reads the active client-side
/// [`AuthSession`] and maps [`Decision`] onto
/// [`RouteGuardDecision`]:
///
/// - `Decision::Allow` → `RouteGuardDecision::Allow`.
/// - `Decision::Deny("unauthorized")` (the standard predicates'
///   not-logged-in branch) → `RouteGuardDecision::Reject(RouteRejection::Unauthorized)`,
///   which the auth plugin's handler then redirects to the login
///   route.
/// - Any other `Decision::Deny(reason)` →
///   `RouteGuardDecision::Reject(RouteRejection::Forbidden(reason))`.
///
/// ```ignore
/// use pocopine_auth::{require_auth, require_role};
/// use pocopine_auth_client::predicate_guard;
///
/// impl RouteComponent for Dashboard {
///     fn config() -> RouteConfig<Self> {
///         RouteConfig::new()
///             .guard(predicate_guard(require_auth()))
///     }
/// }
///
/// impl RouteComponent for AdminPanel {
///     fn config() -> RouteConfig<Self> {
///         RouteConfig::new()
///             .guard(predicate_guard(require_role("admin")))
///     }
/// }
/// ```
///
/// The guard is **UX-only** per RFC-078 §5.10.1 — the security
/// boundary is the server-side `#[server(guard = …)]` check. The
/// same predicate value plugs into both install points, so a
/// route-guard miss never leaves a server function unprotected.
///
/// **Route-aware guards.** This adapter ignores `RouteContext` because
/// `Predicate::check` is a function of `Principal` only. Guards that
/// need to inspect `path` / `params` / `query` (for example,
/// `/users/:id` where the principal must equal `params["id"]`) cannot
/// be expressed through `predicate_guard`; write the closure directly:
///
/// ```ignore
/// .guard(|ctx: &RouteContext<'_>| -> RouteGuardDecision {
///     let principal = pocopine_auth_client::active_principal();
///     match (principal.user(), ctx.params.get("id")) {
///         (Some(u), Some(id)) if u.id == *id => RouteGuardDecision::Allow,
///         _ => RouteGuardDecision::Reject(RouteRejection::Forbidden("not_owner")),
///     }
/// })
/// ```
pub fn predicate_guard<P: Predicate>(predicate: P) -> impl RouteGuard {
    move |_ctx: &RouteContext<'_>| -> RouteGuardDecision {
        let principal = match crate::active_session() {
            Some(session) if !session.is_ready() && !session.is_restoring() => {
                return RouteGuardDecision::Pending;
            }
            Some(session) => session.principal(),
            None => active_principal(),
        };
        predicate_decision(&predicate, &principal)
    }
}

fn predicate_decision<P: Predicate>(predicate: &P, principal: &Principal) -> RouteGuardDecision {
    match Predicate::check(predicate, principal) {
        Decision::Allow => RouteGuardDecision::Allow,
        Decision::Deny("unauthorized") => RouteGuardDecision::Reject(RouteRejection::Unauthorized),
        Decision::Deny(reason) => RouteGuardDecision::Reject(RouteRejection::Forbidden(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_auth::{require_auth, require_role, AuthUser, Role};
    use std::collections::HashMap;

    fn with_ctx<R>(path: &str, f: impl FnOnce(&RouteContext<'_>) -> R) -> R {
        let params = HashMap::new();
        let query = HashMap::new();
        let ctx = RouteContext {
            path,
            params: &params,
            query: &query,
            matched_pattern: Some("/test"),
        };
        f(&ctx)
    }

    #[test]
    fn auth_plugin_default_has_no_login_route() {
        let plugin = auth_plugin();
        assert!(plugin.login_route.is_none());
        assert_eq!(plugin.return_to_query_param, "redirect");
        assert!(!plugin.install_bearer_middleware);
        assert!(!plugin.wait_for_initial_auth_check);
        assert!(plugin.token_refresh.is_none());
        assert!(plugin.session_snapshot_storage.is_none());
    }

    #[test]
    fn auth_plugin_builder_sets_options() {
        let plugin = auth_plugin()
            .login_route("/signin")
            .return_to_query_param("next")
            .with_bearer_middleware(true)
            .wait_for_initial_auth_check(true)
            .with_session_snapshot(crate::storage::InMemory)
            .with_token_refresh(|| async { Ok("fresh".to_string()) });
        assert_eq!(plugin.login_route.as_deref(), Some("/signin"));
        assert_eq!(plugin.return_to_query_param, "next");
        assert!(plugin.install_bearer_middleware);
        assert!(plugin.wait_for_initial_auth_check);
        assert!(plugin.session_snapshot_storage.is_some());
        assert!(plugin.token_refresh.is_some());
    }

    #[test]
    #[should_panic(expected = "with_cross_tab_sync(true) requires with_token_storage")]
    fn cross_tab_without_storage_panics_at_install() {
        // P2: without storage, cross-tab broadcast can't tell peer
        // tabs what the new token is. Fail at install time.
        use pocopine_core::App;
        let plugin = auth_plugin().with_cross_tab_sync(true);
        let _ = AppPlugin::install(plugin, App::new());
    }

    #[test]
    fn cross_tab_with_storage_installs_cleanly() {
        use pocopine_core::App;
        // Stand-up storage (using a test impl) and cross-tab; install
        // should not panic. We can't observe the cross-tab channel on
        // host (it's wasm-only), but the install path itself is safe.
        crate::storage::__reset_storage_for_test();
        let plugin = auth_plugin()
            .with_token_storage(crate::storage::TestStorage::default())
            .with_cross_tab_sync(true);
        // Install consumes self; we just need to confirm no panic.
        let _ = AppPlugin::install(plugin, App::new());
        crate::storage::__reset_storage_for_test();
    }

    #[test]
    fn unauthorized_redirect_returns_none_without_login_route() {
        let handler = UnauthorizedRedirect {
            login_route: None,
            param: "redirect",
        };
        let path = "/dashboard";
        let params = HashMap::new();
        let query = HashMap::new();
        let ctx = RouteRejectionContext {
            path,
            params: &params,
            query: &query,
            matched_pattern: Some("/dashboard"),
        };
        assert!(handler
            .handle(&ctx, &RouteRejection::Unauthorized)
            .is_none());
    }

    #[test]
    fn unauthorized_redirect_appends_return_to_when_configured() {
        let handler = UnauthorizedRedirect {
            login_route: Some("/login".to_string()),
            param: "redirect",
        };
        let path = "/dashboard";
        let params = HashMap::new();
        let query = HashMap::new();
        let ctx = RouteRejectionContext {
            path,
            params: &params,
            query: &query,
            matched_pattern: Some("/dashboard"),
        };
        let action = handler
            .handle(&ctx, &RouteRejection::Unauthorized)
            .expect("redirect produced");
        match action {
            RouteRejectionAction::Redirect(target) => {
                let s = target.as_str();
                assert!(s.starts_with("/login"));
                assert!(s.contains("redirect="));
                // Encoded path: `/dashboard` URL-encoded.
                assert!(s.contains("%2Fdashboard") || s.contains("/dashboard"));
            }
            other => panic!("expected redirect, got {other:?}"),
        }
    }

    #[test]
    fn unauthorized_redirect_re_encodes_reserved_chars_in_query() {
        // Codex review MEDIUM: the router decodes query into a
        // HashMap, so a value that arrived as `q=a%26b` becomes
        // `q=a&b` — reconstructing without re-encoding would
        // corrupt the return-intent into two params. We re-encode
        // when reconstructing the intent's query, then `append_to`
        // re-encodes again because the whole intent travels as the
        // value of the `redirect=` query param. So `a&b` →
        // (once) `a%26b` → (twice) `a%2526b` in the final URL.
        let handler = UnauthorizedRedirect {
            login_route: Some("/login".to_string()),
            param: "redirect",
        };
        let path = "/search";
        let params = HashMap::new();
        let mut query = HashMap::new();
        query.insert("q".to_string(), "a&b".to_string());
        query.insert("k=v".to_string(), "x#y".to_string());
        let ctx = RouteRejectionContext {
            path,
            params: &params,
            query: &query,
            matched_pattern: Some("/search"),
        };
        let action = handler
            .handle(&ctx, &RouteRejection::Unauthorized)
            .expect("redirect produced");
        let target = match action {
            RouteRejectionAction::Redirect(t) => t,
            other => panic!("expected redirect, got {other:?}"),
        };
        let s = target.as_str();
        // Double-encoded reserved characters: the intent string
        // is `q=a%26b&k%3Dv=x%23y` after our re-encoding (one
        // encode); `append_to` URI-encodes the whole string for
        // safe use as a query value (twice). `%26` becomes
        // `%2526`, `%3D` becomes `%253D`, `%23` becomes `%2523`.
        assert!(
            s.contains("a%2526b"),
            "expected double-encoded ampersand (%2526) in {s}"
        );
        assert!(
            s.contains("k%253Dv"),
            "expected double-encoded equals (%253D) in {s}"
        );
        assert!(
            s.contains("x%2523y"),
            "expected double-encoded hash (%2523) in {s}"
        );
    }

    #[test]
    fn unauthorized_redirect_query_param_order_is_deterministic() {
        // Two queries with the same content inserted in different
        // orders must produce the same return-to URL. HashMap
        // iteration is non-deterministic; we sort by key.
        let handler = UnauthorizedRedirect {
            login_route: Some("/login".to_string()),
            param: "redirect",
        };
        let path = "/search";
        let params = HashMap::new();

        let mut q1 = HashMap::new();
        q1.insert("a".to_string(), "1".to_string());
        q1.insert("z".to_string(), "9".to_string());

        let mut q2 = HashMap::new();
        q2.insert("z".to_string(), "9".to_string());
        q2.insert("a".to_string(), "1".to_string());

        let mk = |q: &HashMap<String, String>| {
            let ctx = RouteRejectionContext {
                path,
                params: &params,
                query: q,
                matched_pattern: Some("/search"),
            };
            match handler.handle(&ctx, &RouteRejection::Unauthorized).unwrap() {
                RouteRejectionAction::Redirect(t) => t.as_str().to_string(),
                _ => panic!(),
            }
        };
        assert_eq!(mk(&q1), mk(&q2));
    }

    #[test]
    fn unauthorized_redirect_passes_through_non_unauthorized() {
        let handler = UnauthorizedRedirect {
            login_route: Some("/login".to_string()),
            param: "redirect",
        };
        let path = "/dashboard";
        let params = HashMap::new();
        let query = HashMap::new();
        let ctx = RouteRejectionContext {
            path,
            params: &params,
            query: &query,
            matched_pattern: Some("/dashboard"),
        };
        assert!(handler
            .handle(&ctx, &RouteRejection::Forbidden("admins_only"))
            .is_none());
        assert!(handler.handle(&ctx, &RouteRejection::NotFound).is_none());
    }

    #[test]
    fn predicate_check_decisions() {
        // Drive the Predicate path directly; the route-guard
        // wrapper is just a Decision→RouteGuardDecision map. The
        // wasm-side integration test exercises the full path.
        let auth = require_auth();
        let admin = require_role("admin");

        let anon = pocopine_auth::Principal::anonymous();
        assert_eq!(auth.check(&anon), Decision::Deny("unauthorized"));
        assert_eq!(admin.check(&anon), Decision::Deny("unauthorized"));

        let user =
            pocopine_auth::Principal::from_user(AuthUser::new("u1").with_role(Role::admin()));
        assert_eq!(auth.check(&user), Decision::Allow);
        assert_eq!(admin.check(&user), Decision::Allow);

        let no_role = pocopine_auth::Principal::from_user(AuthUser::new("u1"));
        assert_eq!(admin.check(&no_role), Decision::Deny("forbidden"));
    }

    #[test]
    fn predicate_guard_waits_for_pending_session() {
        let session = AuthSession::pending();
        crate::session::__set_test_session(Some(session.clone()));

        let guard = predicate_guard(require_auth());
        assert_eq!(
            with_ctx("/dashboard", |ctx| guard.decide(ctx)),
            RouteGuardDecision::Pending
        );

        session.sign_in(
            "token",
            pocopine_auth::Principal::from_user(AuthUser::new("u1")),
        );
        assert_eq!(
            with_ctx("/dashboard", |ctx| guard.decide(ctx)),
            RouteGuardDecision::Allow
        );

        crate::session::__set_test_session(None);
    }

    #[test]
    fn predicate_guard_allows_restoring_session_snapshot() {
        let snapshot = crate::AuthSessionSnapshot::new(pocopine_auth::Principal::from_user(
            AuthUser::new("u1"),
        ))
        .unwrap();
        let session = AuthSession::restoring(snapshot);
        crate::session::__set_test_session(Some(session));

        let guard = predicate_guard(require_auth());
        assert_eq!(
            with_ctx("/dashboard", |ctx| guard.decide(ctx)),
            RouteGuardDecision::Allow
        );

        crate::session::__set_test_session(None);
    }
}
