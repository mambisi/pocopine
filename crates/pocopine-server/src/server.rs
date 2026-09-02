//! `Server` — the host-side builder that wires plugin services and
//! lifecycle hooks around an axum [`Router`].
//!
//! Mirror of [`pocopine_core::App`] for server code: plugins receive
//! the in-progress builder and return it after installing tower
//! layers, services, hooks, and any extra routes they need (health
//! endpoints, metrics, etc.).
//!
//! ```no_run
//! use pocopine_server::{axum::Router, static_files, Server};
//!
//! # mod my_app {}
//! // Link the app crate once so its `#[server]` inventory entries are present.
//! use my_app as _;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let router = Router::new().fallback_service(static_files("pkg"));
//!     Server::new(router).serve("0.0.0.0:3000").await
//! }
//! ```
//!
//! Plain [`crate::serve`] still works as a one-liner wrapper.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use pocopine_auth::AuthProvider;
use pocopine_observe::{TRACE_TARGET, fields, spans};
use tower::Layer;
use tower::Service;
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::plugin::{
    self, PluginRegistry, PluginValidationError, ServerBootFailed, ServerBootStarted, ServerHook,
    ServerListening,
};
use crate::{
    ServerFunctionRoute, ServerFunctionRouteConflict, SharedAuthProvider, auth_middleware,
    install_server_functions,
};

/// App-level extension point on the host side.
///
/// Plugins receive the in-progress [`Server`] builder and return it
/// after installing tower layers, plugin services, lifecycle hooks,
/// or extra routes (health, metrics, etc.). This keeps optional
/// integrations out of `pocopine-server`: a separate crate can
/// expose a plugin value, and applications opt into it from their
/// `main`.
pub trait ServerPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, server: Server) -> Server;
}

impl<F> ServerPlugin for F
where
    F: FnOnce(Server) -> Server,
{
    fn install(self, server: Server) -> Server {
        self(server)
    }
}

/// Host-side builder. Wraps an axum [`Router`] until `serve` time.
///
/// **One active `Server` per process.** Plugin services and hooks
/// live in a process-global registry (see
/// [`crate::plugin`]). Calling [`Self::serve`] (or
/// [`Self::try_finalize`]) overwrites whatever registry was
/// previously installed. If a second `Server` finalizes while a
/// first is still serving, the first's `active_plugin::<T>()`
/// lookups and `hook_plugin` dispatches start resolving against the
/// second's registry — services it never provided will panic on
/// dispatch, hooks it registered go silent.
///
/// Real apps run a single HTTP listener per process and don't hit
/// this. Test harnesses that build multiple `Server`s in one
/// process must use [`crate::__reset_for_test`] between activations.
/// Multi-tenant patterns that genuinely need concurrent independent
/// plugin sets should compose into one `Server` (one router, one
/// plugin chain) rather than running two builders.
pub struct Server {
    router: Router,
    plugins: PluginRegistry,
    installing_plugin: Option<&'static str>,
    auth_providers: Vec<SharedAuthProvider>,
    server_function_conflicts: Vec<ServerFunctionRouteConflict>,
}

impl Server {
    /// Wrap `router` in a builder. Plugin work composes on top; the
    /// router itself remains the user's source of truth for route
    /// composition, fallback services, and `with_state` calls.
    ///
    /// All linked `#[server]` functions are installed before plugin
    /// work starts, so later [`Self::layer`] calls wrap those routes.
    ///
    /// **Linking requirement:** `#[server]` routes are collected via
    /// `inventory`, which only sees crates the binary actually
    /// **links**. A `[[bin]]` that never references the lib crate
    /// defining its server functions links none of them — every
    /// server-fn POST then falls through to the static fallback as a
    /// 405. Make the binary reference the defining crate (e.g.
    /// `use my_app::…;` or call its setup fn); a prominent startup
    /// warning is logged when zero routes register.
    ///
    /// When `POCOPINE_ASSETS_BUCKET` is set, the RFC-100 Mode B asset
    /// proxy (`GET /assets/<hash>/<path>`, serving the private bucket)
    /// is installed automatically — zero code in the app's `main`; see
    /// the env contract on [`crate::ASSETS_BUCKET_ENV`].
    pub fn new(router: Router) -> Self {
        let (router, server_function_conflicts) = install_server_functions(router);
        // 0 routes is almost always a linking mistake, not an app without
        // server functions: `inventory` only collects from crates the binary
        // links, so a `[[bin]]` that never references its lib crate links
        // zero `#[server]` fns — and every POST 405s with no hint why.
        if inventory::iter::<ServerFunctionRoute>
            .into_iter()
            .next()
            .is_none()
        {
            tracing::warn!(
                target: "pocopine.log",
                "0 server functions registered — if this app defines #[server] \
                 functions, the binary probably does not reference the crate that \
                 defines them (routes are collected from linked crates only); \
                 every server-fn POST will fall through to the static fallback \
                 as a 405",
            );
        }
        let router = crate::assets::install_assets_proxy(router);
        Self {
            router,
            plugins: PluginRegistry::default(),
            installing_plugin: None,
            auth_providers: Vec::new(),
            server_function_conflicts,
        }
    }

    /// Install a server-level plugin. The plugin runs while the
    /// builder is still being assembled, before plugin validation
    /// and listener bind.
    pub fn plugin<P: ServerPlugin>(mut self, plugin: P) -> Self {
        let name = plugin.name();
        let previous = self.installing_plugin.replace(name);
        let mut server = plugin.install(self);
        server.installing_plugin = previous;
        server
    }

    /// Provide a typed runtime service to other plugins and to
    /// hook closures.
    ///
    /// Services are stored as `Arc<T>` so concurrent request
    /// handlers and event hooks can each hold a clone without
    /// coordinating. Duplicate provides for the same `T` panic and
    /// name both providers in the diagnostic.
    pub fn provide_plugin<T: Send + Sync + 'static>(mut self, service: T) -> Self {
        self.plugins.provide(service, self.installing_plugin);
        self
    }

    /// Install an [`AuthProvider`] as request middleware.
    ///
    /// This is the preferred auth entry point for normal apps. The
    /// provider is recorded on the builder and applied during
    /// [`Self::try_finalize`], after plugins have had a chance to add
    /// routes. That means routes installed by later plugins are still
    /// wrapped, and guarded handlers can read authenticated principals
    /// from `RequestContext` regardless of builder-call order.
    pub fn with_auth<P: AuthProvider + 'static>(self, provider: P) -> Self {
        self.with_auth_arc(Arc::new(provider))
    }

    /// Install a pre-`Arc`'d auth provider.
    pub fn with_auth_arc(mut self, provider: SharedAuthProvider) -> Self {
        self.auth_providers.push(provider);
        self
    }

    /// Dispatch framework event `E` to the installed plugin
    /// service `T`.
    ///
    /// `T` must have been provided with [`Self::provide_plugin`] and
    /// must implement [`crate::ServerHook<E>`]. If the service is
    /// not installed by the time [`Self::serve`] runs, validation
    /// fails and the listener is never bound.
    pub fn hook_plugin<T, E>(mut self) -> Self
    where
        T: ServerHook<E> + Send + Sync + 'static,
        E: Clone + Send + Sync + 'static,
    {
        self.plugins.hook_plugin::<T, E>(self.installing_plugin);
        self
    }

    /// Wrap the router in a tower layer. Mirrors
    /// [`axum::Router::layer`].
    ///
    /// **Install after routes.** axum's `Router::layer` only wraps
    /// routes that exist at the call site — routes added by later
    /// plugins (via [`Self::route`] or [`Self::router_mut`]) silently
    /// bypass this layer. If a plugin both installs middleware and
    /// adds routes, install middleware last:
    ///
    /// ```ignore
    /// Server::new(Router::new())
    ///     .plugin(plugin_that_adds_health_routes())
    ///     .layer(request_event_layer())   // wraps user routes + health routes
    ///     .serve(addr).await
    /// ```
    ///
    /// The same caveat applies to plugins that compose internally:
    /// install layers in their `install` fn after any `route`/
    /// `router_mut` calls. [`Self::with_auth`] is the exception:
    /// the builder records auth providers and applies them during
    /// finalization so plugin routes added later are still wrapped.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error:
            Into<std::convert::Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.router = self.router.layer(layer);
        self
    }

    /// Add a route to the underlying router. Useful when a plugin
    /// installs framework-internal endpoints like `/healthz` or
    /// `/readyz`.
    ///
    /// `Server` holds a stateless `Router` (`Router<()>`), so
    /// `method_router` must be `MethodRouter<()>`. Plugins that need
    /// per-handler state should bind it on the method router with
    /// [`axum::routing::MethodRouter::with_state`] before passing it
    /// here:
    ///
    /// ```ignore
    /// use pocopine_server::axum::routing::get;
    ///
    /// let metrics = Arc::new(MetricsService::new());
    /// server.route(
    ///     "/metrics",
    ///     get(scrape_metrics).with_state(metrics),
    /// )
    /// ```
    ///
    /// For routes that need full router-level state composition
    /// (`Router::with_state`, nested routers with their own state,
    /// etc.) use [`Self::router_mut`] for an arbitrary
    /// `Router -> Router` transform.
    pub fn route(mut self, path: &str, method_router: axum::routing::MethodRouter<()>) -> Self {
        self.router = self.router.route(path, method_router);
        self
    }

    /// Apply a closure to the underlying router. Escape hatch for
    /// integrations that need to reach for axum APIs the builder
    /// has not yet surfaced.
    pub fn router_mut<F>(mut self, f: F) -> Self
    where
        F: FnOnce(Router) -> Router,
    {
        self.router = f(self.router);
        self
    }

    /// Snapshot the current router. Useful for plugins that need
    /// to inspect existing routes during install.
    pub fn router(&self) -> &Router {
        &self.router
    }

    /// Validate the plugin registry, activate it, and return the
    /// underlying axum router.
    ///
    /// **Doc-hidden test seam.** Used by tests that drive the router
    /// with `tower::ServiceExt::oneshot` without binding a listener.
    /// The signature is not part of the stable surface and may change
    /// in a future patch release; plugin authors should always go
    /// through [`Self::serve`].
    ///
    /// On validation failure this function logs each missing-service
    /// diagnostic to `pocopine.log`, resets the active plugin
    /// registry to empty, and returns
    /// [`std::io::ErrorKind::InvalidInput`]. The reset is defensive:
    /// in long-running processes that try to bind a server twice
    /// (the first succeeding, the second failing) the previous
    /// registry's services would otherwise stay live, and any
    /// remaining `pocopine_server::active_plugin` lookup would
    /// resolve against stale state even though the framework
    /// refused to bind. Failed boot is observable as "no plugins"
    /// rather than "old plugins."
    #[doc(hidden)]
    pub fn try_finalize(self) -> std::io::Result<Router> {
        let Self {
            mut router,
            plugins,
            auth_providers,
            server_function_conflicts,
            ..
        } = self;

        if !server_function_conflicts.is_empty() {
            log_server_function_route_conflicts(&server_function_conflicts);
            plugin::activate(PluginRegistry::default());
            return Err(server_function_route_conflict_error(
                &server_function_conflicts,
            ));
        }

        if let Err(errors) = plugins.validate() {
            log_plugin_validation_errors(&errors);
            plugin::activate(PluginRegistry::default());
            return Err(plugin_validation_error(&errors));
        }

        plugin::activate(plugins);
        for provider in auth_providers {
            router = router.layer(axum::middleware::from_fn_with_state(
                provider,
                auth_middleware,
            ));
        }
        Ok(router)
    }

    /// Validate the plugin registry and bind the server to `addr`,
    /// running until the listener is closed.
    ///
    /// If validation fails (a hook references a service that was
    /// never provided) the listener is never bound and the function
    /// returns `io::Error` of kind `InvalidInput`. **No
    /// [`ServerBootFailed`] is emitted** for validation failures —
    /// the registry is reset to empty before any potential emit, so
    /// hooks the failing plugin chain registered cannot observe
    /// their own validation rejection. The `io::Error` is the only
    /// signal. [`ServerBootFailed`] still fires for runtime failures
    /// after validation has succeeded (`address_parse`, `bind`).
    pub async fn serve(self, addr: &str) -> std::io::Result<()> {
        // RFC-123 §3.4 — `pocopine.server.boot` wraps validation through
        // bind; serving itself is not booting and stays outside it.
        let span = tracing::info_span!(
            target: TRACE_TARGET,
            spans::SERVER_BOOT,
            otel.kind = "internal",
            server.address = addr,
            otel.status_code = Empty,
            error.type = Empty,
        );
        match self.boot(addr).instrument(span.clone()).await {
            Ok((listener, router)) => {
                span.record(fields::OTEL_STATUS_CODE, "OK");
                drop(span);
                axum::serve(listener, router).await
            }
            Err((reason, err)) => {
                span.record(fields::OTEL_STATUS_CODE, "ERROR");
                span.record(fields::ERROR_TYPE, reason);
                Err(err)
            }
        }
    }

    /// Validate, bind, and announce. `Err` carries the stable failure
    /// classification recorded on the boot span beside the io error.
    async fn boot(
        self,
        addr: &str,
    ) -> Result<(tokio::net::TcpListener, Router), (&'static str, std::io::Error)> {
        let router = self.try_finalize().map_err(|err| ("validation", err))?;

        if plugin::has_server_boot_started_hooks() {
            plugin::emit(ServerBootStarted {
                addr: addr.to_string(),
            });
        }

        let socket: SocketAddr = match addr.parse() {
            Ok(s) => s,
            Err(err) => {
                if plugin::has_server_boot_failed_hooks() {
                    plugin::emit(ServerBootFailed {
                        reason: "address_parse",
                    });
                }
                tracing::error!(
                    target: "pocopine.log",
                    %addr,
                    error = %err,
                    "pocopine server: invalid bind address"
                );
                return Err((
                    "address_parse",
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, err),
                ));
            }
        };

        let listener = match tokio::net::TcpListener::bind(socket).await {
            Ok(l) => l,
            Err(err) => {
                if plugin::has_server_boot_failed_hooks() {
                    plugin::emit(ServerBootFailed { reason: "bind" });
                }
                tracing::error!(
                    target: "pocopine.log",
                    %socket,
                    error = %err,
                    "pocopine server: bind failed"
                );
                return Err(("bind", err));
            }
        };

        tracing::info!(target: "pocopine.log", addr = %socket, "pocopine server listening");
        if plugin::has_server_listening_hooks() {
            plugin::emit(ServerListening {
                addr: socket.to_string(),
            });
        }

        Ok((listener, router))
    }
}

fn log_plugin_validation_errors(errors: &[PluginValidationError]) {
    tracing::error!(
        target: "pocopine.log",
        count = errors.len(),
        "pocopine server: plugin configuration is invalid; refusing to bind"
    );
    for err in errors {
        tracing::error!(target: "pocopine.log", error = %err);
    }
}

fn plugin_validation_error(errors: &[PluginValidationError]) -> std::io::Error {
    let summary = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("pocopine server: plugin configuration invalid: {summary}"),
    )
}

fn log_server_function_route_conflicts(errors: &[ServerFunctionRouteConflict]) {
    tracing::error!(
        target: "pocopine.log",
        count = errors.len(),
        "pocopine server: server-function routes are invalid; refusing to bind"
    );
    for err in errors {
        tracing::error!(
            target: "pocopine.log",
            path = err.path,
            first = %err.first,
            second = %err.second,
            "duplicate server-function route path"
        );
    }
}

fn server_function_route_conflict_error(errors: &[ServerFunctionRouteConflict]) -> std::io::Error {
    let summary = errors
        .iter()
        .map(|err| format!("{} used by {} and {}", err.path, err.first, err.second))
        .collect::<Vec<_>>()
        .join("; ");
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("pocopine server: server-function route conflict: {summary}"),
    )
}
