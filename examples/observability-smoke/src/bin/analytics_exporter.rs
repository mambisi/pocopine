//! Host-side analytics exporter smoke binary.
//!
//! This writes redacted analytics events as JSON lines to stdout. It is meant
//! to mirror container/AWS/Cloudflare-style log-agent ingestion without pulling
//! a vendor SDK into the framework.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pocopine::analytics::{
        event, route_view, AnalyticsClient, BoundedAnalyticsSink, JsonLinesAnalyticsSink,
    };
    use pocopine::observe::FieldPrivacy;

    let exporter = BoundedAnalyticsSink::new(JsonLinesAnalyticsSink::stdout(), 16);
    let metrics = exporter.clone();
    let analytics = AnalyticsClient::new()
        .without_tracing_events()
        .with_sink(exporter);

    let route_report = analytics.emit(
        route_view("/report/:id")
            .field("surface", "observability-smoke", FieldPrivacy::Public)
            .field("demo_user_hash", "demo-user", FieldPrivacy::Pseudonymous),
    );
    let timing_report = analytics.emit(
        event("server_function_client_completed")
            .field("function", "observe_echo", FieldPrivacy::Public)
            .field("duration_ms", 18.4_f64, FieldPrivacy::Public),
    );
    let flush_report = analytics.flush();

    eprintln!("analytics exporter metrics: {:?}", metrics.metrics());

    if route_report.all_succeeded() && timing_report.all_succeeded() && flush_report.all_succeeded()
    {
        Ok(())
    } else {
        Err("analytics exporter smoke failed".into())
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
