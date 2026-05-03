//! Pine Charts browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine-charts`.

#![cfg(target_arch = "wasm32")]

use pine_charts::{ChartBar, ChartBarSeries, ChartPoint, LegendItem};
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

#[wasm_bindgen_test]
async fn line_chart_shows_crosshair_and_tooltip_on_pointer_move() {
    let host = mount_fixture::<LineChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert!(!chart.has_attribute("data-hover"));

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    let rect = svg.get_bounding_client_rect();
    let init = web_sys::PointerEventInit::new();
    init.set_bubbles(true);
    init.set_client_x((rect.left() + 50.0).round() as i32);
    init.set_client_y((rect.top() + 50.0).round() as i32);
    svg.dispatch_event(
        &web_sys::PointerEvent::new_with_event_init_dict("pointermove", &init).unwrap(),
    )
    .unwrap();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert!(chart.has_attribute("data-hover"));

    let marker = host
        .query_selector(".pine-chart-hover-marker")
        .unwrap()
        .unwrap();
    assert_eq!(marker.get_attribute("cx").as_deref(), Some("50"));
    assert_eq!(marker.get_attribute("cy").as_deref(), Some("0"));
    assert_eq!(marker.get_attribute("data-x").as_deref(), Some("5"));
    assert_eq!(marker.get_attribute("data-y").as_deref(), Some("10"));

    let tooltip = host.query_selector(".pine-chart-tooltip").unwrap().unwrap();
    assert_eq!(tooltip.get_attribute("data-x").as_deref(), Some("5"));
    assert_eq!(tooltip.get_attribute("data-y").as_deref(), Some("10"));
    assert_eq!(
        tooltip.get_attribute("aria-label").as_deref(),
        Some("x 5, y 10")
    );
    assert_eq!(
        tooltip.get_attribute("data-tooltip-x").as_deref(),
        Some("left")
    );
    assert_eq!(
        tooltip.get_attribute("data-tooltip-y").as_deref(),
        Some("below")
    );
    assert!(tooltip
        .get_attribute("style")
        .unwrap_or_default()
        .contains("--pine-chart-tooltip-x: 50%"));

    let leave = web_sys::PointerEvent::new("pointerleave").unwrap();
    svg.dispatch_event(&leave).unwrap();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert!(!chart.has_attribute("data-hover"));

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

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <button class="swap-legend" @click="swap">Swap</button>
  <pine-chart-legend class="fixture-legend"
                     label="Fixture legend"
                     pp-bind:items="items"></pine-chart-legend>
</div>
"#)]
struct LegendFixture {
    items: Vec<LegendItem>,
}

impl Default for LegendFixture {
    fn default() -> Self {
        Self {
            items: vec![LegendItem::new("Organic"), LegendItem::new("Referral")],
        }
    }
}

#[handlers]
impl LegendFixture {
    pub fn swap(&mut self) {
        self.items = vec![LegendItem::new("API"), LegendItem::new("Render")];
    }
}

#[wasm_bindgen_test]
async fn chart_legend_renders_items_and_updates() {
    let host = mount_fixture::<LegendFixture>();
    settle().await;

    let legend = host.query_selector(".fixture-legend").unwrap().unwrap();
    assert_eq!(legend.get_attribute("role").as_deref(), Some("group"));
    assert_eq!(
        legend.get_attribute("aria-label").as_deref(),
        Some("Fixture legend")
    );
    assert!(!legend.has_attribute("data-empty"));

    let items = host.query_selector_all(".pine-chart-legend-item").unwrap();
    assert_eq!(items.length(), 2);

    let first = items.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first.get_attribute("data-series").as_deref(),
        Some("Organic")
    );
    let first_label = first
        .query_selector(".pine-chart-legend-label")
        .unwrap()
        .unwrap();
    assert_eq!(first_label.text_content().as_deref(), Some("Organic"));

    let marker = host
        .query_selector(".pine-chart-legend-marker")
        .unwrap()
        .unwrap();
    assert_eq!(
        marker.get_attribute("data-series").as_deref(),
        Some("Organic")
    );

    host.query_selector("button.swap-legend")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    settle().await;

    let items = host.query_selector_all(".pine-chart-legend-item").unwrap();
    let first = items.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(first.get_attribute("data-series").as_deref(), Some("API"));
    let first_label = first
        .query_selector(".pine-chart-legend-label")
        .unwrap()
        .unwrap();
    assert_eq!(first_label.text_content().as_deref(), Some("API"));

    host.remove();
}
