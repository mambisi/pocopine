use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::animation::{animation_style, DEFAULT_ANIMATION_DURATION_MS, DEFAULT_ANIMATION_EASING};
use crate::cartesian::{
    centered_plot_y, chart_hover_payload, nearest_sample_by_point, nearest_sample_by_x,
    optional_domain, plot_rect_from_edges, pointer_event_svg_point, step_key, sync_tooltip_fields,
    CartesianChartState, CartesianGuideFields, CartesianGuideUpdate, CartesianHoverFields,
    CartesianHoverSample, CartesianHoverUpdate, CartesianLayout, ChartStateFields, PlotEdgeFields,
    DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::events::{
    ChartHoverEnd, ChartSelection, ChartSelectionKeys, CHART_HOVER_END_EVENT, CHART_HOVER_EVENT,
};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::{series_label_or_default, series_legend_items_with_visibility};
use crate::path::line_path;
use crate::scale::LinearScale;
use crate::svg::{format_tick, SvgAxisLabel, SvgLine, SvgTickLabel};

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartLineSeries {
    pub label: String,
    pub data: Vec<ChartPoint>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

impl Default for ChartLineSeries {
    fn default() -> Self {
        Self {
            label: String::new(),
            data: Vec::new(),
            visible: true,
        }
    }
}

impl ChartLineSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartPoint>) -> Self {
        Self {
            label: label.into(),
            data,
            visible: true,
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
    pub x_scale: LinearScale,
    pub y_scale: LinearScale,
    pub samples: Vec<LineChartSample>,
    pub x_ticks: Vec<crate::Tick>,
    pub y_ticks: Vec<crate::Tick>,
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
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
        let normalized = normalize_line_series(series)?;
        let layout = CartesianLayout::new(
            options.width,
            options.height,
            options.margins,
            options.x_domain,
            options.y_domain,
            normalized
                .iter()
                .flat_map(|series| series.data.iter().map(|point| point.x)),
            normalized
                .iter()
                .flat_map(|series| series.data.iter().map(|point| point.y)),
        )?;
        let series = normalized
            .iter()
            .enumerate()
            .map(|(series_index, series)| {
                let samples = series
                    .data
                    .iter()
                    .enumerate()
                    .map(|(point_index, point)| {
                        let x = layout.x_scale.map(point.x)?;
                        let y = layout.y_scale.map(point.y)?;
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

        let line_d = series
            .first()
            .map(|series| series.line_d.clone())
            .unwrap_or_default();

        Ok(Self {
            view_box: layout.view_box,
            line_d,
            series,
            plot: layout.plot,
            x_scale: layout.x_scale,
            y_scale: layout.y_scale,
            samples,
            x_grid: layout.x_grid,
            y_grid: layout.y_grid,
            x_tick_labels: layout.x_tick_labels,
            y_tick_labels: layout.y_tick_labels,
            x_axis_label: layout.x_axis_label,
            y_axis_label: layout.y_axis_label,
            x_axis: layout.x_axis,
            y_axis: layout.y_axis,
            x_ticks: layout.x_ticks,
            y_ticks: layout.y_ticks,
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

    pub(crate) fn hover_update(
        &self,
        plot: ChartRect,
        width: f64,
        height: f64,
    ) -> CartesianHoverUpdate {
        CartesianHoverUpdate::new(
            CartesianHoverSample {
                key: self.key.clone(),
                point: Point {
                    x: self.x,
                    y: self.y,
                },
                data_x: self.data_x,
                data_y: self.data_y,
                series: self.series_label.clone(),
                x_label: self.x_label.clone(),
                y_label: self.y_label.clone(),
                aria_label: self.aria_label.clone(),
            },
            plot,
            width,
            height,
        )
    }

    pub(crate) fn selection(&self, chart: &str) -> ChartSelection {
        ChartSelection::xy(
            chart,
            self.key.clone(),
            self.aria_label.clone(),
            self.aria_label.clone(),
            self.series_label.clone(),
            self.data_x,
            self.data_y,
            self.x_label.clone(),
            self.y_label.clone(),
        )
    }
}

pub fn line_legend_items(series: &[ChartLineSeries]) -> Vec<crate::LegendItem> {
    series_legend_items_with_visibility(
        "line-series",
        series.iter().enumerate().map(|(index, series)| {
            (
                series_label_or_default(&series.label, index),
                series.visible,
            )
        }),
    )
}

pub fn nearest_line_sample(samples: &[LineChartSample], svg_x: f64) -> Option<&LineChartSample> {
    nearest_sample_by_x(samples, svg_x, |sample| sample.x)
}

pub fn nearest_line_sample_at(
    samples: &[LineChartSample],
    svg_point: Point,
) -> Option<&LineChartSample> {
    nearest_sample_by_point(samples, svg_point, |sample| Point {
        x: sample.x,
        y: sample.y,
    })
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
    let normalized = series
        .iter()
        .enumerate()
        .filter(|(_, series)| series.visible)
        .map(|(index, series)| {
            if series.data.is_empty() {
                return Err(ChartError::EmptySeries);
            }
            Ok(NormalizedLineSeries {
                label: line_series_label(series, index),
                data: validate_points(&series.data)?,
            })
        })
        .collect::<ChartResult<Vec<_>>>()?;

    if normalized.is_empty() {
        Err(ChartError::EmptySeries)
    } else {
        Ok(normalized)
    }
}

fn line_series_label(series: &ChartLineSeries, _index: usize) -> String {
    series.label.clone()
}

fn sample_aria_label(series: &str, data_x: f64, data_y: f64) -> String {
    let point_label = format!("x {}, y {}", format_tick(data_x), format_tick(data_y));
    if series.is_empty() {
        point_label
    } else {
        format!("{series}: {point_label}")
    }
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
    pub empty_message: String,
    #[prop]
    pub x_label: String,
    #[prop]
    pub y_label: String,
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
    #[prop]
    pub animate: bool,
    #[prop]
    pub animation_duration: f64,
    #[prop]
    pub animation_easing: String,
    #[prop]
    pub tooltip: String,
    pub tooltip_mode: String,
    pub tooltip_aria_hidden: String,
    pub animation_style: String,
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
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
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
    pub focused_key: String,
    pub selected_key: String,
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
            empty_message: DEFAULT_EMPTY_MESSAGE.into(),
            x_label: String::new(),
            y_label: String::new(),
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
            animate: false,
            animation_duration: DEFAULT_ANIMATION_DURATION_MS,
            animation_easing: DEFAULT_ANIMATION_EASING.into(),
            tooltip: "default".into(),
            tooltip_mode: "default".into(),
            tooltip_aria_hidden: "true".into(),
            animation_style: animation_style(
                DEFAULT_ANIMATION_DURATION_MS,
                DEFAULT_ANIMATION_EASING,
            ),
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
            x_axis_label: SvgAxisLabel::default(),
            y_axis_label: SvgAxisLabel::default(),
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
            focused_key: String::new(),
            selected_key: String::new(),
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
        self.update_animation_style();
        self.sync_tooltip_state();
        self.recompute();
    }

    #[watch(animate)]
    fn on_animate(&mut self, _: bool, _: Option<bool>) {
        self.update_animation_style();
    }

    #[watch(animation_duration)]
    fn on_animation_duration(&mut self, _: f64, _: Option<f64>) {
        self.update_animation_style();
    }

    #[watch(animation_easing)]
    fn on_animation_easing(&mut self, _: String, _: Option<String>) {
        self.update_animation_style();
    }

    #[watch(tooltip)]
    fn on_tooltip(&mut self, _: String, _: Option<String>) {
        self.sync_tooltip_state();
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
        let Some(point) = pointer_event_svg_point(ev, self.width, self.height) else {
            return;
        };
        self.hover_at(point.x, point.y);
    }

    pub fn clear_hover(&mut self) {
        let was_visible = self.hover_visible;
        self.hover_fields().clear();
        self.sync_tooltip_state();
        if was_visible {
            pocopine::emit(CHART_HOVER_END_EVENT, ChartHoverEnd::new("line"));
        }
    }

    pub fn select_sample(&mut self, key: String) {
        if let Some(selection) = self.selection_for_sample(&key) {
            self.selection_keys().select(key, selection);
        }
    }

    pub fn select_hovered_or_clear(&mut self) {
        if self.hover_visible {
            let key = nearest_line_sample_at(
                &self.samples,
                Point {
                    x: self.hover_x,
                    y: self.hover_y,
                },
            )
            .map(|sample| sample.key.clone());
            if let Some(key) = key {
                self.select_sample(key);
                return;
            }
        }

        self.clear_selection();
    }

    pub fn focus_next_sample(&mut self) {
        self.step_sample_focus(1);
    }

    pub fn focus_prev_sample(&mut self) {
        self.step_sample_focus(-1);
    }

    pub fn select_focused_sample(&mut self) {
        if self.focused_key.is_empty() {
            self.step_sample_focus(1);
        }
        let key = self.focused_key.clone();
        if let Some(selection) = self.selection_for_sample(&key) {
            self.selection_keys().select(key, selection);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_keys().clear();
    }
}

impl PineLineChart {
    fn update_animation_style(&mut self) {
        self.animation_style = animation_style(self.animation_duration, &self.animation_easing);
    }

    fn sync_tooltip_state(&mut self) {
        sync_tooltip_fields(
            &self.tooltip,
            self.hover_visible,
            &mut self.tooltip_mode,
            &mut self.tooltip_aria_hidden,
        );
    }

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
                self.plot_edges().apply(geometry.plot);
                self.guides().apply(CartesianGuideUpdate {
                    x_grid: geometry.x_grid,
                    y_grid: geometry.y_grid,
                    x_tick_labels: geometry.x_tick_labels,
                    y_tick_labels: geometry.y_tick_labels,
                    x_axis_label: geometry.x_axis_label,
                    y_axis_label: geometry.y_axis_label,
                    x_axis: geometry.x_axis,
                    y_axis: geometry.y_axis,
                });
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Ready);
                self.reconcile_selection();
                self.clear_hover();
            }
            Err(ChartError::EmptySeries) => {
                self.line_d.clear();
                self.line_series.clear();
                self.samples.clear();
                self.plot_edges().clear();
                self.guides().clear();
                self.clear_hover();
                self.clear_selection();
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Empty);
            }
            Err(error) => {
                self.line_d.clear();
                self.line_series.clear();
                self.samples.clear();
                self.plot_edges().clear();
                self.guides().clear();
                self.clear_hover();
                self.clear_selection();
                self.error = error.to_string();
                self.state_fields().apply(CartesianChartState::Invalid);
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
            x_domain: optional_domain(self.x_min, self.x_max),
            y_domain: optional_domain(self.y_min, self.y_max),
        }
    }

    pub fn hover_at_x(&mut self, svg_x: f64) {
        let svg_y = centered_plot_y(self.plot_rect());
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
        let update = sample.hover_update(self.plot_rect(), self.width, self.height);
        let hover = chart_hover_payload("line", &update);
        self.hover_fields().apply(update);
        self.sync_tooltip_state();
        pocopine::emit(CHART_HOVER_EVENT, hover);
    }

    fn plot_rect(&self) -> ChartRect {
        plot_rect_from_edges(self.plot_x, self.plot_y, self.plot_right, self.plot_bottom)
    }

    fn plot_edges(&mut self) -> PlotEdgeFields<'_> {
        PlotEdgeFields {
            x: &mut self.plot_x,
            y: &mut self.plot_y,
            right: &mut self.plot_right,
            bottom: &mut self.plot_bottom,
        }
    }

    fn guides(&mut self) -> CartesianGuideFields<'_> {
        CartesianGuideFields {
            x_grid: &mut self.x_grid,
            y_grid: &mut self.y_grid,
            x_tick_labels: &mut self.x_tick_labels,
            y_tick_labels: &mut self.y_tick_labels,
            x_axis_label: &mut self.x_axis_label,
            y_axis_label: &mut self.y_axis_label,
            x_axis: &mut self.x_axis,
            y_axis: &mut self.y_axis,
        }
    }

    fn state_fields(&mut self) -> ChartStateFields<'_> {
        ChartStateFields {
            state: &mut self.state,
            ready: &mut self.ready,
            empty: &mut self.empty,
            invalid: &mut self.invalid,
        }
    }

    fn step_sample_focus(&mut self, step: isize) {
        if let Some(key) = step_key(
            self.samples.iter().map(|sample| sample.key.as_str()),
            &self.focused_key,
            step,
        ) {
            self.focused_key = key;
        }
    }

    fn has_sample_key(&self, key: &str) -> bool {
        self.samples.iter().any(|sample| sample.key == key)
    }

    fn selection_for_sample(&self, key: &str) -> Option<ChartSelection> {
        self.samples
            .iter()
            .find(|sample| sample.key == key)
            .map(|sample| sample.selection("line"))
    }

    fn reconcile_selection(&mut self) {
        let has_focused = self.has_sample_key(&self.focused_key);
        let has_selected = self.has_sample_key(&self.selected_key);
        self.selection_keys().reconcile(has_focused, has_selected);
    }

    fn selection_keys(&mut self) -> ChartSelectionKeys<'_> {
        ChartSelectionKeys::new("line", &mut self.focused_key, &mut self.selected_key)
    }

    fn hover_fields(&mut self) -> CartesianHoverFields<'_> {
        CartesianHoverFields {
            visible: &mut self.hover_visible,
            x: &mut self.hover_x,
            y: &mut self.hover_y,
            data_x: &mut self.hover_data_x,
            data_y: &mut self.hover_data_y,
            series: &mut self.hover_series,
            x_label: &mut self.hover_x_label,
            y_label: &mut self.hover_y_label,
            aria_label: &mut self.hover_aria_label,
            placement_x: &mut self.hover_placement_x,
            placement_y: &mut self.hover_placement_y,
            style: &mut self.hover_style,
        }
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
    fn line_geometry_skips_hidden_series_and_marks_legend_inactive() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 1.0)),
            y_domain: Some((0.0, 2.0)),
        };
        let mut series = vec![
            ChartLineSeries::new(
                "Actual",
                vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 2.0)],
            ),
            ChartLineSeries::new("Hidden", Vec::new()),
        ];
        series[1].visible = false;

        let geometry = LineChartGeometry::from_series(&series, &options).unwrap();

        assert_eq!(geometry.series.len(), 1);
        assert_eq!(geometry.series[0].label, "Actual");
        let legend = line_legend_items(&series);
        assert!(legend[0].active);
        assert!(!legend[1].active);
    }

    #[test]
    fn line_geometry_is_empty_when_all_series_are_hidden() {
        let options = LineChartOptions::default();
        let mut series = vec![ChartLineSeries::new(
            "Hidden",
            vec![ChartPoint::new(0.0, 0.0)],
        )];
        series[0].visible = false;

        assert_eq!(
            LineChartGeometry::from_series(&series, &options).unwrap_err(),
            ChartError::EmptySeries
        );
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
