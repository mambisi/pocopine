#![cfg(all(target_arch = "wasm32", feature = "logging"))]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use pocopine::observe::{FieldPrivacy, ObservedEvent};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-plugin-shell",
    template_inline = r#"<main><h1>observability</h1><pp-outlet></pp-outlet></main>"#
)]
struct ObsPluginShell {}

#[handlers]
impl ObsPluginShell {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "obs-plugin-secret-page",
    template_inline = r#"<section><p pp-text="id"></p></section>"#
)]
struct ObsPluginSecretPage {
    #[prop]
    pub id: String,
}

#[handlers]
impl ObsPluginSecretPage {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-plugin-custom",
    template_inline = r#"<section><p>custom observability</p></section>"#
)]
struct ObsPluginCustom {}

#[handlers]
impl ObsPluginCustom {
    pub fn on_mount(&self) {
        self.plugin::<pocopine::logging::FrontendObservability>()
            .emit(ObservedEvent::analytics("component_custom_event").field(
                "source",
                "component",
                FieldPrivacy::Public,
            ));
    }
}

#[wasm_bindgen_test]
fn frontend_observability_plugin_emits_route_and_component_events_without_param_leaks() {
    let _host = append_app_host("<obs-plugin-shell></obs-plugin-shell>");
    push_url("/__obs-plugin/secret-42");

    let capture = TraceCapture::new();
    capture.run(|| {
        App::new()
            .plugin(observability_without_console())
            .register::<ObsPluginShell>()
            .route::<ObsPluginSecretPage>("/__obs-plugin/:id")
            .run();
    });

    let events = capture.events();
    assert!(
        events.iter().any(|event| event.target == "pocopine.trace"
            && event.field("event_name") == Some("frontend_app_started")),
        "observability plugin should translate AppBootStarted into a trace event",
    );

    let route_event = events
        .iter()
        .find(|event| {
            event.target == "pocopine.analytics" && event.field("event_name") == Some("route_view")
        })
        .expect("route_view analytics event should be emitted");
    let route_fields = route_event
        .field("fields")
        .expect("ObservedEvent fields should be captured");
    assert!(
        route_fields.contains("/__obs-plugin/:id"),
        "route_view should report the route pattern, not the concrete path",
    );
    assert!(
        route_fields.contains("obs-plugin-secret-page"),
        "route_view should include the mounted component name",
    );

    let component_event = events
        .iter()
        .find(|event| {
            event.target == "pocopine.analytics"
                && event.field("event_name") == Some("component_view")
                && event
                    .field("fields")
                    .is_some_and(|fields| fields.contains("obs-plugin-secret-page"))
        })
        .expect("component_view analytics event should be emitted for the route component");
    let duration_ms = f64_debug_field(
        component_event
            .field("fields")
            .expect("component_view fields should be captured"),
        "duration_ms",
    )
    .expect("component_view should include a numeric duration_ms");
    assert!(
        (0.0..250.0).contains(&duration_ms),
        "simple component mount duration should stay inside the CI smoke budget, got {duration_ms}ms",
    );

    assert!(
        events
            .iter()
            .all(|event| !event.contains_value("secret-42")),
        "route params and DOM text must not leak into observability event fields",
    );
}

#[wasm_bindgen_test]
fn frontend_observability_service_is_available_to_components() {
    let app_root = append_app_host("");

    let capture = TraceCapture::new();
    capture.run(|| {
        App::new().plugin(observability_without_console()).run();

        let host = append_plain_host("");
        let handle = App::mount_subtree::<ObsPluginCustom>(&host);
        handle.unmount();
        host.remove();
    });

    let events = capture.events();
    assert!(
        events
            .iter()
            .any(|event| event.target == "pocopine.analytics"
                && event.field("event_name") == Some("component_custom_event")
                && event
                    .field("fields")
                    .is_some_and(|fields| fields.contains("component"))),
        "components should be able to emit custom events through Plugin<FrontendObservability>",
    );
    assert!(
        events.iter().any(|event| event.target == "pocopine.trace"
            && event.field("event_name") == Some("component_unmounted")
            && event
                .field("fields")
                .is_some_and(|fields| fields.contains("obs-plugin-custom"))),
        "subtree unmount should flow through the plugin component hook",
    );

    app_root.remove();
}

fn observability_without_console() -> pocopine::logging::FrontendObservabilityPlugin {
    pocopine::logging::frontend_observability_with_config(
        pocopine::logging::FrontendObservabilityConfig::default().without_console_logging(),
    )
}

#[derive(Clone, Debug)]
struct CapturedEvent {
    target: String,
    fields: BTreeMap<String, String>,
}

impl CapturedEvent {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    fn contains_value(&self, needle: &str) -> bool {
        self.fields.values().any(|value| value.contains(needle))
    }
}

#[derive(Clone, Default)]
struct TraceCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl TraceCapture {
    fn new() -> Self {
        Self::default()
    }

    fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer {
            events: Arc::clone(&self.events),
        });
        tracing::subscriber::with_default(subscriber, f)
    }

    fn events(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .expect("trace capture lock poisoned")
            .clone()
    }
}

struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .expect("trace capture lock poisoned")
            .push(CapturedEvent {
                target: event.metadata().target().to_owned(),
                fields: visitor.fields,
            });
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn insert(&mut self, field: &Field, value: impl Into<String>) {
        self.fields.insert(field.name().to_owned(), value.into());
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, format!("{value:?}"));
    }
}

fn append_app_host(inner_html: &str) -> Element {
    append_host(inner_html, true)
}

fn append_plain_host(inner_html: &str) -> Element {
    append_host(inner_html, false)
}

fn append_host(inner_html: &str, is_app_root: bool) -> Element {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .expect("browser document");
    let host = document.create_element("div").expect("host element");
    if is_app_root {
        let _ = host.set_attribute("pp-app", "");
    }
    host.set_inner_html(inner_html);
    document
        .body()
        .expect("document body")
        .append_child(host.as_ref())
        .expect("append host");
    host
}

fn push_url(path: &str) {
    let window = web_sys::window().expect("browser window");
    window
        .history()
        .expect("history")
        .push_state_with_url(&JsValue::NULL, "", Some(path))
        .expect("push test URL");
}

fn f64_debug_field(fields: &str, name: &str) -> Option<f64> {
    let needle = format!("\"{name}\": ObservedField {{ value: F64(");
    let start = fields.find(&needle)? + needle.len();
    let rest = &fields[start..];
    let end = rest.find(')')?;
    rest[..end].parse().ok()
}
