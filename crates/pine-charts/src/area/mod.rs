use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::animation::{
    animation_style, exit_animation_delay_ms, DEFAULT_ANIMATION_DURATION_MS,
    DEFAULT_ANIMATION_EASING,
};
use crate::cartesian::{
    centered_plot_y, chart_hover_payload, optional_domain, plot_rect_from_edges,
    pointer_event_svg_point, step_key, tooltip_aria_hidden, tooltip_mode, CartesianChartState,
    CartesianGuideFields, CartesianGuideUpdate, CartesianHoverFields, ChartStateFields,
    PlotEdgeFields, DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{ChartError, ChartResult};
use crate::events::{
    ChartHoverEnd, ChartSelection, ChartSelectionEnd, CHART_HOVER_END_EVENT, CHART_HOVER_EVENT,
    CHART_SELECT_END_EVENT, CHART_SELECT_EVENT,
};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::{series_label_or_default, series_legend_items_with_visibility};
use crate::line::{
    nearest_line_sample_at, ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions,
    LineChartSample,
};
use crate::path::area_path;
use crate::svg::{SvgAxisLabel, SvgLine, SvgTickLabel};
use crate::LegendItem;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartAreaSeries {
    pub label: String,
    pub data: Vec<ChartPoint>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

impl Default for ChartAreaSeries {
    fn default() -> Self {
        Self {
            label: String::new(),
            data: Vec::new(),
            visible: true,
        }
    }
}

impl ChartAreaSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartPoint>) -> Self {
        Self {
            label: label.into(),
            data,
            visible: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AreaChartGeometry {
    pub view_box: String,
    pub series: Vec<AreaChartSeriesRender>,
    pub plot: ChartRect,
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

impl AreaChartGeometry {
    pub fn new(points: &[ChartPoint], options: &LineChartOptions) -> ChartResult<Self> {
        Self::from_line_geometry(LineChartGeometry::new(points, options)?)
    }

    pub fn from_series(
        series: &[ChartAreaSeries],
        options: &LineChartOptions,
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
        let baseline = geometry.plot.bottom();
        let series = geometry
            .series
            .iter()
            .map(|series| {
                Ok(AreaChartSeriesRender {
                    key: series.key.clone(),
                    label: series.label.clone(),
                    area_d: area_path(
                        series.samples.iter().map(|sample| Point {
                            x: sample.x,
                            y: sample.y,
                        }),
                        baseline,
                    )?,
                    line_d: series.line_d.clone(),
                    samples: series.samples.clone(),
                    leaving: false,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;

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
pub struct AreaChartSeriesRender {
    pub key: String,
    pub label: String,
    pub area_d: String,
    pub line_d: String,
    pub samples: Vec<LineChartSample>,
    pub leaving: bool,
}

pub fn area_legend_items(series: &[ChartAreaSeries]) -> Vec<LegendItem> {
    series_legend_items_with_visibility(
        "area-series",
        series.iter().enumerate().map(|(index, series)| {
            (
                series_label_or_default(&series.label, index),
                series.visible,
            )
        }),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineAreaChart.poco", role = "panel")]
pub struct PineAreaChart {
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub series: Vec<ChartAreaSeries>,
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
    pub exit_generation: u32,
    pub state: String,
    pub view_box: String,
    pub area_series: Vec<AreaChartSeriesRender>,
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

impl Default for PineAreaChart {
    fn default() -> Self {
        let options = LineChartOptions::default();
        Self {
            points: Vec::new(),
            series: Vec::new(),
            label: "Area chart".into(),
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
            exit_generation: 0,
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            area_series: Vec::new(),
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
impl PineAreaChart {
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
        self.recompute_with_leaving();
    }

    #[watch(series)]
    fn on_series(&mut self, _: Vec<ChartAreaSeries>, _: Option<Vec<ChartAreaSeries>>) {
        self.recompute_with_leaving();
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
            pocopine::emit(CHART_HOVER_END_EVENT, ChartHoverEnd::new("area"));
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

    pub fn clear_selection(&mut self) {
        self.focused_key.clear();
        self.clear_selected_key();
    }
}

impl PineAreaChart {
    fn update_animation_style(&mut self) {
        self.animation_style = animation_style(self.animation_duration, &self.animation_easing);
    }

    fn sync_tooltip_state(&mut self) {
        self.tooltip_mode = tooltip_mode(&self.tooltip).into();
        self.tooltip_aria_hidden =
            tooltip_aria_hidden(&self.tooltip_mode, self.hover_visible).into();
    }

    fn recompute(&mut self) {
        self.recompute_inner(false);
    }

    fn recompute_with_leaving(&mut self) {
        self.recompute_inner(self.animate);
    }

    fn recompute_inner(&mut self, retain_leaving: bool) {
        let geometry = if self.series.is_empty() {
            AreaChartGeometry::new(&self.points, &self.options())
        } else {
            AreaChartGeometry::from_series(&self.series, &self.options())
        };

        match geometry {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                let mut area_series = geometry.series;
                if retain_leaving && self.merge_leaving_area_series(&mut area_series) {
                    self.schedule_leaving_cleanup();
                }
                self.area_series = area_series;
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
                if retain_leaving && !self.area_series.is_empty() {
                    self.area_series.iter_mut().for_each(|series| {
                        series.leaving = true;
                    });
                    self.samples.clear();
                    self.clear_hover();
                    self.clear_selection();
                    self.error.clear();
                    self.state_fields().apply(CartesianChartState::Ready);
                    self.schedule_leaving_cleanup();
                    return;
                }
                self.area_series.clear();
                self.samples.clear();
                self.plot_edges().clear();
                self.guides().clear();
                self.clear_hover();
                self.clear_selection();
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Empty);
            }
            Err(error) => {
                self.area_series.clear();
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

    fn merge_leaving_area_series(&self, next: &mut Vec<AreaChartSeriesRender>) -> bool {
        let mut added = false;
        for previous in &self.area_series {
            if next.iter().any(|series| series.key == previous.key) {
                continue;
            }
            let mut leaving = previous.clone();
            leaving.leaving = true;
            next.push(leaving);
            added = true;
        }
        added
    }

    fn schedule_leaving_cleanup(&mut self) {
        self.exit_generation = self.exit_generation.wrapping_add(1);
        let generation = self.exit_generation;
        let delay = exit_animation_delay_ms(self.animation_duration);
        let me = pocopine::this::<Self>();
        pocopine::timers::after_scoped(delay, move || {
            me.update(|chart| chart.prune_leaving_area_series(generation));
        });
    }

    fn prune_leaving_area_series(&mut self, generation: u32) {
        if generation != self.exit_generation {
            return;
        }
        self.area_series.retain(|series| !series.leaving);
        if self.area_series.is_empty() {
            self.state_fields().apply(CartesianChartState::Empty);
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
        let hover = chart_hover_payload("area", &update);
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
            .map(|sample| sample.selection("area"))
    }

    fn reconcile_selection(&mut self) {
        if !self.has_sample_key(&self.focused_key) {
            self.focused_key.clear();
        }
        if !self.has_sample_key(&self.selected_key) {
            self.clear_selected_key();
        }
    }

    fn clear_selected_key(&mut self) {
        let key = std::mem::take(&mut self.selected_key);
        if !key.is_empty() {
            pocopine::emit(CHART_SELECT_END_EVENT, ChartSelectionEnd::new("area", key));
        }
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
    fn area_geometry_closes_series_to_plot_bottom() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 1.0)),
            y_domain: Some((0.0, 1.0)),
        };

        let geometry = AreaChartGeometry::new(
            &[ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.series.len(), 1);
        assert_eq!(geometry.series[0].line_d, "M0,100 L100,0");
        assert_eq!(geometry.series[0].area_d, "M0,100 L0,100 L100,0 L100,100Z");
        assert_eq!(geometry.samples.len(), 2);
    }

    #[test]
    fn area_geometry_maps_multiple_series() {
        let options = LineChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            x_domain: Some((0.0, 1.0)),
            y_domain: Some((0.0, 2.0)),
        };
        let series = vec![
            ChartAreaSeries::new(
                "Actual",
                vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 2.0)],
            ),
            ChartAreaSeries::new(
                "Target",
                vec![ChartPoint::new(0.0, 2.0), ChartPoint::new(1.0, 1.0)],
            ),
        ];

        let geometry = AreaChartGeometry::from_series(&series, &options).unwrap();

        assert_eq!(geometry.series.len(), 2);
        assert_eq!(geometry.series[0].label, "Actual");
        assert_eq!(geometry.series[0].line_d, "M0,100 L100,0");
        assert_eq!(geometry.series[0].area_d, "M0,100 L0,100 L100,0 L100,100Z");
        assert_eq!(geometry.series[1].label, "Target");
        assert_eq!(geometry.series[1].line_d, "M0,0 L100,50");
        assert_eq!(geometry.samples[2].series_label, "Target");

        let legend = area_legend_items(&series);
        assert_eq!(legend.len(), 2);
        assert_eq!(legend[0].series, "Actual");
        assert_eq!(legend[1].series, "Target");
    }

    #[test]
    fn component_recomputes_state() {
        let mut chart = PineAreaChart {
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(1.0, 1.0)],
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            ..PineAreaChart::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.state, "ready");
        assert_eq!(chart.area_series.len(), 1);
        assert_eq!(chart.area_series[0].line_d, "M0,100 L100,0");

        chart.hover_at_x(95.0);

        assert!(chart.hover_visible);
        assert_eq!(chart.hover_x, 100.0);
        assert_eq!(chart.hover_y, 0.0);
        assert_eq!(chart.hover_x_label, "1");

        chart.hover_at(50.0, -1.0);

        assert!(!chart.hover_visible);
    }
}
