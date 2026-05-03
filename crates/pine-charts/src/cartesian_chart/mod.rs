use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id};
use serde::{Deserialize, Serialize};

use crate::cartesian::{optional_domain, plot_rect_from_edges, CartesianLayout};
use crate::error::{ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect};
use crate::line::{
    ChartLineSeries, ChartPoint, LineChartGeometry, LineChartOptions, LineChartSample,
};
use crate::svg::{SvgAxisLabel, SvgLine, SvgTickLabel};

const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 320.0;
const DEFAULT_STROKE_WIDTH: f64 = 3.0;
const DEFAULT_MARKER_RADIUS: f64 = 3.0;

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
    pub grid: Option<&'a CartesianGridConfig>,
    pub x_axis: Option<&'a CartesianAxisConfig>,
    pub y_axis: Option<&'a CartesianAxisConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineCartesianChart.poco", role = "panel")]
#[slot(default, accepts = [PineChartGrid, PineXAxis, PineYAxis, PineLineSeries])]
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
    pub state: String,
    pub view_box: String,
    pub grid: Option<CartesianGridConfig>,
    pub x_axis_config: Option<CartesianAxisConfig>,
    pub y_axis_config: Option<CartesianAxisConfig>,
    pub series: Vec<CartesianLineSeriesConfig>,
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
            state: "empty".into(),
            view_box: format!("0 0 {DEFAULT_WIDTH} {DEFAULT_HEIGHT}"),
            grid: None,
            x_axis_config: None,
            y_axis_config: None,
            series: Vec::new(),
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

    fn recompute(&mut self) {
        let options = CartesianChartOptions {
            width: self.width,
            height: self.height,
            margins: self.margins(),
            x_domain: optional_domain(self.x_min, self.x_max),
            y_domain: optional_domain(self.y_min, self.y_max),
            grid: self.grid.as_ref(),
            x_axis: self.x_axis_config.as_ref(),
            y_axis: self.y_axis_config.as_ref(),
        };

        match render_cartesian_chart(options, &self.series) {
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

    #[allow(dead_code)]
    fn plot_rect(&self) -> ChartRect {
        plot_rect_from_edges(self.plot_x, self.plot_y, self.plot_right, self.plot_bottom)
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
            });
        });
    }
}

pub fn render_cartesian_chart(
    options: CartesianChartOptions<'_>,
    series: &[CartesianLineSeriesConfig],
) -> ChartResult<CartesianChartRender> {
    if series.is_empty() || series.iter().any(|series| series.points.is_empty()) {
        return Err(ChartError::EmptySeries);
    }

    let line_series = series
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
        series,
    ))
}

fn render_from_geometry(
    geometry: LineChartGeometry,
    grid: Option<&CartesianGridConfig>,
    x_axis: Option<&CartesianAxisConfig>,
    y_axis: Option<&CartesianAxisConfig>,
    series_config: &[CartesianLineSeriesConfig],
) -> CartesianChartRender {
    let rendered_series = geometry
        .series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let config = &series_config[index];
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
                .map(move |sample| marker_from_sample(sample, &series_config[index]))
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
        line_series: rendered_series,
        markers,
    }
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

#[allow(dead_code)]
fn layout_for_series(
    width: f64,
    height: f64,
    margins: ChartMargins,
    x_domain: Option<(f64, f64)>,
    y_domain: Option<(f64, f64)>,
    series: &[CartesianLineSeriesConfig],
) -> ChartResult<CartesianLayout> {
    CartesianLayout::new(
        width,
        height,
        margins,
        x_domain,
        y_domain,
        series
            .iter()
            .flat_map(|series| series.points.iter().map(|point| point.x)),
        series
            .iter()
            .flat_map(|series| series.points.iter().map(|point| point.y)),
    )
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
                grid: Some(&grid),
                x_axis: Some(&x_axis),
                y_axis: Some(&y_axis),
            },
            &series,
        )
        .unwrap();

        assert_eq!(render.view_box, "0 0 100 100");
        assert_eq!(render.line_series[0].key, "actual");
        assert_eq!(render.line_series[0].line_d, "M0,100 L50,0 L100,50");
        assert_eq!(render.markers.len(), 3);
        assert!(!render.x_grid.is_empty());
        assert!(!render.x_tick_labels.is_empty());
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
                grid: None,
                x_axis: None,
                y_axis: None,
            },
            &[],
        )
        .unwrap_err();

        assert_eq!(error, ChartError::EmptySeries);
    }
}
