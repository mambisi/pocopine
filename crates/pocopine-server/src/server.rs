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
//! # mod app { pub fn __get_post_route(r: pocopine_server::axum::Router) -> pocopine_server::axum::Router { r } }
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let router = Router::new().fallback_service(static_files("pkg"));
//!     let router = app::__get_post_route(router);
//!     Server::new(router).serve("0.0.0.0:3000").await
//! }
//! ```
//!
//! Plain [`crate::serve`] still works as a one-liner wrapper.

use std::net::SocketAddr;

use axum::Router;
use tower::Layer;
use tower::Service;

use crate::plugin::{
    self, PluginRegistry, PluginValidationError, ServerBootFailed, ServerBootStarted, ServerHook,
    ServerListening,
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
pub struct Server {
    router: Router,
    plugins: PluginRegistry,
    installing_plugin: Option<&'static str>,
}

impl Server {
    /// Wrap `router` in a builder. Plugin work composes on top; the
    /// router itself remains the user's source of truth for route
    /// composition, fallback services, and `with_state` calls.
    pub fn new(router: Router) -> Self {
        Self {
            router,
            plugins: PluginRegistry::default(),
            installing_plugin: None,
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
    /// underlying axum router. Doc-hidden — used by tests that drive
    /// the router with `tower::ServiceExt::oneshot` without binding
    /// a listener.
    #[doc(hidden)]
    pub fn try_finalize(self) -> std::io::Result<Router> {
        let Self {
            router, plugins, ..
        } = self;

        if let Err(errors) = plugins.validate() {
            log_plugin_validation_errors(&errors);
            plugin::activate(PluginRegistry::default());
            if plugin::has_server_boot_failed_hooks() {
                plugin::emit(ServerBootFailed {
                    reason: "plugin_validation",
                });
            }
            return Err(plugin_validation_error(&errors));
        }

        plugin::activate(plugins);
        Ok(router)
    }

    /// Validate the plugin registry and bind the server to `addr`,
    /// running until the listener is closed.
    ///
    /// If validation fails (a hook references a service that was
    /// never provided) the listener is never bound, an
    /// [`ServerBootFailed`] event is emitted, and the function
    /// returns `io::Error` of kind `InvalidInput`.
    pub async fn serve(self, addr: &str) -> std::io::Result<()> {
        let router = self.try_finalize()?;

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
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err));
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
                return Err(err);
            }
        };

        tracing::info!(target: "pocopine.log", addr = %socket, "pocopine server listening");
        if plugin::has_server_listening_hooks() {
            plugin::emit(ServerListening {
                addr: socket.to_string(),
            });
        }

        axum::serve(listener, router).await
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
