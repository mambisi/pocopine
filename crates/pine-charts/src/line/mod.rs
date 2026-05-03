use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::path::line_path;
use crate::scale::LinearScale;

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
    pub plot: ChartRect,
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
        if points.is_empty() {
            return Err(ChartError::EmptySeries);
        }

        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let points = validate_points(points)?;
        let x_domain = domain_or_extent(options.x_domain, points.iter().map(|point| point.x))?;
        let y_domain = domain_or_extent(options.y_domain, points.iter().map(|point| point.y))?;
        let x_scale = LinearScale::new(x_domain, (plot.x, plot.right()))?;
        let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
        let mapped = points
            .iter()
            .map(|point| {
                Ok(Point {
                    x: x_scale.map(point.x)?,
                    y: y_scale.map(point.y)?,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;

        let x_ticks = x_scale.ticks(5);
        let y_ticks = y_scale.ticks(5);

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            line_d: line_path(mapped)?,
            plot,
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
pub struct SvgLine {
    pub key: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl SvgLine {
    pub fn new(key: String, x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self {
            key,
            x1,
            y1,
            x2,
            y2,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgTickLabel {
    pub key: String,
    pub value: f64,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub line_x1: f64,
    pub line_y1: f64,
    pub line_x2: f64,
    pub line_y2: f64,
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

fn format_tick(value: f64) -> String {
    let rounded = if value.abs() >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    };
    rounded
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn validate_points(points: &[ChartPoint]) -> ChartResult<Vec<ChartPoint>> {
    points.iter().copied().map(ChartPoint::validate).collect()
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
    pub line_d: String,
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
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
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            line_d: String::new(),
            x_grid: Vec::new(),
            y_grid: Vec::new(),
            x_tick_labels: Vec::new(),
            y_tick_labels: Vec::new(),
            x_axis: SvgLine::default(),
            y_axis: SvgLine::default(),
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
}

impl PineLineChart {
    fn recompute(&mut self) {
        match LineChartGeometry::new(&self.points, &self.options()) {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.line_d = geometry.line_d;
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
            }
            Err(ChartError::EmptySeries) => {
                self.line_d.clear();
                self.clear_guides();
                self.error.clear();
                self.state = "empty".into();
                self.ready = false;
                self.empty = true;
                self.invalid = false;
            }
            Err(error) => {
                self.line_d.clear();
                self.clear_guides();
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
        assert_eq!(geometry.x_grid.len(), 6);
        assert_eq!(geometry.y_grid.len(), 6);
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
        assert!(!chart.x_tick_labels.is_empty());
        assert!(!chart.y_tick_labels.is_empty());
    }
}
