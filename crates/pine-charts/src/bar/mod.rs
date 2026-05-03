use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect};
use crate::scale::{BandScale, LinearScale};
use crate::svg::{format_tick, SvgLine, SvgTickLabel};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartBar {
    pub label: String,
    pub value: f64,
}

impl ChartBar {
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    fn validate(&self) -> ChartResult<Self> {
        Ok(Self {
            label: self.label.clone(),
            value: finite("bar.value", self.value)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarChartOptions {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub y_domain: Option<(f64, f64)>,
    pub padding_inner: f64,
    pub padding_outer: f64,
}

impl Default for BarChartOptions {
    fn default() -> Self {
        Self {
            width: 640.0,
            height: 320.0,
            margins: ChartMargins::default(),
            y_domain: None,
            padding_inner: 0.2,
            padding_outer: 0.1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarChartGeometry {
    pub view_box: String,
    pub plot: ChartRect,
    pub bars: Vec<SvgBar>,
    pub y_ticks: Vec<crate::Tick>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
}

impl BarChartGeometry {
    pub fn new(data: &[ChartBar], options: &BarChartOptions) -> ChartResult<Self> {
        if data.is_empty() {
            return Err(ChartError::EmptySeries);
        }

        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let data = validate_data(data)?;
        let y_domain = domain_or_bar_extent(options.y_domain, data.iter().map(|bar| bar.value))?;
        let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
        let x_scale = BandScale::new(
            data.len(),
            (plot.x, plot.right()),
            options.padding_inner,
            options.padding_outer,
        )?;
        let baseline = baseline_value(y_domain);
        let baseline_y = y_scale.map(baseline)?;

        let bars = data
            .iter()
            .enumerate()
            .map(|(index, bar)| {
                let x = x_scale.position(index).unwrap_or(plot.x);
                let value_y = y_scale.map(bar.value)?;
                let y = value_y.min(baseline_y);
                let height = (baseline_y - value_y).abs();
                Ok(SvgBar {
                    key: format!("bar-{index}-{}", bar.label),
                    label: bar.label.clone(),
                    value: bar.value,
                    aria_label: format!("{}: {}", bar.label, format_tick(bar.value)),
                    x,
                    y,
                    width: x_scale.bandwidth(),
                    height,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;

        let y_ticks = y_scale.ticks(5);

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            plot,
            bars,
            y_grid: grid_lines_for_y(&y_ticks, plot),
            x_tick_labels: category_tick_labels(&data, x_scale, plot),
            y_tick_labels: tick_labels_for_y(&y_ticks, plot),
            x_axis: SvgLine::new(
                "x-axis".into(),
                plot.x,
                baseline_y,
                plot.right(),
                baseline_y,
            ),
            y_axis: SvgLine::new("y-axis".into(), plot.x, plot.y, plot.x, plot.bottom()),
            y_ticks,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgBar {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub aria_label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
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

fn category_tick_labels(data: &[ChartBar], scale: BandScale, plot: ChartRect) -> Vec<SvgTickLabel> {
    data.iter()
        .enumerate()
        .filter_map(|(index, bar)| {
            let x = scale.center(index)?;
            Some(SvgTickLabel {
                key: format!("x-tick-{index}-{}", bar.label),
                value: index as f64,
                label: bar.label.clone(),
                x,
                y: plot.bottom() + 18.0,
                line_x1: x,
                line_y1: plot.bottom(),
                line_x2: x,
                line_y2: plot.bottom() + 6.0,
            })
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

fn validate_data(data: &[ChartBar]) -> ChartResult<Vec<ChartBar>> {
    data.iter().map(ChartBar::validate).collect()
}

fn domain_or_bar_extent(
    domain: Option<(f64, f64)>,
    values: impl IntoIterator<Item = f64>,
) -> ChartResult<(f64, f64)> {
    if let Some((start, end)) = domain {
        return expanded_domain(finite("domain.start", start)?, finite("domain.end", end)?);
    }

    let mut min = 0.0_f64;
    let mut max = 0.0_f64;
    let mut saw_value = false;
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

fn expanded_domain(start: f64, end: f64) -> ChartResult<(f64, f64)> {
    if start != end {
        return Ok((start, end));
    }

    let pad = if start == 0.0 { 1.0 } else { start.abs() * 0.1 };
    Ok((start - pad, end + pad))
}

fn baseline_value(domain: (f64, f64)) -> f64 {
    let lo = domain.0.min(domain.1);
    let hi = domain.0.max(domain.1);
    0.0_f64.clamp(lo, hi)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineBarChart.poco", role = "panel")]
pub struct PineBarChart {
    #[prop]
    pub data: Vec<ChartBar>,
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
    pub y_min: Option<f64>,
    #[prop]
    pub y_max: Option<f64>,
    #[prop]
    pub padding_inner: f64,
    #[prop]
    pub padding_outer: f64,
    pub state: String,
    pub view_box: String,
    pub bars: Vec<SvgBar>,
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

impl Default for PineBarChart {
    fn default() -> Self {
        let options = BarChartOptions::default();
        Self {
            data: Vec::new(),
            label: "Bar chart".into(),
            width: options.width,
            height: options.height,
            margin_top: options.margins.top,
            margin_right: options.margins.right,
            margin_bottom: options.margins.bottom,
            margin_left: options.margins.left,
            y_min: None,
            y_max: None,
            padding_inner: options.padding_inner,
            padding_outer: options.padding_outer,
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            bars: Vec::new(),
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
impl PineBarChart {
    fn on_setup(&mut self) {
        self.recompute();
    }

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartBar>, _: Option<Vec<ChartBar>>) {
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

    #[watch(y_min)]
    fn on_y_min(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
        self.recompute();
    }

    #[watch(y_max)]
    fn on_y_max(&mut self, _: Option<f64>, _: Option<Option<f64>>) {
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
}

impl PineBarChart {
    fn recompute(&mut self) {
        match BarChartGeometry::new(&self.data, &self.options()) {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.bars = geometry.bars;
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
                self.clear_geometry();
                self.error.clear();
                self.state = "empty".into();
                self.ready = false;
                self.empty = true;
                self.invalid = false;
            }
            Err(error) => {
                self.clear_geometry();
                self.error = error.to_string();
                self.state = "invalid".into();
                self.ready = false;
                self.empty = false;
                self.invalid = true;
            }
        }
    }

    fn options(&self) -> BarChartOptions {
        BarChartOptions {
            width: self.width,
            height: self.height,
            margins: ChartMargins::new(
                self.margin_top,
                self.margin_right,
                self.margin_bottom,
                self.margin_left,
            ),
            y_domain: zip_domain(self.y_min, self.y_max),
            padding_inner: self.padding_inner,
            padding_outer: self.padding_outer,
        }
    }

    fn clear_geometry(&mut self) {
        self.bars.clear();
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
    fn bar_geometry_maps_values_into_rects() {
        let options = BarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            y_domain: Some((0.0, 10.0)),
            padding_inner: 0.0,
            padding_outer: 0.0,
        };
        let geometry = BarChartGeometry::new(
            &[
                ChartBar::new("A", 2.0),
                ChartBar::new("B", 10.0),
                ChartBar::new("C", 5.0),
            ],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.view_box, "0 0 100 100");
        assert_eq!(geometry.bars.len(), 3);
        assert_eq!(geometry.bars[0].x, 0.0);
        assert_eq!(geometry.bars[0].y, 80.0);
        assert_eq!(geometry.bars[0].height, 20.0);
        assert_eq!(geometry.bars[1].y, 0.0);
        assert_eq!(geometry.bars[1].height, 100.0);
        assert_eq!(geometry.x_tick_labels.len(), 3);
        assert!(!geometry.y_tick_labels.is_empty());
    }

    #[test]
    fn bar_geometry_handles_negative_values() {
        let options = BarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            y_domain: Some((-10.0, 10.0)),
            padding_inner: 0.0,
            padding_outer: 0.0,
        };
        let geometry = BarChartGeometry::new(
            &[ChartBar::new("Loss", -5.0), ChartBar::new("Gain", 10.0)],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.x_axis.y1, 50.0);
        assert_eq!(geometry.bars[0].y, 50.0);
        assert_eq!(geometry.bars[0].height, 25.0);
        assert_eq!(geometry.bars[1].y, 0.0);
        assert_eq!(geometry.bars[1].height, 50.0);
    }

    #[test]
    fn component_recomputes_state() {
        let mut chart = PineBarChart {
            data: vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            y_min: Some(0.0),
            y_max: Some(4.0),
            padding_inner: 0.0,
            padding_outer: 0.0,
            ..PineBarChart::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.state, "ready");
        assert_eq!(chart.bars.len(), 2);
        assert_eq!(chart.bars[1].height, 100.0);
    }
}
