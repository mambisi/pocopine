//! OTLP smoke server — host-only.
//!
//! Run this with an OpenTelemetry Collector listening on `localhost:4317`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use observability_smoke::router;
    use pocopine::logging::{init_server_logging, OtlpConfig, ServerLoggingConfig};
    use pocopine_server::serve;

    let service_name = std::env::var("POCOPINE_SERVICE_NAME")
        .or_else(|_| std::env::var("OTEL_SERVICE_NAME"))
        .unwrap_or_else(|_| "pocopine-otlp-smoke".to_owned());
    let otlp = OtlpConfig::from_env().with_service_name(service_name);

    init_server_logging(
        ServerLoggingConfig::json()
            .with_env_filter("info,pocopine=debug")
            .with_otlp(otlp),
    )
    .map_err(std::io::Error::other)?;

    let addr =
        std::env::var("POCOPINE_OTLP_SMOKE_ADDR").unwrap_or_else(|_| "127.0.0.1:3010".to_owned());
    tracing::info!(target: "pocopine.log", %addr, "serving OTLP smoke example");
    serve(router(), &addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
