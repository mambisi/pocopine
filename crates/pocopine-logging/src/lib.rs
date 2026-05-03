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

    use tracing_subscriber::fmt as tracing_fmt;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{filter, EnvFilter};

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
    }

    impl ServerLoggingConfig {
        pub fn compact() -> Self {
            Self {
                env_filter: None,
                format: LogFormat::Compact,
                ansi: true,
            }
        }

        pub fn json() -> Self {
            Self {
                env_filter: None,
                format: LogFormat::Json,
                ansi: false,
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
    }

    impl Default for ServerLoggingConfig {
        fn default() -> Self {
            Self::compact()
        }
    }

    #[derive(Debug)]
    pub enum InitLoggingError {
        Filter(filter::ParseError),
        AlreadyInitialized,
    }

    impl fmt::Display for InitLoggingError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Filter(err) => write!(f, "invalid tracing filter: {err}"),
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
        match config.format {
            LogFormat::Compact => tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_ansi(config.ansi),
                )
                .try_init(),
            LogFormat::Pretty => tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_fmt::layer()
                        .pretty()
                        .with_target(true)
                        .with_ansi(config.ansi),
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

    fn build_filter(env_filter: Option<String>) -> Result<EnvFilter, InitLoggingError> {
        let filter = env_filter
            .or_else(|| std::env::var("RUST_LOG").ok())
            .unwrap_or_else(|| "info,pocopine=debug".to_owned());
        EnvFilter::try_new(filter).map_err(InitLoggingError::Filter)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    init_default, init_server_logging, InitLoggingError, LogFormat, ServerLoggingConfig,
};

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
