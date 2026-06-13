#![cfg(not(target_arch = "wasm32"))]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use pocopine::analytics::{
    AnalyticsClient, BoundedAnalyticsSink, ExporterMetrics, JsonLinesAnalyticsSink, event,
};
use pocopine::observe::{FieldPrivacy, RedactionPolicy};

#[derive(Clone, Default)]
struct SharedBuffer {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedBuffer {
    fn lines(&self) -> Vec<serde_json::Value> {
        let output = String::from_utf8(self.bytes.lock().expect("buffer lock poisoned").clone())
            .expect("json-lines output is utf-8");
        output
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid analytics json-line"))
            .collect()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .expect("buffer lock poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn bounded_json_lines_exporter_redacts_and_counts_backpressure() {
    let output = SharedBuffer::default();
    let exporter = BoundedAnalyticsSink::new(JsonLinesAnalyticsSink::new(output.clone()), 2);
    let metrics = exporter.clone();
    let analytics = AnalyticsClient::new()
        .without_tracing_events()
        .with_redaction(RedactionPolicy::public_only())
        .with_sink(exporter);

    let first = analytics.emit(
        event("route_view")
            .field("route", "/settings", FieldPrivacy::Public)
            .field("email", "person@example.test", FieldPrivacy::Sensitive),
    );
    let second = analytics.emit(
        event("cta_clicked")
            .field("cta", "upgrade", FieldPrivacy::Public)
            .field("session", "session-123", FieldPrivacy::Pseudonymous),
    );
    let third = analytics.emit(event("overflow").field("route", "/overflow", FieldPrivacy::Public));

    assert!(first.all_succeeded());
    assert!(second.all_succeeded());
    assert_eq!(third.attempted, 1);
    assert_eq!(third.succeeded, 0);
    assert_eq!(third.failed, 1);

    let flush = analytics.flush();

    assert!(flush.all_succeeded());
    assert_eq!(
        metrics.metrics(),
        ExporterMetrics {
            pending: 0,
            enqueued: 2,
            dropped: 1,
            delivered: 2,
            failed: 0,
        }
    );

    let lines = output.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["name"], "route_view");
    assert_eq!(lines[0]["fields"]["route"]["value"], "/settings");
    assert!(lines[0]["fields"].get("email").is_none());
    assert_eq!(lines[1]["name"], "cta_clicked");
    assert_eq!(lines[1]["fields"]["cta"]["value"], "upgrade");
    assert!(lines[1]["fields"].get("session").is_none());
}
