//! Logging adapters for pocopine applications.
//!
//! Runtime crates should emit `tracing` events. Application entrypoints
//! install one logging subscriber appropriate for their environment.

use pocopine_observe::{emit_tracing, ObservedEvent};

pub fn log_event(event: &ObservedEvent) {
    emit_tracing(event);
}

#[cfg(not(target_arch = "wasm32"))]
mod server {
    use std::fmt;
    #[cfg(feature = "otlp")]
    use std::time::Duration;

    use pocopine_observe::{
        emit_tracing, EventClass, EventPriority, FieldPrivacy, ObserveContext, ObservedEvent,
    };
    use pocopine_server::{
        request_event_layer, HttpRequestCompleted, HttpRequestFailed, HttpRequestStarted, Server,
        ServerBootFailed, ServerBootStarted, ServerFunctionCompleted, ServerFunctionFailed,
        ServerFunctionRejected, ServerFunctionStarted, ServerHook, ServerListening, ServerPlugin,
    };
    use tracing_subscriber::fmt as tracing_fmt;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{filter, EnvFilter};

    #[cfg(feature = "otlp")]
    const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4317";
    #[cfg(feature = "otlp")]
    const DEFAULT_OTLP_SERVICE_NAME: &str = "pocopine-app";

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum LogFormat {
        Compact,
        Pretty,
        Json,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ServerLoggingConfig {
        pub env_filter: Option<String>,
        pub format: LogFormat,
        pub ansi: bool,
        #[cfg(feature = "otlp")]
        pub otlp: Option<OtlpConfig>,
    }

    impl ServerLoggingConfig {
        pub fn compact() -> Self {
            Self {
                env_filter: None,
                format: LogFormat::Compact,
                ansi: true,
                #[cfg(feature = "otlp")]
                otlp: None,
            }
        }

        pub fn json() -> Self {
            Self {
                env_filter: None,
                format: LogFormat::Json,
                ansi: false,
                #[cfg(feature = "otlp")]
                otlp: None,
            }
        }

        pub fn with_env_filter(mut self, env_filter: impl Into<String>) -> Self {
            self.env_filter = Some(env_filter.into());
            self
        }

        pub fn with_ansi(mut self, ansi: bool) -> Self {
            self.ansi = ansi;
            self
        }

        #[cfg(feature = "otlp")]
        pub fn with_otlp(mut self, otlp: OtlpConfig) -> Self {
            self.otlp = Some(otlp);
            self
        }

        #[cfg(feature = "otlp")]
        pub fn with_otlp_from_env(self) -> Self {
            self.with_otlp(OtlpConfig::from_env())
        }
    }

    impl Default for ServerLoggingConfig {
        fn default() -> Self {
            Self::compact()
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ServerObservabilityConfig {
        pub service: Option<String>,
        pub environment: Option<String>,
        pub boot: bool,
        pub http_requests: bool,
        pub server_functions: bool,
        pub include_unmatched_paths: bool,
    }

    impl ServerObservabilityConfig {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_service(mut self, service: impl Into<String>) -> Self {
            self.service = Some(service.into());
            self
        }

        pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
            self.environment = Some(environment.into());
            self
        }

        pub fn with_boot(mut self, enabled: bool) -> Self {
            self.boot = enabled;
            self
        }

        pub fn with_http_requests(mut self, enabled: bool) -> Self {
            self.http_requests = enabled;
            self
        }

        pub fn with_server_functions(mut self, enabled: bool) -> Self {
            self.server_functions = enabled;
            self
        }

        /// Include raw URI paths for unmatched HTTP requests.
        ///
        /// Matched routes always emit the route pattern instead of the concrete
        /// path. Unmatched paths can contain tenant ids, slugs, or other
        /// pseudonymous values, so the default is to omit them.
        pub fn with_unmatched_paths(mut self, enabled: bool) -> Self {
            self.include_unmatched_paths = enabled;
            self
        }
    }

    impl Default for ServerObservabilityConfig {
        fn default() -> Self {
            Self {
                service: None,
                environment: None,
                boot: true,
                http_requests: true,
                server_functions: true,
                include_unmatched_paths: false,
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ServerObservabilityPlugin {
        config: ServerObservabilityConfig,
    }

    impl ServerObservabilityPlugin {
        pub fn new(config: ServerObservabilityConfig) -> Self {
            Self { config }
        }
    }

    pub fn server_observability() -> ServerObservabilityPlugin {
        ServerObservabilityPlugin::new(ServerObservabilityConfig::default())
    }

    pub fn server_observability_with_config(
        config: ServerObservabilityConfig,
    ) -> ServerObservabilityPlugin {
        ServerObservabilityPlugin::new(config)
    }

    impl ServerPlugin for ServerObservabilityPlugin {
        fn name(&self) -> &'static str {
            "pocopine.logging.server_observability"
        }

        fn install(self, server: Server) -> Server {
            let ServerObservabilityConfig {
                service,
                environment,
                boot,
                http_requests,
                server_functions,
                include_unmatched_paths,
            } = self.config;

            let mut server = server.provide_plugin(ServerObservability {
                service,
                environment,
                include_unmatched_paths,
            });

            if http_requests || server_functions {
                server = server.layer(request_event_layer());
            }

            if boot {
                server = server
                    .hook_plugin::<ServerObservability, ServerBootStarted>()
                    .hook_plugin::<ServerObservability, ServerListening>()
                    .hook_plugin::<ServerObservability, ServerBootFailed>();
            }

            if http_requests {
                server = server
                    .hook_plugin::<ServerObservability, HttpRequestStarted>()
                    .hook_plugin::<ServerObservability, HttpRequestCompleted>()
                    .hook_plugin::<ServerObservability, HttpRequestFailed>();
            }

            if server_functions {
                server = server
                    .hook_plugin::<ServerObservability, ServerFunctionStarted>()
                    .hook_plugin::<ServerObservability, ServerFunctionCompleted>()
                    .hook_plugin::<ServerObservability, ServerFunctionRejected>()
                    .hook_plugin::<ServerObservability, ServerFunctionFailed>();
            }

            server
        }
    }

    #[derive(Debug)]
    pub struct ServerObservability {
        service: Option<String>,
        environment: Option<String>,
        include_unmatched_paths: bool,
    }

    impl ServerObservability {
        pub fn emit(&self, event: ObservedEvent) {
            let event = self.with_base_context(event);
            emit_tracing(&event);
        }

        fn with_base_context(&self, mut event: ObservedEvent) -> ObservedEvent {
            if event.context.service.is_none() {
                event.context.service = self.service.clone();
            }
            if event.context.environment.is_none() {
                event.context.environment = self.environment.clone();
            }
            event
        }

        fn context(&self) -> ObserveContext {
            ObserveContext {
                service: self.service.clone(),
                environment: self.environment.clone(),
                ..ObserveContext::default()
            }
        }

        fn context_with_route(&self, route: &str) -> ObserveContext {
            self.context().with_route(route)
        }

        fn request_event(
            &self,
            name: &'static str,
            class: EventClass,
            method: String,
            path: String,
            route_pattern: Option<String>,
            request_id: u64,
        ) -> ObservedEvent {
            let matched = route_pattern.is_some();
            let mut event = ObservedEvent::new(name, class)
                .field("method", method, FieldPrivacy::Public)
                .field("matched", matched, FieldPrivacy::Public)
                .field("request_id", request_id, FieldPrivacy::Public);

            if let Some(route) = route_pattern {
                event = event.context(self.context_with_route(&route)).field(
                    "route",
                    route,
                    FieldPrivacy::Public,
                );
            } else {
                event = event.context(self.context());
                if self.include_unmatched_paths {
                    event = event.field("path", path, FieldPrivacy::Pseudonymous);
                }
            }

            event
        }
    }

    impl ServerHook<ServerBootStarted> for ServerObservability {
        fn call(&self, event: ServerBootStarted) {
            self.emit(
                ObservedEvent::trace("server_boot_started")
                    .context(self.context())
                    .field("addr", event.addr, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<ServerListening> for ServerObservability {
        fn call(&self, event: ServerListening) {
            self.emit(
                ObservedEvent::trace("server_listening")
                    .context(self.context())
                    .field("addr", event.addr, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<ServerBootFailed> for ServerObservability {
        fn call(&self, event: ServerBootFailed) {
            self.emit(
                ObservedEvent::log("server_boot_failed")
                    .priority(EventPriority::Critical)
                    .context(self.context())
                    .field("reason", event.reason, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<HttpRequestStarted> for ServerObservability {
        fn call(&self, event: HttpRequestStarted) {
            let observed = self
                .request_event(
                    "http_request_started",
                    EventClass::Trace,
                    event.method,
                    event.path,
                    event.route_pattern,
                    event.request_id,
                )
                .priority(EventPriority::Low);
            self.emit(observed);
        }
    }

    impl ServerHook<HttpRequestCompleted> for ServerObservability {
        fn call(&self, event: HttpRequestCompleted) {
            let status = event.status as u64;
            let duration_ms = event.duration_ms;
            let observed = self
                .request_event(
                    "http_request_completed",
                    EventClass::Trace,
                    event.method,
                    event.path,
                    event.route_pattern,
                    event.request_id,
                )
                .field("status", status, FieldPrivacy::Public)
                .field("duration_ms", duration_ms, FieldPrivacy::Public);
            self.emit(observed);
        }
    }

    impl ServerHook<HttpRequestFailed> for ServerObservability {
        fn call(&self, event: HttpRequestFailed) {
            let reason = event.reason;
            let duration_ms = event.duration_ms;
            let observed = self
                .request_event(
                    "http_request_failed",
                    EventClass::Log,
                    event.method,
                    event.path,
                    event.route_pattern,
                    event.request_id,
                )
                .priority(EventPriority::High)
                .field("reason", reason, FieldPrivacy::Public)
                .field("duration_ms", duration_ms, FieldPrivacy::Public);
            self.emit(observed);
        }
    }

    impl ServerHook<ServerFunctionStarted> for ServerObservability {
        fn call(&self, event: ServerFunctionStarted) {
            self.emit(
                ObservedEvent::trace("server_function_started")
                    .priority(EventPriority::Low)
                    .context(self.context())
                    .field("function", event.function, FieldPrivacy::Public)
                    .field("function_path", event.function_path, FieldPrivacy::Public)
                    .field("request_id", event.request_id, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<ServerFunctionCompleted> for ServerObservability {
        fn call(&self, event: ServerFunctionCompleted) {
            self.emit(
                ObservedEvent::trace("server_function_completed")
                    .context(self.context())
                    .field("function", event.function, FieldPrivacy::Public)
                    .field("function_path", event.function_path, FieldPrivacy::Public)
                    .field("request_id", event.request_id, FieldPrivacy::Public)
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<ServerFunctionRejected> for ServerObservability {
        fn call(&self, event: ServerFunctionRejected) {
            self.emit(
                ObservedEvent::log("server_function_rejected")
                    .priority(EventPriority::High)
                    .context(self.context())
                    .field("function", event.function, FieldPrivacy::Public)
                    .field("function_path", event.function_path, FieldPrivacy::Public)
                    .field("request_id", event.request_id, FieldPrivacy::Public)
                    .field("status", event.status as u64, FieldPrivacy::Public)
                    .field("reason", event.reason, FieldPrivacy::Public),
            );
        }
    }

    impl ServerHook<ServerFunctionFailed> for ServerObservability {
        fn call(&self, event: ServerFunctionFailed) {
            self.emit(
                ObservedEvent::log("server_function_failed")
                    .priority(EventPriority::High)
                    .context(self.context())
                    .field("function", event.function, FieldPrivacy::Public)
                    .field("function_path", event.function_path, FieldPrivacy::Public)
                    .field("request_id", event.request_id, FieldPrivacy::Public)
                    .field("error_class", event.error_class, FieldPrivacy::Public)
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
            );
        }
    }

    #[cfg(feature = "otlp")]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct OtlpConfig {
        pub endpoint: String,
        pub service_name: String,
        pub timeout: Duration,
    }

    #[cfg(feature = "otlp")]
    impl OtlpConfig {
        pub fn grpc(endpoint: impl Into<String>) -> Self {
            Self {
                endpoint: endpoint.into(),
                ..Self::default()
            }
        }

        pub fn from_env() -> Self {
            Self::from_env_vars(|name| std::env::var(name).ok())
        }

        pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
            self.endpoint = endpoint.into();
            self
        }

        pub fn with_service_name(mut self, service_name: impl Into<String>) -> Self {
            self.service_name = service_name.into();
            self
        }

        pub fn with_timeout(mut self, timeout: Duration) -> Self {
            self.timeout = timeout;
            self
        }

        fn from_env_vars(mut var: impl FnMut(&str) -> Option<String>) -> Self {
            let endpoint = first_non_empty(
                &mut var,
                &[
                    "POCOPINE_OTLP_ENDPOINT",
                    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                ],
            )
            .unwrap_or_else(|| DEFAULT_OTLP_ENDPOINT.to_owned());
            let service_name =
                first_non_empty(&mut var, &["POCOPINE_SERVICE_NAME", "OTEL_SERVICE_NAME"])
                    .unwrap_or_else(|| DEFAULT_OTLP_SERVICE_NAME.to_owned());

            Self {
                endpoint,
                service_name,
                timeout: default_otlp_timeout(),
            }
        }
    }

    #[cfg(feature = "otlp")]
    impl Default for OtlpConfig {
        fn default() -> Self {
            Self {
                endpoint: DEFAULT_OTLP_ENDPOINT.to_owned(),
                service_name: DEFAULT_OTLP_SERVICE_NAME.to_owned(),
                timeout: default_otlp_timeout(),
            }
        }
    }

    #[derive(Debug)]
    pub enum InitLoggingError {
        Filter(filter::ParseError),
        #[cfg(feature = "otlp")]
        Otlp(String),
        AlreadyInitialized,
    }

    impl fmt::Display for InitLoggingError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Filter(err) => write!(f, "invalid tracing filter: {err}"),
                #[cfg(feature = "otlp")]
                Self::Otlp(err) => write!(f, "initialize OTLP tracing: {err}"),
                Self::AlreadyInitialized => {
                    f.write_str("global tracing subscriber is already initialized")
                }
            }
        }
    }

    impl std::error::Error for InitLoggingError {}

    /// Install compact server logging with the default `RUST_LOG` handling.
    pub fn init_default() -> Result<(), InitLoggingError> {
        init_server_logging(ServerLoggingConfig::compact())
    }

    pub fn init_server_logging(config: ServerLoggingConfig) -> Result<(), InitLoggingError> {
        let filter = build_filter(config.env_filter)?;
        #[cfg(feature = "otlp")]
        if let Some(otlp) = config.otlp.as_ref() {
            return init_server_logging_with_otlp(filter, config.format, config.ansi, otlp);
        }

        init_server_logging_local(filter, config.format, config.ansi)
    }

    fn init_server_logging_local(
        filter: EnvFilter,
        format: LogFormat,
        ansi: bool,
    ) -> Result<(), InitLoggingError> {
        match format {
            LogFormat::Compact => tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_ansi(ansi),
                )
                .try_init(),
            LogFormat::Pretty => tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_fmt::layer()
                        .pretty()
                        .with_target(true)
                        .with_ansi(ansi),
                )
                .try_init(),
            LogFormat::Json => tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_fmt::layer()
                        .json()
                        .with_target(true)
                        .with_ansi(false),
                )
                .try_init(),
        }
        .map_err(|_| InitLoggingError::AlreadyInitialized)
    }

    #[cfg(feature = "otlp")]
    fn init_server_logging_with_otlp(
        filter: EnvFilter,
        format: LogFormat,
        ansi: bool,
        otlp: &OtlpConfig,
    ) -> Result<(), InitLoggingError> {
        let tracer = build_otlp_tracer(otlp)?;
        let otel_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_level(true)
            .with_target(true);

        match format {
            LogFormat::Compact => tracing_subscriber::registry()
                .with(filter)
                .with(otel_layer)
                .with(
                    tracing_fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_ansi(ansi),
                )
                .try_init(),
            LogFormat::Pretty => tracing_subscriber::registry()
                .with(filter)
                .with(otel_layer)
                .with(
                    tracing_fmt::layer()
                        .pretty()
                        .with_target(true)
                        .with_ansi(ansi),
                )
                .try_init(),
            LogFormat::Json => tracing_subscriber::registry()
                .with(filter)
                .with(otel_layer)
                .with(
                    tracing_fmt::layer()
                        .json()
                        .with_target(true)
                        .with_ansi(false),
                )
                .try_init(),
        }
        .map_err(|_| InitLoggingError::AlreadyInitialized)
    }

    #[cfg(feature = "otlp")]
    fn build_otlp_tracer(
        otlp: &OtlpConfig,
    ) -> Result<opentelemetry_sdk::trace::SdkTracer, InitLoggingError> {
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_otlp::WithExportConfig as _;

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(otlp.endpoint.clone())
            .with_timeout(otlp.timeout)
            .build()
            .map_err(|err| InitLoggingError::Otlp(err.to_string()))?;
        let resource = opentelemetry_sdk::Resource::builder()
            .with_service_name(otlp.service_name.clone())
            .build();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build();
        Ok(provider.tracer(otlp.service_name.clone()))
    }

    fn build_filter(env_filter: Option<String>) -> Result<EnvFilter, InitLoggingError> {
        let filter = env_filter
            .or_else(|| std::env::var("RUST_LOG").ok())
            .unwrap_or_else(|| "info,pocopine=debug".to_owned());
        EnvFilter::try_new(filter).map_err(InitLoggingError::Filter)
    }

    #[cfg(feature = "otlp")]
    fn default_otlp_timeout() -> Duration {
        Duration::from_secs(10)
    }

    #[cfg(feature = "otlp")]
    fn first_non_empty(
        var: &mut impl FnMut(&str) -> Option<String>,
        names: &[&str],
    ) -> Option<String> {
        names
            .iter()
            .filter_map(|name| var(name))
            .map(|value| value.trim().to_owned())
            .find(|value| !value.is_empty())
    }

    #[cfg(all(test, feature = "otlp"))]
    mod tests {
        use super::*;

        #[test]
        fn otlp_config_prefers_pocopine_env_names() {
            let config = OtlpConfig::from_env_vars(|name| match name {
                "POCOPINE_OTLP_ENDPOINT" => Some("http://collector:4317".to_owned()),
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT" => Some("http://otel-traces:4317".to_owned()),
                "POCOPINE_SERVICE_NAME" => Some("blog-api".to_owned()),
                "OTEL_SERVICE_NAME" => Some("otel-service".to_owned()),
                _ => None,
            });

            assert_eq!(config.endpoint, "http://collector:4317");
            assert_eq!(config.service_name, "blog-api");
        }

        #[test]
        fn otlp_config_uses_standard_env_names_and_defaults() {
            let config = OtlpConfig::from_env_vars(|name| match name {
                "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("http://otel:4317".to_owned()),
                _ => None,
            });

            assert_eq!(config.endpoint, "http://otel:4317");
            assert_eq!(config.service_name, DEFAULT_OTLP_SERVICE_NAME);
            assert_eq!(config.timeout, default_otlp_timeout());
        }

        #[test]
        fn server_logging_config_accepts_otlp_config() {
            let config = ServerLoggingConfig::json()
                .with_otlp(OtlpConfig::grpc("http://collector:4317").with_service_name("site"));

            assert_eq!(
                config.otlp.as_ref().unwrap().endpoint,
                "http://collector:4317"
            );
            assert_eq!(config.otlp.as_ref().unwrap().service_name, "site");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    init_default, init_server_logging, server_observability, server_observability_with_config,
    InitLoggingError, LogFormat, ServerLoggingConfig, ServerObservability,
    ServerObservabilityConfig, ServerObservabilityPlugin,
};

#[cfg(all(not(target_arch = "wasm32"), feature = "otlp"))]
pub use server::OtlpConfig;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::fmt;

    use js_sys::{Object, Reflect};
    use pocopine_core::{
        App, AppBootCompleted, AppBootFailed, AppBootStarted, AppPlugin, ComponentMounted,
        ComponentReady, ComponentSetup, ComponentUnmounted, Hook, RouteNavigationCompleted,
        RouteNavigationFailed, RouteNavigationStarted, ServerFunctionClientCompleted,
        ServerFunctionClientFailed, ServerFunctionClientStarted,
    };
    use pocopine_observe::{
        emit_tracing, EventPriority, FieldPrivacy, ObserveContext, ObservedEvent,
    };
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;
    use wasm_bindgen::JsValue;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum ConsoleLogFormat {
        Text,
        Json,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ConsoleLoggingConfig {
        pub max_level: Level,
        pub target_prefix: Option<String>,
        pub format: ConsoleLogFormat,
    }

    impl ConsoleLoggingConfig {
        pub fn debug() -> Self {
            Self {
                max_level: Level::DEBUG,
                target_prefix: Some("pocopine".to_owned()),
                format: ConsoleLogFormat::Text,
            }
        }

        pub fn json() -> Self {
            Self {
                max_level: Level::DEBUG,
                target_prefix: Some("pocopine".to_owned()),
                format: ConsoleLogFormat::Json,
            }
        }

        pub fn with_max_level(mut self, max_level: Level) -> Self {
            self.max_level = max_level;
            self
        }

        pub fn with_format(mut self, format: ConsoleLogFormat) -> Self {
            self.format = format;
            self
        }

        pub fn with_target_prefix(mut self, target_prefix: impl Into<String>) -> Self {
            self.target_prefix = Some(target_prefix.into());
            self
        }

        pub fn without_target_prefix(mut self) -> Self {
            self.target_prefix = None;
            self
        }
    }

    impl Default for ConsoleLoggingConfig {
        fn default() -> Self {
            Self::debug()
        }
    }

    #[derive(Debug)]
    pub enum InitLoggingError {
        AlreadyInitialized,
    }

    impl fmt::Display for InitLoggingError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::AlreadyInitialized => {
                    f.write_str("global tracing subscriber is already initialized")
                }
            }
        }
    }

    impl std::error::Error for InitLoggingError {}

    pub fn init_console_logging(config: ConsoleLoggingConfig) -> Result<(), InitLoggingError> {
        tracing_subscriber::registry()
            .with(ConsoleLayer { config })
            .try_init()
            .map_err(|_| InitLoggingError::AlreadyInitialized)
    }

    struct ConsoleLayer {
        config: ConsoleLoggingConfig,
    }

    impl<S> Layer<S> for ConsoleLayer
    where
        S: Subscriber,
    {
        fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
            if !level_allowed(*metadata.level(), self.config.max_level) {
                return false;
            }
            match &self.config.target_prefix {
                Some(prefix) => metadata.target().starts_with(prefix),
                None => true,
            }
        }

        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let metadata = event.metadata();
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);

            match self.config.format {
                ConsoleLogFormat::Text => {
                    let message = format!(
                        "{} {} {}",
                        metadata.level(),
                        metadata.target(),
                        visitor.finish_text()
                    );
                    write_console(*metadata.level(), &JsValue::from_str(message.trim_end()));
                }
                ConsoleLogFormat::Json => {
                    let value = visitor.into_console_object(metadata);
                    write_console(*metadata.level(), &value);
                }
            }
        }
    }

    #[derive(Default)]
    struct FieldVisitor {
        message: Option<String>,
        fields: Vec<ConsoleField>,
    }

    impl FieldVisitor {
        fn finish_text(&self) -> String {
            let mut out = self.message.clone().unwrap_or_default();
            for field in &self.fields {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&field.name);
                out.push('=');
                out.push_str(&field.value.to_field_text());
            }
            out
        }

        fn into_console_object(self, metadata: &Metadata<'_>) -> JsValue {
            let object = Object::new();
            set_property(
                &object,
                "level",
                JsValue::from_str(&metadata.level().to_string()),
            );
            set_property(&object, "target", JsValue::from_str(metadata.target()));
            if let Some(message) = self.message {
                set_property(&object, "message", JsValue::from_str(&message));
            }

            let fields = Object::new();
            for field in self.fields {
                set_property(&fields, &field.name, field.value.into_js_value());
            }
            set_property(&object, "fields", fields.into());
            object.into()
        }

        fn record_value(&mut self, field: &Field, value: ConsoleFieldValue) {
            if field.name() == "message" {
                self.message = Some(value.to_message_text());
            } else {
                self.fields.push(ConsoleField {
                    name: field.name().to_owned(),
                    value,
                });
            }
        }
    }

    impl Visit for FieldVisitor {
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.record_value(field, ConsoleFieldValue::Bool(value));
        }

        fn record_f64(&mut self, field: &Field, value: f64) {
            self.record_value(field, ConsoleFieldValue::F64(value));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.record_value(field, ConsoleFieldValue::I64(value));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.record_value(field, ConsoleFieldValue::U64(value));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.record_value(field, ConsoleFieldValue::String(value.to_owned()));
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.record_value(field, ConsoleFieldValue::Debug(format!("{value:?}")));
        }
    }

    struct ConsoleField {
        name: String,
        value: ConsoleFieldValue,
    }

    enum ConsoleFieldValue {
        Bool(bool),
        F64(f64),
        I64(i64),
        U64(u64),
        String(String),
        Debug(String),
    }

    impl ConsoleFieldValue {
        fn to_field_text(&self) -> String {
            match self {
                Self::Bool(value) => value.to_string(),
                Self::F64(value) => value.to_string(),
                Self::I64(value) => value.to_string(),
                Self::U64(value) => value.to_string(),
                Self::String(value) => format!("{value:?}"),
                Self::Debug(value) => value.clone(),
            }
        }

        fn to_message_text(&self) -> String {
            match self {
                Self::String(value) | Self::Debug(value) => value.clone(),
                _ => self.to_field_text(),
            }
        }

        fn into_js_value(self) -> JsValue {
            match self {
                Self::Bool(value) => JsValue::from_bool(value),
                Self::F64(value) => JsValue::from_f64(value),
                Self::I64(value) => js_value_from_i64(value),
                Self::U64(value) => js_value_from_u64(value),
                Self::String(value) | Self::Debug(value) => JsValue::from_str(&value),
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct FrontendObservabilityConfig {
        pub console_logging: Option<ConsoleLoggingConfig>,
        pub service: Option<String>,
        pub environment: Option<String>,
        pub component_setup: bool,
        pub component_ready: bool,
    }

    impl FrontendObservabilityConfig {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_console_logging(mut self, config: ConsoleLoggingConfig) -> Self {
            self.console_logging = Some(config);
            self
        }

        pub fn without_console_logging(mut self) -> Self {
            self.console_logging = None;
            self
        }

        pub fn with_service(mut self, service: impl Into<String>) -> Self {
            self.service = Some(service.into());
            self
        }

        pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
            self.environment = Some(environment.into());
            self
        }

        pub fn with_component_setup(mut self, enabled: bool) -> Self {
            self.component_setup = enabled;
            self
        }

        pub fn with_component_ready(mut self, enabled: bool) -> Self {
            self.component_ready = enabled;
            self
        }
    }

    impl Default for FrontendObservabilityConfig {
        fn default() -> Self {
            Self {
                console_logging: Some(ConsoleLoggingConfig::json()),
                service: None,
                environment: None,
                component_setup: false,
                component_ready: false,
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct FrontendObservabilityPlugin {
        config: FrontendObservabilityConfig,
    }

    impl FrontendObservabilityPlugin {
        pub fn new(config: FrontendObservabilityConfig) -> Self {
            Self { config }
        }
    }

    pub fn frontend_observability() -> FrontendObservabilityPlugin {
        FrontendObservabilityPlugin::new(FrontendObservabilityConfig::default())
    }

    pub fn frontend_observability_with_config(
        config: FrontendObservabilityConfig,
    ) -> FrontendObservabilityPlugin {
        FrontendObservabilityPlugin::new(config)
    }

    impl AppPlugin for FrontendObservabilityPlugin {
        fn name(&self) -> &'static str {
            "pocopine.logging.frontend_observability"
        }

        fn install(self, app: App) -> App {
            let FrontendObservabilityConfig {
                console_logging,
                service,
                environment,
                component_setup,
                component_ready,
            } = self.config;

            if let Some(console_config) = console_logging {
                if let Err(err) = init_console_logging(console_config) {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "pocopine: frontend observability could not initialize console logging: {err}"
                    )));
                }
            }

            let mut app = app
                .provide_plugin(FrontendObservability {
                    service,
                    environment,
                })
                .hook_plugin::<FrontendObservability, AppBootStarted>()
                .hook_plugin::<FrontendObservability, AppBootCompleted>()
                .hook_plugin::<FrontendObservability, AppBootFailed>()
                .hook_plugin::<FrontendObservability, RouteNavigationStarted>()
                .hook_plugin::<FrontendObservability, RouteNavigationCompleted>()
                .hook_plugin::<FrontendObservability, RouteNavigationFailed>()
                .hook_plugin::<FrontendObservability, ServerFunctionClientStarted>()
                .hook_plugin::<FrontendObservability, ServerFunctionClientCompleted>()
                .hook_plugin::<FrontendObservability, ServerFunctionClientFailed>()
                .hook_plugin::<FrontendObservability, ComponentMounted>()
                .hook_plugin::<FrontendObservability, ComponentUnmounted>();

            if component_setup {
                app = app.hook_plugin::<FrontendObservability, ComponentSetup>();
            }
            if component_ready {
                app = app.hook_plugin::<FrontendObservability, ComponentReady>();
            }
            app
        }
    }

    #[derive(Debug)]
    pub struct FrontendObservability {
        service: Option<String>,
        environment: Option<String>,
    }

    impl FrontendObservability {
        pub fn emit(&self, event: ObservedEvent) {
            let event = self.with_base_context(event);
            emit_tracing(&event);
        }

        fn with_base_context(&self, mut event: ObservedEvent) -> ObservedEvent {
            if event.context.service.is_none() {
                event.context.service = self.service.clone();
            }
            if event.context.environment.is_none() {
                event.context.environment = self.environment.clone();
            }
            event
        }

        fn context(&self) -> ObserveContext {
            ObserveContext {
                service: self.service.clone(),
                environment: self.environment.clone(),
                ..ObserveContext::default()
            }
        }

        fn context_with_component(&self, component: &str) -> ObserveContext {
            self.context().with_component(component)
        }

        fn context_with_route(&self, route: &str) -> ObserveContext {
            self.context().with_route(route)
        }
    }

    impl Hook<AppBootStarted> for FrontendObservability {
        fn call(&self, event: AppBootStarted) {
            self.emit(
                ObservedEvent::trace("frontend_app_started")
                    .context(self.context())
                    .field(
                        "component_count",
                        event.component_count as u64,
                        FieldPrivacy::Public,
                    )
                    .field(
                        "route_count",
                        event.route_count as u64,
                        FieldPrivacy::Public,
                    ),
            );
        }
    }

    impl Hook<AppBootCompleted> for FrontendObservability {
        fn call(&self, event: AppBootCompleted) {
            self.emit(
                ObservedEvent::trace("frontend_app_boot_completed")
                    .context(self.context())
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<AppBootFailed> for FrontendObservability {
        fn call(&self, event: AppBootFailed) {
            self.emit(
                ObservedEvent::log("frontend_app_boot_failed")
                    .priority(EventPriority::High)
                    .context(self.context())
                    .field("reason", event.reason, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<RouteNavigationStarted> for FrontendObservability {
        fn call(&self, event: RouteNavigationStarted) {
            let mut observed = ObservedEvent::trace("route_navigation_started").field(
                "matched",
                event.route_pattern.is_some(),
                FieldPrivacy::Public,
            );
            if let Some(route) = event.route_pattern {
                observed = observed.context(self.context_with_route(route)).field(
                    "route",
                    route,
                    FieldPrivacy::Public,
                );
            } else {
                observed = observed.context(self.context());
            }
            if let Some(component) = event.component {
                observed = observed.field("component", component, FieldPrivacy::Public);
            }
            self.emit(observed);
        }
    }

    impl Hook<RouteNavigationCompleted> for FrontendObservability {
        fn call(&self, event: RouteNavigationCompleted) {
            match (event.route_pattern, event.component) {
                (Some(route), Some(component)) => self.emit(
                    ObservedEvent::analytics("route_view")
                        .context(self.context_with_route(route))
                        .field("route", route, FieldPrivacy::Public)
                        .field("component", component, FieldPrivacy::Public)
                        .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
                ),
                _ => self.emit(
                    ObservedEvent::trace("route_navigation_completed")
                        .context(self.context())
                        .field("matched", false, FieldPrivacy::Public)
                        .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
                ),
            }
        }
    }

    impl Hook<RouteNavigationFailed> for FrontendObservability {
        fn call(&self, event: RouteNavigationFailed) {
            let mut observed = ObservedEvent::log("route_navigation_failed")
                .priority(EventPriority::High)
                .field("reason", event.reason, FieldPrivacy::Public)
                .field("duration_ms", event.duration_ms, FieldPrivacy::Public);
            if let Some(route) = event.route_pattern {
                observed = observed.context(self.context_with_route(route)).field(
                    "route",
                    route,
                    FieldPrivacy::Public,
                );
            } else {
                observed = observed.context(self.context());
            }
            if let Some(component) = event.component {
                observed = observed.field("component", component, FieldPrivacy::Public);
            }
            self.emit(observed);
        }
    }

    impl Hook<ServerFunctionClientStarted> for FrontendObservability {
        fn call(&self, event: ServerFunctionClientStarted) {
            self.emit(
                ObservedEvent::trace("server_function_client_started")
                    .context(self.context_with_route(&event.route))
                    .field("route", event.route, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<ServerFunctionClientCompleted> for FrontendObservability {
        fn call(&self, event: ServerFunctionClientCompleted) {
            self.emit(
                ObservedEvent::trace("server_function_client_completed")
                    .context(self.context_with_route(&event.route))
                    .field("route", event.route, FieldPrivacy::Public)
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public)
                    .field(
                        "status_code",
                        event.status_code as u64,
                        FieldPrivacy::Public,
                    ),
            );
        }
    }

    impl Hook<ServerFunctionClientFailed> for FrontendObservability {
        fn call(&self, event: ServerFunctionClientFailed) {
            self.emit(
                ObservedEvent::log("server_function_client_failed")
                    .priority(EventPriority::High)
                    .context(self.context_with_route(&event.route))
                    .field("route", event.route, FieldPrivacy::Public)
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public)
                    .field("error_kind", event.error_kind, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<ComponentSetup> for FrontendObservability {
        fn call(&self, event: ComponentSetup) {
            self.emit(
                ObservedEvent::trace("component_setup")
                    .priority(EventPriority::Low)
                    .context(self.context_with_component(event.component))
                    .field("component", event.component, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<ComponentMounted> for FrontendObservability {
        fn call(&self, event: ComponentMounted) {
            self.emit(
                ObservedEvent::analytics("component_view")
                    .context(self.context_with_component(event.component))
                    .field("component", event.component, FieldPrivacy::Public)
                    .field("duration_ms", event.duration_ms, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<ComponentReady> for FrontendObservability {
        fn call(&self, event: ComponentReady) {
            self.emit(
                ObservedEvent::trace("component_ready")
                    .priority(EventPriority::Low)
                    .context(self.context_with_component(event.component))
                    .field("component", event.component, FieldPrivacy::Public),
            );
        }
    }

    impl Hook<ComponentUnmounted> for FrontendObservability {
        fn call(&self, event: ComponentUnmounted) {
            self.emit(
                ObservedEvent::trace("component_unmounted")
                    .context(self.context_with_component(event.component))
                    .field("component", event.component, FieldPrivacy::Public),
            );
        }
    }

    fn write_console(level: Level, value: &JsValue) {
        match level {
            Level::ERROR => web_sys::console::error_1(value),
            Level::WARN => web_sys::console::warn_1(value),
            Level::INFO => web_sys::console::info_1(value),
            Level::DEBUG | Level::TRACE => web_sys::console::debug_1(value),
        }
    }

    fn set_property(object: &Object, name: &str, value: JsValue) {
        let _ = Reflect::set(object, &JsValue::from_str(name), &value);
    }

    fn js_value_from_i64(value: i64) -> JsValue {
        if (MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
            JsValue::from_f64(value as f64)
        } else {
            JsValue::from_str(&value.to_string())
        }
    }

    fn js_value_from_u64(value: u64) -> JsValue {
        if value <= MAX_SAFE_INTEGER as u64 {
            JsValue::from_f64(value as f64)
        } else {
            JsValue::from_str(&value.to_string())
        }
    }

    fn level_allowed(level: Level, max_level: Level) -> bool {
        level_rank(level) <= level_rank(max_level)
    }

    fn level_rank(level: Level) -> u8 {
        match level {
            Level::ERROR => 1,
            Level::WARN => 2,
            Level::INFO => 3,
            Level::DEBUG => 4,
            Level::TRACE => 5,
        }
    }

    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    const MIN_SAFE_INTEGER: i64 = -MAX_SAFE_INTEGER;
}

#[cfg(target_arch = "wasm32")]
pub use web::{
    frontend_observability, frontend_observability_with_config, init_console_logging,
    ConsoleLogFormat, ConsoleLoggingConfig, FrontendObservability, FrontendObservabilityConfig,
    FrontendObservabilityPlugin, InitLoggingError,
};
