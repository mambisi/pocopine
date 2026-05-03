//! Pine Charts browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine-charts`.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pine_charts::{
    ChartAreaSeries, ChartBar, ChartBarSeries, ChartLineSeries, ChartPieSlice, ChartPoint,
    ChartScatterSeries, LegendItem, CHART_SELECT_EVENT, LEGEND_TOGGLE_EVENT,
};
use pocopine::prelude::*;
use wasm_bindgen::closure::Closure;
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

fn dispatch_pointer_move(svg: &Element, x: f64, y: f64) {
    let rect = svg.get_bounding_client_rect();
    let init = web_sys::PointerEventInit::new();
    init.set_bubbles(true);
    init.set_client_x((rect.left() + x).round() as i32);
    init.set_client_y((rect.top() + y).round() as i32);
    svg.dispatch_event(
        &web_sys::PointerEvent::new_with_event_init_dict("pointermove", &init).unwrap(),
    )
    .unwrap();
}

fn dispatch_click(element: &Element) {
    element
        .dispatch_event(&web_sys::Event::new("click").unwrap())
        .unwrap();
}

fn dispatch_keydown(element: &Element, key: &str) {
    let init = web_sys::KeyboardEventInit::new();
    init.set_bubbles(true);
    init.set_key(key);
    element
        .dispatch_event(
            &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap(),
        )
        .unwrap();
}

fn assert_near(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= 0.75,
        "{label}: expected {expected}, got {actual}"
    );
}

fn assert_svg_fills_frame_without_stretching(frame: &Element, svg: &Element) {
    let frame_rect = frame.get_bounding_client_rect();
    let svg_rect = svg.get_bounding_client_rect();
    let svg_width = svg.get_attribute("width").unwrap().parse::<f64>().unwrap();
    let svg_height = svg.get_attribute("height").unwrap().parse::<f64>().unwrap();

    assert!(svg_rect.left() >= frame_rect.left() - 0.75);
    assert!(svg_rect.top() >= frame_rect.top() - 0.75);
    assert!(svg_rect.right() <= frame_rect.right() + 0.75);
    assert!(svg_rect.bottom() <= frame_rect.bottom() + 0.75);
    assert_near(svg_rect.width(), svg_width, "svg rendered width");
    assert_near(svg_rect.height(), svg_height, "svg rendered height");
}

fn listen_string_field(
    target: &Element,
    event_name: &str,
    field: &'static str,
) -> Rc<RefCell<Option<String>>> {
    let seen = Rc::new(RefCell::new(None::<String>));
    let seen_for_listener = seen.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let event: web_sys::CustomEvent = ev.dyn_into().unwrap();
        let value = js_sys::Reflect::get(&event.detail(), &wasm_bindgen::JsValue::from_str(field))
            .ok()
            .and_then(|value| value.as_string());
        *seen_for_listener.borrow_mut() = value;
    });
    target
        .add_event_listener_with_callback(event_name, cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
    seen
}

fn listen_bool_field(
    target: &Element,
    event_name: &str,
    field: &'static str,
) -> Rc<Cell<Option<bool>>> {
    let seen = Rc::new(Cell::new(None::<bool>));
    let seen_for_listener = seen.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let event: web_sys::CustomEvent = ev.dyn_into().unwrap();
        let value = js_sys::Reflect::get(&event.detail(), &wasm_bindgen::JsValue::from_str(field))
            .ok()
            .and_then(|value| value.as_bool());
        seen_for_listener.set(value);
    });
    target
        .add_event_listener_with_callback(event_name, cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
    seen
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-line-chart class="sales-chart"
                   label="Sales"
                   x_label="Week"
                   y_label="Sales"
                   width="100"
                   height="100"
                   margin_top="0"
                   margin_right="0"
                   margin_bottom="0"
                   margin_left="0"
                   show_markers="true"
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
    assert_eq!(svg.get_attribute("viewBox").as_deref(), Some("0 0 100 100"));
    assert!(
        svg.get_attribute("viewbox").is_none(),
        "lowercase viewbox is inert in SVG and breaks responsive coordinates",
    );

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

    let markers = host.query_selector_all(".pine-chart-marker").unwrap();
    assert_eq!(markers.length(), 3, "one marker per sampled point");
    let first_marker = markers.item(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(first_marker.get_attribute("cx").as_deref(), Some("0"));
    assert_eq!(first_marker.get_attribute("cy").as_deref(), Some("100"));
    assert_eq!(first_marker.get_attribute("data-x").as_deref(), Some("0"));
    assert_eq!(first_marker.get_attribute("data-y").as_deref(), Some("0"));

    let labels = host.query_selector_all(".pine-chart-tick-label").unwrap();
    assert_eq!(labels.length(), 12, "x and y tick labels render");

    let axis_labels = host.query_selector_all(".pine-chart-axis-label").unwrap();
    assert_eq!(axis_labels.length(), 2, "x and y axis labels render");
    let x_axis_label = host
        .query_selector(".pine-chart-axis-label-x")
        .unwrap()
        .unwrap();
    assert_eq!(x_axis_label.text_content().as_deref(), Some("Week"));
    assert_eq!(x_axis_label.get_attribute("x").as_deref(), Some("50"));
    assert_eq!(x_axis_label.get_attribute("y").as_deref(), Some("96"));
    let y_axis_label = host
        .query_selector(".pine-chart-axis-label-y")
        .unwrap()
        .unwrap();
    assert_eq!(y_axis_label.text_content().as_deref(), Some("Sales"));
    assert_eq!(
        y_axis_label.get_attribute("transform").as_deref(),
        Some("rotate(-90 12 50)")
    );

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-line-chart class="multi-line-chart"
                   label="Multi"
                   width="100"
                   height="100"
                   margin_top="0"
                   margin_right="0"
                   margin_bottom="0"
                   margin_left="0"
                   show_markers="true"
                   pp-bind:series="series"></pine-line-chart>
</div>
"#)]
struct MultiLineChartFixture {
    series: Vec<ChartLineSeries>,
}

impl Default for MultiLineChartFixture {
    fn default() -> Self {
        Self {
            series: vec![
                ChartLineSeries::new(
                    "Actual",
                    vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
                ),
                ChartLineSeries::new(
                    "Target",
                    vec![ChartPoint::new(0.0, 1.0), ChartPoint::new(1.0, 0.0)],
                ),
            ],
        }
    }
}

#[handlers]
impl MultiLineChartFixture {}

#[wasm_bindgen_test]
async fn multi_line_chart_renders_series_metadata_and_hover() {
    let host = mount_fixture::<MultiLineChartFixture>();
    settle().await;

    let paths = host.query_selector_all("path.pine-chart-line").unwrap();
    assert_eq!(paths.length(), 2);

    let first_path = paths.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first_path.get_attribute("data-series").as_deref(),
        Some("Actual")
    );
    assert_eq!(
        first_path.get_attribute("d").as_deref(),
        Some("M0,100 L100,0")
    );

    let second_path = paths.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        second_path.get_attribute("data-series").as_deref(),
        Some("Target")
    );
    assert_eq!(
        second_path.get_attribute("d").as_deref(),
        Some("M0,0 L100,100")
    );

    let markers = host.query_selector_all(".pine-chart-marker").unwrap();
    assert_eq!(markers.length(), 4);
    let target_marker = markers.get(2).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        target_marker.get_attribute("data-series").as_deref(),
        Some("Target")
    );

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 95.0, 95.0);
    settle().await;

    let hover_marker = host
        .query_selector(".pine-chart-hover-marker")
        .unwrap()
        .unwrap();
    assert_eq!(
        hover_marker.get_attribute("data-series").as_deref(),
        Some("Target")
    );

    let tooltip = host.query_selector(".pine-chart-tooltip").unwrap().unwrap();
    assert_eq!(
        tooltip.get_attribute("data-series").as_deref(),
        Some("Target")
    );
    assert_eq!(
        tooltip.get_attribute("aria-label").as_deref(),
        Some("Target: x 1, y 0")
    );

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-scatter-chart class="segment-scatter"
                      label="Segments"
                      x_label="Size"
                      y_label="Retention"
                      width="100"
                      height="100"
                      margin_top="0"
                      margin_right="0"
                      margin_bottom="0"
                      margin_left="0"
                      point_radius="5"
                      pp-bind:series="series"></pine-scatter-chart>
</div>
"#)]
struct ScatterChartFixture {
    series: Vec<ChartScatterSeries>,
}

impl Default for ScatterChartFixture {
    fn default() -> Self {
        Self {
            series: vec![
                ChartScatterSeries::new(
                    "Segment A",
                    vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
                ),
                ChartScatterSeries::new(
                    "Segment B",
                    vec![ChartPoint::new(0.0, 1.0), ChartPoint::new(1.0, 0.0)],
                ),
            ],
        }
    }
}

#[handlers]
impl ScatterChartFixture {}

#[wasm_bindgen_test]
async fn scatter_chart_renders_points_axes_and_hover() {
    let host = mount_fixture::<ScatterChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-scatter-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("role").as_deref(), Some("img"));
    assert_eq!(
        chart.get_attribute("aria-label").as_deref(),
        Some("Segments")
    );
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("ready"));

    let points = host
        .query_selector_all(".pine-chart-scatter-point")
        .unwrap();
    assert_eq!(points.length(), 4);
    let first = points.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first.namespace_uri().as_deref(),
        Some("http://www.w3.org/2000/svg"),
    );
    assert_eq!(first.get_attribute("cx").as_deref(), Some("0"));
    assert_eq!(first.get_attribute("cy").as_deref(), Some("100"));
    assert_eq!(first.get_attribute("r").as_deref(), Some("5"));
    assert!(first.has_attribute("data-key"));
    assert_eq!(
        first.get_attribute("data-series").as_deref(),
        Some("Segment A")
    );

    let axis_labels = host.query_selector_all(".pine-chart-axis-label").unwrap();
    assert_eq!(axis_labels.length(), 2);

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 95.0, 95.0);
    settle().await;

    let marker = host
        .query_selector(".pine-chart-hover-marker")
        .unwrap()
        .unwrap();
    assert_eq!(
        marker.get_attribute("data-series").as_deref(),
        Some("Segment B")
    );
    assert_eq!(marker.get_attribute("data-x").as_deref(), Some("1"));
    assert_eq!(marker.get_attribute("data-y").as_deref(), Some("0"));

    let tooltip = host.query_selector(".pine-chart-tooltip").unwrap().unwrap();
    assert_eq!(
        tooltip.get_attribute("aria-label").as_deref(),
        Some("Segment B: x 1, y 0")
    );

    let selected_label = listen_string_field(&chart, CHART_SELECT_EVENT, "label");
    dispatch_click(&first);
    settle().await;

    assert!(first.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));
    assert_eq!(
        selected_label.borrow().as_deref(),
        Some("Segment A: x 0, y 0")
    );

    let chart = host.query_selector(".pine-scatter-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("tabindex").as_deref(), Some("0"));
    dispatch_keydown(&chart, "ArrowRight");
    settle().await;

    let second = points.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));

    dispatch_keydown(&chart, " ");
    settle().await;

    assert!(second.has_attribute("data-selected"));
    assert!(!first.has_attribute("data-selected"));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-area-chart class="multi-area-chart"
                   label="Area"
                   width="100"
                   height="100"
                   margin_top="0"
                   margin_right="0"
                   margin_bottom="0"
                   margin_left="0"
                   show_markers="true"
                   pp-bind:series="series"></pine-area-chart>
</div>
"#)]
struct AreaChartFixture {
    series: Vec<ChartAreaSeries>,
}

impl Default for AreaChartFixture {
    fn default() -> Self {
        Self {
            series: vec![
                ChartAreaSeries::new(
                    "Actual",
                    vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
                ),
                ChartAreaSeries::new(
                    "Target",
                    vec![ChartPoint::new(0.0, 1.0), ChartPoint::new(1.0, 0.0)],
                ),
            ],
        }
    }
}

#[handlers]
impl AreaChartFixture {}

#[wasm_bindgen_test]
async fn area_chart_renders_fills_lines_and_hover_metadata() {
    let host = mount_fixture::<AreaChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-area-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("role").as_deref(), Some("img"));
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("ready"));

    let areas = host.query_selector_all(".pine-chart-area").unwrap();
    assert_eq!(areas.length(), 2);
    let first_area = areas.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first_area.get_attribute("data-series").as_deref(),
        Some("Actual")
    );
    assert_eq!(
        first_area.get_attribute("d").as_deref(),
        Some("M0,100 L0,100 L100,0 L100,100Z")
    );

    let lines = host
        .query_selector_all(".pine-area-chart .pine-chart-line")
        .unwrap();
    assert_eq!(lines.length(), 2);
    let second_line = lines.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        second_line.get_attribute("data-series").as_deref(),
        Some("Target")
    );
    assert_eq!(
        second_line.get_attribute("d").as_deref(),
        Some("M0,0 L100,100")
    );

    let markers = host
        .query_selector_all(".pine-area-chart .pine-chart-marker")
        .unwrap();
    assert_eq!(markers.length(), 4);

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    let rect = svg.get_bounding_client_rect();
    let init = web_sys::PointerEventInit::new();
    init.set_bubbles(true);
    init.set_client_x((rect.left() + 95.0).round() as i32);
    init.set_client_y((rect.top() + 95.0).round() as i32);
    svg.dispatch_event(
        &web_sys::PointerEvent::new_with_event_init_dict("pointermove", &init).unwrap(),
    )
    .unwrap();
    settle().await;

    let tooltip = host.query_selector(".pine-chart-tooltip").unwrap().unwrap();
    assert_eq!(
        tooltip.get_attribute("data-series").as_deref(),
        Some("Target")
    );
    assert_eq!(
        tooltip.get_attribute("aria-label").as_deref(),
        Some("Target: x 1, y 0")
    );

    host.remove();
}

#[wasm_bindgen_test]
async fn line_chart_shows_crosshair_and_tooltip_on_pointer_move() {
    let host = mount_fixture::<LineChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert!(!chart.has_attribute("data-hover"));

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 50.0, 50.0);
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert!(chart.has_attribute("data-hover"));

    let marker = host
        .query_selector(".pine-chart-hover-marker")
        .unwrap()
        .unwrap();
    let hover = host.query_selector(".pine-chart-hover").unwrap().unwrap();
    assert_eq!(
        hover.get_attribute("visibility").as_deref(),
        Some("visible")
    );
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
    let hover = host.query_selector(".pine-chart-hover").unwrap().unwrap();
    assert_eq!(hover.get_attribute("visibility").as_deref(), Some("hidden"));

    host.remove();
}

#[wasm_bindgen_test]
async fn line_chart_supports_marker_selection_and_keyboard_focus() {
    let host = mount_fixture::<LineChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-line-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("tabindex").as_deref(), Some("0"));
    let selected_label = listen_string_field(&chart, CHART_SELECT_EVENT, "label");

    let markers = host.query_selector_all(".pine-chart-marker").unwrap();
    assert_eq!(markers.length(), 3);
    let second = markers.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-key"));

    dispatch_click(&second);
    settle().await;

    assert!(second.has_attribute("data-focused"));
    assert!(second.has_attribute("data-selected"));
    assert_eq!(selected_label.borrow().as_deref(), Some("x 5, y 10"));

    dispatch_keydown(&chart, "ArrowRight");
    settle().await;

    let third = markers.get(2).unwrap().dyn_into::<Element>().unwrap();
    assert!(third.has_attribute("data-focused"));
    assert!(second.has_attribute("data-selected"));
    assert!(!third.has_attribute("data-selected"));

    dispatch_keydown(&chart, "Enter");
    settle().await;

    assert!(third.has_attribute("data-selected"));
    assert!(!second.has_attribute("data-selected"));
    assert_eq!(selected_label.borrow().as_deref(), Some("x 10, y 5"));

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

    assert!(
        host.query_selector("path.pine-chart-line")
            .unwrap()
            .is_none(),
        "empty charts should not render a data path",
    );

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
    assert!(first.has_attribute("data-key"));

    let labels = host.query_selector_all(".pine-chart-tick-label").unwrap();
    assert!(labels.length() >= 6, "category and value labels render");

    host.remove();
}

#[wasm_bindgen_test]
async fn bar_chart_shows_tooltip_on_pointer_move() {
    let host = mount_fixture::<BarChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-bar-chart").unwrap().unwrap();
    assert!(!chart.has_attribute("data-hover"));
    let selected_label = listen_string_field(&chart, CHART_SELECT_EVENT, "label");

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 10.0, 90.0);
    settle().await;

    let chart = host.query_selector(".pine-bar-chart").unwrap().unwrap();
    assert!(chart.has_attribute("data-hover"));

    let first = host
        .query_selector(".pine-chart-bar")
        .unwrap()
        .unwrap()
        .dyn_into::<Element>()
        .unwrap();
    assert!(first.has_attribute("data-hovered"));

    let tooltip = host
        .query_selector(".pine-chart-bar-tooltip")
        .unwrap()
        .unwrap();
    assert_eq!(tooltip.get_attribute("data-category").as_deref(), Some("A"));
    assert_eq!(tooltip.get_attribute("data-value").as_deref(), Some("2"));
    assert_eq!(tooltip.get_attribute("aria-label").as_deref(), Some("A: 2"));
    assert_eq!(
        tooltip.get_attribute("data-tooltip-x").as_deref(),
        Some("right")
    );

    dispatch_click(&first);
    settle().await;

    assert!(first.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));
    assert_eq!(selected_label.borrow().as_deref(), Some("A: 2"));

    dispatch_keydown(&chart, "ArrowRight");
    settle().await;

    let bars = host.query_selector_all(".pine-chart-bar").unwrap();
    let second = bars.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));

    dispatch_keydown(&chart, "Enter");
    settle().await;

    assert!(second.has_attribute("data-selected"));
    assert!(!first.has_attribute("data-selected"));
    assert_eq!(selected_label.borrow().as_deref(), Some("B: 10"));

    let leave = web_sys::PointerEvent::new("pointerleave").unwrap();
    svg.dispatch_event(&leave).unwrap();
    settle().await;

    let chart = host.query_selector(".pine-bar-chart").unwrap().unwrap();
    assert!(!chart.has_attribute("data-hover"));
    assert!(!first.has_attribute("data-hovered"));

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

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 30.0, 85.0);
    settle().await;

    let second = bars.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-hovered"));
    let tooltip = host
        .query_selector(".pine-chart-bar-tooltip")
        .unwrap()
        .unwrap();
    assert_eq!(
        tooltip.get_attribute("data-series").as_deref(),
        Some("Returning")
    );
    assert_eq!(tooltip.get_attribute("data-category").as_deref(), Some("A"));
    assert_eq!(tooltip.get_attribute("data-value").as_deref(), Some("3"));
    assert_eq!(
        tooltip.get_attribute("aria-label").as_deref(),
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

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 25.0, 60.0);
    settle().await;

    let second = bars.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-hovered"));
    let tooltip = host
        .query_selector(".pine-chart-bar-tooltip")
        .unwrap()
        .unwrap();
    assert_eq!(
        tooltip.get_attribute("data-series").as_deref(),
        Some("Returning")
    );
    assert_eq!(tooltip.get_attribute("data-category").as_deref(), Some("A"));
    assert_eq!(tooltip.get_attribute("data-value").as_deref(), Some("3"));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-pie-chart class="share-chart"
                  label="Share"
                  width="100"
                  height="100"
                  margin_top="0"
                  margin_right="0"
                  margin_bottom="0"
                  margin_left="0"
                  inner_radius="0.5"
                  pp-bind:center_label="center_label"
                  pp-bind:center_value="center_value"
                  pp-bind:data="data"></pine-pie-chart>
</div>
"#)]
struct PieChartFixture {
    data: Vec<ChartPieSlice>,
    center_label: String,
    center_value: String,
}

impl Default for PieChartFixture {
    fn default() -> Self {
        Self {
            data: vec![
                ChartPieSlice::new("Organic", 3.0),
                ChartPieSlice::new("Referral", 1.0),
            ],
            center_label: "Total".into(),
            center_value: "4".into(),
        }
    }
}

#[handlers]
impl PieChartFixture {}

#[wasm_bindgen_test]
async fn pie_chart_renders_donut_slices_and_selection() {
    let host = mount_fixture::<PieChartFixture>();
    settle().await;

    let chart = host.query_selector(".pine-pie-chart").unwrap().unwrap();
    assert_eq!(chart.get_attribute("role").as_deref(), Some("img"));
    assert_eq!(chart.get_attribute("aria-label").as_deref(), Some("Share"));
    assert_eq!(chart.get_attribute("data-state").as_deref(), Some("ready"));
    assert!(chart.has_attribute("data-donut"));
    assert_eq!(chart.get_attribute("tabindex").as_deref(), Some("0"));
    assert!(!chart.has_attribute("data-hover"));

    let slices = host.query_selector_all(".pine-chart-pie-slice").unwrap();
    assert_eq!(slices.length(), 2);
    let first = slices.get(0).unwrap().dyn_into::<Element>().unwrap();
    assert_eq!(
        first.namespace_uri().as_deref(),
        Some("http://www.w3.org/2000/svg"),
    );
    assert_eq!(
        first.get_attribute("data-label").as_deref(),
        Some("Organic")
    );
    assert_eq!(first.get_attribute("data-value").as_deref(), Some("3"));
    assert_eq!(
        first.get_attribute("aria-label").as_deref(),
        Some("Organic: 3 (75%)")
    );

    let center_value = host
        .query_selector(".pine-chart-center-value")
        .unwrap()
        .unwrap();
    assert_eq!(center_value.text_content().as_deref(), Some("4"));
    assert_eq!(center_value.get_attribute("x").as_deref(), Some("50"));
    assert_eq!(center_value.get_attribute("y").as_deref(), Some("40"));
    let center_label = host
        .query_selector(".pine-chart-center-label")
        .unwrap()
        .unwrap();
    assert_eq!(center_label.text_content().as_deref(), Some("Total"));
    assert_eq!(center_label.get_attribute("x").as_deref(), Some("50"));
    assert_eq!(center_label.get_attribute("y").as_deref(), Some("66"));

    let svg = host.query_selector("svg.pine-chart-svg").unwrap().unwrap();
    dispatch_pointer_move(&svg, 50.0, 10.0);
    settle().await;

    assert!(first.has_attribute("data-hovered"));
    let tooltip = host
        .query_selector(".pine-chart-pie-tooltip")
        .unwrap()
        .unwrap();
    assert_eq!(
        tooltip.get_attribute("data-label").as_deref(),
        Some("Organic")
    );
    assert_eq!(tooltip.get_attribute("data-value").as_deref(), Some("3"));
    assert_eq!(
        tooltip.get_attribute("data-percentage").as_deref(),
        Some("75")
    );

    let selected_label = listen_string_field(&chart, CHART_SELECT_EVENT, "label");
    dispatch_click(&first);
    settle().await;

    assert!(first.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));
    assert_eq!(selected_label.borrow().as_deref(), Some("Organic: 3 (75%)"));

    dispatch_keydown(&chart, "ArrowRight");
    settle().await;
    let second = slices.get(1).unwrap().dyn_into::<Element>().unwrap();
    assert!(second.has_attribute("data-focused"));
    assert!(first.has_attribute("data-selected"));

    dispatch_keydown(&chart, "Enter");
    settle().await;

    assert!(second.has_attribute("data-selected"));
    assert!(!first.has_attribute("data-selected"));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <button class="swap-legend" @click="swap">Swap</button>
  <pine-chart-legend class="fixture-legend"
                     label="Fixture legend"
                     interactive="true"
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
    assert_eq!(first.get_attribute("tabindex").as_deref(), Some("0"));
    assert_eq!(first.get_attribute("aria-pressed").as_deref(), Some("true"));

    let active = listen_bool_field(&legend, LEGEND_TOGGLE_EVENT, "active");
    dispatch_keydown(&first, "Enter");
    settle().await;

    assert_eq!(active.get(), Some(false));
    assert!(!first.has_attribute("data-active"));
    assert_eq!(
        first.get_attribute("aria-pressed").as_deref(),
        Some("false")
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

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="responsive-host" style="width: 320px">
  <pine-chart-responsive class="responsive-chart" aspect_ratio="2">
    <pine-line-chart class="responsive-line"
                     label="Responsive"
                     x_label="Week"
                     y_label="Value"
                     width="10"
                     height="10"
                     margin_top="0"
                     margin_right="0"
                     margin_bottom="0"
                     margin_left="0"
                     pp-bind:points="points"></pine-line-chart>
  </pine-chart-responsive>
</div>
"#)]
struct ResponsiveChartFixture {
    points: Vec<ChartPoint>,
}

impl Default for ResponsiveChartFixture {
    fn default() -> Self {
        Self {
            points: vec![
                ChartPoint::new(0.0, 1.0),
                ChartPoint::new(1.0, 2.0),
                ChartPoint::new(2.0, 4.0),
            ],
        }
    }
}

#[handlers]
impl ResponsiveChartFixture {}

#[wasm_bindgen_test]
async fn responsive_container_resizes_child_chart_props() {
    let host = mount_fixture::<ResponsiveChartFixture>();
    settle().await;
    sleep_ms(50).await;
    settle().await;

    let container = host.query_selector(".responsive-chart").unwrap().unwrap();
    assert_eq!(
        container.get_attribute("data-ready").as_deref(),
        Some("true")
    );
    assert_eq!(
        container.get_attribute("data-width").as_deref(),
        Some("320")
    );
    assert_eq!(
        container.get_attribute("data-height").as_deref(),
        Some("160")
    );

    let svg = host
        .query_selector(".responsive-line svg.pine-chart-svg")
        .unwrap()
        .unwrap();
    assert_eq!(svg.get_attribute("width").as_deref(), Some("320"));
    assert_eq!(svg.get_attribute("height").as_deref(), Some("160"));
    assert_eq!(svg.get_attribute("viewBox").as_deref(), Some("0 0 320 160"));

    let parent = host.query_selector(".responsive-host").unwrap().unwrap();
    parent.set_attribute("style", "width: 480px").unwrap();
    sleep_ms(50).await;
    settle().await;

    assert_eq!(
        container.get_attribute("data-width").as_deref(),
        Some("480")
    );
    assert_eq!(
        container.get_attribute("data-height").as_deref(),
        Some("240")
    );
    assert_eq!(svg.get_attribute("width").as_deref(), Some("480"));
    assert_eq!(svg.get_attribute("height").as_deref(), Some("240"));
    assert_eq!(svg.get_attribute("viewBox").as_deref(), Some("0 0 480 240"));

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="padded-responsive-host" style="width: 360px">
  <style>
    .padded-responsive {
      box-sizing: border-box;
      padding: 20px;
      border: 1px solid transparent;
    }
  </style>
  <pine-chart-responsive class="padded-responsive"
                         aspect_ratio="2"
                         min_height="120">
    <pine-line-chart class="padded-responsive-line"
                     label="Padded responsive"
                     x_label="Week"
                     y_label="Value"
                     width="10"
                     height="10"
                     margin_top="0"
                     margin_right="0"
                     margin_bottom="0"
                     margin_left="0"
                     pp-bind:points="points"></pine-line-chart>
  </pine-chart-responsive>
</div>
"#)]
struct PaddedResponsiveChartFixture {
    points: Vec<ChartPoint>,
}

impl Default for PaddedResponsiveChartFixture {
    fn default() -> Self {
        Self {
            points: vec![
                ChartPoint::new(0.0, 1.0),
                ChartPoint::new(1.0, 2.0),
                ChartPoint::new(2.0, 4.0),
            ],
        }
    }
}

#[handlers]
impl PaddedResponsiveChartFixture {}

#[wasm_bindgen_test]
async fn responsive_container_sizes_to_inner_frame_when_padded() {
    let host = mount_fixture::<PaddedResponsiveChartFixture>();
    settle().await;
    sleep_ms(50).await;
    settle().await;

    let container = host.query_selector(".padded-responsive").unwrap().unwrap();
    assert_eq!(
        container.get_attribute("data-ready").as_deref(),
        Some("true")
    );
    assert_eq!(
        container.get_attribute("data-width").as_deref(),
        Some("318")
    );
    assert_eq!(
        container.get_attribute("data-height").as_deref(),
        Some("159")
    );

    let frame = host
        .query_selector(".padded-responsive .pine-chart-responsive-frame")
        .unwrap()
        .unwrap();
    let svg = host
        .query_selector(".padded-responsive-line svg.pine-chart-svg")
        .unwrap()
        .unwrap();
    assert_eq!(svg.get_attribute("width").as_deref(), Some("318"));
    assert_eq!(svg.get_attribute("height").as_deref(), Some("159"));
    assert_eq!(svg.get_attribute("viewBox").as_deref(), Some("0 0 318 159"));

    let frame_rect = frame.get_bounding_client_rect();
    let svg_rect = svg.get_bounding_client_rect();
    assert!(svg_rect.left() >= frame_rect.left() - 0.5);
    assert!(svg_rect.top() >= frame_rect.top() - 0.5);
    assert!(svg_rect.right() <= frame_rect.right() + 0.5);
    assert!(svg_rect.bottom() <= frame_rect.bottom() + 0.5);

    host.remove();
}

#[derive(serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="demo-responsive-shell" style="width: 900px">
  <style>
    .demo-responsive-shell {
      display: block;
    }

    .demo-chart-panel {
      box-sizing: border-box;
      border: 1px solid transparent;
      padding: 20px;
      position: relative;
    }

    .demo-chart-panel--radial {
      margin-inline: auto;
    }

    .pine-chart-responsive-frame {
      min-width: 0;
    }

    .pine-chart-responsive-frame > pine-line-chart,
    .pine-chart-responsive-frame > pine-pie-chart,
    .demo-chart-panel .pine-line-chart,
    .demo-chart-panel .pine-pie-chart {
      display: block;
      height: 100%;
      position: relative;
      width: 100%;
    }

    .demo-chart-panel .pine-chart-svg {
      display: block;
      height: auto;
      max-width: 100%;
      overflow: visible;
    }
  </style>
  <pine-chart-responsive class="demo-chart-panel demo-wide-chart"
                         aspect_ratio="2"
                         min_height="220">
    <pine-line-chart class="demo-line-chart"
                     label="Wide"
                     x_label="Week"
                     y_label="Value"
                     margin_top="0"
                     margin_right="0"
                     margin_bottom="0"
                     margin_left="0"
                     pp-bind:points="points"></pine-line-chart>
  </pine-chart-responsive>
  <pine-chart-responsive class="demo-chart-panel demo-chart-panel--radial demo-radial-chart"
                         width="min(520px, 100%)"
                         aspect_ratio="1"
                         min_height="260">
    <pine-pie-chart class="demo-half-donut"
                    label="Goal progress"
                    margin_top="0"
                    margin_right="0"
                    margin_bottom="0"
                    margin_left="0"
                    inner_radius="0.58"
                    start_angle="180"
                    end_angle="360"
                    center_label="Progress"
                    center_value="74%"
                    pp-bind:data="slices"></pine-pie-chart>
  </pine-chart-responsive>
</div>
"#)]
struct DemoResponsiveChartFixture {
    points: Vec<ChartPoint>,
    slices: Vec<ChartPieSlice>,
}

impl Default for DemoResponsiveChartFixture {
    fn default() -> Self {
        Self {
            points: vec![
                ChartPoint::new(0.0, 1.0),
                ChartPoint::new(1.0, 2.0),
                ChartPoint::new(2.0, 4.0),
            ],
            slices: vec![
                ChartPieSlice::new("Done", 74.0),
                ChartPieSlice::new("Remaining", 26.0),
            ],
        }
    }
}

#[handlers]
impl DemoResponsiveChartFixture {}

#[wasm_bindgen_test]
async fn demo_responsive_charts_stay_contained_and_unstretched() {
    let host = mount_fixture::<DemoResponsiveChartFixture>();
    settle().await;
    sleep_ms(50).await;
    settle().await;

    let shell = host
        .query_selector(".demo-responsive-shell")
        .unwrap()
        .unwrap();
    let wide = host.query_selector(".demo-wide-chart").unwrap().unwrap();
    assert_eq!(wide.get_attribute("data-width").as_deref(), Some("858"));
    assert_eq!(wide.get_attribute("data-height").as_deref(), Some("429"));
    let wide_frame = wide
        .query_selector(".pine-chart-responsive-frame")
        .unwrap()
        .unwrap();
    let wide_svg = wide
        .query_selector(".demo-line-chart svg.pine-chart-svg")
        .unwrap()
        .unwrap();
    assert_eq!(wide_svg.get_attribute("width").as_deref(), Some("858"));
    assert_eq!(wide_svg.get_attribute("height").as_deref(), Some("429"));
    assert_svg_fills_frame_without_stretching(&wide_frame, &wide_svg);

    let radial = host.query_selector(".demo-radial-chart").unwrap().unwrap();
    let shell_rect = shell.get_bounding_client_rect();
    let radial_rect = radial.get_bounding_client_rect();
    assert_near(radial_rect.width(), 520.0, "radial panel width");
    assert_near(
        radial_rect.left() + radial_rect.width() * 0.5,
        shell_rect.left() + shell_rect.width() * 0.5,
        "radial panel center",
    );
    assert_eq!(radial.get_attribute("data-width").as_deref(), Some("478"));
    assert_eq!(radial.get_attribute("data-height").as_deref(), Some("478"));
    let radial_frame = radial
        .query_selector(".pine-chart-responsive-frame")
        .unwrap()
        .unwrap();
    let radial_svg = radial
        .query_selector(".demo-half-donut svg.pine-chart-svg")
        .unwrap()
        .unwrap();
    assert_svg_fills_frame_without_stretching(&radial_frame, &radial_svg);

    let center_label = radial
        .query_selector(".pine-chart-center-label")
        .unwrap()
        .unwrap();
    assert_eq!(center_label.text_content().as_deref(), Some("Progress"));
    assert_eq!(center_label.get_attribute("y").as_deref(), Some("239"));

    shell.set_attribute("style", "width: 260px").unwrap();
    sleep_ms(50).await;
    settle().await;

    assert_eq!(wide.get_attribute("data-width").as_deref(), Some("218"));
    assert_eq!(wide.get_attribute("data-height").as_deref(), Some("220"));
    assert_svg_fills_frame_without_stretching(&wide_frame, &wide_svg);
    assert_eq!(radial.get_attribute("data-width").as_deref(), Some("218"));
    assert_eq!(radial.get_attribute("data-height").as_deref(), Some("260"));
    assert_svg_fills_frame_without_stretching(&radial_frame, &radial_svg);
    assert_eq!(center_label.get_attribute("y").as_deref(), Some("130"));

    host.remove();
}
