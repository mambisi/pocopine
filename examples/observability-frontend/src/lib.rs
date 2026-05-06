#![cfg(target_arch = "wasm32")]

use pocopine::logging::{FrontendObservability, FrontendObservabilityConfig};
use pocopine::observe::{FieldPrivacy, ObservedEvent};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-frontend-shell",
    template_inline = r#"
    <div class="shell">
      <header>
        <h1>Frontend observability</h1>
        <nav>
          <a href="/" pp-route>Home</a>
          <a href="/report/demo-42" pp-route>Report</a>
        </nav>
      </header>
      <main>
        <pp-outlet></pp-outlet>
      </main>
    </div>
    "#
)]
pub struct ObsFrontendShell {}

#[handlers]
impl ObsFrontendShell {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-frontend-home",
    template_inline = r#"
    <section>
      <h2>Home</h2>
      <p>Open DevTools to see structured pocopine events in the console.</p>
      <button @click="track_feature">Track feature</button>
      <p>Tracked feature uses: <output pp-text="tracked"></output></p>
    </section>
    "#
)]
pub struct ObsFrontendHome {
    pub tracked: u32,
}

#[handlers]
impl ObsFrontendHome {
    pub fn track_feature(&mut self) {
        self.tracked += 1;
        self.plugin::<FrontendObservability>().emit(
            ObservedEvent::analytics("feature_used")
                .field("feature", "observability_button", FieldPrivacy::Public)
                .field("count", self.tracked, FieldPrivacy::Public),
        );
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-frontend-report",
    template_inline = r#"
    <section>
      <h2>Report</h2>
      <p>Current report: <output pp-text="id"></output></p>
      <div class="metric-grid">
        <article class="metric-card">
          <span>Route pattern</span>
          <output pp-text="route_pattern"></output>
        </article>
        <article class="metric-card">
          <span>Report id length</span>
          <output pp-text="report_id_len"></output>
        </article>
        <article class="metric-card">
          <span>Events emitted</span>
          <output pp-text="events_emitted"></output>
        </article>
        <article class="metric-card">
          <span>Private fields exported</span>
          <output pp-text="private_fields_exported"></output>
        </article>
      </div>
      <p>Latest metric event: <output pp-text="latest_metric"></output></p>
    </section>
    "#
)]
pub struct ObsFrontendReport {
    #[prop]
    pub id: String,
    pub route_pattern: String,
    pub report_id_len: u64,
    pub events_emitted: u32,
    pub private_fields_exported: u32,
    pub latest_metric: String,
}

#[handlers]
impl ObsFrontendReport {
    pub fn on_setup(&mut self) {
        self.route_pattern = "/report/:id".to_string();
        self.report_id_len = self.id.len() as u64;
        self.events_emitted = 3;
        self.private_fields_exported = 0;
        self.latest_metric = "report_metrics_visible".to_string();
    }

    pub fn on_mount(&self) {
        // Keep the raw report id out of telemetry; route params may contain
        // customer or account identifiers in real applications.
        self.plugin::<FrontendObservability>().emit(
            ObservedEvent::analytics("report_viewed").field(
                "report_id_len",
                self.id.len() as u64,
                FieldPrivacy::Public,
            ),
        );
        self.plugin::<FrontendObservability>().emit(
            ObservedEvent::metric("report_metrics_visible")
                .field("route", "/report/:id", FieldPrivacy::Public)
                .field("report_id_len", self.report_id_len, FieldPrivacy::Public)
                .field("visible_metrics", 4_u64, FieldPrivacy::Public),
        );
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-frontend-not-found",
    template_inline = r#"
    <section>
      <h2>Not Found</h2>
      <p>The requested route does not exist.</p>
    </section>
    "#
)]
pub struct ObsFrontendNotFound {}

#[handlers]
impl ObsFrontendNotFound {}

#[wasm_bindgen(start)]
pub fn main() {
    let observability = pocopine::logging::frontend_observability_with_config(
        FrontendObservabilityConfig::default()
            .with_service("observability-frontend")
            .with_environment("dev"),
    );

    pocopine::app! {
        components: [
            ObsFrontendShell,
            ObsFrontendHome,
            ObsFrontendReport,
            ObsFrontendNotFound,
        ],
        plugins: [
            observability,
        ],
        routes: [
            ("/", ObsFrontendHome),
            ("/report/:id", ObsFrontendReport),
            ("*", ObsFrontendNotFound),
        ],
    };
}
