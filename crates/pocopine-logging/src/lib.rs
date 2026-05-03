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
    init_default, init_server_logging, InitLoggingError, LogFormat, ServerLoggingConfig,
};

#[cfg(all(not(target_arch = "wasm32"), feature = "otlp"))]
pub use server::OtlpConfig;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::fmt;

    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;
    use wasm_bindgen::JsValue;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ConsoleLoggingConfig {
        pub max_level: Level,
        pub target_prefix: Option<String>,
    }

    impl ConsoleLoggingConfig {
        pub fn debug() -> Self {
            Self {
                max_level: Level::DEBUG,
                target_prefix: Some("pocopine".to_owned()),
            }
        }

        pub fn with_max_level(mut self, max_level: Level) -> Self {
            self.max_level = max_level;
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
            let message = format!(
                "{} {} {}",
                metadata.level(),
                metadata.target(),
                visitor.finish()
            );
            let value = JsValue::from_str(message.trim_end());
            match *metadata.level() {
                Level::ERROR => web_sys::console::error_1(&value),
                Level::WARN => web_sys::console::warn_1(&value),
                Level::INFO => web_sys::console::info_1(&value),
                Level::DEBUG | Level::TRACE => web_sys::console::debug_1(&value),
            }
        }
    }

    #[derive(Default)]
    struct FieldVisitor {
        message: Option<String>,
        fields: Vec<String>,
    }

    impl FieldVisitor {
        fn finish(self) -> String {
            let mut out = self.message.unwrap_or_default();
            for field in self.fields {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&field);
            }
            out
        }
    }

    impl Visit for FieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            let rendered = format!("{value:?}");
            if field.name() == "message" {
                self.message = Some(rendered);
            } else {
                self.fields.push(format!("{}={rendered}", field.name()));
            }
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
}

#[cfg(target_arch = "wasm32")]
pub use web::{init_console_logging, ConsoleLoggingConfig, InitLoggingError};
