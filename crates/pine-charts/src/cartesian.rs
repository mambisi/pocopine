use wasm_bindgen::JsCast;

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::scale::LinearScale;
use crate::svg::{format_tick, SvgAxisLabel, SvgLine, SvgTickLabel};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CartesianLayout {
    pub view_box: String,
    pub plot: ChartRect,
    pub x_scale: LinearScale,
    pub y_scale: LinearScale,
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

impl CartesianLayout {
    pub fn new<X, Y>(
        width: f64,
        height: f64,
        margins: ChartMargins,
        x_domain: Option<(f64, f64)>,
        y_domain: Option<(f64, f64)>,
        x_values: X,
        y_values: Y,
    ) -> ChartResult<Self>
    where
        X: IntoIterator<Item = f64>,
        Y: IntoIterator<Item = f64>,
    {
        let width = finite("width", width)?;
        let height = finite("height", height)?;
        let plot = ChartRect::from_outer(width, height, margins)?;
        let x_domain = domain_or_extent(x_domain, x_values)?;
        let y_domain = domain_or_extent(y_domain, y_values)?;
        let x_scale = LinearScale::new(x_domain, (plot.x, plot.right()))?;
        let y_scale = LinearScale::new(y_domain, (plot.bottom(), plot.y))?;
        let x_ticks = x_scale.ticks(5);
        let y_ticks = y_scale.ticks(5);

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            plot,
            x_scale,
            y_scale,
            x_grid: grid_lines_for_x(&x_ticks, plot),
            y_grid: grid_lines_for_y(&y_ticks, plot),
            x_tick_labels: tick_labels_for_x(&x_ticks, plot),
            y_tick_labels: tick_labels_for_y(&y_ticks, plot),
            x_axis_label: x_axis_label(plot, height),
            y_axis_label: y_axis_label(plot),
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CartesianHoverPlacement {
    pub x: &'static str,
    pub y: &'static str,
    pub style: String,
}

impl CartesianHoverPlacement {
    pub fn new(hover: Point, plot: ChartRect, width: f64, height: f64) -> Self {
        let x = if hover.x >= plot.x + plot.width * 0.5 {
            "left"
        } else {
            "right"
        };
        let y = if hover.y <= plot.y + plot.height * 0.5 {
            "below"
        } else {
            "above"
        };
        let hover_x_pct = (hover.x / width * 100.0).clamp(0.0, 100.0);
        let hover_y_pct = (hover.y / height * 100.0).clamp(0.0, 100.0);

        Self {
            x,
            y,
            style: format!(
                "--pine-chart-tooltip-x: {hover_x_pct}%; --pine-chart-tooltip-y: {hover_y_pct}%;"
            ),
        }
    }
}

pub(crate) struct ChartStateFields<'a> {
    pub state: &'a mut String,
    pub ready: &'a mut bool,
    pub empty: &'a mut bool,
    pub invalid: &'a mut bool,
}

impl ChartStateFields<'_> {
    pub fn apply(self, next: CartesianChartState) {
        match next {
            CartesianChartState::Ready => {
                *self.state = "ready".into();
                *self.ready = true;
                *self.empty = false;
                *self.invalid = false;
            }
            CartesianChartState::Empty => {
                *self.state = "empty".into();
                *self.ready = false;
                *self.empty = true;
                *self.invalid = false;
            }
            CartesianChartState::Invalid => {
                *self.state = "invalid".into();
                *self.ready = false;
                *self.empty = false;
                *self.invalid = true;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CartesianChartState {
    Ready,
    Empty,
    Invalid,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CartesianHoverSample {
    pub point: Point,
    pub data_x: f64,
    pub data_y: f64,
    pub series: String,
    pub x_label: String,
    pub y_label: String,
    pub aria_label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CartesianHoverUpdate {
    pub sample: CartesianHoverSample,
    pub placement: CartesianHoverPlacement,
}

impl CartesianHoverUpdate {
    pub fn new(sample: CartesianHoverSample, plot: ChartRect, width: f64, height: f64) -> Self {
        Self {
            placement: CartesianHoverPlacement::new(sample.point, plot, width, height),
            sample,
        }
    }
}

pub(crate) struct CartesianHoverFields<'a> {
    pub visible: &'a mut bool,
    pub x: &'a mut f64,
    pub y: &'a mut f64,
    pub data_x: &'a mut f64,
    pub data_y: &'a mut f64,
    pub series: &'a mut String,
    pub x_label: &'a mut String,
    pub y_label: &'a mut String,
    pub aria_label: &'a mut String,
    pub placement_x: &'a mut String,
    pub placement_y: &'a mut String,
    pub style: &'a mut String,
}

impl CartesianHoverFields<'_> {
    pub fn clear(self) {
        *self.visible = false;
        *self.x = 0.0;
        *self.y = 0.0;
        *self.data_x = 0.0;
        *self.data_y = 0.0;
        self.series.clear();
        self.x_label.clear();
        self.y_label.clear();
        self.aria_label.clear();
        *self.placement_x = "right".into();
        *self.placement_y = "above".into();
        self.style.clear();
    }

    pub fn apply(self, update: CartesianHoverUpdate) {
        *self.visible = true;
        *self.x = update.sample.point.x;
        *self.y = update.sample.point.y;
        *self.data_x = update.sample.data_x;
        *self.data_y = update.sample.data_y;
        *self.series = update.sample.series;
        *self.x_label = update.sample.x_label;
        *self.y_label = update.sample.y_label;
        *self.aria_label = update.sample.aria_label;
        *self.placement_x = update.placement.x.into();
        *self.placement_y = update.placement.y.into();
        *self.style = update.placement.style;
    }
}

pub(crate) struct PlotEdgeFields<'a> {
    pub x: &'a mut f64,
    pub y: &'a mut f64,
    pub right: &'a mut f64,
    pub bottom: &'a mut f64,
}

impl PlotEdgeFields<'_> {
    pub fn apply(self, plot: ChartRect) {
        *self.x = plot.x;
        *self.y = plot.y;
        *self.right = plot.right();
        *self.bottom = plot.bottom();
    }

    pub fn clear(self) {
        *self.x = 0.0;
        *self.y = 0.0;
        *self.right = 0.0;
        *self.bottom = 0.0;
    }
}

pub(crate) struct CartesianGuideUpdate {
    pub x_grid: Vec<SvgLine>,
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
}

pub(crate) struct CartesianGuideFields<'a> {
    pub x_grid: &'a mut Vec<SvgLine>,
    pub y_grid: &'a mut Vec<SvgLine>,
    pub x_tick_labels: &'a mut Vec<SvgTickLabel>,
    pub y_tick_labels: &'a mut Vec<SvgTickLabel>,
    pub x_axis_label: &'a mut SvgAxisLabel,
    pub y_axis_label: &'a mut SvgAxisLabel,
    pub x_axis: &'a mut SvgLine,
    pub y_axis: &'a mut SvgLine,
}

impl CartesianGuideFields<'_> {
    pub fn apply(self, update: CartesianGuideUpdate) {
        *self.x_grid = update.x_grid;
        *self.y_grid = update.y_grid;
        *self.x_tick_labels = update.x_tick_labels;
        *self.y_tick_labels = update.y_tick_labels;
        *self.x_axis_label = update.x_axis_label;
        *self.y_axis_label = update.y_axis_label;
        *self.x_axis = update.x_axis;
        *self.y_axis = update.y_axis;
    }

    pub fn clear(self) {
        self.x_grid.clear();
        self.y_grid.clear();
        self.x_tick_labels.clear();
        self.y_tick_labels.clear();
        *self.x_axis_label = SvgAxisLabel::default();
        *self.y_axis_label = SvgAxisLabel::default();
        *self.x_axis = SvgLine::default();
        *self.y_axis = SvgLine::default();
    }
}

pub(crate) struct CategoricalGuideUpdate {
    pub y_grid: Vec<SvgLine>,
    pub x_tick_labels: Vec<SvgTickLabel>,
    pub y_tick_labels: Vec<SvgTickLabel>,
    pub x_axis_label: SvgAxisLabel,
    pub y_axis_label: SvgAxisLabel,
    pub x_axis: SvgLine,
    pub y_axis: SvgLine,
}

pub(crate) struct CategoricalGuideFields<'a> {
    pub y_grid: &'a mut Vec<SvgLine>,
    pub x_tick_labels: &'a mut Vec<SvgTickLabel>,
    pub y_tick_labels: &'a mut Vec<SvgTickLabel>,
    pub x_axis_label: &'a mut SvgAxisLabel,
    pub y_axis_label: &'a mut SvgAxisLabel,
    pub x_axis: &'a mut SvgLine,
    pub y_axis: &'a mut SvgLine,
}

impl CategoricalGuideFields<'_> {
    pub fn apply(self, update: CategoricalGuideUpdate) {
        *self.y_grid = update.y_grid;
        *self.x_tick_labels = update.x_tick_labels;
        *self.y_tick_labels = update.y_tick_labels;
        *self.x_axis_label = update.x_axis_label;
        *self.y_axis_label = update.y_axis_label;
        *self.x_axis = update.x_axis;
        *self.y_axis = update.y_axis;
    }

    pub fn clear(self) {
        self.y_grid.clear();
        self.x_tick_labels.clear();
        self.y_tick_labels.clear();
        *self.x_axis_label = SvgAxisLabel::default();
        *self.y_axis_label = SvgAxisLabel::default();
        *self.x_axis = SvgLine::default();
        *self.y_axis = SvgLine::default();
    }
}

pub(crate) fn pointer_event_svg_point(
    ev: wasm_bindgen::JsValue,
    width: f64,
    height: f64,
) -> Option<Point> {
    let ev = ev.dyn_into::<web_sys::PointerEvent>().ok()?;
    let target = ev.current_target()?;
    let element = target.dyn_into::<web_sys::Element>().ok()?;
    let rect = element.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }

    let ratio_x = ((ev.client_x() as f64) - rect.left()) / rect.width();
    let ratio_y = ((ev.client_y() as f64) - rect.top()) / rect.height();
    Point::new(ratio_x * width, ratio_y * height).ok()
}

pub(crate) fn centered_plot_y(plot: ChartRect) -> f64 {
    plot.y + plot.height * 0.5
}

pub(crate) fn plot_rect_from_edges(
    plot_x: f64,
    plot_y: f64,
    plot_right: f64,
    plot_bottom: f64,
) -> ChartRect {
    ChartRect {
        x: plot_x,
        y: plot_y,
        width: plot_right - plot_x,
        height: plot_bottom - plot_y,
    }
}

pub(crate) fn x_axis_label(plot: ChartRect, height: f64) -> SvgAxisLabel {
    SvgAxisLabel {
        x: plot.x + plot.width * 0.5,
        y: height - 4.0,
        transform: String::new(),
    }
}

pub(crate) fn y_axis_label(plot: ChartRect) -> SvgAxisLabel {
    let x = 12.0;
    let y = plot.y + plot.height * 0.5;
    SvgAxisLabel {
        x,
        y,
        transform: format!("rotate(-90 {x} {y})"),
    }
}

pub(crate) fn optional_domain(start: Option<f64>, end: Option<f64>) -> Option<(f64, f64)> {
    match (start, end) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    }
}

pub(crate) fn step_key<'a>(
    keys: impl IntoIterator<Item = &'a str>,
    current: &str,
    step: isize,
) -> Option<String> {
    let keys = keys.into_iter().collect::<Vec<_>>();
    if keys.is_empty() {
        return None;
    }

    let current_index = keys.iter().position(|key| *key == current);
    let next_index = match current_index {
        Some(index) => (index as isize + step).clamp(0, keys.len() as isize - 1),
        None if step < 0 => keys.len() as isize - 1,
        None => 0,
    };

    keys.get(next_index as usize).map(|key| (*key).to_string())
}

pub(crate) fn nearest_sample_by_x<T, F>(samples: &[T], svg_x: f64, sample_x: F) -> Option<&T>
where
    F: Fn(&T) -> f64,
{
    let svg_x = finite("hover.x", svg_x).ok()?;
    samples.iter().min_by(|a, b| {
        let a_distance = (sample_x(a) - svg_x).abs();
        let b_distance = (sample_x(b) - svg_x).abs();
        a_distance
            .partial_cmp(&b_distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

pub(crate) fn nearest_sample_by_point<T, F>(
    samples: &[T],
    svg_point: Point,
    sample_point: F,
) -> Option<&T>
where
    F: Fn(&T) -> Point,
{
    let svg_point = Point::new(svg_point.x, svg_point.y).ok()?;
    samples.iter().min_by(|a, b| {
        let a = sample_point(a);
        let b = sample_point(b);
        let a_distance = squared_distance(a.x, a.y, svg_point.x, svg_point.y);
        let b_distance = squared_distance(b.x, b.y, svg_point.x, svg_point.y);
        a_distance
            .partial_cmp(&b_distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn squared_distance(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    dx * dx + dy * dy
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_builds_axes_grid_and_expanded_domains() {
        let layout =
            CartesianLayout::new(100.0, 100.0, ChartMargins::ZERO, None, None, [5.0], [5.0])
                .unwrap();

        assert_eq!(layout.view_box, "0 0 100 100");
        assert_eq!(
            layout.plot,
            ChartRect::from_outer(100.0, 100.0, ChartMargins::ZERO).unwrap()
        );
        assert_eq!(layout.x_scale.map(5.0).unwrap(), 50.0);
        assert_eq!(layout.y_scale.map(5.0).unwrap(), 50.0);
        assert!(!layout.x_grid.is_empty());
        assert!(!layout.y_grid.is_empty());
        assert_eq!(layout.x_axis_label.x, 50.0);
        assert_eq!(layout.x_axis_label.y, 96.0);
        assert_eq!(layout.y_axis_label.transform, "rotate(-90 12 50)");
    }

    #[test]
    fn nearest_sample_helpers_pick_expected_point() {
        let samples = [
            Point::new(10.0, 90.0).unwrap(),
            Point::new(50.0, 20.0).unwrap(),
        ];

        assert_eq!(
            nearest_sample_by_x(&samples, 45.0, |point| point.x),
            Some(&samples[1]),
        );
        assert_eq!(
            nearest_sample_by_point(&samples, Point::new(8.0, 88.0).unwrap(), |point| *point),
            Some(&samples[0]),
        );
    }

    #[test]
    fn hover_placement_uses_plot_halves_and_percent_style() {
        let placement = CartesianHoverPlacement::new(
            Point::new(75.0, 25.0).unwrap(),
            ChartRect::from_outer(100.0, 100.0, ChartMargins::ZERO).unwrap(),
            100.0,
            100.0,
        );

        assert_eq!(placement.x, "left");
        assert_eq!(placement.y, "below");
        assert!(placement.style.contains("--pine-chart-tooltip-x: 75%"));
    }

    #[test]
    fn step_key_clamps_and_starts_from_edge() {
        let keys = ["a", "b", "c"];

        assert_eq!(step_key(keys, "", 1).as_deref(), Some("a"));
        assert_eq!(step_key(keys, "", -1).as_deref(), Some("c"));
        assert_eq!(step_key(keys, "b", 1).as_deref(), Some("c"));
        assert_eq!(step_key(keys, "c", 1).as_deref(), Some("c"));
    }
}
