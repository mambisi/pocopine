use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id};
use serde::{Deserialize, Serialize};

use crate::bar::{bar_aria_label, baseline_value, category_tick_labels, ChartBar};
use crate::cartesian::{
    expanded_domain, grid_lines_for_y, optional_domain, tick_labels_for_y, x_axis_label,
    y_axis_label, DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect};
use crate::legend::series_label_or_default;
use crate::line::{
    ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions, LineChartSample,
};
use crate::marks::{
    color_or, color_or_current, key_or_index, label_or_key, rotation_transform, text_anchor,
};
use crate::path::{area_path, line_path};
use crate::scale::{BandScale, LinearScale};
use crate::svg::{format_tick, SvgAxisLabel, SvgLine, SvgTickLabel};

const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 320.0;
const DEFAULT_STROKE_WIDTH: f64 = 3.0;
const DEFAULT_MARKER_RADIUS: f64 = 3.0;
const DEFAULT_REFERENCE_RADIUS: f64 = 6.0;
const DEFAULT_PADDING_INNER: f64 = 0.2;
const DEFAULT_PADDING_OUTER: f64 = 0.1;
const DEFAULT_SERIES_PADDING_INNER: f64 = 0.1;

create_context!(ROOT: Handle<PineCartesianChart>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianGridConfig {
    pub key: String,
    pub x: bool,
    pub y: bool,
}

impl Default for CartesianGridConfig {
    fn default() -> Self {
        Self {
            key: "grid".into(),
            x: true,
            y: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianAxisConfig {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianLineSeriesConfig {
    pub key: String,
    pub label: String,
    pub color: String,
    pub stroke_width: f64,
    pub show_markers: bool,
    pub marker_radius: f64,
    pub points: Vec<ChartPoint>,
    pub data: Vec<ChartBar>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianBarSeriesConfig {
    pub key: String,
    pub label: String,
    pub color: String,
    pub data: Vec<ChartBar>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianAreaSeriesConfig {
    pub key: String,
    pub label: String,
    pub fill: String,
    pub color: String,
    pub stroke_width: f64,
    pub points: Vec<ChartPoint>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianScatterSeriesConfig {
    pub key: String,
    pub label: String,
    pub color: String,
    pub point_radius: f64,
    pub points: Vec<ChartPoint>,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceLineConfig {
    pub key: String,
    pub label: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub color: String,
    pub stroke_width: f64,
    pub stroke_dasharray: String,
    pub layer: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceDotConfig {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub layer: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceLabelConfig {
    pub key: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
    pub angle: f64,
    pub fill: String,
    pub text_anchor: String,
    pub font_weight: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianLineSeriesRender {
    pub key: String,
    pub label: String,
    pub line_d: String,
    pub color: String,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianMarkerRender {
    pub key: String,
    pub series_label: String,
    pub x: f64,
    pub y: f64,
    pub data_x: f64,
    pub data_y: f64,
    pub color: String,
    pub radius: f64,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianBarRender {
    pub key: String,
    pub label: String,
    pub category_label: String,
    pub series_label: String,
    pub value: f64,
    pub aria_label: String,
    pub color: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianAreaSeriesRender {
    pub key: String,
    pub label: String,
    pub area_d: String,
    pub line_d: String,
    pub fill: String,
    pub color: String,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianScatterPointRender {
    pub key: String,
    pub series_label: String,
    pub x: f64,
    pub y: f64,
    pub data_x: f64,
    pub data_y: f64,
    pub color: String,
    pub radius: f64,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceLineRender {
    pub key: String,
    pub label: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub color: String,
    pub stroke_width: f64,
    pub stroke_dasharray: String,
    pub layer: String,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceDotRender {
    pub key: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub data_x: f64,
    pub data_y: f64,
    pub radius: f64,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub layer: String,
    pub aria_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CartesianReferenceLabelRender {
    pub key: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub data_x: f64,
    pub data_y: f64,
    pub fill: String,
    pub text_anchor: String,
    pub font_weight: String,
    pub transform: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CartesianChartRender {
    pub view_box: String,
    pub plot: ChartRect,
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
    pub bars: Vec<CartesianBarRender>,
    pub areas: Vec<CartesianAreaSeriesRender>,
    pub line_series: Vec<CartesianLineSeriesRender>,
    pub scatter_points: Vec<CartesianScatterPointRender>,
    pub markers: Vec<CartesianMarkerRender>,
    pub reference_lines: Vec<CartesianReferenceLineRender>,
    pub reference_dots: Vec<CartesianReferenceDotRender>,
    pub reference_labels: Vec<CartesianReferenceLabelRender>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CartesianChartOptions<'a> {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub x_domain: Option<(f64, f64)>,
    pub y_domain: Option<(f64, f64)>,
    pub padding_inner: f64,
    pub padding_outer: f64,
    pub series_padding_inner: f64,
    pub grid: Option<&'a CartesianGridConfig>,
    pub x_axis: Option<&'a CartesianAxisConfig>,
    pub y_axis: Option<&'a CartesianAxisConfig>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CartesianChartInputs<'a> {
    pub line_series: &'a [CartesianLineSeriesConfig],
    pub bar_series: &'a [CartesianBarSeriesConfig],
    pub area_series: &'a [CartesianAreaSeriesConfig],
    pub scatter_series: &'a [CartesianScatterSeriesConfig],
    pub reference_lines: &'a [CartesianReferenceLineConfig],
    pub reference_dots: &'a [CartesianReferenceDotConfig],
    pub reference_labels: &'a [CartesianReferenceLabelConfig],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianChart.poco", role = "panel")]
#[slot(default, accepts = [PineChartGrid, PineXAxis, PineYAxis, PineLineSeries, PineBarSeries, PineAreaSeries, PineScatterSeries, PineCartesianReferenceLine, PineCartesianReferenceDot, PineCartesianReferenceLabel])]
pub struct PineCartesianChart {
    #[prop]
    pub label: String,
    #[prop]
    pub empty_message: String,
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
    pub padding_inner: f64,
    #[prop]
    pub padding_outer: f64,
    #[prop]
    pub series_padding_inner: f64,
    pub state: String,
    pub view_box: String,
    pub grid: Option<CartesianGridConfig>,
    pub x_axis_config: Option<CartesianAxisConfig>,
    pub y_axis_config: Option<CartesianAxisConfig>,
    pub series: Vec<CartesianLineSeriesConfig>,
    pub bar_series: Vec<CartesianBarSeriesConfig>,
    pub area_series: Vec<CartesianAreaSeriesConfig>,
    pub scatter_series: Vec<CartesianScatterSeriesConfig>,
    pub reference_lines: Vec<CartesianReferenceLineConfig>,
    pub reference_dots: Vec<CartesianReferenceDotConfig>,
    pub reference_labels: Vec<CartesianReferenceLabelConfig>,
    pub bars: Vec<CartesianBarRender>,
    pub areas: Vec<CartesianAreaSeriesRender>,
    pub line_series: Vec<CartesianLineSeriesRender>,
    pub scatter_points: Vec<CartesianScatterPointRender>,
    pub markers: Vec<CartesianMarkerRender>,
    pub reference_background_lines: Vec<CartesianReferenceLineRender>,
    pub reference_foreground_lines: Vec<CartesianReferenceLineRender>,
    pub reference_background_dots: Vec<CartesianReferenceDotRender>,
    pub reference_foreground_dots: Vec<CartesianReferenceDotRender>,
    pub svg_reference_labels: Vec<CartesianReferenceLabelRender>,
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
    pub x_label: String,
    pub y_label: String,
    pub show_grid: bool,
    pub show_x_axis: bool,
    pub show_y_axis: bool,
    pub show_bars: bool,
    pub show_areas: bool,
    pub show_scatter: bool,
    pub show_markers: bool,
    pub show_reference_background: bool,
    pub show_reference_foreground: bool,
    pub show_reference_labels: bool,
    pub error: String,
    pub ready: bool,
    pub empty: bool,
    pub invalid: bool,
}

impl Default for PineCartesianChart {
    fn default() -> Self {
        let margins = ChartMargins::default();
        Self {
            label: "Cartesian chart".into(),
            empty_message: DEFAULT_EMPTY_MESSAGE.into(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            margin_top: margins.top,
            margin_right: margins.right,
            margin_bottom: margins.bottom,
            margin_left: margins.left,
            x_min: None,
            x_max: None,
            y_min: None,
            y_max: None,
            padding_inner: DEFAULT_PADDING_INNER,
            padding_outer: DEFAULT_PADDING_OUTER,
            series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
            state: "empty".into(),
            view_box: format!("0 0 {DEFAULT_WIDTH} {DEFAULT_HEIGHT}"),
            grid: None,
            x_axis_config: None,
            y_axis_config: None,
            series: Vec::new(),
            bar_series: Vec::new(),
            area_series: Vec::new(),
            scatter_series: Vec::new(),
            reference_lines: Vec::new(),
            reference_dots: Vec::new(),
            reference_labels: Vec::new(),
            bars: Vec::new(),
            areas: Vec::new(),
            line_series: Vec::new(),
            scatter_points: Vec::new(),
            markers: Vec::new(),
            reference_background_lines: Vec::new(),
            reference_foreground_lines: Vec::new(),
            reference_background_dots: Vec::new(),
            reference_foreground_dots: Vec::new(),
            svg_reference_labels: Vec::new(),
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
            x_label: String::new(),
            y_label: String::new(),
            show_grid: false,
            show_x_axis: false,
            show_y_axis: false,
            show_bars: false,
            show_areas: false,
            show_scatter: false,
            show_markers: false,
            show_reference_background: false,
            show_reference_foreground: false,
            show_reference_labels: false,
            error: String::new(),
            ready: false,
            empty: true,
            invalid: false,
        }
    }
}

#[handlers]
impl PineCartesianChart {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
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

    #[watch(padding_inner)]
    fn on_padding_inner(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(padding_outer)]
    fn on_padding_outer(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(series_padding_inner)]
    fn on_series_padding_inner(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }
}

impl PineCartesianChart {
    pub fn set_grid(&mut self, grid: CartesianGridConfig) {
        self.grid = Some(grid);
        self.recompute();
    }

    pub fn remove_grid(&mut self, key: &str) {
        if self.grid.as_ref().is_some_and(|grid| grid.key == key) {
            self.grid = None;
            self.recompute();
        }
    }

    pub fn set_x_axis(&mut self, axis: CartesianAxisConfig) {
        self.x_axis_config = Some(axis);
        self.recompute();
    }

    pub fn remove_x_axis(&mut self, key: &str) {
        if self
            .x_axis_config
            .as_ref()
            .is_some_and(|axis| axis.key == key)
        {
            self.x_axis_config = None;
            self.recompute();
        }
    }

    pub fn set_y_axis(&mut self, axis: CartesianAxisConfig) {
        self.y_axis_config = Some(axis);
        self.recompute();
    }

    pub fn remove_y_axis(&mut self, key: &str) {
        if self
            .y_axis_config
            .as_ref()
            .is_some_and(|axis| axis.key == key)
        {
            self.y_axis_config = None;
            self.recompute();
        }
    }

    pub fn upsert_line_series(&mut self, series: CartesianLineSeriesConfig) {
        let key = series.key.clone();
        if let Some(existing) = self.series.iter_mut().find(|existing| existing.key == key) {
            *existing = series;
        } else {
            self.series.push(series);
        }
        self.recompute();
    }

    pub fn remove_line_series(&mut self, key: &str) {
        self.series.retain(|series| series.key != key);
        self.recompute();
    }

    pub fn upsert_bar_series(&mut self, series: CartesianBarSeriesConfig) {
        let key = series.key.clone();
        if let Some(existing) = self
            .bar_series
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = series;
        } else {
            self.bar_series.push(series);
        }
        self.recompute();
    }

    pub fn remove_bar_series(&mut self, key: &str) {
        self.bar_series.retain(|series| series.key != key);
        self.recompute();
    }

    pub fn upsert_area_series(&mut self, series: CartesianAreaSeriesConfig) {
        let key = series.key.clone();
        if let Some(existing) = self
            .area_series
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = series;
        } else {
            self.area_series.push(series);
        }
        self.recompute();
    }

    pub fn remove_area_series(&mut self, key: &str) {
        self.area_series.retain(|series| series.key != key);
        self.recompute();
    }

    pub fn upsert_scatter_series(&mut self, series: CartesianScatterSeriesConfig) {
        let key = series.key.clone();
        if let Some(existing) = self
            .scatter_series
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = series;
        } else {
            self.scatter_series.push(series);
        }
        self.recompute();
    }

    pub fn remove_scatter_series(&mut self, key: &str) {
        self.scatter_series.retain(|series| series.key != key);
        self.recompute();
    }

    pub fn upsert_reference_line(&mut self, line: CartesianReferenceLineConfig) {
        let key = line.key.clone();
        if let Some(existing) = self
            .reference_lines
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = line;
        } else {
            self.reference_lines.push(line);
        }
        self.recompute();
    }

    pub fn remove_reference_line(&mut self, key: &str) {
        self.reference_lines.retain(|line| line.key != key);
        self.recompute();
    }

    pub fn upsert_reference_dot(&mut self, dot: CartesianReferenceDotConfig) {
        let key = dot.key.clone();
        if let Some(existing) = self
            .reference_dots
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = dot;
        } else {
            self.reference_dots.push(dot);
        }
        self.recompute();
    }

    pub fn remove_reference_dot(&mut self, key: &str) {
        self.reference_dots.retain(|dot| dot.key != key);
        self.recompute();
    }

    pub fn upsert_reference_label(&mut self, label: CartesianReferenceLabelConfig) {
        let key = label.key.clone();
        if let Some(existing) = self
            .reference_labels
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = label;
        } else {
            self.reference_labels.push(label);
        }
        self.recompute();
    }

    pub fn remove_reference_label(&mut self, key: &str) {
        self.reference_labels.retain(|label| label.key != key);
        self.recompute();
    }

    fn recompute(&mut self) {
        let options = CartesianChartOptions {
            width: self.width,
            height: self.height,
            margins: self.margins(),
            x_domain: optional_domain(self.x_min, self.x_max),
            y_domain: optional_domain(self.y_min, self.y_max),
            padding_inner: self.padding_inner,
            padding_outer: self.padding_outer,
            series_padding_inner: self.series_padding_inner,
            grid: self.grid.as_ref(),
            x_axis: self.x_axis_config.as_ref(),
            y_axis: self.y_axis_config.as_ref(),
        };

        match render_cartesian_chart(
            options,
            CartesianChartInputs {
                line_series: &self.series,
                bar_series: &self.bar_series,
                area_series: &self.area_series,
                scatter_series: &self.scatter_series,
                reference_lines: &self.reference_lines,
                reference_dots: &self.reference_dots,
                reference_labels: &self.reference_labels,
            },
        ) {
            Ok(render) => {
                self.view_box = render.view_box;
                self.plot_x = render.plot.x;
                self.plot_y = render.plot.y;
                self.plot_right = render.plot.right();
                self.plot_bottom = render.plot.bottom();
                self.x_grid = render.x_grid;
                self.y_grid = render.y_grid;
                self.x_tick_labels = render.x_tick_labels;
                self.y_tick_labels = render.y_tick_labels;
                self.x_axis_label = render.x_axis_label;
                self.y_axis_label = render.y_axis_label;
                self.x_axis = render.x_axis;
                self.y_axis = render.y_axis;
                self.bars = render.bars;
                self.areas = render.areas;
                self.line_series = render.line_series;
                self.scatter_points = render.scatter_points;
                self.markers = render.markers;
                let (foreground_lines, background_lines): (Vec<_>, Vec<_>) = render
                    .reference_lines
                    .into_iter()
                    .partition(|line| line.layer == "reference-foreground");
                let (foreground_dots, background_dots): (Vec<_>, Vec<_>) = render
                    .reference_dots
                    .into_iter()
                    .partition(|dot| dot.layer == "reference-foreground");
                self.reference_background_lines = background_lines;
                self.reference_foreground_lines = foreground_lines;
                self.reference_background_dots = background_dots;
                self.reference_foreground_dots = foreground_dots;
                self.svg_reference_labels = render.reference_labels;
                self.show_grid = !self.x_grid.is_empty() || !self.y_grid.is_empty();
                self.show_x_axis = self.x_axis_config.is_some();
                self.show_y_axis = self.y_axis_config.is_some();
                self.x_label = self
                    .x_axis_config
                    .as_ref()
                    .map(|axis| axis.label.clone())
                    .unwrap_or_default();
                self.y_label = self
                    .y_axis_config
                    .as_ref()
                    .map(|axis| axis.label.clone())
                    .unwrap_or_default();
                self.show_bars = !self.bars.is_empty();
                self.show_areas = !self.areas.is_empty();
                self.show_scatter = !self.scatter_points.is_empty();
                self.show_markers = !self.markers.is_empty();
                self.show_reference_background = !self.reference_background_lines.is_empty()
                    || !self.reference_background_dots.is_empty();
                self.show_reference_foreground = !self.reference_foreground_lines.is_empty()
                    || !self.reference_foreground_dots.is_empty();
                self.show_reference_labels = !self.svg_reference_labels.is_empty();
                self.error.clear();
                self.state = "ready".into();
                self.ready = true;
                self.empty = false;
                self.invalid = false;
            }
            Err(ChartError::EmptySeries) => {
                self.clear_render();
                self.error.clear();
                self.state = "empty".into();
                self.ready = false;
                self.empty = true;
                self.invalid = false;
            }
            Err(error) => {
                self.clear_render();
                self.error = error.to_string();
                self.state = "invalid".into();
                self.ready = false;
                self.empty = false;
                self.invalid = true;
            }
        }
    }

    fn clear_render(&mut self) {
        self.bars.clear();
        self.areas.clear();
        self.line_series.clear();
        self.scatter_points.clear();
        self.markers.clear();
        self.reference_background_lines.clear();
        self.reference_foreground_lines.clear();
        self.reference_background_dots.clear();
        self.reference_foreground_dots.clear();
        self.svg_reference_labels.clear();
        self.x_grid.clear();
        self.y_grid.clear();
        self.x_tick_labels.clear();
        self.y_tick_labels.clear();
        self.x_axis_label = SvgAxisLabel::default();
        self.y_axis_label = SvgAxisLabel::default();
        self.x_axis = SvgLine::default();
        self.y_axis = SvgLine::default();
        self.plot_x = 0.0;
        self.plot_y = 0.0;
        self.plot_right = 0.0;
        self.plot_bottom = 0.0;
        self.show_grid = false;
        self.show_x_axis = false;
        self.show_y_axis = false;
        self.show_bars = false;
        self.show_areas = false;
        self.show_scatter = false;
        self.show_markers = false;
        self.show_reference_background = false;
        self.show_reference_foreground = false;
        self.show_reference_labels = false;
    }

    fn margins(&self) -> ChartMargins {
        ChartMargins::new(
            self.margin_top,
            self.margin_right,
            self.margin_bottom,
            self.margin_left,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineChartGrid.poco", role = "visual")]
pub struct PineChartGrid {
    #[prop]
    pub key: String,
    #[prop]
    pub x: bool,
    #[prop]
    pub y: bool,
    pub component_key: String,
}

impl Default for PineChartGrid {
    fn default() -> Self {
        Self {
            key: String::new(),
            x: true,
            y: true,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineChartGrid {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "grid", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_grid(&self.component_key));
    }

    #[watch(x)]
    fn on_x(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }
}

impl PineChartGrid {
    fn sync(&self) {
        update_root(|root| {
            root.set_grid(CartesianGridConfig {
                key: self.component_key.clone(),
                x: self.x,
                y: self.y,
            });
        });
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[component(template = "PineXAxis.poco", role = "visual")]
pub struct PineXAxis {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    pub component_key: String,
}

#[handlers]
impl PineXAxis {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "x-axis", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_x_axis(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineXAxis {
    fn sync(&self) {
        update_root(|root| {
            root.set_x_axis(CartesianAxisConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
            });
        });
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[component(template = "PineYAxis.poco", role = "visual")]
pub struct PineYAxis {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    pub component_key: String,
}

#[handlers]
impl PineYAxis {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "y-axis", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_y_axis(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineYAxis {
    fn sync(&self) {
        update_root(|root| {
            root.set_y_axis(CartesianAxisConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineLineSeries.poco", role = "visual")]
pub struct PineLineSeries {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub color: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub show_markers: bool,
    #[prop]
    pub marker_radius: f64,
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub data: Vec<ChartBar>,
    #[prop]
    pub visible: bool,
    pub component_key: String,
}

impl Default for PineLineSeries {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            color: "currentColor".into(),
            stroke_width: DEFAULT_STROKE_WIDTH,
            show_markers: false,
            marker_radius: DEFAULT_MARKER_RADIUS,
            points: Vec::new(),
            data: Vec::new(),
            visible: true,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineLineSeries {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "line-series", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_line_series(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(show_markers)]
    fn on_show_markers(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }

    #[watch(marker_radius)]
    fn on_marker_radius(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartPoint>, _: Option<Vec<ChartPoint>>) {
        self.sync();
    }

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartBar>, _: Option<Vec<ChartBar>>) {
        self.sync();
    }

    #[watch(visible)]
    fn on_visible(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }
}

impl PineLineSeries {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_line_series(CartesianLineSeriesConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                color: color_or_current(&self.color),
                stroke_width: self.stroke_width,
                show_markers: self.show_markers,
                marker_radius: self.marker_radius,
                points: self.points.clone(),
                data: self.data.clone(),
                visible: self.visible,
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineBarSeries.poco", role = "visual")]
pub struct PineBarSeries {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub color: String,
    #[prop]
    pub data: Vec<ChartBar>,
    #[prop]
    pub visible: bool,
    pub component_key: String,
}

impl Default for PineBarSeries {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            color: "currentColor".into(),
            data: Vec::new(),
            visible: true,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineBarSeries {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "bar-series", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_bar_series(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartBar>, _: Option<Vec<ChartBar>>) {
        self.sync();
    }

    #[watch(visible)]
    fn on_visible(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }
}

impl PineBarSeries {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_bar_series(CartesianBarSeriesConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                color: color_or_current(&self.color),
                data: self.data.clone(),
                visible: self.visible,
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineAreaSeries.poco", role = "visual")]
pub struct PineAreaSeries {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub fill: String,
    #[prop]
    pub color: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub visible: bool,
    pub component_key: String,
}

impl Default for PineAreaSeries {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            fill: "currentColor".into(),
            color: "currentColor".into(),
            stroke_width: DEFAULT_STROKE_WIDTH,
            points: Vec::new(),
            visible: true,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineAreaSeries {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "area-series", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_area_series(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartPoint>, _: Option<Vec<ChartPoint>>) {
        self.sync();
    }

    #[watch(visible)]
    fn on_visible(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }
}

impl PineAreaSeries {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_area_series(CartesianAreaSeriesConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                fill: color_or_current(&self.fill),
                color: color_or_current(&self.color),
                stroke_width: self.stroke_width,
                points: self.points.clone(),
                visible: self.visible,
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineScatterSeries.poco", role = "visual")]
pub struct PineScatterSeries {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub color: String,
    #[prop]
    pub point_radius: f64,
    #[prop]
    pub points: Vec<ChartPoint>,
    #[prop]
    pub visible: bool,
    pub component_key: String,
}

impl Default for PineScatterSeries {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            color: "currentColor".into(),
            point_radius: DEFAULT_MARKER_RADIUS,
            points: Vec::new(),
            visible: true,
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineScatterSeries {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "scatter-series", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_scatter_series(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(point_radius)]
    fn on_point_radius(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(points)]
    fn on_points(&mut self, _: Vec<ChartPoint>, _: Option<Vec<ChartPoint>>) {
        self.sync();
    }

    #[watch(visible)]
    fn on_visible(&mut self, _: bool, _: Option<bool>) {
        self.sync();
    }
}

impl PineScatterSeries {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_scatter_series(CartesianScatterSeriesConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                color: color_or_current(&self.color),
                point_radius: self.point_radius,
                points: self.points.clone(),
                visible: self.visible,
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianReferenceLine.poco", role = "visual")]
pub struct PineCartesianReferenceLine {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub x: Option<f64>,
    #[prop]
    pub y: Option<f64>,
    #[prop]
    pub color: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub stroke_dasharray: String,
    #[prop]
    pub layer: String,
    pub component_key: String,
}

impl Default for PineCartesianReferenceLine {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            x: None,
            y: None,
            color: "currentColor".into(),
            stroke_width: 1.0,
            stroke_dasharray: String::new(),
            layer: "reference-background".into(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineCartesianReferenceLine {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "reference-line", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_reference_line(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.sync();
    }

    #[watch(color)]
    fn on_color(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(stroke_dasharray)]
    fn on_stroke_dasharray(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(layer)]
    fn on_layer(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineCartesianReferenceLine {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_reference_line(CartesianReferenceLineConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                x: self.x,
                y: self.y,
                color: color_or_current(&self.color),
                stroke_width: self.stroke_width,
                stroke_dasharray: self.stroke_dasharray.clone(),
                layer: self.layer.clone(),
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianReferenceDot.poco", role = "visual")]
pub struct PineCartesianReferenceDot {
    #[prop]
    pub key: String,
    #[prop]
    pub label: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub radius: f64,
    #[prop]
    pub fill: String,
    #[prop]
    pub stroke: String,
    #[prop]
    pub stroke_width: f64,
    #[prop]
    pub layer: String,
    pub component_key: String,
}

impl Default for PineCartesianReferenceDot {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            x: 0.0,
            y: 0.0,
            radius: DEFAULT_REFERENCE_RADIUS,
            fill: "currentColor".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            layer: "reference-foreground".into(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineCartesianReferenceDot {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "reference-dot", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_reference_dot(&self.component_key));
    }

    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(radius)]
    fn on_radius(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke)]
    fn on_stroke(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(stroke_width)]
    fn on_stroke_width(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(layer)]
    fn on_layer(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineCartesianReferenceDot {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_reference_dot(CartesianReferenceDotConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                x: self.x,
                y: self.y,
                radius: self.radius,
                fill: color_or_current(&self.fill),
                stroke: color_or(&self.stroke, "none"),
                stroke_width: self.stroke_width,
                layer: self.layer.clone(),
            });
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianReferenceLabel.poco", role = "visual")]
pub struct PineCartesianReferenceLabel {
    #[prop]
    pub key: String,
    #[prop]
    pub text: String,
    #[prop]
    pub x: f64,
    #[prop]
    pub y: f64,
    #[prop]
    pub dx: f64,
    #[prop]
    pub dy: f64,
    #[prop]
    pub angle: f64,
    #[prop]
    pub fill: String,
    #[prop]
    pub text_anchor: String,
    #[prop]
    pub font_weight: String,
    pub component_key: String,
}

impl Default for PineCartesianReferenceLabel {
    fn default() -> Self {
        Self {
            key: String::new(),
            text: String::new(),
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            angle: 0.0,
            fill: "currentColor".into(),
            text_anchor: "middle".into(),
            font_weight: "600".into(),
            component_key: String::new(),
        }
    }
}

#[handlers]
impl PineCartesianReferenceLabel {
    fn on_setup(&mut self) {
        ensure_component_key(&mut self.component_key, "reference-label", &self.key);
        self.sync();
    }

    fn on_unmount(&mut self) {
        update_root(|root| root.remove_reference_label(&self.component_key));
    }

    #[watch(text)]
    fn on_text(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(x)]
    fn on_x(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(y)]
    fn on_y(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(dx)]
    fn on_dx(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(dy)]
    fn on_dy(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(angle)]
    fn on_angle(&mut self, _: f64, _: Option<f64>) {
        self.sync();
    }

    #[watch(fill)]
    fn on_fill(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(text_anchor)]
    fn on_text_anchor(&mut self, _: String, _: Option<String>) {
        self.sync();
    }

    #[watch(font_weight)]
    fn on_font_weight(&mut self, _: String, _: Option<String>) {
        self.sync();
    }
}

impl PineCartesianReferenceLabel {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_reference_label(CartesianReferenceLabelConfig {
                key: self.component_key.clone(),
                text: self.text.clone(),
                x: self.x,
                y: self.y,
                dx: self.dx,
                dy: self.dy,
                angle: self.angle,
                fill: color_or_current(&self.fill),
                text_anchor: self.text_anchor.clone(),
                font_weight: self.font_weight.clone(),
            });
        });
    }
}

pub fn render_cartesian_chart(
    options: CartesianChartOptions<'_>,
    inputs: CartesianChartInputs<'_>,
) -> ChartResult<CartesianChartRender> {
    if !has_renderable_series(
        inputs.line_series,
        inputs.bar_series,
        inputs.area_series,
        inputs.scatter_series,
    ) {
        return Err(ChartError::EmptySeries);
    }

    if uses_categorical_x(inputs.line_series, inputs.bar_series) {
        return render_categorical_chart(options, inputs);
    }

    let renderable_lines: Vec<&CartesianLineSeriesConfig> = inputs
        .line_series
        .iter()
        .filter(|series| series.visible && !series.points.is_empty())
        .collect();
    let renderable_areas: Vec<&CartesianAreaSeriesConfig> = inputs
        .area_series
        .iter()
        .filter(|series| series.visible && !series.points.is_empty())
        .collect();
    let renderable_scatters: Vec<&CartesianScatterSeriesConfig> = inputs
        .scatter_series
        .iter()
        .filter(|series| series.visible && !series.points.is_empty())
        .collect();
    let numeric_series = renderable_lines
        .iter()
        .map(|series| ChartLineSeries::new(series.label.clone(), series.points.clone()))
        .chain(
            renderable_areas
                .iter()
                .map(|series| ChartLineSeries::new(series.label.clone(), series.points.clone())),
        )
        .chain(
            renderable_scatters
                .iter()
                .map(|series| ChartLineSeries::new(series.label.clone(), series.points.clone())),
        )
        .collect::<Vec<_>>();
    let line_options = LineChartOptions {
        width: options.width,
        height: options.height,
        margins: options.margins,
        x_domain: options.x_domain,
        y_domain: options.y_domain,
    };
    let geometry = LineChartGeometry::from_series(&numeric_series, &line_options)?;

    render_from_geometry(
        geometry,
        options.grid,
        options.x_axis,
        options.y_axis,
        GeometryRenderInputs {
            line_config: &renderable_lines,
            area_config: &renderable_areas,
            scatter_config: &renderable_scatters,
            reference_lines: inputs.reference_lines,
            reference_dots: inputs.reference_dots,
            reference_labels: inputs.reference_labels,
        },
    )
}

#[derive(Clone, Copy)]
struct GeometryRenderInputs<'a> {
    line_config: &'a [&'a CartesianLineSeriesConfig],
    area_config: &'a [&'a CartesianAreaSeriesConfig],
    scatter_config: &'a [&'a CartesianScatterSeriesConfig],
    reference_lines: &'a [CartesianReferenceLineConfig],
    reference_dots: &'a [CartesianReferenceDotConfig],
    reference_labels: &'a [CartesianReferenceLabelConfig],
}

fn render_from_geometry(
    geometry: LineChartGeometry,
    grid: Option<&CartesianGridConfig>,
    x_axis: Option<&CartesianAxisConfig>,
    y_axis: Option<&CartesianAxisConfig>,
    inputs: GeometryRenderInputs<'_>,
) -> ChartResult<CartesianChartRender> {
    let line_end = inputs.line_config.len();
    let area_start = line_end;
    let area_end = area_start + inputs.area_config.len();
    let scatter_start = area_end;
    let scatter_end = scatter_start + inputs.scatter_config.len();
    if geometry.series.len() != scatter_end {
        return Err(ChartError::InvalidOption {
            field: "cartesian.series_order",
            value: format!(
                "expected {} derived series, got {}",
                scatter_end,
                geometry.series.len()
            ),
        });
    }

    let line_geometry = &geometry.series[..line_end];
    let area_geometry = &geometry.series[area_start..area_end];
    let scatter_geometry = &geometry.series[scatter_start..scatter_end];

    let rendered_series = line_geometry
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let config = inputs.line_config[index];
            CartesianLineSeriesRender {
                key: config.key.clone(),
                label: series.label.clone(),
                line_d: series.line_d.clone(),
                color: color_or_current(&config.color),
                stroke_width: positive_or_default(config.stroke_width, DEFAULT_STROKE_WIDTH),
            }
        })
        .collect::<Vec<_>>();

    let markers = line_geometry
        .iter()
        .enumerate()
        .filter(|(index, _)| inputs.line_config[*index].show_markers)
        .flat_map(|(index, series)| {
            series
                .samples
                .iter()
                .map(move |sample| marker_from_sample(sample, inputs.line_config[index]))
        })
        .collect();

    let areas = area_geometry
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let config = inputs.area_config[index];
            Ok(CartesianAreaSeriesRender {
                key: config.key.clone(),
                label: series.label.clone(),
                area_d: area_path(
                    series.samples.iter().map(|sample| crate::geometry::Point {
                        x: sample.x,
                        y: sample.y,
                    }),
                    geometry.plot.bottom(),
                )?,
                line_d: series.line_d.clone(),
                fill: color_or_current(&config.fill),
                color: color_or_current(&config.color),
                stroke_width: positive_or_default(config.stroke_width, DEFAULT_STROKE_WIDTH),
            })
        })
        .collect::<ChartResult<Vec<_>>>()?;

    let scatter_points = scatter_geometry
        .iter()
        .enumerate()
        .flat_map(|(index, series)| {
            series
                .samples
                .iter()
                .map(move |sample| scatter_point_from_sample(sample, inputs.scatter_config[index]))
        })
        .collect();
    let rendered_reference_lines = render_reference_lines(
        inputs.reference_lines,
        geometry.plot,
        |value| geometry.x_scale.map(value),
        |value| geometry.y_scale.map(value),
    )?;
    let rendered_reference_dots = render_reference_dots(
        inputs.reference_dots,
        |value| geometry.x_scale.map(value),
        |value| geometry.y_scale.map(value),
    )?;
    let rendered_reference_labels = render_reference_labels(
        inputs.reference_labels,
        |value| geometry.x_scale.map(value),
        |value| geometry.y_scale.map(value),
    )?;

    let grid = grid.cloned().unwrap_or(CartesianGridConfig {
        key: String::new(),
        x: false,
        y: false,
    });

    Ok(CartesianChartRender {
        view_box: geometry.view_box,
        plot: geometry.plot,
        x_grid: if grid.x { geometry.x_grid } else { Vec::new() },
        y_grid: if grid.y { geometry.y_grid } else { Vec::new() },
        x_tick_labels: if x_axis.is_some() {
            geometry.x_tick_labels
        } else {
            Vec::new()
        },
        y_tick_labels: if y_axis.is_some() {
            geometry.y_tick_labels
        } else {
            Vec::new()
        },
        x_axis_label: geometry.x_axis_label,
        y_axis_label: geometry.y_axis_label,
        x_axis: geometry.x_axis,
        y_axis: geometry.y_axis,
        bars: Vec::new(),
        areas,
        line_series: rendered_series,
        scatter_points,
        markers,
        reference_lines: rendered_reference_lines,
        reference_dots: rendered_reference_dots,
        reference_labels: rendered_reference_labels,
    })
}

fn render_categorical_chart(
    options: CartesianChartOptions<'_>,
    inputs: CartesianChartInputs<'_>,
) -> ChartResult<CartesianChartRender> {
    let categories = categorical_categories(inputs.line_series, inputs.bar_series)?;
    let width = finite("width", options.width)?;
    let height = finite("height", options.height)?;
    let plot = ChartRect::from_outer(width, height, options.margins)?;
    let y_domain = categorical_y_domain(
        options.y_domain,
        inputs.line_series,
        inputs.bar_series,
        inputs.area_series,
        inputs.scatter_series,
    )?;
    let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
    let x_scale = BandScale::new(
        categories.len(),
        (plot.x, plot.right()),
        options.padding_inner,
        options.padding_outer,
    )?;
    let baseline = baseline_value(y_domain);
    let baseline_y = y_scale.map(baseline)?;
    let y_ticks = y_scale.ticks(5);
    let grid = options.grid.cloned().unwrap_or(CartesianGridConfig {
        key: String::new(),
        x: false,
        y: false,
    });

    let bars = render_categorical_bars(
        inputs.bar_series,
        &categories,
        x_scale,
        y_scale,
        baseline_y,
        options.series_padding_inner,
    )?;
    let (line_series, markers) =
        render_categorical_lines(inputs.line_series, &categories, x_scale, y_scale)?;
    let areas = render_categorical_areas(inputs.area_series, &categories, x_scale, y_scale, plot)?;
    let scatter_points =
        render_categorical_scatter_points(inputs.scatter_series, &categories, x_scale, y_scale)?;
    let rendered_reference_lines = render_reference_lines(
        inputs.reference_lines,
        plot,
        |value| categorical_point_x(value, categories.len(), x_scale),
        |value| y_scale.map(value),
    )?;
    let rendered_reference_dots = render_reference_dots(
        inputs.reference_dots,
        |value| categorical_point_x(value, categories.len(), x_scale),
        |value| y_scale.map(value),
    )?;
    let rendered_reference_labels = render_reference_labels(
        inputs.reference_labels,
        |value| categorical_point_x(value, categories.len(), x_scale),
        |value| y_scale.map(value),
    )?;

    Ok(CartesianChartRender {
        view_box: format!("0 0 {width} {height}"),
        plot,
        x_grid: if grid.x {
            categorical_x_grid(&categories, x_scale, plot)
        } else {
            Vec::new()
        },
        y_grid: if grid.y {
            grid_lines_for_y(&y_ticks, plot)
        } else {
            Vec::new()
        },
        x_tick_labels: if options.x_axis.is_some() {
            category_tick_labels(&categories, x_scale, plot)
        } else {
            Vec::new()
        },
        y_tick_labels: if options.y_axis.is_some() {
            tick_labels_for_y(&y_ticks, plot)
        } else {
            Vec::new()
        },
        x_axis_label: x_axis_label(plot, height),
        y_axis_label: y_axis_label(plot),
        x_axis: SvgLine::new(
            "x-axis".into(),
            plot.x,
            baseline_y,
            plot.right(),
            baseline_y,
        ),
        y_axis: SvgLine::new("y-axis".into(), plot.x, plot.y, plot.x, plot.bottom()),
        bars,
        areas,
        line_series,
        scatter_points,
        markers,
        reference_lines: rendered_reference_lines,
        reference_dots: rendered_reference_dots,
        reference_labels: rendered_reference_labels,
    })
}

fn has_renderable_series(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
    area_series: &[CartesianAreaSeriesConfig],
    scatter_series: &[CartesianScatterSeriesConfig],
) -> bool {
    line_series
        .iter()
        .any(|series| series.visible && (!series.points.is_empty() || !series.data.is_empty()))
        || bar_series
            .iter()
            .any(|series| series.visible && !series.data.is_empty())
        || area_series
            .iter()
            .any(|series| series.visible && !series.points.is_empty())
        || scatter_series
            .iter()
            .any(|series| series.visible && !series.points.is_empty())
}

fn uses_categorical_x(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> bool {
    bar_series
        .iter()
        .any(|series| series.visible && !series.data.is_empty())
        || line_series
            .iter()
            .any(|series| series.visible && !series.data.is_empty())
}

fn categorical_categories(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> ChartResult<Vec<String>> {
    let categories = bar_series
        .iter()
        .find(|series| series.visible && !series.data.is_empty())
        .map(|series| {
            series
                .data
                .iter()
                .map(|bar| bar.label.clone())
                .collect::<Vec<_>>()
        })
        .or_else(|| {
            line_series
                .iter()
                .find(|series| series.visible && !series.data.is_empty())
                .map(|series| {
                    series
                        .data
                        .iter()
                        .map(|bar| bar.label.clone())
                        .collect::<Vec<_>>()
                })
        })
        .ok_or(ChartError::EmptySeries)?;

    for series in bar_series
        .iter()
        .filter(|series| series.visible && !series.data.is_empty())
    {
        validate_categories(
            &series_label_or_default(&series.label, 0),
            &categories,
            &series.data,
        )?;
    }
    for series in line_series
        .iter()
        .filter(|series| series.visible && !series.data.is_empty())
    {
        validate_categories(
            &series_label_or_default(&series.label, 0),
            &categories,
            &series.data,
        )?;
    }

    Ok(categories)
}

fn validate_categories(series: &str, categories: &[String], data: &[ChartBar]) -> ChartResult<()> {
    if data.len() != categories.len() {
        let expected = categories
            .get(data.len())
            .or_else(|| categories.last())
            .cloned()
            .unwrap_or_default();
        return Err(ChartError::MismatchedSeries {
            series: series.into(),
            expected,
            actual: String::new(),
        });
    }

    for (bar, expected) in data.iter().zip(categories.iter()) {
        if bar.label != *expected {
            return Err(ChartError::MismatchedSeries {
                series: series.into(),
                expected: expected.clone(),
                actual: bar.label.clone(),
            });
        }
    }

    Ok(())
}

fn categorical_y_domain(
    domain: Option<(f64, f64)>,
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
    area_series: &[CartesianAreaSeriesConfig],
    scatter_series: &[CartesianScatterSeriesConfig],
) -> ChartResult<(f64, f64)> {
    let include_zero = bar_series
        .iter()
        .any(|series| series.visible && !series.data.is_empty());
    let values = bar_series
        .iter()
        .filter(|series| series.visible)
        .flat_map(|series| series.data.iter().map(|bar| bar.value))
        .chain(
            line_series
                .iter()
                .filter(|series| series.visible)
                .flat_map(|series| series.data.iter().map(|bar| bar.value)),
        )
        .chain(
            line_series
                .iter()
                .filter(|series| series.visible)
                .flat_map(|series| series.points.iter().map(|point| point.y)),
        )
        .chain(
            area_series
                .iter()
                .filter(|series| series.visible)
                .flat_map(|series| series.points.iter().map(|point| point.y)),
        )
        .chain(
            scatter_series
                .iter()
                .filter(|series| series.visible)
                .flat_map(|series| series.points.iter().map(|point| point.y)),
        );

    domain_or_y_extent(domain, values, include_zero)
}

fn render_categorical_bars(
    bar_series: &[CartesianBarSeriesConfig],
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
    baseline_y: f64,
    series_padding_inner: f64,
) -> ChartResult<Vec<CartesianBarRender>> {
    let renderable = bar_series
        .iter()
        .filter(|series| series.visible && !series.data.is_empty())
        .collect::<Vec<_>>();
    let mut bars = Vec::with_capacity(categories.len() * renderable.len());

    for (category_index, category_label) in categories.iter().enumerate() {
        let category_x = category_scale.position(category_index).unwrap_or_default();
        let series_scale = BandScale::new(
            renderable.len(),
            (category_x, category_x + category_scale.bandwidth()),
            series_padding_inner,
            0.0,
        )?;

        for (series_index, series) in renderable.iter().enumerate() {
            let value = finite("bar.value", series.data[category_index].value)?;
            let value_y = y_scale.map(value)?;
            let y = value_y.min(baseline_y);
            let height = (baseline_y - value_y).abs();
            let x = series_scale.position(series_index).unwrap_or(category_x);
            let series_label = series_label_or_default(&series.label, series_index);
            bars.push(CartesianBarRender {
                key: format!("{}-bar-{category_index}-{series_index}", series.key),
                label: category_label.clone(),
                category_label: category_label.clone(),
                series_label: series_label.clone(),
                value,
                aria_label: bar_aria_label(category_label, &series_label, value),
                color: color_or_current(&series.color),
                x,
                y,
                width: series_scale.bandwidth(),
                height,
            });
        }
    }

    Ok(bars)
}

fn render_categorical_lines(
    line_series: &[CartesianLineSeriesConfig],
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<(Vec<CartesianLineSeriesRender>, Vec<CartesianMarkerRender>)> {
    let mut rendered = Vec::new();
    let mut markers = Vec::new();

    for (series_index, config) in line_series
        .iter()
        .filter(|series| series.visible && (!series.data.is_empty() || !series.points.is_empty()))
        .enumerate()
    {
        let series_label = series_label_or_default(&config.label, series_index);
        let samples = if !config.data.is_empty() {
            categorical_data_samples(config, &series_label, categories, category_scale, y_scale)?
        } else {
            categorical_point_samples(config, &series_label, categories, category_scale, y_scale)?
        };
        let line_d = line_path(samples.iter().map(|sample| crate::geometry::Point {
            x: sample.x,
            y: sample.y,
        }))?;
        rendered.push(CartesianLineSeriesRender {
            key: config.key.clone(),
            label: series_label.clone(),
            line_d,
            color: color_or_current(&config.color),
            stroke_width: positive_or_default(config.stroke_width, DEFAULT_STROKE_WIDTH),
        });
        if config.show_markers {
            markers.extend(
                samples
                    .iter()
                    .map(|sample| marker_from_sample(sample, config)),
            );
        }
    }

    Ok((rendered, markers))
}

fn render_categorical_areas(
    area_series: &[CartesianAreaSeriesConfig],
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
    plot: ChartRect,
) -> ChartResult<Vec<CartesianAreaSeriesRender>> {
    area_series
        .iter()
        .filter(|series| series.visible && !series.points.is_empty())
        .enumerate()
        .map(|(series_index, config)| {
            let series_label = series_label_or_default(&config.label, series_index);
            let samples = categorical_points_samples(
                &config.key,
                &config.points,
                &series_label,
                categories,
                category_scale,
                y_scale,
            )?;
            let points = samples
                .iter()
                .map(|sample| crate::geometry::Point {
                    x: sample.x,
                    y: sample.y,
                })
                .collect::<Vec<_>>();
            Ok(CartesianAreaSeriesRender {
                key: config.key.clone(),
                label: series_label,
                area_d: area_path(points.iter().copied(), plot.bottom())?,
                line_d: line_path(points.iter().copied())?,
                fill: color_or_current(&config.fill),
                color: color_or_current(&config.color),
                stroke_width: positive_or_default(config.stroke_width, DEFAULT_STROKE_WIDTH),
            })
        })
        .collect()
}

fn render_categorical_scatter_points(
    scatter_series: &[CartesianScatterSeriesConfig],
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<Vec<CartesianScatterPointRender>> {
    let mut points = Vec::new();
    for (series_index, config) in scatter_series
        .iter()
        .filter(|series| series.visible && !series.points.is_empty())
        .enumerate()
    {
        let series_label = series_label_or_default(&config.label, series_index);
        let samples = categorical_points_samples(
            &config.key,
            &config.points,
            &series_label,
            categories,
            category_scale,
            y_scale,
        )?;
        points.extend(
            samples
                .iter()
                .map(|sample| scatter_point_from_sample(sample, config)),
        );
    }

    Ok(points)
}

fn render_reference_lines(
    lines: &[CartesianReferenceLineConfig],
    plot: ChartRect,
    mut map_x: impl FnMut(f64) -> ChartResult<f64>,
    mut map_y: impl FnMut(f64) -> ChartResult<f64>,
) -> ChartResult<Vec<CartesianReferenceLineRender>> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let layer = reference_layer(&line.layer, "reference_line.layer")?;
            let label = label_or_key(&line.label, &line.key, index);
            let (x1, y1, x2, y2, axis, value) = match (line.x, line.y) {
                (Some(x), None) => {
                    let value = finite("reference_line.x", x)?;
                    let x = map_x(value)?;
                    (x, plot.y, x, plot.bottom(), "x", value)
                }
                (None, Some(y)) => {
                    let value = finite("reference_line.y", y)?;
                    let y = map_y(value)?;
                    (plot.x, y, plot.right(), y, "y", value)
                }
                (Some(_), Some(_)) => {
                    return Err(ChartError::InvalidOption {
                        field: "reference_line.axis",
                        value: "expected exactly one of x or y, got both".into(),
                    });
                }
                (None, None) => {
                    return Err(ChartError::InvalidOption {
                        field: "reference_line.axis",
                        value: "expected exactly one of x or y, got neither".into(),
                    });
                }
            };

            Ok(CartesianReferenceLineRender {
                key: key_or_index("reference-line", &line.key, index),
                label: label.clone(),
                x1,
                y1,
                x2,
                y2,
                color: color_or_current(&line.color),
                stroke_width: positive_or_default(line.stroke_width, 1.0),
                stroke_dasharray: line.stroke_dasharray.trim().into(),
                layer: layer.into(),
                aria_label: format!("{label}: {axis} {value}"),
            })
        })
        .collect()
}

fn render_reference_dots(
    dots: &[CartesianReferenceDotConfig],
    mut map_x: impl FnMut(f64) -> ChartResult<f64>,
    mut map_y: impl FnMut(f64) -> ChartResult<f64>,
) -> ChartResult<Vec<CartesianReferenceDotRender>> {
    dots.iter()
        .enumerate()
        .map(|(index, dot)| {
            let data_x = finite("reference_dot.x", dot.x)?;
            let data_y = finite("reference_dot.y", dot.y)?;
            let label = label_or_key(&dot.label, &dot.key, index);
            Ok(CartesianReferenceDotRender {
                key: key_or_index("reference-dot", &dot.key, index),
                label: label.clone(),
                x: map_x(data_x)?,
                y: map_y(data_y)?,
                data_x,
                data_y,
                radius: positive_or_default(dot.radius, DEFAULT_REFERENCE_RADIUS),
                fill: color_or_current(&dot.fill),
                stroke: color_or(&dot.stroke, "none"),
                stroke_width: non_negative(dot.stroke_width),
                layer: reference_layer(&dot.layer, "reference_dot.layer")?.into(),
                aria_label: format!("{label}: x {data_x}, y {data_y}"),
            })
        })
        .collect()
}

fn render_reference_labels(
    labels: &[CartesianReferenceLabelConfig],
    mut map_x: impl FnMut(f64) -> ChartResult<f64>,
    mut map_y: impl FnMut(f64) -> ChartResult<f64>,
) -> ChartResult<Vec<CartesianReferenceLabelRender>> {
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let data_x = finite("reference_label.x", label.x)?;
            let data_y = finite("reference_label.y", label.y)?;
            let dx = finite("reference_label.dx", label.dx)?;
            let dy = finite("reference_label.dy", label.dy)?;
            let angle = finite("reference_label.angle", label.angle)?;
            let x = map_x(data_x)? + dx;
            let y = map_y(data_y)? + dy;
            Ok(CartesianReferenceLabelRender {
                key: key_or_index("reference-label", &label.key, index),
                text: label.text.clone(),
                x,
                y,
                data_x,
                data_y,
                fill: color_or_current(&label.fill),
                text_anchor: text_anchor(&label.text_anchor).into(),
                font_weight: if label.font_weight.trim().is_empty() {
                    "600".into()
                } else {
                    label.font_weight.trim().into()
                },
                transform: rotation_transform(angle, x, y),
            })
        })
        .collect()
}

fn categorical_data_samples(
    config: &CartesianLineSeriesConfig,
    series_label: &str,
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<Vec<LineChartSample>> {
    config
        .data
        .iter()
        .enumerate()
        .map(|(index, bar)| {
            let value = finite("line.value", bar.value)?;
            let x = category_scale.center(index).unwrap_or_default();
            let y = y_scale.map(value)?;
            Ok(LineChartSample {
                key: format!("{}-point-{index}-{}", config.key, bar.label),
                series_label: series_label.into(),
                data_x: index as f64,
                data_y: value,
                x,
                y,
                x_label: categories[index].clone(),
                y_label: format_tick(value),
                aria_label: bar_aria_label(&categories[index], series_label, value),
            })
        })
        .collect()
}

fn categorical_point_samples(
    config: &CartesianLineSeriesConfig,
    series_label: &str,
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<Vec<LineChartSample>> {
    categorical_points_samples(
        &config.key,
        &config.points,
        series_label,
        categories,
        category_scale,
        y_scale,
    )
}

fn categorical_points_samples(
    key: &str,
    points: &[ChartPoint],
    series_label: &str,
    categories: &[String],
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<Vec<LineChartSample>> {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let data_x = finite("point.x", point.x)?;
            let data_y = finite("point.y", point.y)?;
            let x = categorical_point_x(data_x, categories.len(), category_scale)?;
            let y = y_scale.map(data_y)?;
            let label_index = data_x.round().clamp(0.0, categories.len() as f64 - 1.0) as usize;
            let x_label = categories.get(label_index).cloned().unwrap_or_default();
            Ok(LineChartSample {
                key: format!("{}-point-{index}-{}", key, format_tick(data_x)),
                series_label: series_label.into(),
                data_x,
                data_y,
                x,
                y,
                x_label: x_label.clone(),
                y_label: format_tick(data_y),
                aria_label: bar_aria_label(&x_label, series_label, data_y),
            })
        })
        .collect()
}

fn categorical_point_x(
    value: f64,
    category_count: usize,
    category_scale: BandScale,
) -> ChartResult<f64> {
    if category_count <= 1 {
        return Ok(category_scale.center(0).unwrap_or_default());
    }

    let first = category_scale.center(0).unwrap_or_default();
    let last = category_scale.center(category_count - 1).unwrap_or(first);
    LinearScale::new((0.0, category_count as f64 - 1.0), (first, last))?.map(value)
}

fn categorical_x_grid(categories: &[String], scale: BandScale, plot: ChartRect) -> Vec<SvgLine> {
    categories
        .iter()
        .enumerate()
        .filter_map(|(index, label)| {
            let x = scale.center(index)?;
            Some(SvgLine::new(
                format!("x-grid-{index}-{label}"),
                x,
                plot.y,
                x,
                plot.bottom(),
            ))
        })
        .collect()
}

fn domain_or_y_extent(
    domain: Option<(f64, f64)>,
    values: impl IntoIterator<Item = f64>,
    include_zero: bool,
) -> ChartResult<(f64, f64)> {
    if let Some((start, end)) = domain {
        return expanded_domain(finite("domain.start", start)?, finite("domain.end", end)?);
    }

    let mut min = if include_zero { 0.0 } else { f64::INFINITY };
    let mut max = if include_zero { 0.0 } else { f64::NEG_INFINITY };
    let mut saw_value = include_zero;
    for value in values {
        let value = finite("domain.value", value)?;
        min = min.min(value);
        max = max.max(value);
        saw_value = true;
    }

    if !saw_value {
        return Err(ChartError::EmptySeries);
    }

    expanded_domain(min, max)
}

fn marker_from_sample(
    sample: &LineChartSample,
    config: &CartesianLineSeriesConfig,
) -> CartesianMarkerRender {
    CartesianMarkerRender {
        key: format!("{}-{}", config.key, sample.key),
        series_label: sample.series_label.clone(),
        x: sample.x,
        y: sample.y,
        data_x: sample.data_x,
        data_y: sample.data_y,
        color: color_or_current(&config.color),
        radius: positive_or_default(config.marker_radius, DEFAULT_MARKER_RADIUS),
        aria_label: sample.aria_label.clone(),
    }
}

fn scatter_point_from_sample(
    sample: &LineChartSample,
    config: &CartesianScatterSeriesConfig,
) -> CartesianScatterPointRender {
    CartesianScatterPointRender {
        key: format!("{}-{}", config.key, sample.key),
        series_label: sample.series_label.clone(),
        x: sample.x,
        y: sample.y,
        data_x: sample.data_x,
        data_y: sample.data_y,
        color: color_or_current(&config.color),
        radius: positive_or_default(config.point_radius, DEFAULT_MARKER_RADIUS),
        aria_label: sample.aria_label.clone(),
    }
}

fn update_root(f: impl FnOnce(&mut PineCartesianChart)) {
    if let Some(root) = ROOT.inject() {
        root.update(f);
    }
}

fn ensure_component_key(target: &mut String, prefix: &str, authored_key: &str) {
    if target.is_empty() {
        *target = component_key(prefix, authored_key);
    }
}

fn component_key(prefix: &str, authored_key: &str) -> String {
    let authored_key = authored_key.trim();
    if !authored_key.is_empty() {
        return authored_key.into();
    }

    current_scope_id()
        .map(|scope| format!("{prefix}-{}", scope.0))
        .unwrap_or_else(|| prefix.into())
}

fn positive_or_default(value: f64, default: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        default
    }
}

fn non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn reference_layer(value: &str, field: &'static str) -> ChartResult<&'static str> {
    match value.trim() {
        "reference-foreground" => Ok("reference-foreground"),
        "reference-background" | "" => Ok("reference-background"),
        value => Err(ChartError::InvalidOption {
            field,
            value: value.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cartesian_chart_renders_line_series_from_child_configs() {
        let series = vec![CartesianLineSeriesConfig {
            key: "actual".into(),
            label: "Actual".into(),
            color: "#1d6fd8".into(),
            stroke_width: 4.0,
            show_markers: true,
            marker_radius: 5.0,
            points: vec![
                ChartPoint::new(0.0, 10.0),
                ChartPoint::new(5.0, 20.0),
                ChartPoint::new(10.0, 15.0),
            ],
            data: Vec::new(),
            visible: true,
        }];

        let grid = CartesianGridConfig::default();
        let x_axis = CartesianAxisConfig {
            key: "x".into(),
            label: "Week".into(),
        };
        let y_axis = CartesianAxisConfig {
            key: "y".into(),
            label: "Metric".into(),
        };
        let render = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: Some(&grid),
                x_axis: Some(&x_axis),
                y_axis: Some(&y_axis),
            },
            CartesianChartInputs {
                line_series: &series,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap();

        assert_eq!(render.view_box, "0 0 100 100");
        assert_eq!(render.line_series[0].key, "actual");
        assert_eq!(render.line_series[0].line_d, "M0,100 L50,0 L100,50");
        assert!(render.bars.is_empty());
        assert_eq!(render.markers.len(), 3);
        assert!(!render.x_grid.is_empty());
        assert!(!render.x_tick_labels.is_empty());
    }

    #[test]
    fn cartesian_chart_composes_bar_and_line_series_on_categories() {
        let bars = vec![CartesianBarSeriesConfig {
            key: "actual".into(),
            label: "Actual".into(),
            color: "#16a085".into(),
            data: vec![ChartBar::new("W1", 10.0), ChartBar::new("W2", 20.0)],
            visible: true,
        }];
        let lines = vec![CartesianLineSeriesConfig {
            key: "target".into(),
            label: "Target".into(),
            color: "#1d6fd8".into(),
            stroke_width: 2.0,
            show_markers: true,
            marker_radius: 4.0,
            points: Vec::new(),
            data: vec![ChartBar::new("W1", 12.0), ChartBar::new("W2", 18.0)],
            visible: true,
        }];

        let render = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: 0.0,
                padding_outer: 0.0,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: Some(&CartesianGridConfig::default()),
                x_axis: Some(&CartesianAxisConfig {
                    key: "x".into(),
                    label: "Week".into(),
                }),
                y_axis: Some(&CartesianAxisConfig {
                    key: "y".into(),
                    label: "Metric".into(),
                }),
            },
            CartesianChartInputs {
                line_series: &lines,
                bar_series: &bars,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap();

        assert_eq!(render.bars.len(), 2);
        assert_eq!(render.bars[0].category_label, "W1");
        assert_eq!(render.bars[0].series_label, "Actual");
        assert_eq!(render.line_series[0].label, "Target");
        assert_eq!(render.markers.len(), 2);
        assert_eq!(render.x_tick_labels[0].label, "W1");
        assert_eq!(render.x_tick_labels[1].label, "W2");
    }

    #[test]
    fn cartesian_chart_composes_area_and_scatter_series() {
        let areas = vec![CartesianAreaSeriesConfig {
            key: "forecast-band".into(),
            label: "Forecast".into(),
            fill: "#1d6fd833".into(),
            color: "#1d6fd8".into(),
            stroke_width: 2.0,
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(10.0, 10.0)],
            visible: true,
        }];
        let scatters = vec![CartesianScatterSeriesConfig {
            key: "samples".into(),
            label: "Samples".into(),
            color: "#d96c2c".into(),
            point_radius: 6.0,
            points: vec![ChartPoint::new(5.0, 5.0)],
            visible: true,
        }];

        let render = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            CartesianChartInputs {
                area_series: &areas,
                scatter_series: &scatters,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap();

        assert_eq!(render.areas.len(), 1);
        assert_eq!(render.areas[0].area_d, "M0,100 L0,100 L100,0 L100,100Z");
        assert_eq!(render.areas[0].line_d, "M0,100 L100,0");
        assert_eq!(render.scatter_points.len(), 1);
        assert_eq!(render.scatter_points[0].series_label, "Samples");
        assert_eq!(render.scatter_points[0].x, 50.0);
        assert_eq!(render.scatter_points[0].y, 50.0);
        assert_eq!(render.scatter_points[0].aria_label, "Samples: x 5, y 5");
        assert!(render.line_series.is_empty());
    }

    #[test]
    fn cartesian_chart_maps_reference_marks_through_shared_scales() {
        let series = vec![CartesianLineSeriesConfig {
            key: "actual".into(),
            label: "Actual".into(),
            color: "#1d6fd8".into(),
            stroke_width: 2.0,
            show_markers: false,
            marker_radius: 3.0,
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(10.0, 10.0)],
            data: Vec::new(),
            visible: true,
        }];
        let reference_lines = vec![CartesianReferenceLineConfig {
            key: "goal".into(),
            label: "Goal".into(),
            x: None,
            y: Some(5.0),
            color: "#667085".into(),
            stroke_width: 1.5,
            stroke_dasharray: "4 4".into(),
            layer: "reference-background".into(),
        }];
        let reference_dots = vec![CartesianReferenceDotConfig {
            key: "release".into(),
            label: "Release".into(),
            x: 5.0,
            y: 5.0,
            radius: 7.0,
            fill: "#d96c2c".into(),
            stroke: "#ffffff".into(),
            stroke_width: 2.0,
            layer: "reference-foreground".into(),
        }];
        let reference_labels = vec![CartesianReferenceLabelConfig {
            key: "release-label".into(),
            text: "Release".into(),
            x: 5.0,
            y: 5.0,
            dx: 0.0,
            dy: -8.0,
            angle: 0.0,
            fill: "#18212f".into(),
            text_anchor: "middle".into(),
            font_weight: "700".into(),
        }];

        let render = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            CartesianChartInputs {
                line_series: &series,
                reference_lines: &reference_lines,
                reference_dots: &reference_dots,
                reference_labels: &reference_labels,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap();

        assert_eq!(render.reference_lines.len(), 1);
        assert_eq!(render.reference_lines[0].x1, 0.0);
        assert_eq!(render.reference_lines[0].y1, 50.0);
        assert_eq!(render.reference_lines[0].x2, 100.0);
        assert_eq!(render.reference_lines[0].layer, "reference-background");
        assert_eq!(render.reference_dots[0].x, 50.0);
        assert_eq!(render.reference_dots[0].y, 50.0);
        assert_eq!(render.reference_dots[0].layer, "reference-foreground");
        assert_eq!(render.reference_labels[0].x, 50.0);
        assert_eq!(render.reference_labels[0].y, 42.0);
        assert_eq!(render.reference_labels[0].text, "Release");
    }

    #[test]
    fn cartesian_chart_rejects_reference_line_without_exactly_one_axis() {
        let series = vec![CartesianLineSeriesConfig {
            key: "actual".into(),
            label: "Actual".into(),
            color: "#1d6fd8".into(),
            stroke_width: 2.0,
            show_markers: false,
            marker_radius: 3.0,
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(10.0, 10.0)],
            data: Vec::new(),
            visible: true,
        }];
        let reference_lines = vec![CartesianReferenceLineConfig {
            key: "bad".into(),
            label: "Bad".into(),
            x: Some(1.0),
            y: Some(2.0),
            color: "#667085".into(),
            stroke_width: 1.0,
            stroke_dasharray: String::new(),
            layer: "reference-background".into(),
        }];

        let error = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            CartesianChartInputs {
                line_series: &series,
                reference_lines: &reference_lines,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            ChartError::InvalidOption {
                field: "reference_line.axis",
                value: "expected exactly one of x or y, got both".into(),
            }
        );
    }

    #[test]
    fn cartesian_chart_rejects_short_reference_layer_aliases() {
        let series = vec![CartesianLineSeriesConfig {
            key: "actual".into(),
            label: "Actual".into(),
            color: "#1d6fd8".into(),
            stroke_width: 2.0,
            show_markers: false,
            marker_radius: 3.0,
            points: vec![ChartPoint::new(0.0, 0.0), ChartPoint::new(10.0, 10.0)],
            data: Vec::new(),
            visible: true,
        }];
        let reference_dots = vec![CartesianReferenceDotConfig {
            key: "dot".into(),
            label: "Dot".into(),
            x: 1.0,
            y: 2.0,
            radius: 6.0,
            fill: "#d96c2c".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            layer: "foreground".into(),
        }];

        let error = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            CartesianChartInputs {
                line_series: &series,
                reference_dots: &reference_dots,
                ..CartesianChartInputs::default()
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            ChartError::InvalidOption {
                field: "reference_dot.layer",
                value: "foreground".into(),
            }
        );
    }

    #[test]
    fn cartesian_chart_without_series_is_empty() {
        let error = render_cartesian_chart(
            CartesianChartOptions {
                width: 100.0,
                height: 100.0,
                margins: ChartMargins::ZERO,
                x_domain: None,
                y_domain: None,
                padding_inner: DEFAULT_PADDING_INNER,
                padding_outer: DEFAULT_PADDING_OUTER,
                series_padding_inner: DEFAULT_SERIES_PADDING_INNER,
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            CartesianChartInputs::default(),
        )
        .unwrap_err();

        assert_eq!(error, ChartError::EmptySeries);
    }
}
