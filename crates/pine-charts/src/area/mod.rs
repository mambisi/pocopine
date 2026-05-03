use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cartesian::{
    apply_chart_state, centered_plot_y, clear_plot_edges, plot_rect_from_edges,
    pointer_event_svg_point, CartesianChartState, CartesianHoverFields,
};
use crate::error::{ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::{series_label_or_default, series_legend_items};
use crate::line::{
    nearest_line_sample_at, ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions,
    LineChartSample,
};
use crate::path::area_path;
use crate::svg::{SvgAxisLabel, SvgLine, SvgTickLabel};
use crate::LegendItem;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartAreaSeries {
    pub label: String,
    pub data: Vec<ChartPoint>,
}

impl ChartAreaSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartPoint>) -> Self {
        Self {
            label: label.into(),
            data,
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
            .map(|series| ChartLineSeries::new(series.label.clone(), series.data.clone()))
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
}

pub fn area_legend_items(series: &[ChartAreaSeries]) -> Vec<LegendItem> {
    series_legend_items(
        "area-series",
        series
            .iter()
            .enumerate()
            .map(|(index, series)| series_label_or_default(&series.label, index)),
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
        self.recompute();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartPoint>, _: Option<Vec<ChartPoint>>) {
        self.recompute();
    }

    #[watch(series)]
    fn on_series(&mut self, _: Vec<ChartAreaSeries>, _: Option<Vec<ChartAreaSeries>>) {
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
        self.hover_fields().clear();
    }
}

impl PineAreaChart {
    fn recompute(&mut self) {
        let geometry = if self.series.is_empty() {
            AreaChartGeometry::new(&self.points, &self.options())
        } else {
            AreaChartGeometry::from_series(&self.series, &self.options())
        };

        match geometry {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.area_series = geometry.series;
                self.samples = geometry.samples;
                self.plot_x = geometry.plot.x;
                self.plot_y = geometry.plot.y;
                self.plot_right = geometry.plot.right();
                self.plot_bottom = geometry.plot.bottom();
                self.x_grid = geometry.x_grid;
                self.y_grid = geometry.y_grid;
                self.x_tick_labels = geometry.x_tick_labels;
                self.y_tick_labels = geometry.y_tick_labels;
                self.x_axis_label = geometry.x_axis_label;
                self.y_axis_label = geometry.y_axis_label;
                self.x_axis = geometry.x_axis;
                self.y_axis = geometry.y_axis;
                self.error.clear();
                apply_chart_state(
                    &mut self.state,
                    &mut self.ready,
                    &mut self.empty,
                    &mut self.invalid,
                    CartesianChartState::Ready,
                );
                self.clear_hover();
            }
            Err(ChartError::EmptySeries) => {
                self.area_series.clear();
                self.samples.clear();
                self.clear_plot();
                self.clear_guides();
                self.clear_hover();
                self.error.clear();
                apply_chart_state(
                    &mut self.state,
                    &mut self.ready,
                    &mut self.empty,
                    &mut self.invalid,
                    CartesianChartState::Empty,
                );
            }
            Err(error) => {
                self.area_series.clear();
                self.samples.clear();
                self.clear_plot();
                self.clear_guides();
                self.clear_hover();
                self.error = error.to_string();
                apply_chart_state(
                    &mut self.state,
                    &mut self.ready,
                    &mut self.empty,
                    &mut self.invalid,
                    CartesianChartState::Invalid,
                );
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
        self.x_axis_label = SvgAxisLabel::default();
        self.y_axis_label = SvgAxisLabel::default();
        self.x_axis = SvgLine::default();
        self.y_axis = SvgLine::default();
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
        self.hover_fields().apply(update);
    }

    fn plot_rect(&self) -> ChartRect {
        plot_rect_from_edges(self.plot_x, self.plot_y, self.plot_right, self.plot_bottom)
    }

    fn clear_plot(&mut self) {
        clear_plot_edges(
            &mut self.plot_x,
            &mut self.plot_y,
            &mut self.plot_right,
            &mut self.plot_bottom,
        );
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
