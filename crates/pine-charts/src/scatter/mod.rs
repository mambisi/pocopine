use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::animation::{animation_style, DEFAULT_ANIMATION_DURATION_MS, DEFAULT_ANIMATION_EASING};
use crate::cartesian::{
    chart_hover_payload, nearest_sample_by_point, optional_domain, plot_rect_from_edges,
    pointer_event_svg_point, step_key, tooltip_aria_hidden, tooltip_mode, CartesianChartState,
    CartesianGuideFields, CartesianGuideUpdate, CartesianHoverFields, ChartStateFields,
    PlotEdgeFields, DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{ChartError, ChartResult};
use crate::events::{
    ChartHoverEnd, ChartSelection, CHART_HOVER_END_EVENT, CHART_HOVER_EVENT, CHART_SELECT_EVENT,
};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::{series_label_or_default, series_legend_items_with_visibility};
use crate::line::{ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions};
use crate::svg::{SvgAxisLabel, SvgLine, SvgTickLabel};
use crate::{LegendItem, LineChartSample};

pub type ScatterChartOptions = LineChartOptions;
pub type ScatterChartSample = LineChartSample;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartScatterSeries {
    pub label: String,
    pub data: Vec<ChartPoint>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

impl Default for ChartScatterSeries {
    fn default() -> Self {
        Self {
            label: String::new(),
            data: Vec::new(),
            visible: true,
        }
    }
}

impl ChartScatterSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartPoint>) -> Self {
        Self {
            label: label.into(),
            data,
            visible: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScatterChartGeometry {
    pub view_box: String,
    pub series: Vec<ScatterChartSeriesRender>,
    pub plot: ChartRect,
    pub samples: Vec<ScatterChartSample>,
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

impl ScatterChartGeometry {
    pub fn new(points: &[ChartPoint], options: &ScatterChartOptions) -> ChartResult<Self> {
        Self::from_line_geometry(LineChartGeometry::new(points, options)?)
    }

    pub fn from_series(
        series: &[ChartScatterSeries],
        options: &ScatterChartOptions,
    ) -> ChartResult<Self> {
        let line_series = series
            .iter()
            .map(|series| {
                let mut line = ChartLineSeries::new(series.label.clone(), series.data.clone());
                line.visible = series.visible;
                line
            })
            .collect::<Vec<_>>();
        Self::from_line_geometry(LineChartGeometry::from_series(&line_series, options)?)
    }

    fn from_line_geometry(geometry: LineChartGeometry) -> ChartResult<Self> {
        let series = geometry
            .series
            .iter()
            .enumerate()
            .map(|(index, series)| ScatterChartSeriesRender {
                key: format!("scatter-series-{index}-{}", series.label),
                label: series.label.clone(),
                samples: series.samples.clone(),
            })
            .collect::<Vec<_>>();

        Ok(Self {
            view_box: geometry.view_box,
            series,
            plot: geometry.plot,
            samples: geometry.samples,
            x_ticks: geometry.x_ticks,
            y_ticks: geometry.y_ticks,
            x_grid: geometry.x_grid,
            y_grid: geometry.y_grid,
            x_tick_labels: geometry.x_tick_labels,
            y_tick_labels: geometry.y_tick_labels,
            x_axis_label: geometry.x_axis_label,
            y_axis_label: geometry.y_axis_label,
            x_axis: geometry.x_axis,
            y_axis: geometry.y_axis,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScatterChartSeriesRender {
    pub key: String,
    pub label: String,
    pub samples: Vec<ScatterChartSample>,
}

pub fn scatter_legend_items(series: &[ChartScatterSeries]) -> Vec<LegendItem> {
    series_legend_items_with_visibility(
        "scatter-series",
        series.iter().enumerate().map(|(index, series)| {
            (
                series_label_or_default(&series.label, index),
                series.visible,
            )
        }),
    )
}

pub fn nearest_scatter_sample(
    samples: &[ScatterChartSample],
    svg_point: Point,
) -> Option<&ScatterChartSample> {
    nearest_sample_by_point(samples, svg_point, |sample| Point {
        x: sample.x,
        y: sample.y,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineScatterChart.poco", role = "panel")]
pub struct PineScatterChart {
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub series: Vec<ChartScatterSeries>,
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
    pub point_radius: f64,
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
    pub scatter_series: Vec<ScatterChartSeriesRender>,
    pub samples: Vec<ScatterChartSample>,
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

impl Default for PineScatterChart {
    fn default() -> Self {
        let options = ScatterChartOptions::default();
        Self {
            points: Vec::new(),
            series: Vec::new(),
            label: "Scatter chart".into(),
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
            point_radius: 4.0,
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
            scatter_series: Vec::new(),
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
impl PineScatterChart {
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
    fn on_series(&mut self, _: Vec<ChartScatterSeries>, _: Option<Vec<ChartScatterSeries>>) {
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
            pocopine::emit(CHART_HOVER_END_EVENT, ChartHoverEnd::new("scatter"));
        }
    }

    pub fn select_sample(&mut self, key: String) {
        if let Some(selection) = self.selection_for_sample(&key) {
            self.focused_key = key.clone();
            self.selected_key = key;
            pocopine::emit(CHART_SELECT_EVENT, selection);
        }
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
        if let Some(selection) = self.selection_for_sample(&self.focused_key) {
            self.selected_key = self.focused_key.clone();
            pocopine::emit(CHART_SELECT_EVENT, selection);
        }
    }
}

impl PineScatterChart {
    fn update_animation_style(&mut self) {
        self.animation_style = animation_style(self.animation_duration, &self.animation_easing);
    }

    fn sync_tooltip_state(&mut self) {
        self.tooltip_mode = tooltip_mode(&self.tooltip).into();
        self.tooltip_aria_hidden =
            tooltip_aria_hidden(&self.tooltip_mode, self.hover_visible).into();
    }

    fn recompute(&mut self) {
        let geometry = if self.series.is_empty() {
            ScatterChartGeometry::new(&self.points, &self.options())
        } else {
            ScatterChartGeometry::from_series(&self.series, &self.options())
        };

        match geometry {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.scatter_series = geometry.series;
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
                self.scatter_series.clear();
                self.samples.clear();
                self.plot_edges().clear();
                self.guides().clear();
                self.clear_hover();
                self.clear_selection();
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Empty);
            }
            Err(error) => {
                self.scatter_series.clear();
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

    fn options(&self) -> ScatterChartOptions {
        ScatterChartOptions {
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

    pub fn hover_at(&mut self, svg_x: f64, svg_y: f64) {
        let Ok(point) = Point::new(svg_x, svg_y) else {
            self.clear_hover();
            return;
        };
        if !self.ready || !self.plot_rect().contains(point) {
            self.clear_hover();
            return;
        }

        let Some(sample) = nearest_scatter_sample(&self.samples, point) else {
            self.clear_hover();
            return;
        };
        let update = sample.hover_update(self.plot_rect(), self.width, self.height);
        let hover = chart_hover_payload("scatter", &update);
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
            .map(|sample| sample.selection("scatter"))
    }

    fn reconcile_selection(&mut self) {
        if !self.has_sample_key(&self.focused_key) {
            self.focused_key.clear();
        }
        if !self.has_sample_key(&self.selected_key) {
            self.selected_key.clear();
        }
    }

    fn clear_selection(&mut self) {
        self.focused_key.clear();
        self.selected_key.clear();
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
    fn scatter_geometry_maps_multiple_series() {
        let options = ScatterChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 1.0)),
            y_domain: Some((0.0, 2.0)),
        };
        let series = vec![
            ChartScatterSeries::new(
                "Segment A",
                vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 2.0)],
            ),
            ChartScatterSeries::new(
                "Segment B",
                vec![ChartPoint::new(0.0, 2.0), ChartPoint::new(1.0, 1.0)],
            ),
        ];

        let geometry = ScatterChartGeometry::from_series(&series, &options).unwrap();

        assert_eq!(geometry.view_box, "0 0 100 100");
        assert_eq!(geometry.series.len(), 2);
        assert_eq!(geometry.series[0].label, "Segment A");
        assert_eq!(geometry.series[0].samples[1].x, 100.0);
        assert_eq!(geometry.series[0].samples[1].y, 0.0);
        assert_eq!(geometry.samples.len(), 4);
        assert_eq!(geometry.samples[2].series_label, "Segment B");

        let legend = scatter_legend_items(&series);
        assert_eq!(legend.len(), 2);
        assert_eq!(legend[0].label, "Segment A");
        assert_eq!(legend[0].series, "Segment A");
    }

    #[test]
    fn nearest_scatter_sample_uses_svg_xy_distance() {
        let samples = vec![
            ScatterChartSample {
                key: "a".into(),
                series_label: "A".into(),
                data_x: 1.0,
                data_y: 9.0,
                x: 50.0,
                y: 10.0,
                x_label: "1".into(),
                y_label: "9".into(),
                aria_label: "A: x 1, y 9".into(),
            },
            ScatterChartSample {
                key: "b".into(),
                series_label: "B".into(),
                data_x: 1.0,
                data_y: 1.0,
                x: 50.0,
                y: 90.0,
                x_label: "1".into(),
                y_label: "1".into(),
                aria_label: "B: x 1, y 1".into(),
            },
        ];

        let sample = nearest_scatter_sample(&samples, Point::new(52.0, 84.0).unwrap()).unwrap();

        assert_eq!(sample.key, "b");
    }

    #[test]
    fn component_recomputes_state_and_hover() {
        let mut chart = PineScatterChart {
            series: vec![ChartScatterSeries::new(
                "Cohort",
                vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
            )],
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            ..PineScatterChart::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.state, "ready");
        assert_eq!(chart.samples.len(), 2);

        chart.hover_at(95.0, 5.0);

        assert!(chart.hover_visible);
        assert_eq!(chart.hover_x, 100.0);
        assert_eq!(chart.hover_y, 0.0);
        assert_eq!(chart.hover_series, "Cohort");
        assert_eq!(chart.hover_aria_label, "Cohort: x 1, y 1");

        chart.hover_at(50.0, -1.0);

        assert!(!chart.hover_visible);
    }
}
