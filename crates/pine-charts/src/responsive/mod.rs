use js_sys::{Array, Reflect};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{Element, ResizeObserver, ResizeObserverEntry};

const WIDTH_FIELD: &str = "width";
const HEIGHT_FIELD: &str = "height";
const EPSILON: f64 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponsiveChartSize {
    pub width: f64,
    pub height: f64,
}

impl ResponsiveChartSize {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartResponsive.poco", role = "panel")]
#[slot(default, accepts = [
    crate::area::PineAreaChart,
    crate::bar::PineBarChart,
    crate::layered::PineLayerChart,
    crate::line::PineLineChart,
    crate::pie::PinePieChart,
    crate::scatter::PineScatterChart,
])]
pub struct PineChartResponsive {
    #[prop]
    pub width: String,
    #[prop]
    pub height: String,
    #[prop]
    pub aspect_ratio: f64,
    #[prop]
    pub min_width: f64,
    #[prop]
    pub min_height: f64,
    #[prop]
    pub style: String,
    pub container_style: String,
    pub frame_style: String,
    pub measured_width: f64,
    pub measured_height: f64,
    pub ready: bool,
}

impl Default for PineChartResponsive {
    fn default() -> Self {
        let mut this = Self {
            width: "100%".into(),
            height: String::new(),
            aspect_ratio: 2.0,
            min_width: 0.0,
            min_height: 0.0,
            style: String::new(),
            container_style: String::new(),
            frame_style: String::new(),
            measured_width: 0.0,
            measured_height: 0.0,
            ready: false,
        };
        this.recompute_style();
        this
    }
}

#[handlers]
impl PineChartResponsive {
    fn on_setup(&mut self) {
        self.recompute_style();
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(root) = refs.get("root") else {
            return;
        };
        let Some(frame) = refs.get("frame") else {
            return;
        };

        ensure_host_layout(&root);
        let frame_for_initial = frame.clone();
        let handle_for_initial = handle.clone();
        pocopine::tick::next(move || {
            let (width, height) = measured_size(&frame_for_initial);
            handle_for_initial.update(|chart| {
                chart.recompute_style();
                chart.apply_measured_size(&frame_for_initial, width, height);
            });
        });

        let frame_for_observer = frame.clone();
        let handle_for_observer = handle.clone();
        let closure = Closure::wrap(Box::new(move |entries: JsValue, _observer: JsValue| {
            let (width, height) =
                observer_size(&entries).unwrap_or_else(|| measured_size(&frame_for_observer));
            handle_for_observer.update(|chart| {
                chart.apply_measured_size(&frame_for_observer, width, height);
            });
        }) as Box<dyn FnMut(JsValue, JsValue)>);

        let Ok(observer) = ResizeObserver::new(closure.as_ref().unchecked_ref()) else {
            return;
        };
        observer.observe(&frame);

        pocopine::on_scope_unmount(move || {
            observer.disconnect();
            drop(closure);
        });
    }

    #[watch(width)]
    fn on_width(&mut self, _: String, _: Option<String>) {
        self.recompute_style();
    }

    #[watch(height)]
    fn on_height(&mut self, _: String, _: Option<String>) {
        self.recompute_style();
    }

    #[watch(aspect_ratio)]
    fn on_aspect_ratio(&mut self, _: f64, _: Option<f64>) {
        self.recompute_style();
    }

    #[watch(min_width)]
    fn on_min_width(&mut self, _: f64, _: Option<f64>) {
        self.recompute_style();
    }

    #[watch(min_height)]
    fn on_min_height(&mut self, _: f64, _: Option<f64>) {
        self.recompute_style();
    }

    #[watch(style)]
    fn on_style(&mut self, _: String, _: Option<String>) {
        self.recompute_style();
    }
}

impl PineChartResponsive {
    pub fn resolve_size(
        &self,
        measured_width: f64,
        measured_height: f64,
    ) -> Option<ResponsiveChartSize> {
        let measured_height = if self.derives_height_from_aspect() {
            0.0
        } else {
            measured_height
        };
        resolve_size(
            measured_width,
            measured_height,
            self.aspect_ratio,
            self.min_width,
            self.min_height,
        )
    }

    fn apply_measured_size(&mut self, root: &Element, measured_width: f64, measured_height: f64) {
        let Some(size) = self.resolve_size(measured_width, measured_height) else {
            self.ready = false;
            self.recompute_style();
            return;
        };

        if self.ready
            && near_eq(self.measured_width, size.width)
            && near_eq(self.measured_height, size.height)
        {
            return;
        }

        self.measured_width = size.width;
        self.measured_height = size.height;
        self.ready = true;
        self.recompute_style();
        apply_child_size(root, size);
    }

    fn recompute_style(&mut self) {
        let resolved_height = if self.ready && self.derives_height_from_aspect() {
            Some(self.measured_height)
        } else {
            None
        };
        self.container_style = responsive_container_style(&self.width, self.min_width, &self.style);
        self.frame_style = responsive_frame_style(
            &self.height,
            self.aspect_ratio,
            self.min_height,
            resolved_height,
        );
    }

    fn derives_height_from_aspect(&self) -> bool {
        let height = self.height.trim();
        (height.is_empty() || height == "auto")
            && self.aspect_ratio.is_finite()
            && self.aspect_ratio > 0.0
    }
}

fn responsive_container_style(width: &str, min_width: f64, author_style: &str) -> String {
    let mut parts = vec![
        "display: block".to_string(),
        format!("width: {}", css_length_or(width, "100%")),
    ];

    if min_width.is_finite() && min_width > 0.0 {
        parts.push(format!("min-width: {}px", trim_float(min_width)));
    }

    let generated = parts.join("; ");
    merge_style(&generated, author_style)
}

fn responsive_frame_style(
    height: &str,
    aspect_ratio: f64,
    min_height: f64,
    resolved_height: Option<f64>,
) -> String {
    responsive_frame_style_inner(height, aspect_ratio, min_height, resolved_height)
}

fn responsive_frame_style_inner(
    height: &str,
    aspect_ratio: f64,
    min_height: f64,
    resolved_height: Option<f64>,
) -> String {
    let mut parts = vec!["display: block".to_string(), "width: 100%".to_string()];

    let height = height.trim();
    if let Some(resolved_height) =
        resolved_height.filter(|height| height.is_finite() && *height > 0.0)
    {
        parts.push(format!("height: {}px", trim_float(resolved_height)));
    } else if !height.is_empty() && height != "auto" {
        parts.push(format!("height: {height}"));
    } else if aspect_ratio.is_finite() && aspect_ratio > 0.0 {
        parts.push(format!("aspect-ratio: {}", trim_float(aspect_ratio)));
    }

    if min_height.is_finite() && min_height > 0.0 {
        parts.push(format!("min-height: {}px", trim_float(min_height)));
    }

    parts.join("; ")
}

pub fn resolve_size(
    measured_width: f64,
    measured_height: f64,
    aspect_ratio: f64,
    min_width: f64,
    min_height: f64,
) -> Option<ResponsiveChartSize> {
    let mut width = usable_dimension(measured_width);
    let measured_height = usable_dimension(measured_height);
    let min_width = usable_dimension(min_width).unwrap_or(0.0);
    let min_height = usable_dimension(min_height).unwrap_or(0.0);

    if let Some(value) = width {
        width = Some(value.max(min_width));
    } else if min_width > 0.0 {
        width = Some(min_width);
    }

    let width = width?;
    let height = match measured_height {
        Some(value) => value,
        None if aspect_ratio.is_finite() && aspect_ratio > 0.0 => width / aspect_ratio,
        None if min_height > 0.0 => min_height,
        None => return None,
    }
    .max(min_height);

    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some(ResponsiveChartSize::new(round_px(width), round_px(height)))
}

pub fn responsive_style(
    width: &str,
    height: &str,
    aspect_ratio: f64,
    min_width: f64,
    min_height: f64,
    author_style: &str,
) -> String {
    responsive_style_with_resolved_height(
        width,
        height,
        aspect_ratio,
        min_width,
        min_height,
        author_style,
        None,
    )
}

fn responsive_style_with_resolved_height(
    width: &str,
    height: &str,
    aspect_ratio: f64,
    min_width: f64,
    min_height: f64,
    author_style: &str,
    resolved_height: Option<f64>,
) -> String {
    let mut parts = vec![
        "display: block".to_string(),
        format!("width: {}", css_length_or(width, "100%")),
    ];

    let height = height.trim();
    if let Some(resolved_height) =
        resolved_height.filter(|height| height.is_finite() && *height > 0.0)
    {
        parts.push(format!("height: {}px", trim_float(resolved_height)));
    } else if !height.is_empty() && height != "auto" {
        parts.push(format!("height: {height}"));
    } else if aspect_ratio.is_finite() && aspect_ratio > 0.0 {
        parts.push(format!("aspect-ratio: {}", trim_float(aspect_ratio)));
    }

    if min_width.is_finite() && min_width > 0.0 {
        parts.push(format!("min-width: {}px", trim_float(min_width)));
    }
    if min_height.is_finite() && min_height > 0.0 {
        parts.push(format!("min-height: {}px", trim_float(min_height)));
    }

    let generated = parts.join("; ");
    merge_style(&generated, author_style)
}

fn apply_child_size(root: &Element, size: ResponsiveChartSize) {
    let Some(child) = root.children().item(0) else {
        return;
    };

    let wrote_width = write_child_prop(&child, WIDTH_FIELD, size.width);
    let wrote_height = write_child_prop(&child, HEIGHT_FIELD, size.height);

    if !wrote_width {
        let _ = child.set_attribute(WIDTH_FIELD, &trim_float(size.width));
    }
    if !wrote_height {
        let _ = child.set_attribute(HEIGHT_FIELD, &trim_float(size.height));
    }

    if !wrote_width || !wrote_height {
        if let Some(svg) = chart_svg(&child) {
            if !wrote_width {
                let _ = svg.set_attribute(WIDTH_FIELD, &trim_float(size.width));
            }
            if !wrote_height {
                let _ = svg.set_attribute(HEIGHT_FIELD, &trim_float(size.height));
            }
        }
    }
}

fn write_child_prop(child: &Element, field: &str, value: f64) -> bool {
    let Some((scope_id, proxy)) = pocopine_core::mount::child_component_scope(child) else {
        return false;
    };
    let can_write = pocopine::Scope::find(scope_id)
        .map(|scope| scope.state.borrow().is_prop(field))
        .unwrap_or(false);
    if !can_write {
        return false;
    }

    Reflect::set(&proxy, &JsValue::from_str(field), &JsValue::from_f64(value)).is_ok()
}

fn chart_svg(child: &Element) -> Option<Element> {
    if child.local_name() == "svg" {
        return Some(child.clone());
    }
    child
        .query_selector("svg.pine-chart-svg, svg")
        .ok()
        .flatten()
}

fn observer_size(entries: &JsValue) -> Option<(f64, f64)> {
    let entries = entries.clone().dyn_into::<Array>().ok()?;
    let entry = entries.get(0).dyn_into::<ResizeObserverEntry>().ok()?;
    let rect = entry.content_rect();
    if rect.width() > 0.0 {
        Some((rect.width(), rect.height()))
    } else {
        None
    }
}

fn ensure_host_layout(root: &Element) {
    let Some(host) = root.parent_element() else {
        return;
    };
    let style = host.get_attribute("style").unwrap_or_default();
    if style.to_ascii_lowercase().contains("display") {
        return;
    }
    let _ = host.set_attribute("style", &merge_style("display: block", &style));
}

fn measured_size(root: &Element) -> (f64, f64) {
    let mut current = Some(root.clone());
    while let Some(el) = current {
        let rect = el.get_bounding_client_rect();
        if rect.width() > 0.0 {
            return (rect.width(), rect.height());
        }
        current = el.parent_element();
    }
    (0.0, 0.0)
}

fn usable_dimension(value: f64) -> Option<f64> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

fn round_px(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn near_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPSILON
}

fn css_length_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn merge_style(generated: &str, author_style: &str) -> String {
    let author_style = author_style.trim();
    if author_style.is_empty() {
        generated.to_string()
    } else {
        format!(
            "{}; {}",
            generated.trim_end_matches(';').trim(),
            author_style.trim_start_matches(';').trim()
        )
    }
}

fn trim_float(value: f64) -> String {
    let rounded = round_px(value);
    let mut text = rounded.to_string();
    if text.ends_with(".0") {
        text.truncate(text.len() - 2);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_height_from_aspect_ratio() {
        assert_eq!(
            resolve_size(320.0, 0.0, 2.0, 0.0, 0.0),
            Some(ResponsiveChartSize::new(320.0, 160.0))
        );
    }

    #[test]
    fn prefers_measured_height_when_available() {
        assert_eq!(
            resolve_size(320.0, 180.0, 2.0, 0.0, 0.0),
            Some(ResponsiveChartSize::new(320.0, 180.0))
        );
    }

    #[test]
    fn clamps_to_minimum_dimensions() {
        assert_eq!(
            resolve_size(120.0, 30.0, 2.0, 200.0, 100.0),
            Some(ResponsiveChartSize::new(200.0, 100.0))
        );
    }

    #[test]
    fn min_height_does_not_replace_aspect_height() {
        assert_eq!(
            resolve_size(480.0, 0.0, 2.0, 0.0, 120.0),
            Some(ResponsiveChartSize::new(480.0, 240.0))
        );
    }

    #[test]
    fn style_keeps_author_overrides_last() {
        assert_eq!(
            responsive_style("100%", "", 2.0, 120.0, 80.0, "height: 240px"),
            "display: block; width: 100%; aspect-ratio: 2; min-width: 120px; min-height: 80px; height: 240px"
        );
    }

    #[test]
    fn component_aspect_height_ignores_child_feedback_height() {
        let chart = PineChartResponsive::default();
        assert_eq!(
            chart.resolve_size(320.0, 185.0),
            Some(ResponsiveChartSize::new(320.0, 160.0))
        );
    }

    #[test]
    fn resolved_aspect_height_becomes_explicit_css_height() {
        let mut chart = PineChartResponsive {
            ready: true,
            measured_width: 320.0,
            measured_height: 160.0,
            ..Default::default()
        };
        chart.recompute_style();

        assert!(chart.frame_style.contains("height: 160px"));
        assert!(!chart.frame_style.contains("aspect-ratio"));
        assert!(!chart.container_style.contains("height"));
    }
}
