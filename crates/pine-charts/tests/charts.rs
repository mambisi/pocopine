//! Pine Charts browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine-charts`.

#![cfg(target_arch = "wasm32")]

use pine_charts::{ChartBar, ChartBarSeries, ChartPoint};
use pocopine::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount_fixture<C: pocopine::__private::Component>() -> Element {
    pine_charts::register_all();
    pocopine_core::animate::disable_transitions();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    let root = doc().create_element(C::NAME).unwrap();
    host.append_child(&root).unwrap();
    body.append_child(&host).unwrap();
    let mounted = pocopine::App::mount_subtree::<C>(&root);
    pocopine_core::mount::finalize_compiled_subtree(&root);
    mounted.leak();
    host
}

async fn tick() {
    for _ in 0..3 {
        let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        let window = web_sys::window().unwrap();
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

async fn settle() {
    tick().await;
    sleep_ms(0).await;
    tick().await;
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-line-chart class="sales-chart"
                   label="Sales"
                   width="100"
                   height="100"
                   margin_top="0"
                   margin_right="0"
                   margin_bottom="0"
                   margin_left="0"
                   pp-bind:points="points"></pine-line-chart>
</div>
"#)]
struct LineChartFixture {
    points: Vec<ChartPoint>,
}

impl Default for LineChartFixture {
    fn default() -> Self {
        Self {
            points: vec![
                ChartPoint::new(0.0, 0.0),
                ChartPoint::new(5.0, 10.0),
                ChartPoint::new(10.0, 5.0),
            ],
        }
    }
}

#[handlers]
impl LineChartFixture {}

#[wasm_bindgen_test]
async fn line_chart_renders_svg_path_axes_and_grid() {
    let host = mount_fixture::<LineChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("role").as_deref(), Some("img"));
    assert_eq!(chart.get_attribute("aria-label").as_deref(), Some("Sales"));
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("ready"));
    assert!(!chart.has_attribute("data-empty"));
    assert!(!chart.has_attribute("data-invalid"));

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    let view_box = svg
        .get_attribute("viewBox")
        .or_else(|| svg.get_attribute("viewbox"));
    assert_eq!(view_box.as_deref(), Some("0 0 100 100"));

    let path = host
        .query_selector("path.pine-chart-line")
        .unwrap()
        .unwrap();
    assert_eq!(
        path.get_attribute("d").as_deref(),
        Some("M0,100 L50,0 L100,50")
    );

    let grid_lines = host.query_selector_all(".pine-chart-grid-line").unwrap();
    assert_eq!(grid_lines.length(), 12, "six x-grid plus six y-grid lines");

    let axes = host.query_selector_all(".pine-chart-axis").unwrap();
    assert_eq!(axes.length(), 2, "x and y axis domain lines");

    let labels = host.query_selector_all(".pine-chart-tick-label").unwrap();
    assert_eq!(labels.length(), 12, "x and y tick labels render");

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <button class="swap" @click="swap">Swap</button>
  <span class="swap-count" pp-text="swaps"></span>
  <pine-line-chart class="reactive-chart"
                   label="Reactive"
                   width="100"
                   height="100"
                   margin_top="0"
                   margin_right="0"
                   margin_bottom="0"
                   margin_left="0"
                   pp-bind:points="points"></pine-line-chart>
</div>
"#)]
struct ReactiveChartFixture {
    points: Vec<ChartPoint>,
    swaps: u32,
}

impl Default for ReactiveChartFixture {
    fn default() -> Self {
        Self {
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
            swaps: 0,
        }
    }
}

#[handlers]
impl ReactiveChartFixture {
    pub fn swap(&mut self) {
        self.swaps += 1;
        self.points = vec![ChartPoint::new(0.0, 1.0), ChartPoint::new(1.0, 0.0)];
    }
}

#[wasm_bindgen_test]
async fn line_chart_recomputes_when_bound_points_change() {
    let host = mount_fixture::<ReactiveChartFixture>();
    settle().await;

    let path = host
        .query_selector("path.pine-chart-line")
        .unwrap()
        .unwrap();
    assert_eq!(path.get_attribute("d").as_deref(), Some("M0,100 L100,0"));

    host.query_selector("button.swap")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    settle().await;

    let swaps = host.query_selector(".swap-count").unwrap().unwrap();
    assert_eq!(swaps.text_content().as_deref(), Some("1"));

    let path = host
        .query_selector("path.pine-chart-line")
        .unwrap()
        .unwrap();
    assert_eq!(path.get_attribute("d").as_deref(), Some("M0,0 L100,100"));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-line-chart class="empty-chart"
                   label="Empty"
                   width="100"
                   height="100"
                   pp-bind:points="points"></pine-line-chart>
</div>
"#)]
struct EmptyChartFixture {
    points: Vec<ChartPoint>,
}

impl Default for EmptyChartFixture {
    fn default() -> Self {
        Self { points: Vec::new() }
    }
}

#[handlers]
impl EmptyChartFixture {}

#[wasm_bindgen_test]
async fn line_chart_reports_empty_state() {
    let host = mount_fixture::<EmptyChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("empty"));
    assert!(chart.has_attribute("data-empty"));
    assert!(!chart.has_attribute("data-invalid"));

    let path = host
        .query_selector("path.pine-chart-line")
        .unwrap()
        .unwrap();
    assert_eq!(path.get_attribute("d").as_deref(), Some(""));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-line-chart class="invalid-chart"
                   label="Invalid"
                   width="0"
                   height="100"
                   pp-bind:points="points"></pine-line-chart>
</div>
"#)]
struct InvalidChartFixture {
    points: Vec<ChartPoint>,
}

impl Default for InvalidChartFixture {
    fn default() -> Self {
        Self {
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
        }
    }
}

#[handlers]
impl InvalidChartFixture {}

#[wasm_bindgen_test]
async fn line_chart_reports_invalid_state_and_status() {
    let host = mount_fixture::<InvalidChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert_eq!(
        chart.get_attribute("data-state").as_deref(),
        Some("invalid")
    );
    assert!(!chart.has_attribute("data-empty"));
    assert!(chart.has_attribute("data-invalid"));

    let status = host.query_selector(".pine-chart-status").unwrap().unwrap();
    assert!(
        status
            .text_content()
            .unwrap_or_default()
            .contains("positive"),
        "invalid status should expose the validation error"
    );

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-bar-chart class="revenue-bars"
                  label="Revenue"
                  width="100"
                  height="100"
                  margin_top="0"
                  margin_right="0"
                  margin_bottom="0"
                  margin_left="0"
                  padding_inner="0"
                  padding_outer="0"
                  y_min="0"
                  y_max="10"
                  pp-bind:data="bars"></pine-bar-chart>
</div>
"#)]
struct BarChartFixture {
    bars: Vec<ChartBar>,
}

impl Default for BarChartFixture {
    fn default() -> Self {
        Self {
            bars: vec![
                ChartBar::new("A", 2.0),
                ChartBar::new("B", 10.0),
                ChartBar::new("C", 5.0),
            ],
        }
    }
}

#[handlers]
impl BarChartFixture {}

#[wasm_bindgen_test]
async fn bar_chart_renders_svg_rects_axes_and_labels() {
    let host = mount_fixture::<BarChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-bar-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("role").as_deref(), Some("img"));
    assert_eq!(
        chart.get_attribute("aria-label").as_deref(),
        Some("Revenue")
    );
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("ready"));

    let bars = host.query_selector_all(".pine-chart-bar").unwrap();
    assert_eq!(bars.length(), 3);

    let first = bars.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first.namespace_uri().as_deref(),
        Some("http://www.w3.org/2000/svg"),
    );
    assert_eq!(first.get_attribute("x").as_deref(), Some("0"));
    assert_eq!(first.get_attribute("y").as_deref(), Some("80"));
    assert_eq!(first.get_attribute("height").as_deref(), Some("20"));
    assert_eq!(first.get_attribute("aria-label").as_deref(), Some("A: 2"));
    assert_eq!(first.get_attribute("data-category").as_deref(), Some("A"));

    let labels = host.query_selector_all(".pine-chart-tick-label").unwrap();
    assert!(labels.length() >= 6, "category and value labels render");

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-bar-chart class="grouped-bars"
                  label="Grouped"
                  width="100"
                  height="100"
                  margin_top="0"
                  margin_right="0"
                  margin_bottom="0"
                  margin_left="0"
                  padding_inner="0"
                  padding_outer="0"
                  series_padding_inner="0"
                  y_min="0"
                  y_max="10"
                  pp-bind:series="series"></pine-bar-chart>
</div>
"#)]
struct GroupedBarChartFixture {
    series: Vec<ChartBarSeries>,
}

impl Default for GroupedBarChartFixture {
    fn default() -> Self {
        Self {
            series: vec![
                ChartBarSeries::new(
                    "New",
                    vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
                ),
                ChartBarSeries::new(
                    "Returning",
                    vec![ChartBar::new("A", 3.0), ChartBar::new("B", 10.0)],
                ),
            ],
        }
    }
}

#[handlers]
impl GroupedBarChartFixture {}

#[wasm_bindgen_test]
async fn grouped_bar_chart_renders_series_metadata() {
    let host = mount_fixture::<GroupedBarChartFixture>();
    settle().await;

    let bars = host.query_selector_all(".pine-chart-bar").unwrap();
    assert_eq!(bars.length(), 4);

    let first = bars.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(first.get_attribute("x").as_deref(), Some("0"));
    assert_eq!(first.get_attribute("width").as_deref(), Some("25"));
    assert_eq!(first.get_attribute("data-series").as_deref(), Some("New"));
    assert_eq!(first.get_attribute("data-category").as_deref(), Some("A"));

    let second = bars.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(second.get_attribute("x").as_deref(), Some("25"));
    assert_eq!(
        second.get_attribute("data-series").as_deref(),
        Some("Returning")
    );
    assert_eq!(
        second.get_attribute("aria-label").as_deref(),
        Some("Returning, A: 3")
    );

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-bar-chart class="stacked-bars"
                  label="Stacked"
                  mode="stacked"
                  width="100"
                  height="100"
                  margin_top="0"
                  margin_right="0"
                  margin_bottom="0"
                  margin_left="0"
                  padding_inner="0"
                  padding_outer="0"
                  y_min="0"
                  y_max="10"
                  pp-bind:series="series"></pine-bar-chart>
</div>
"#)]
struct StackedBarChartFixture {
    series: Vec<ChartBarSeries>,
}

impl Default for StackedBarChartFixture {
    fn default() -> Self {
        Self {
            series: vec![
                ChartBarSeries::new(
                    "New",
                    vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
                ),
                ChartBarSeries::new(
                    "Returning",
                    vec![ChartBar::new("A", 3.0), ChartBar::new("B", 6.0)],
                ),
            ],
        }
    }
}

#[handlers]
impl StackedBarChartFixture {}

#[wasm_bindgen_test]
async fn stacked_bar_chart_accumulates_segments() {
    let host = mount_fixture::<StackedBarChartFixture>();
    settle().await;

    let bars = host.query_selector_all(".pine-chart-bar").unwrap();
    assert_eq!(bars.length(), 4);

    let first = bars.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(first.get_attribute("x").as_deref(), Some("0"));
    assert_eq!(first.get_attribute("width").as_deref(), Some("50"));
    assert_eq!(first.get_attribute("y").as_deref(), Some("80"));
    assert_eq!(first.get_attribute("height").as_deref(), Some("20"));

    let second = bars.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(second.get_attribute("x").as_deref(), Some("0"));
    assert_eq!(second.get_attribute("width").as_deref(), Some("50"));
    assert_eq!(second.get_attribute("y").as_deref(), Some("50"));
    assert_eq!(second.get_attribute("height").as_deref(), Some("30"));

    host.remove();
}
