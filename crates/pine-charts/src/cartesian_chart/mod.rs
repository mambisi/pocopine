use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id};
use serde::{Deserialize, Serialize};

use crate::bar::{baseline_value, bar_aria_label, category_tick_labels, ChartBar};
use crate::cartesian::{
    expanded_domain, grid_lines_for_y, optional_domain, tick_labels_for_y, x_axis_label,
    y_axis_label,
};
use crate::legend::series_label_or_default;
use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect};
use crate::line::{
    ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions, LineChartSample,
};
use crate::path::line_path;
use crate::scale::{BandScale, LinearScale};
use crate::svg::{format_tick, SvgAxisLabel, SvgLine, SvgTickLabel};

const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 320.0;
const DEFAULT_STROKE_WIDTH: f64 = 3.0;
const DEFAULT_MARKER_RADIUS: f64 = 3.0;
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartesianBarSeriesConfig {
    pub key: String,
    pub label: String,
    pub color: String,
    pub data: Vec<ChartBar>,
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
    pub line_series: Vec<CartesianLineSeriesRender>,
    pub markers: Vec<CartesianMarkerRender>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianChart.poco", role = "panel")]
#[slot(default, accepts = [PineChartGrid, PineXAxis, PineYAxis, PineLineSeries, PineBarSeries])]
pub struct PineCartesianChart {
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
    pub bars: Vec<CartesianBarRender>,
    pub line_series: Vec<CartesianLineSeriesRender>,
    pub markers: Vec<CartesianMarkerRender>,
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
    pub show_markers: bool,
    pub error: String,
    pub ready: bool,
    pub empty: bool,
    pub invalid: bool,
}

impl Default for PineCartesianChart {
    fn default() -> Self {
        let options = LineChartOptions::default();
        Self {
            label: "Cartesian chart".into(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            margin_top: options.margins.top,
            margin_right: options.margins.right,
            margin_bottom: options.margins.bottom,
            margin_left: options.margins.left,
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
            bars: Vec::new(),
            line_series: Vec::new(),
            markers: Vec::new(),
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
            show_markers: false,
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

        match render_cartesian_chart(options, &self.series, &self.bar_series) {
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
                self.line_series = render.line_series;
                self.markers = render.markers;
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
                self.show_markers = !self.markers.is_empty();
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
        self.line_series.clear();
        self.markers.clear();
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
        self.show_markers = false;
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
    pub component_key: String,
}

impl Default for PineBarSeries {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            color: "currentColor".into(),
            data: Vec::new(),
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
}

impl PineBarSeries {
    fn sync(&self) {
        update_root(|root| {
            root.upsert_bar_series(CartesianBarSeriesConfig {
                key: self.component_key.clone(),
                label: self.label.clone(),
                color: color_or_current(&self.color),
                data: self.data.clone(),
            });
        });
    }
}

pub fn render_cartesian_chart(
    options: CartesianChartOptions<'_>,
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> ChartResult<CartesianChartRender> {
    if !has_renderable_series(line_series, bar_series) {
        return Err(ChartError::EmptySeries);
    }

    if uses_categorical_x(line_series, bar_series) {
        return render_categorical_chart(options, line_series, bar_series);
    }

    let renderable_lines: Vec<&CartesianLineSeriesConfig> = line_series
        .iter()
        .filter(|series| !series.points.is_empty())
        .collect();
    let line_series = renderable_lines
        .iter()
        .map(|series| ChartLineSeries::new(series.label.clone(), series.points.clone()))
        .collect::<Vec<_>>();
    let line_options = LineChartOptions {
        width: options.width,
        height: options.height,
        margins: options.margins,
        x_domain: options.x_domain,
        y_domain: options.y_domain,
    };
    let geometry = LineChartGeometry::from_series(&line_series, &line_options)?;

    Ok(render_from_geometry(
        geometry,
        options.grid,
        options.x_axis,
        options.y_axis,
        &renderable_lines,
    ))
}

fn render_from_geometry(
    geometry: LineChartGeometry,
    grid: Option<&CartesianGridConfig>,
    x_axis: Option<&CartesianAxisConfig>,
    y_axis: Option<&CartesianAxisConfig>,
    series_config: &[&CartesianLineSeriesConfig],
) -> CartesianChartRender {
    let rendered_series = geometry
        .series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let config = series_config[index];
            CartesianLineSeriesRender {
                key: config.key.clone(),
                label: series.label.clone(),
                line_d: series.line_d.clone(),
                color: color_or_current(&config.color),
                stroke_width: positive_or_default(config.stroke_width, DEFAULT_STROKE_WIDTH),
            }
        })
        .collect::<Vec<_>>();

    let markers = geometry
        .series
        .iter()
        .enumerate()
        .filter(|(index, _)| series_config[*index].show_markers)
        .flat_map(|(index, series)| {
            series
                .samples
                .iter()
                .map(move |sample| marker_from_sample(sample, series_config[index]))
        })
        .collect();

    let grid = grid.cloned().unwrap_or(CartesianGridConfig {
        key: String::new(),
        x: false,
        y: false,
    });

    CartesianChartRender {
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
        line_series: rendered_series,
        markers,
    }
}

fn render_categorical_chart(
    options: CartesianChartOptions<'_>,
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> ChartResult<CartesianChartRender> {
    let categories = categorical_categories(line_series, bar_series)?;
    let width = finite("width", options.width)?;
    let height = finite("height", options.height)?;
    let plot = ChartRect::from_outer(width, height, options.margins)?;
    let y_domain = categorical_y_domain(options.y_domain, line_series, bar_series)?;
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
        bar_series,
        &categories,
        x_scale,
        y_scale,
        baseline_y,
        options.series_padding_inner,
    )?;
    let (line_series, markers) =
        render_categorical_lines(line_series, &categories, x_scale, y_scale)?;

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
        line_series,
        markers,
    })
}

fn has_renderable_series(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> bool {
    line_series
        .iter()
        .any(|series| !series.points.is_empty() || !series.data.is_empty())
        || bar_series.iter().any(|series| !series.data.is_empty())
}

fn uses_categorical_x(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> bool {
    bar_series.iter().any(|series| !series.data.is_empty())
        || line_series.iter().any(|series| !series.data.is_empty())
}

fn categorical_categories(
    line_series: &[CartesianLineSeriesConfig],
    bar_series: &[CartesianBarSeriesConfig],
) -> ChartResult<Vec<String>> {
    let categories = bar_series
        .iter()
        .find(|series| !series.data.is_empty())
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
                .find(|series| !series.data.is_empty())
                .map(|series| {
                    series
                        .data
                        .iter()
                        .map(|bar| bar.label.clone())
                        .collect::<Vec<_>>()
                })
        })
        .ok_or(ChartError::EmptySeries)?;

    for series in bar_series.iter().filter(|series| !series.data.is_empty()) {
        validate_categories(&series_label_or_default(&series.label, 0), &categories, &series.data)?;
    }
    for series in line_series.iter().filter(|series| !series.data.is_empty()) {
        validate_categories(&series_label_or_default(&series.label, 0), &categories, &series.data)?;
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
) -> ChartResult<(f64, f64)> {
    let include_zero = bar_series.iter().any(|series| !series.data.is_empty());
    let values = bar_series
        .iter()
        .flat_map(|series| series.data.iter().map(|bar| bar.value))
        .chain(
            line_series
                .iter()
                .flat_map(|series| series.data.iter().map(|bar| bar.value)),
        )
        .chain(
            line_series
                .iter()
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
        .filter(|series| !series.data.is_empty())
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
        .filter(|series| !series.data.is_empty() || !series.points.is_empty())
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
    config
        .points
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
                key: format!("{}-point-{index}-{}", config.key, format_tick(data_x)),
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

fn color_or_current(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "currentColor".into()
    } else {
        trimmed.into()
    }
}

fn positive_or_default(value: f64, default: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        default
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
            &series,
            &[],
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
            &lines,
            &bars,
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
            &[],
            &[],
        )
        .unwrap_err();

        assert_eq!(error, ChartError::EmptySeries);
    }
}
