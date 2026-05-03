use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::path::line_path;
use crate::scale::LinearScale;
use crate::svg::{format_tick, SvgLine, SvgTickLabel};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartPoint {
    pub x: f64,
    pub y: f64,
}

impl ChartPoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn validate(self) -> ChartResult<Self> {
        Ok(Self {
            x: finite("point.x", self.x)?,
            y: finite("point.y", self.y)?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartLineSeries {
    pub label: String,
    pub data: Vec<ChartPoint>,
}

impl ChartLineSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartPoint>) -> Self {
        Self {
            label: label.into(),
            data,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineChartOptions {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub x_domain: Option<(f64, f64)>,
    pub y_domain: Option<(f64, f64)>,
}

impl Default for LineChartOptions {
    fn default() -> Self {
        Self {
            width: 640.0,
            height: 320.0,
            margins: ChartMargins::default(),
            x_domain: None,
            y_domain: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineChartGeometry {
    pub view_box: String,
    pub line_d: String,
    pub series: Vec<LineChartSeriesRender>,
    pub plot: ChartRect,
    pub samples: Vec<LineChartSample>,
    pub x_ticks: Vec<crate::Tick>,
    pub y_ticks: Vec<crate::Tick>,
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
}

impl LineChartGeometry {
    pub fn new(points: &[ChartPoint], options: &LineChartOptions) -> ChartResult<Self> {
        Self::from_series(&[ChartLineSeries::new("", points.to_vec())], options)
    }

    pub fn from_series(
        series: &[ChartLineSeries],
        options: &LineChartOptions,
    ) -> ChartResult<Self> {
        if series.is_empty() || series.iter().any(|series| series.data.is_empty()) {
            return Err(ChartError::EmptySeries);
        }

        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let normalized = normalize_line_series(series)?;
        let x_domain = domain_or_extent(
            options.x_domain,
            normalized
                .iter()
                .flat_map(|series| series.data.iter().map(|point| point.x)),
        )?;
        let y_domain = domain_or_extent(
            options.y_domain,
            normalized
                .iter()
                .flat_map(|series| series.data.iter().map(|point| point.y)),
        )?;
        let x_scale = LinearScale::new(x_domain, (plot.x, plot.right()))?;
        let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
        let series = normalized
            .iter()
            .enumerate()
            .map(|(series_index, series)| {
                let samples = series
                    .data
                    .iter()
                    .enumerate()
                    .map(|(point_index, point)| {
                        let x = x_scale.map(point.x)?;
                        let y = y_scale.map(point.y)?;
                        Ok(LineChartSample {
                            key: format!(
                                "series-{series_index}-point-{point_index}-{}",
                                format_tick(point.x)
                            ),
                            series_label: series.label.clone(),
                            data_x: point.x,
                            data_y: point.y,
                            x,
                            y,
                            x_label: format_tick(point.x),
                            y_label: format_tick(point.y),
                            aria_label: sample_aria_label(&series.label, point.x, point.y),
                        })
                    })
                    .collect::<ChartResult<Vec<_>>>()?;
                Ok(LineChartSeriesRender {
                    key: format!("line-series-{series_index}-{}", series.label),
                    label: series.label.clone(),
                    line_d: line_path(samples.iter().map(LineChartSample::point))?,
                    samples,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;
        let samples = series
            .iter()
            .flat_map(|series| series.samples.iter().cloned())
            .collect::<Vec<_>>();

        let x_ticks = x_scale.ticks(5);
        let y_ticks = y_scale.ticks(5);
        let line_d = series
            .first()
            .map(|series| series.line_d.clone())
            .unwrap_or_default();

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            line_d,
            series,
            plot,
            samples,
            x_grid: grid_lines_for_x(&x_ticks, plot),
            y_grid: grid_lines_for_y(&y_ticks, plot),
            x_tick_labels: tick_labels_for_x(&x_ticks, plot),
            y_tick_labels: tick_labels_for_y(&y_ticks, plot),
            x_axis: SvgLine::new(
                "x-axis".into(),
                plot.x,
                plot.bottom(),
                plot.right(),
                plot.bottom(),
            ),
            y_axis: SvgLine::new("y-axis".into(), plot.x, plot.y, plot.x, plot.bottom()),
            x_ticks,
            y_ticks,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LineChartSeriesRender {
    pub key: String,
    pub label: String,
    pub line_d: String,
    pub samples: Vec<LineChartSample>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LineChartSample {
    pub key: String,
    pub series_label: String,
    pub data_x: f64,
    pub data_y: f64,
    pub x: f64,
    pub y: f64,
    pub x_label: String,
    pub y_label: String,
    pub aria_label: String,
}

impl LineChartSample {
    fn point(&self) -> Point {
        Point {
            x: self.x,
            y: self.y,
        }
    }
}

pub fn line_legend_items(series: &[ChartLineSeries]) -> Vec<crate::LegendItem> {
    series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let label = legend_line_series_label(series, index);
            crate::LegendItem::with_series(
                format!("line-series-{index}-{label}"),
                label.clone(),
                label,
            )
        })
        .collect()
}

pub fn nearest_line_sample(samples: &[LineChartSample], svg_x: f64) -> Option<&LineChartSample> {
    let svg_x = finite("hover.x", svg_x).ok()?;
    samples.iter().min_by(|a, b| {
        let a_distance = (a.x - svg_x).abs();
        let b_distance = (b.x - svg_x).abs();
        a_distance
            .partial_cmp(&b_distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

pub fn nearest_line_sample_at(
    samples: &[LineChartSample],
    svg_point: Point,
) -> Option<&LineChartSample> {
    let svg_point = Point::new(svg_point.x, svg_point.y).ok()?;
    samples.iter().min_by(|a, b| {
        let a_distance = squared_distance(a.x, a.y, svg_point.x, svg_point.y);
        let b_distance = squared_distance(b.x, b.y, svg_point.x, svg_point.y);
        a_distance
            .partial_cmp(&b_distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn squared_distance(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    dx * dx + dy * dy
}

fn grid_lines_for_x(ticks: &[crate::Tick], plot: ChartRect) -> Vec<SvgLine> {
    ticks
        .iter()
        .enumerate()
        .map(|(index, tick)| {
            SvgLine::new(
                format!("x-grid-{index}-{}", format_tick(tick.value)),
                tick.position,
                plot.y,
                tick.position,
                plot.bottom(),
            )
        })
        .collect()
}

fn grid_lines_for_y(ticks: &[crate::Tick], plot: ChartRect) -> Vec<SvgLine> {
    ticks
        .iter()
        .enumerate()
        .map(|(index, tick)| {
            SvgLine::new(
                format!("y-grid-{index}-{}", format_tick(tick.value)),
                plot.x,
                tick.position,
                plot.right(),
                tick.position,
            )
        })
        .collect()
}

fn tick_labels_for_x(ticks: &[crate::Tick], plot: ChartRect) -> Vec<SvgTickLabel> {
    ticks
        .iter()
        .enumerate()
        .map(|(index, tick)| SvgTickLabel {
            key: format!("x-tick-{index}-{}", format_tick(tick.value)),
            value: tick.value,
            label: format_tick(tick.value),
            x: tick.position,
            y: plot.bottom() + 18.0,
            line_x1: tick.position,
            line_y1: plot.bottom(),
            line_x2: tick.position,
            line_y2: plot.bottom() + 6.0,
        })
        .collect()
}

fn tick_labels_for_y(ticks: &[crate::Tick], plot: ChartRect) -> Vec<SvgTickLabel> {
    ticks
        .iter()
        .enumerate()
        .map(|(index, tick)| SvgTickLabel {
            key: format!("y-tick-{index}-{}", format_tick(tick.value)),
            value: tick.value,
            label: format_tick(tick.value),
            x: plot.x - 8.0,
            y: tick.position + 4.0,
            line_x1: plot.x - 6.0,
            line_y1: tick.position,
            line_x2: plot.x,
            line_y2: tick.position,
        })
        .collect()
}

fn validate_points(points: &[ChartPoint]) -> ChartResult<Vec<ChartPoint>> {
    points.iter().copied().map(ChartPoint::validate).collect()
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedLineSeries {
    label: String,
    data: Vec<ChartPoint>,
}

fn normalize_line_series(series: &[ChartLineSeries]) -> ChartResult<Vec<NormalizedLineSeries>> {
    series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            if series.data.is_empty() {
                return Err(ChartError::EmptySeries);
            }
            Ok(NormalizedLineSeries {
                label: line_series_label(series, index),
                data: validate_points(&series.data)?,
            })
        })
        .collect()
}

fn line_series_label(series: &ChartLineSeries, _index: usize) -> String {
    series.label.clone()
}

fn legend_line_series_label(series: &ChartLineSeries, index: usize) -> String {
    if series.label.is_empty() {
        format!("Series {}", index + 1)
    } else {
        series.label.clone()
    }
}

fn sample_aria_label(series: &str, data_x: f64, data_y: f64) -> String {
    let point_label = format!("x {}, y {}", format_tick(data_x), format_tick(data_y));
    if series.is_empty() {
        point_label
    } else {
        format!("{series}: {point_label}")
    }
}

fn domain_or_extent(
    domain: Option<(f64, f64)>,
    values: impl IntoIterator<Item = f64>,
) -> ChartResult<(f64, f64)> {
    if let Some((start, end)) = domain {
        return expanded_domain(finite("domain.start", start)?, finite("domain.end", end)?);
    }

    let mut iter = values.into_iter();
    let Some(first) = iter.next() else {
        return Err(ChartError::EmptySeries);
    };

    let mut min = finite("domain.value", first)?;
    let mut max = min;
    for value in iter {
        let value = finite("domain.value", value)?;
        min = min.min(value);
        max = max.max(value);
    }

    expanded_domain(min, max)
}

fn expanded_domain(start: f64, end: f64) -> ChartResult<(f64, f64)> {
    if start != end {
        return Ok((start, end));
    }

    let pad = if start == 0.0 { 1.0 } else { start.abs() * 0.1 };
    Ok((start - pad, end + pad))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineLineChart.poco", role = "panel")]
pub struct PineLineChart {
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub series: Vec<ChartLineSeries>,
    #[prop]
    pub label: String,
    #[prop]
    pub width: f64,
    #[prop]
    pub height: f64,
    #[prop]
    pub margin_top: f64,
    #[prop]
    pub margin_right: f64,
    #[prop]
    pub margin_bottom: f64,
    #[prop]
    pub margin_left: f64,
    #[prop]
    pub x_min: Option<f64>,
    #[prop]
    pub x_max: Option<f64>,
    #[prop]
    pub y_min: Option<f64>,
    #[prop]
    pub y_max: Option<f64>,
    #[prop]
    pub show_markers: bool,
    pub state: String,
    pub view_box: String,
    pub line_d: String,
    pub line_series: Vec<LineChartSeriesRender>,
    pub samples: Vec<LineChartSample>,
    pub plot_x: f64,
    pub plot_y: f64,
    pub plot_right: f64,
    pub plot_bottom: f64,
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
    pub hover_visible: bool,
    pub hover_x: f64,
    pub hover_y: f64,
    pub hover_data_x: f64,
    pub hover_data_y: f64,
    pub hover_series: String,
    pub hover_x_label: String,
    pub hover_y_label: String,
    pub hover_aria_label: String,
    pub hover_placement_x: String,
    pub hover_placement_y: String,
    pub hover_style: String,
    pub error: String,
    pub ready: bool,
    pub empty: bool,
    pub invalid: bool,
}

impl Default for PineLineChart {
    fn default() -> Self {
        let options = LineChartOptions::default();
        Self {
            points: Vec::new(),
            label: "Line chart".into(),
            width: options.width,
            height: options.height,
            margin_top: options.margins.top,
            margin_right: options.margins.right,
            margin_bottom: options.margins.bottom,
            margin_left: options.margins.left,
            x_min: None,
            x_max: None,
            y_min: None,
            y_max: None,
            show_markers: false,
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            line_d: String::new(),
            series: Vec::new(),
            line_series: Vec::new(),
            samples: Vec::new(),
            plot_x: 0.0,
            plot_y: 0.0,
            plot_right: 0.0,
            plot_bottom: 0.0,
            x_grid: Vec::new(),
            y_grid: Vec::new(),
            x_tick_labels: Vec::new(),
            y_tick_labels: Vec::new(),
            x_axis: SvgLine::default(),
            y_axis: SvgLine::default(),
            hover_visible: false,
            hover_x: 0.0,
            hover_y: 0.0,
            hover_data_x: 0.0,
            hover_data_y: 0.0,
            hover_series: String::new(),
            hover_x_label: String::new(),
            hover_y_label: String::new(),
            hover_aria_label: String::new(),
            hover_placement_x: "right".into(),
            hover_placement_y: "above".into(),
            hover_style: String::new(),
            error: String::new(),
            ready: false,
            empty: true,
            invalid: false,
        }
    }
}

#[handlers]
impl PineLineChart {
    fn on_setup(&mut self) {
        self.recompute();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartPoint>, _: Option<Vec<ChartPoint>>) {
        self.recompute();
    }

    #[watch(series)]
    fn on_series(&mut self, _: Vec<ChartLineSeries>, _: Option<Vec<ChartLineSeries>>) {
        self.recompute();
    }

    #[watch(width)]
    fn on_width(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(height)]
    fn on_height(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(margin_top)]
    fn on_margin_top(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(margin_right)]
    fn on_margin_right(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(margin_bottom)]
    fn on_margin_bottom(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(margin_left)]
    fn on_margin_left(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(x_min)]
    fn on_x_min(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.recompute();
    }

    #[watch(x_max)]
    fn on_x_max(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.recompute();
    }

    #[watch(y_min)]
    fn on_y_min(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.recompute();
    }

    #[watch(y_max)]
    fn on_y_max(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.recompute();
    }

    pub fn on_pointer_move(&mut self, ev: wasm_bindgen::JsValue) {
        let Ok(ev) = ev.dyn_into::<web_sys::PointerEvent>() else {
            return;
        };
        let Some(target) = ev.current_target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        let rect = element.get_bounding_client_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 || self.width <= 0.0 || self.height <= 0.0 {
            return;
        }

        let ratio_x = ((ev.client_x() as f64) - rect.left()) / rect.width();
        let ratio_y = ((ev.client_y() as f64) - rect.top()) / rect.height();
        self.hover_at(ratio_x * self.width, ratio_y * self.height);
    }

    pub fn clear_hover(&mut self) {
        self.hover_visible = false;
        self.hover_x = 0.0;
        self.hover_y = 0.0;
        self.hover_data_x = 0.0;
        self.hover_data_y = 0.0;
        self.hover_series.clear();
        self.hover_x_label.clear();
        self.hover_y_label.clear();
        self.hover_aria_label.clear();
        self.hover_placement_x = "right".into();
        self.hover_placement_y = "above".into();
        self.hover_style.clear();
    }
}

impl PineLineChart {
    fn recompute(&mut self) {
        let geometry = if self.series.is_empty() {
            LineChartGeometry::new(&self.points, &self.options())
        } else {
            LineChartGeometry::from_series(&self.series, &self.options())
        };

        match geometry {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.line_d = geometry.line_d;
                self.line_series = geometry.series;
                self.samples = geometry.samples;
                self.plot_x = geometry.plot.x;
                self.plot_y = geometry.plot.y;
                self.plot_right = geometry.plot.right();
                self.plot_bottom = geometry.plot.bottom();
                self.x_grid = geometry.x_grid;
                self.y_grid = geometry.y_grid;
                self.x_tick_labels = geometry.x_tick_labels;
                self.y_tick_labels = geometry.y_tick_labels;
                self.x_axis = geometry.x_axis;
                self.y_axis = geometry.y_axis;
                self.error.clear();
                self.state = "ready".into();
                self.ready = true;
                self.empty = false;
                self.invalid = false;
                self.clear_hover();
            }
            Err(ChartError::EmptySeries) => {
                self.line_d.clear();
                self.line_series.clear();
                self.samples.clear();
                self.clear_plot();
                self.clear_guides();
                self.clear_hover();
                self.error.clear();
                self.state = "empty".into();
                self.ready = false;
                self.empty = true;
                self.invalid = false;
            }
            Err(error) => {
                self.line_d.clear();
                self.line_series.clear();
                self.samples.clear();
                self.clear_plot();
                self.clear_guides();
                self.clear_hover();
                self.error = error.to_string();
                self.state = "invalid".into();
                self.ready = false;
                self.empty = false;
                self.invalid = true;
            }
        }
    }

    fn options(&self) -> LineChartOptions {
        LineChartOptions {
            width: self.width,
            height: self.height,
            margins: ChartMargins::new(
                self.margin_top,
                self.margin_right,
                self.margin_bottom,
                self.margin_left,
            ),
            x_domain: zip_domain(self.x_min, self.x_max),
            y_domain: zip_domain(self.y_min, self.y_max),
        }
    }

    fn clear_guides(&mut self) {
        self.x_grid.clear();
        self.y_grid.clear();
        self.x_tick_labels.clear();
        self.y_tick_labels.clear();
        self.x_axis = SvgLine::default();
        self.y_axis = SvgLine::default();
    }

    pub fn hover_at_x(&mut self, svg_x: f64) {
        let svg_y = self.plot_y + (self.plot_bottom - self.plot_y) * 0.5;
        self.hover_at(svg_x, svg_y);
    }

    pub fn hover_at(&mut self, svg_x: f64, svg_y: f64) {
        let Ok(point) = Point::new(svg_x, svg_y) else {
            self.clear_hover();
            return;
        };
        if !self.ready || !self.plot_rect().contains(point) {
            self.clear_hover();
            return;
        }

        let Some(sample) = nearest_line_sample_at(&self.samples, point) else {
            self.clear_hover();
            return;
        };
        let hover_x = sample.x;
        let hover_y = sample.y;
        let hover_data_x = sample.data_x;
        let hover_data_y = sample.data_y;
        let hover_series = sample.series_label.clone();
        let hover_x_label = sample.x_label.clone();
        let hover_y_label = sample.y_label.clone();
        let hover_aria_label = sample.aria_label.clone();
        let hover_placement_x = if hover_x >= self.plot_x + (self.plot_right - self.plot_x) * 0.5 {
            "left"
        } else {
            "right"
        };
        let hover_placement_y = if hover_y <= self.plot_y + (self.plot_bottom - self.plot_y) * 0.5 {
            "below"
        } else {
            "above"
        };
        let hover_x_pct = (hover_x / self.width * 100.0).clamp(0.0, 100.0);
        let hover_y_pct = (hover_y / self.height * 100.0).clamp(0.0, 100.0);

        self.hover_visible = true;
        self.hover_x = hover_x;
        self.hover_y = hover_y;
        self.hover_data_x = hover_data_x;
        self.hover_data_y = hover_data_y;
        self.hover_series = hover_series;
        self.hover_x_label = hover_x_label;
        self.hover_y_label = hover_y_label;
        self.hover_aria_label = hover_aria_label;
        self.hover_placement_x = hover_placement_x.into();
        self.hover_placement_y = hover_placement_y.into();
        self.hover_style = format!(
            "--pine-chart-tooltip-x: {hover_x_pct}%; --pine-chart-tooltip-y: {hover_y_pct}%;"
        );
    }

    fn plot_rect(&self) -> ChartRect {
        ChartRect {
            x: self.plot_x,
            y: self.plot_y,
            width: self.plot_right - self.plot_x,
            height: self.plot_bottom - self.plot_y,
        }
    }

    fn clear_plot(&mut self) {
        self.plot_x = 0.0;
        self.plot_y = 0.0;
        self.plot_right = 0.0;
        self.plot_bottom = 0.0;
    }
}

fn zip_domain(start: Option<f64>, end: Option<f64>) -> Option<(f64, f64)> {
    match (start, end) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_geometry_maps_points_into_svg_space() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 10.0)),
            y_domain: Some((0.0, 10.0)),
        };
        let geometry = LineChartGeometry::new(
            &[
                ChartPoint::new(0.0, 0.0),
                ChartPoint::new(5.0, 10.0),
                ChartPoint::new(10.0, 5.0),
            ],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.view_box, "0 0 100 100");
        assert_eq!(geometry.line_d, "M0,100 L50,0 L100,50");
        assert_eq!(geometry.series.len(), 1);
        assert_eq!(geometry.series[0].line_d, "M0,100 L50,0 L100,50");
        assert_eq!(geometry.samples.len(), 3);
        assert_eq!(geometry.samples[1].data_x, 5.0);
        assert_eq!(geometry.samples[1].x, 50.0);
        assert_eq!(geometry.samples[1].y_label, "10");
        assert_eq!(geometry.x_grid.len(), 6);
        assert_eq!(geometry.y_grid.len(), 6);
    }

    #[test]
    fn line_geometry_maps_multiple_series() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 1.0)),
            y_domain: Some((0.0, 2.0)),
        };
        let series = vec![
            ChartLineSeries::new(
                "Actual",
                vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 2.0)],
            ),
            ChartLineSeries::new(
                "Target",
                vec![ChartPoint::new(0.0, 2.0), ChartPoint::new(1.0, 1.0)],
            ),
        ];

        let geometry = LineChartGeometry::from_series(&series, &options).unwrap();

        assert_eq!(geometry.series.len(), 2);
        assert_eq!(geometry.series[0].label, "Actual");
        assert_eq!(geometry.series[0].line_d, "M0,100 L100,0");
        assert_eq!(geometry.series[1].label, "Target");
        assert_eq!(geometry.series[1].line_d, "M0,0 L100,50");
        assert_eq!(geometry.samples.len(), 4);
        assert_eq!(geometry.samples[0].series_label, "Actual");
        assert_eq!(geometry.samples[2].series_label, "Target");

        let legend = line_legend_items(&series);
        assert_eq!(legend.len(), 2);
        assert_eq!(legend[0].label, "Actual");
        assert_eq!(legend[0].series, "Actual");
        assert_eq!(legend[1].label, "Target");
        assert_eq!(legend[1].series, "Target");
    }

    #[test]
    fn nearest_sample_uses_svg_x_distance() {
        let samples = vec![
            LineChartSample {
                key: "a".into(),
                series_label: String::new(),
                data_x: 1.0,
                data_y: 2.0,
                x: 10.0,
                y: 80.0,
                x_label: "1".into(),
                y_label: "2".into(),
                aria_label: "x 1, y 2".into(),
            },
            LineChartSample {
                key: "b".into(),
                series_label: String::new(),
                data_x: 5.0,
                data_y: 8.0,
                x: 50.0,
                y: 20.0,
                x_label: "5".into(),
                y_label: "8".into(),
                aria_label: "x 5, y 8".into(),
            },
        ];

        let sample = nearest_line_sample(&samples, 41.0).unwrap();

        assert_eq!(sample.key, "b");
    }

    #[test]
    fn nearest_sample_at_uses_svg_xy_distance() {
        let samples = vec![
            LineChartSample {
                key: "actual".into(),
                series_label: "Actual".into(),
                data_x: 1.0,
                data_y: 9.0,
                x: 50.0,
                y: 10.0,
                x_label: "1".into(),
                y_label: "9".into(),
                aria_label: "Actual: x 1, y 9".into(),
            },
            LineChartSample {
                key: "target".into(),
                series_label: "Target".into(),
                data_x: 1.0,
                data_y: 1.0,
                x: 50.0,
                y: 90.0,
                x_label: "1".into(),
                y_label: "1".into(),
                aria_label: "Target: x 1, y 1".into(),
            },
        ];

        let sample = nearest_line_sample_at(&samples, Point { x: 52.0, y: 84.0 }).unwrap();

        assert_eq!(sample.key, "target");
    }

    #[test]
    fn line_geometry_expands_flat_domains() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: None,
            y_domain: None,
        };
        let geometry = LineChartGeometry::new(&[ChartPoint::new(5.0, 5.0)], &options).unwrap();

        assert_eq!(geometry.line_d, "M50,50");
    }

    #[test]
    fn component_recomputes_state() {
        let mut chart = PineLineChart {
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            ..PineLineChart::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.state, "ready");
        assert_eq!(chart.line_d, "M0,100 L100,0");
        assert_eq!(chart.line_series.len(), 1);
        assert!(!chart.x_tick_labels.is_empty());
        assert!(!chart.y_tick_labels.is_empty());

        chart.hover_at_x(95.0);

        assert!(chart.hover_visible);
        assert_eq!(chart.hover_x, 100.0);
        assert_eq!(chart.hover_y, 0.0);
        assert_eq!(chart.hover_x_label, "1");
        assert_eq!(chart.hover_y_label, "1");

        chart.hover_at(50.0, -1.0);

        assert!(!chart.hover_visible);
    }
}
