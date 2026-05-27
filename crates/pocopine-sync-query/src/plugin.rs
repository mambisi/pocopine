//! App-level plugin: install once, components find `QueryClient`
//! through the provided-services mechanism.
//!
//! Matches the surface of `pocopine_sync::sync_plugin()` — `query_client_plugin()`
//! returns a builder, `app.plugin(plugin)` installs it, and lifecycle
//! hooks request `Plugin<Rc<QueryClient>>` (or extract via
//! `pocopine_core::current_app_service()`).

use std::rc::Rc;

use pocopine_core::{App, AppPlugin};

use crate::client::QueryClient;
use crate::driver::QueryClientConfig;

/// Build the query-client app plugin.
///
/// Apps that want shape-aware reactive queries install this once:
///
/// ```ignore
/// fn app(app: App) -> App {
///     app.plugin(pocopine_sync_query::query_client_plugin())
/// }
/// ```
///
/// Components then access `Rc<QueryClient>` via the scope's plugin
/// services. Each `Resource::query().observe()` call routes through
/// the installed client.
pub fn query_client_plugin() -> QueryClientPlugin {
    QueryClientPlugin::default()
}

/// Builder for the query-client app plugin. Configure endpoint /
/// transport options here before installing.
#[derive(Default)]
pub struct QueryClientPlugin {
    config: QueryClientConfig,
}

impl QueryClientPlugin {
    /// Override the sync HTTP endpoint prefix this client posts to.
    /// Defaults to [`pocopine_sync::SYNC_ENDPOINT_PREFIX`].
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.endpoint = endpoint.into();
        self
    }

    /// Replace the full driver configuration. Use this when the
    /// app needs to tune the poll interval, disable live wakeup,
    /// or strip credentials from sync requests.
    pub fn config(mut self, config: QueryClientConfig) -> Self {
        self.config = config;
        self
    }

    /// Build the runtime client without installing. Useful for tests
    /// and host-side bench harnesses that don't run a full `App`.
    pub fn into_client(self) -> QueryClient {
        QueryClient::with_config(self.config)
    }
}

impl AppPlugin for QueryClientPlugin {
    fn name(&self) -> &'static str {
        "pocopine-sync-query"
    }

    fn install(self, app: App) -> App {
        // Provide the client as an `Rc<QueryClient>` so multiple
        // observers can hold cheap clones of the service handle
        // while the runtime owns the registry exclusively through
        // `RefCell` interior mutability.
        app.provide_plugin::<Rc<QueryClient>>(Rc::new(self.into_client()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_constructs_default_plugin() {
        let plugin = query_client_plugin();
        let client = plugin.into_client();
        assert_eq!(client.endpoint(), pocopine_sync::SYNC_ENDPOINT_PREFIX);
    }

    #[test]
    fn builder_accepts_endpoint_override() {
        let plugin = query_client_plugin().endpoint("/custom/prefix");
        assert_eq!(plugin.config.endpoint, "/custom/prefix");
        let client = plugin.into_client();
        assert_eq!(client.endpoint(), "/custom/prefix");
    }
}
