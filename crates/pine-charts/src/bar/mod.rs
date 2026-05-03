use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cartesian::{
    optional_domain, plot_rect_from_edges, pointer_event_svg_point, step_key, x_axis_label,
    y_axis_label, CartesianChartState, CartesianHoverPlacement, CategoricalGuideFields,
    CategoricalGuideUpdate, ChartStateFields, PlotEdgeFields,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::{series_label_or_default, series_legend_items, LegendItem};
use crate::scale::{BandScale, LinearScale};
use crate::svg::{format_tick, SvgAxisLabel, SvgLine, SvgTickLabel};

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
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartBarSeries {
    pub label: String,
    pub data: Vec<ChartBar>,
}

impl ChartBarSeries {
    pub fn new(label: impl Into<String>, data: Vec<ChartBar>) -> Self {
        Self {
            label: label.into(),
            data,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BarChartMode {
    #[default]
    Grouped,
    Stacked,
}

impl BarChartMode {
    fn parse(value: &str) -> ChartResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "grouped" => Ok(Self::Grouped),
            "stacked" => Ok(Self::Stacked),
            value => Err(ChartError::InvalidOption {
                field: "mode",
                value: value.into(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarChartOptions {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub y_domain: Option<(f64, f64)>,
    pub mode: BarChartMode,
    pub padding_inner: f64,
    pub padding_outer: f64,
    pub series_padding_inner: f64,
}

impl Default for BarChartOptions {
    fn default() -> Self {
        Self {
            width: 640.0,
            height: 320.0,
            margins: ChartMargins::default(),
            y_domain: None,
            mode: BarChartMode::Grouped,
            padding_inner: 0.2,
            padding_outer: 0.1,
            series_padding_inner: 0.1,
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
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
    pub legend_items: Vec<LegendItem>,
}

impl BarChartGeometry {
    pub fn new(data: &[ChartBar], options: &BarChartOptions) -> ChartResult<Self> {
        let series = [ChartBarSeries::new("", data.to_vec())];
        Self::from_series(&series, options)
    }

    pub fn from_series(series: &[ChartBarSeries], options: &BarChartOptions) -> ChartResult<Self> {
        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let data = NormalizedBarData::from_series(series)?;
        let y_domain = match options.mode {
            BarChartMode::Grouped => {
                domain_or_bar_extent(options.y_domain, data.values().copied())?
            }
            BarChartMode::Stacked => stacked_domain(options.y_domain, &data)?,
        };
        let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
        let x_scale = BandScale::new(
            data.categories.len(),
            (plot.x, plot.right()),
            options.padding_inner,
            options.padding_outer,
        )?;
        let baseline = baseline_value(y_domain);
        let baseline_y = y_scale.map(baseline)?;
        let bars = match options.mode {
            BarChartMode::Grouped => grouped_bars(
                &data,
                x_scale,
                y_scale,
                baseline_y,
                options.series_padding_inner,
            )?,
            BarChartMode::Stacked => stacked_bars(&data, x_scale, y_scale)?,
        };

        let y_ticks = y_scale.ticks(5);

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            plot,
            bars,
            y_grid: grid_lines_for_y(&y_ticks, plot),
            x_tick_labels: category_tick_labels(&data.categories, x_scale, plot),
            y_tick_labels: tick_labels_for_y(&y_ticks, plot),
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
            legend_items: data.legend_items(),
            y_ticks,
        })
    }
}

pub fn bar_legend_items(series: &[ChartBarSeries]) -> Vec<LegendItem> {
    series_legend_items(
        "bar-series",
        series
            .iter()
            .enumerate()
            .map(|(index, series)| series_label(series, index)),
    )
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgBar {
    pub key: String,
    pub label: String,
    pub category_label: String,
    pub series_label: String,
    pub value: f64,
    pub aria_label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SvgBar {
    fn contains(&self, point: Point) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }

    fn hover_anchor(&self) -> Point {
        let y = if self.value >= 0.0 {
            self.y
        } else {
            self.y + self.height
        };
        Point {
            x: self.x + self.width * 0.5,
            y,
        }
    }

    fn hover_update(&self, plot: ChartRect, width: f64, height: f64) -> BarHoverUpdate {
        let point = self.hover_anchor();
        BarHoverUpdate {
            key: self.key.clone(),
            category: self.category_label.clone(),
            value: self.value,
            value_label: format_tick(self.value),
            series: display_series_label(&self.series_label),
            aria_label: self.aria_label.clone(),
            placement: CartesianHoverPlacement::new(point, plot, width, height),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BarHoverUpdate {
    key: String,
    category: String,
    value: f64,
    value_label: String,
    series: String,
    aria_label: String,
    placement: CartesianHoverPlacement,
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedBarData {
    categories: Vec<String>,
    series: Vec<NormalizedBarSeries>,
}

impl NormalizedBarData {
    fn from_series(series: &[ChartBarSeries]) -> ChartResult<Self> {
        if series.is_empty() || series.iter().all(|series| series.data.is_empty()) {
            return Err(ChartError::EmptySeries);
        }

        let first = series
            .iter()
            .find(|series| !series.data.is_empty())
            .ok_or(ChartError::EmptySeries)?;
        let categories = first
            .data
            .iter()
            .map(|bar| bar.label.clone())
            .collect::<Vec<_>>();

        let mut normalized = Vec::with_capacity(series.len());
        for (series_index, series) in series.iter().enumerate() {
            if series.data.len() != categories.len() {
                let expected = categories
                    .get(series.data.len())
                    .or_else(|| categories.last())
                    .cloned()
                    .unwrap_or_default();
                return Err(ChartError::MismatchedSeries {
                    series: series_label(series, series_index),
                    expected,
                    actual: String::new(),
                });
            }

            let values = series
                .data
                .iter()
                .zip(categories.iter())
                .map(|(bar, expected)| {
                    if bar.label != *expected {
                        return Err(ChartError::MismatchedSeries {
                            series: series_label(series, series_index),
                            expected: expected.clone(),
                            actual: bar.label.clone(),
                        });
                    }
                    finite("bar.value", bar.value)
                })
                .collect::<ChartResult<Vec<_>>>()?;

            normalized.push(NormalizedBarSeries {
                label: series_label(series, series_index),
                values,
            });
        }

        Ok(Self {
            categories,
            series: normalized,
        })
    }

    fn values(&self) -> impl Iterator<Item = &f64> {
        self.series.iter().flat_map(|series| series.values.iter())
    }

    fn legend_items(&self) -> Vec<LegendItem> {
        if self.series.len() <= 1 {
            return Vec::new();
        }

        series_legend_items(
            "bar-series",
            self.series.iter().map(|series| series.label.clone()),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedBarSeries {
    label: String,
    values: Vec<f64>,
}

fn series_label(series: &ChartBarSeries, index: usize) -> String {
    series_label_or_default(&series.label, index)
}

fn grouped_bars(
    data: &NormalizedBarData,
    category_scale: BandScale,
    y_scale: LinearScale,
    baseline_y: f64,
    series_padding_inner: f64,
) -> ChartResult<Vec<SvgBar>> {
    let mut bars = Vec::with_capacity(data.categories.len() * data.series.len());

    for (category_index, category_label) in data.categories.iter().enumerate() {
        let category_x = category_scale.position(category_index).unwrap_or_default();
        let series_scale = BandScale::new(
            data.series.len(),
            (category_x, category_x + category_scale.bandwidth()),
            series_padding_inner,
            0.0,
        )?;

        for (series_index, series) in data.series.iter().enumerate() {
            let value = series.values[category_index];
            let value_y = y_scale.map(value)?;
            let y = value_y.min(baseline_y);
            let height = (baseline_y - value_y).abs();
            let x = series_scale.position(series_index).unwrap_or(category_x);
            bars.push(SvgBar {
                key: format!("bar-grouped-{category_index}-{series_index}"),
                label: category_label.clone(),
                category_label: category_label.clone(),
                series_label: series.label.clone(),
                value,
                aria_label: bar_aria_label(category_label, &series.label, value),
                x,
                y,
                width: series_scale.bandwidth(),
                height,
            });
        }
    }

    Ok(bars)
}

fn stacked_bars(
    data: &NormalizedBarData,
    category_scale: BandScale,
    y_scale: LinearScale,
) -> ChartResult<Vec<SvgBar>> {
    let mut bars = Vec::with_capacity(data.categories.len() * data.series.len());

    for (category_index, category_label) in data.categories.iter().enumerate() {
        let x = category_scale.position(category_index).unwrap_or_default();
        let mut positive_offset = 0.0_f64;
        let mut negative_offset = 0.0_f64;

        for (series_index, series) in data.series.iter().enumerate() {
            let value = series.values[category_index];
            let (start, end) = if value >= 0.0 {
                let start = positive_offset;
                positive_offset += value;
                (start, positive_offset)
            } else {
                let start = negative_offset;
                negative_offset += value;
                (start, negative_offset)
            };
            let start_y = y_scale.map(start)?;
            let end_y = y_scale.map(end)?;
            bars.push(SvgBar {
                key: format!("bar-stacked-{category_index}-{series_index}"),
                label: category_label.clone(),
                category_label: category_label.clone(),
                series_label: series.label.clone(),
                value,
                aria_label: bar_aria_label(category_label, &series.label, value),
                x,
                y: start_y.min(end_y),
                width: category_scale.bandwidth(),
                height: (start_y - end_y).abs(),
            });
        }
    }

    Ok(bars)
}

fn bar_aria_label(category: &str, series: &str, value: f64) -> String {
    if series.is_empty() || series == "Series 1" {
        format!("{category}: {}", format_tick(value))
    } else {
        format!("{series}, {category}: {}", format_tick(value))
    }
}

fn display_series_label(series: &str) -> String {
    if series.is_empty() || series == "Series 1" {
        String::new()
    } else {
        series.into()
    }
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

fn category_tick_labels(
    categories: &[String],
    scale: BandScale,
    plot: ChartRect,
) -> Vec<SvgTickLabel> {
    categories
        .iter()
        .enumerate()
        .filter_map(|(index, label)| {
            let x = scale.center(index)?;
            Some(SvgTickLabel {
                key: format!("x-tick-{index}-{label}"),
                value: index as f64,
                label: label.clone(),
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

fn stacked_domain(domain: Option<(f64, f64)>, data: &NormalizedBarData) -> ChartResult<(f64, f64)> {
    if let Some((start, end)) = domain {
        return expanded_domain(finite("domain.start", start)?, finite("domain.end", end)?);
    }

    let mut min = 0.0_f64;
    let mut max = 0.0_f64;
    for category_index in 0..data.categories.len() {
        let mut positive = 0.0_f64;
        let mut negative = 0.0_f64;
        for series in &data.series {
            let value = finite("domain.value", series.values[category_index])?;
            if value >= 0.0 {
                positive += value;
            } else {
                negative += value;
            }
        }
        max = max.max(positive);
        min = min.min(negative);
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
    pub series: Vec<ChartBarSeries>,
    #[prop]
    pub mode: String,
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
    pub bars: Vec<SvgBar>,
    pub plot_x: f64,
    pub plot_y: f64,
    pub plot_right: f64,
    pub plot_bottom: f64,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
    pub legend_items: Vec<LegendItem>,
    pub hover_visible: bool,
    pub hover_key: String,
    pub hover_category: String,
    pub hover_value: f64,
    pub hover_value_label: String,
    pub hover_series: String,
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

impl Default for PineBarChart {
    fn default() -> Self {
        let options = BarChartOptions::default();
        Self {
            data: Vec::new(),
            series: Vec::new(),
            mode: "grouped".into(),
            label: "Bar chart".into(),
            x_label: String::new(),
            y_label: String::new(),
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
            series_padding_inner: options.series_padding_inner,
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            bars: Vec::new(),
            plot_x: 0.0,
            plot_y: 0.0,
            plot_right: 0.0,
            plot_bottom: 0.0,
            y_grid: Vec::new(),
            x_tick_labels: Vec::new(),
            y_tick_labels: Vec::new(),
            x_axis_label: SvgAxisLabel::default(),
            y_axis_label: SvgAxisLabel::default(),
            x_axis: SvgLine::default(),
            y_axis: SvgLine::default(),
            legend_items: Vec::new(),
            hover_visible: false,
            hover_key: String::new(),
            hover_category: String::new(),
            hover_value: 0.0,
            hover_value_label: String::new(),
            hover_series: String::new(),
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
impl PineBarChart {
    fn on_setup(&mut self) {
        self.recompute();
    }

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartBar>, _: Option<Vec<ChartBar>>) {
        self.recompute();
    }

    #[watch(series)]
    fn on_series(&mut self, _: Vec<ChartBarSeries>, _: Option<Vec<ChartBarSeries>>) {
        self.recompute();
    }

    #[watch(mode)]
    fn on_mode(&mut self, _: String, _: Option<String>) {
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

    #[watch(series_padding_inner)]
    fn on_series_padding_inner(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    pub fn on_pointer_move(&mut self, ev: wasm_bindgen::JsValue) {
        let Some(point) = pointer_event_svg_point(ev, self.width, self.height) else {
            return;
        };
        self.hover_at(point.x, point.y);
    }

    pub fn clear_hover(&mut self) {
        self.hover_visible = false;
        self.hover_key.clear();
        self.hover_category.clear();
        self.hover_value = 0.0;
        self.hover_value_label.clear();
        self.hover_series.clear();
        self.hover_aria_label.clear();
        self.hover_placement_x = "right".into();
        self.hover_placement_y = "above".into();
        self.hover_style.clear();
    }

    pub fn select_bar(&mut self, key: String) {
        if self.has_bar_key(&key) {
            self.focused_key = key.clone();
            self.selected_key = key;
        }
    }

    pub fn focus_next_bar(&mut self) {
        self.step_bar_focus(1);
    }

    pub fn focus_prev_bar(&mut self) {
        self.step_bar_focus(-1);
    }

    pub fn select_focused_bar(&mut self) {
        if self.focused_key.is_empty() {
            self.step_bar_focus(1);
        }
        if !self.focused_key.is_empty() {
            self.selected_key = self.focused_key.clone();
        }
    }
}

impl PineBarChart {
    fn recompute(&mut self) {
        let options = match self.options() {
            Ok(options) => options,
            Err(error) => {
                self.set_invalid(error);
                return;
            }
        };
        let result = if self.series.is_empty() {
            BarChartGeometry::new(&self.data, &options)
        } else {
            BarChartGeometry::from_series(&self.series, &options)
        };

        match result {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.bars = geometry.bars;
                self.plot_edges().apply(geometry.plot);
                self.guides().apply(CategoricalGuideUpdate {
                    y_grid: geometry.y_grid,
                    x_tick_labels: geometry.x_tick_labels,
                    y_tick_labels: geometry.y_tick_labels,
                    x_axis_label: geometry.x_axis_label,
                    y_axis_label: geometry.y_axis_label,
                    x_axis: geometry.x_axis,
                    y_axis: geometry.y_axis,
                });
                self.legend_items = geometry.legend_items;
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Ready);
                self.reconcile_selection();
                self.clear_hover();
            }
            Err(ChartError::EmptySeries) => {
                self.clear_geometry();
                self.clear_hover();
                self.clear_selection();
                self.error.clear();
                self.state_fields().apply(CartesianChartState::Empty);
            }
            Err(error) => {
                self.set_invalid(error);
            }
        }
    }

    fn options(&self) -> ChartResult<BarChartOptions> {
        Ok(BarChartOptions {
            width: self.width,
            height: self.height,
            margins: ChartMargins::new(
                self.margin_top,
                self.margin_right,
                self.margin_bottom,
                self.margin_left,
            ),
            y_domain: optional_domain(self.y_min, self.y_max),
            mode: BarChartMode::parse(&self.mode)?,
            padding_inner: self.padding_inner,
            padding_outer: self.padding_outer,
            series_padding_inner: self.series_padding_inner,
        })
    }

    fn set_invalid(&mut self, error: ChartError) {
        self.clear_geometry();
        self.clear_hover();
        self.clear_selection();
        self.error = error.to_string();
        self.state_fields().apply(CartesianChartState::Invalid);
    }

    fn clear_geometry(&mut self) {
        self.bars.clear();
        self.plot_edges().clear();
        self.guides().clear();
        self.legend_items.clear();
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

        let Some(update) = self
            .bars
            .iter()
            .rev()
            .find(|bar| bar.contains(point))
            .map(|bar| bar.hover_update(self.plot_rect(), self.width, self.height))
        else {
            self.clear_hover();
            return;
        };
        self.apply_hover(update);
    }

    fn apply_hover(&mut self, update: BarHoverUpdate) {
        self.hover_visible = true;
        self.hover_key = update.key;
        self.hover_category = update.category;
        self.hover_value = update.value;
        self.hover_value_label = update.value_label;
        self.hover_series = update.series;
        self.hover_aria_label = update.aria_label;
        self.hover_placement_x = update.placement.x.into();
        self.hover_placement_y = update.placement.y.into();
        self.hover_style = update.placement.style;
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

    fn guides(&mut self) -> CategoricalGuideFields<'_> {
        CategoricalGuideFields {
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

    fn step_bar_focus(&mut self, step: isize) {
        if let Some(key) = step_key(
            self.bars.iter().map(|bar| bar.key.as_str()),
            &self.focused_key,
            step,
        ) {
            self.focused_key = key;
        }
    }

    fn has_bar_key(&self, key: &str) -> bool {
        self.bars.iter().any(|bar| bar.key == key)
    }

    fn reconcile_selection(&mut self) {
        if !self.has_bar_key(&self.focused_key) {
            self.focused_key.clear();
        }
        if !self.has_bar_key(&self.selected_key) {
            self.selected_key.clear();
        }
    }

    fn clear_selection(&mut self) {
        self.focused_key.clear();
        self.selected_key.clear();
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
            mode: BarChartMode::Grouped,
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
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
            mode: BarChartMode::Grouped,
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
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
    fn grouped_bar_geometry_places_series_within_categories() {
        let options = BarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            y_domain: Some((0.0, 10.0)),
            mode: BarChartMode::Grouped,
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
        };
        let geometry = BarChartGeometry::from_series(
            &[
                ChartBarSeries::new(
                    "New",
                    vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
                ),
                ChartBarSeries::new(
                    "Returning",
                    vec![ChartBar::new("A", 3.0), ChartBar::new("B", 10.0)],
                ),
            ],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.bars.len(), 4);
        assert_eq!(geometry.bars[0].x, 0.0);
        assert_eq!(geometry.bars[0].width, 25.0);
        assert_eq!(geometry.bars[1].x, 25.0);
        assert_eq!(geometry.bars[1].width, 25.0);
        assert_eq!(geometry.bars[2].x, 50.0);
        assert_eq!(geometry.bars[3].x, 75.0);
        assert_eq!(geometry.bars[1].series_label, "Returning");
        assert_eq!(geometry.legend_items.len(), 2);
        assert_eq!(geometry.legend_items[0].label, "New");
        assert_eq!(geometry.legend_items[1].series, "Returning");
    }

    #[test]
    fn stacked_bar_geometry_accumulates_series_per_category() {
        let options = BarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            y_domain: Some((0.0, 10.0)),
            mode: BarChartMode::Stacked,
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
        };
        let geometry = BarChartGeometry::from_series(
            &[
                ChartBarSeries::new(
                    "New",
                    vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
                ),
                ChartBarSeries::new(
                    "Returning",
                    vec![ChartBar::new("A", 3.0), ChartBar::new("B", 6.0)],
                ),
            ],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.bars.len(), 4);
        assert_eq!(geometry.bars[0].x, 0.0);
        assert_eq!(geometry.bars[0].width, 50.0);
        assert_eq!(geometry.bars[0].y, 80.0);
        assert_eq!(geometry.bars[0].height, 20.0);
        assert_eq!(geometry.bars[1].y, 50.0);
        assert_eq!(geometry.bars[1].height, 30.0);
        assert_eq!(geometry.bars[3].y, 0.0);
        assert_eq!(geometry.bars[3].height, 60.0);
    }

    #[test]
    fn series_must_share_category_order() {
        let options = BarChartOptions::default();
        let error = BarChartGeometry::from_series(
            &[
                ChartBarSeries::new("A", vec![ChartBar::new("Jan", 2.0)]),
                ChartBarSeries::new("B", vec![ChartBar::new("Feb", 3.0)]),
            ],
            &options,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ChartError::MismatchedSeries {
                series: "B".into(),
                expected: "Jan".into(),
                actual: "Feb".into(),
            }
        );
    }

    #[test]
    fn helper_builds_legend_items_from_series() {
        let items = bar_legend_items(&[
            ChartBarSeries::new("Organic", Vec::new()),
            ChartBarSeries::new("Referral", Vec::new()),
        ]);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "Organic");
        assert_eq!(items[0].series, "Organic");
        assert_eq!(items[1].key, "bar-series-1-Referral");
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
            mode: "grouped".into(),
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
            ..PineBarChart::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.state, "ready");
        assert_eq!(chart.bars.len(), 2);
        assert_eq!(chart.bars[1].height, 100.0);
    }

    #[test]
    fn component_tracks_hovered_bar() {
        let mut chart = PineBarChart {
            series: vec![
                ChartBarSeries::new(
                    "New",
                    vec![ChartBar::new("A", 2.0), ChartBar::new("B", 4.0)],
                ),
                ChartBarSeries::new(
                    "Returning",
                    vec![ChartBar::new("A", 3.0), ChartBar::new("B", 10.0)],
                ),
            ],
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            y_min: Some(0.0),
            y_max: Some(10.0),
            mode: "grouped".into(),
            padding_inner: 0.0,
            padding_outer: 0.0,
            series_padding_inner: 0.0,
            ..PineBarChart::default()
        };

        chart.recompute();
        chart.hover_at(30.0, 85.0);

        assert!(chart.hover_visible);
        assert_eq!(chart.hover_key, "bar-grouped-0-1");
        assert_eq!(chart.hover_category, "A");
        assert_eq!(chart.hover_value, 3.0);
        assert_eq!(chart.hover_value_label, "3");
        assert_eq!(chart.hover_series, "Returning");
        assert_eq!(chart.hover_aria_label, "Returning, A: 3");

        chart.hover_at(50.0, -1.0);

        assert!(!chart.hover_visible);
    }
}
