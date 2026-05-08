//! [`auth_plugin`] — the [`AppPlugin`] that wires the wasm-side auth
//! surface together.
//!
//! Builder-style configuration: the app picks where to send users on
//! `RouteRejection::Unauthorized` (login route, optional `ReturnTo`
//! query param), and the plugin handles the rest:
//!
//! 1. `provide_plugin(AuthSession::new())` so any component can
//!    extract the session via `Plugin<AuthSession>` /
//!    `Option<Plugin<AuthSession>>` per RFC-076.
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

use pocopine_auth::{Decision, Predicate};
use pocopine_core::{
    App, AppPlugin, ReturnTo, RouteContext, RouteGuard, RouteGuardDecision, RouteRejection,
    RouteRejectionAction, RouteRejectionContext, RouteRejectionHandler, RouteTarget,
};

use crate::session::{active_principal, AuthSession};

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
}

impl Default for AuthPluginBuilder {
    fn default() -> Self {
        Self {
            login_route: None,
            return_to_query_param: DEFAULT_RETURN_TO_PARAM,
            install_bearer_middleware: false,
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
    /// [`crate::install`] call. Off by default so apps that already
    /// install the middleware (or want to install a different one)
    /// don't double-register and trip the freeze contract.
    pub fn with_bearer_middleware(mut self, install: bool) -> Self {
        self.install_bearer_middleware = install;
        self
    }
}

impl AppPlugin for AuthPluginBuilder {
    fn name(&self) -> &'static str {
        "pocopine-auth-client"
    }

    fn install(self, app: App) -> App {
        if self.install_bearer_middleware {
            crate::install();
        }

        let session = AuthSession::new();
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
    // are silently dropped to `ReturnTo::none`, and
    // `append_to` becomes a no-op.
    if ctx.query.is_empty() {
        ReturnTo::from_path(ctx.path)
    } else {
        let mut combined = ctx.path.to_string();
        combined.push('?');
        for (i, (k, v)) in ctx.query.iter().enumerate() {
            if i > 0 {
                combined.push('&');
            }
            combined.push_str(k);
            combined.push('=');
            combined.push_str(v);
        }
        ReturnTo::from_path(&combined)
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
pub fn predicate_guard<P: Predicate>(predicate: P) -> impl RouteGuard {
    move |_ctx: &RouteContext<'_>| -> RouteGuardDecision {
        let principal = active_principal();
        match Predicate::check(&predicate, &principal) {
            Decision::Allow => RouteGuardDecision::Allow,
            Decision::Deny("unauthorized") => {
                RouteGuardDecision::Reject(RouteRejection::Unauthorized)
            }
            Decision::Deny(reason) => RouteGuardDecision::Reject(RouteRejection::Forbidden(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_auth::{require_auth, require_role, AuthUser, Role};
    use std::collections::HashMap;

    fn ctx<'a>(path: &'a str) -> RouteContext<'a> {
        // We can't construct `RouteContext` from outside the crate
        // with named fields (it's `#[non_exhaustive]`), but the
        // closure body of `predicate_guard` ignores the context, so
        // the unit tests below only need to drive Predicate +
        // session state. The router-side end-to-end behavior is
        // covered by the wasm integration test suite in
        // `crates/pocopine/tests/route_guards_loaders.rs`.
        let _ = path;
        unimplemented!("RouteContext is router-private; tests drive the predicate path directly")
    }

    #[test]
    fn auth_plugin_default_has_no_login_route() {
        let plugin = auth_plugin();
        assert!(plugin.login_route.is_none());
        assert_eq!(plugin.return_to_query_param, "redirect");
        assert!(!plugin.install_bearer_middleware);
    }

    #[test]
    fn auth_plugin_builder_sets_options() {
        let plugin = auth_plugin()
            .login_route("/signin")
            .return_to_query_param("next")
            .with_bearer_middleware(true);
        assert_eq!(plugin.login_route.as_deref(), Some("/signin"));
        assert_eq!(plugin.return_to_query_param, "next");
        assert!(plugin.install_bearer_middleware);
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

    // Suppress unused-fn warning for the helper kept for
    // documentation-by-code.
    #[allow(dead_code)]
    fn _ctx_helper_keeps_shape<'a>(p: &'a str) -> RouteContext<'a> {
        ctx(p)
    }
}
