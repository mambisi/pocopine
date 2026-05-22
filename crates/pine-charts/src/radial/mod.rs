use core::f64::consts::PI;
use core::fmt::Write;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::animation::{animation_style, DEFAULT_ANIMATION_DURATION_MS, DEFAULT_ANIMATION_EASING};
use crate::cartesian::{
    pointer_event_svg_point, step_key, sync_tooltip_fields, CartesianChartState,
    CartesianHoverPlacement, ChartStateFields, DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::events::{
    ChartHover, ChartHoverEnd, ChartSelection, ChartSelectionKeys, CHART_HOVER_END_EVENT,
    CHART_HOVER_EVENT,
};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::LegendItem;
use crate::svg::format_tick;

const FULL_CIRCLE_DEGREES: f64 = 360.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartRadialBar {
    pub label: String,
    pub value: f64,
    pub max: f64,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

impl Default for ChartRadialBar {
    fn default() -> Self {
        Self {
            label: String::new(),
            value: 0.0,
            max: 1.0,
            visible: true,
        }
    }
}

impl ChartRadialBar {
    pub fn new(label: impl Into<String>, value: f64, max: f64) -> Self {
        Self {
            label: label.into(),
            value,
            max,
            visible: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadialBarChartOptions {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub inner_radius: f64,
    pub ring_gap: f64,
    pub start_angle: f64,
    pub end_angle: f64,
}

impl Default for RadialBarChartOptions {
    fn default() -> Self {
        Self {
            width: 320.0,
            height: 320.0,
            margins: ChartMargins::new(16.0, 16.0, 16.0, 16.0),
            inner_radius: 0.36,
            ring_gap: 6.0,
            start_angle: -90.0,
            end_angle: 270.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadialBarChartGeometry {
    pub view_box: String,
    pub plot: ChartRect,
    pub center: Point,
    pub outer_radius: f64,
    pub inner_radius: f64,
    pub ring_gap: f64,
    pub bars: Vec<SvgRadialBar>,
    pub legend_items: Vec<LegendItem>,
}

impl RadialBarChartGeometry {
    pub fn new(data: &[ChartRadialBar], options: &RadialBarChartOptions) -> ChartResult<Self> {
        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let inner_ratio = validate_inner_radius(options.inner_radius)?;
        let ring_gap = validate_ring_gap(options.ring_gap)?;
        let start_angle = finite("start_angle", options.start_angle)?;
        let end_angle = finite("end_angle", options.end_angle)?;
        if end_angle <= start_angle {
            return Err(ChartError::InvalidRange {
                start: start_angle,
                end: end_angle,
            });
        }

        let normalized = normalize_bars(data)?;
        let center = Point {
            x: plot.x + plot.width * 0.5,
            y: plot.y + plot.height * 0.5,
        };
        let outer_radius = plot.width.min(plot.height) * 0.5;
        let inner_radius = outer_radius * inner_ratio;
        let count = normalized.len();
        let gap_total = ring_gap * count.saturating_sub(1) as f64;
        let thickness = (outer_radius - inner_radius - gap_total) / count as f64;
        if thickness <= 0.0 {
            return Err(ChartError::InvalidOption {
                field: "ring_gap",
                value: ring_gap.to_string(),
            });
        }

        let span = end_angle - start_angle;
        let bars = normalized
            .iter()
            .enumerate()
            .map(|(index, bar)| {
                let outer = outer_radius - index as f64 * (thickness + ring_gap);
                let inner = outer - thickness;
                let radius = inner + thickness * 0.5;
                let percentage = bar.value / bar.max * 100.0;
                let percentage_label = format!("{}%", format_tick(percentage));
                let value_label = format_tick(bar.value);
                let max_label = format_tick(bar.max);
                let label = bar.label.clone();
                let aria_label =
                    format!("{label}: {value_label} of {max_label} ({percentage_label})");
                let value_end_angle = start_angle + span * (bar.value / bar.max);
                let track_d = arc_path(center, radius, start_angle, end_angle)?;
                let value_d = if bar.value == 0.0 {
                    String::new()
                } else {
                    arc_path(center, radius, start_angle, value_end_angle)?
                };
                let label_angle = if bar.value == 0.0 {
                    start_angle
                } else {
                    start_angle + (value_end_angle - start_angle) * 0.5
                };
                let label_point = polar_point(center, radius, label_angle);

                Ok(SvgRadialBar {
                    key: bar.key.clone(),
                    label,
                    value: bar.value,
                    max: bar.max,
                    value_label,
                    max_label,
                    percentage,
                    percentage_label,
                    aria_label,
                    track_d,
                    value_d,
                    value_visible: bar.value > 0.0,
                    radius,
                    inner_radius: inner,
                    outer_radius: outer,
                    stroke_width: thickness,
                    start_angle,
                    end_angle,
                    value_end_angle,
                    label_x: label_point.x,
                    label_y: label_point.y,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            plot,
            center,
            outer_radius,
            inner_radius,
            ring_gap,
            bars,
            legend_items: radial_bar_legend_items(data),
        })
    }
}

pub fn radial_bar_legend_items(data: &[ChartRadialBar]) -> Vec<LegendItem> {
    data.iter()
        .enumerate()
        .filter(|(_, bar)| bar.max > 0.0)
        .map(|(index, bar)| {
            let label = radial_bar_label(&bar.label, index);
            LegendItem::with_series_active(
                format!("radial-bar-{index}-{label}"),
                label.clone(),
                label,
                bar.visible,
            )
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgRadialBar {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub max: f64,
    pub value_label: String,
    pub max_label: String,
    pub percentage: f64,
    pub percentage_label: String,
    pub aria_label: String,
    pub track_d: String,
    pub value_d: String,
    pub value_visible: bool,
    pub radius: f64,
    pub inner_radius: f64,
    pub outer_radius: f64,
    pub stroke_width: f64,
    pub start_angle: f64,
    pub end_angle: f64,
    pub value_end_angle: f64,
    pub label_x: f64,
    pub label_y: f64,
}

impl SvgRadialBar {
    fn selection(&self) -> ChartSelection {
        ChartSelection::share(
            "radial",
            self.key.clone(),
            self.aria_label.clone(),
            self.aria_label.clone(),
            self.value,
            self.value_label.clone(),
            self.percentage,
            self.percentage_label.clone(),
        )
    }

    fn contains(&self, point: Point, center: Point) -> bool {
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = (dx * dx + dy * dy).sqrt();
        if radius < self.inner_radius || radius > self.outer_radius {
            return false;
        }

        angle_in_span(dy.atan2(dx) * 180.0 / PI, self.start_angle, self.end_angle)
    }

    fn hover_update(&self, plot: ChartRect, width: f64, height: f64) -> RadialHoverUpdate {
        RadialHoverUpdate {
            key: self.key.clone(),
            label: self.label.clone(),
            value: self.value,
            value_label: self.value_label.clone(),
            percentage: self.percentage,
            percentage_label: self.percentage_label.clone(),
            aria_label: self.aria_label.clone(),
            placement: CartesianHoverPlacement::new(
                Point {
                    x: self.label_x,
                    y: self.label_y,
                },
                plot,
                width,
                height,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedRadialBar {
    key: String,
    label: String,
    value: f64,
    max: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct RadialHoverUpdate {
    key: String,
    label: String,
    value: f64,
    value_label: String,
    percentage: f64,
    percentage_label: String,
    aria_label: String,
    placement: CartesianHoverPlacement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[component(template = "PineRadialBarChart.poco", role = "panel")]
pub struct PineRadialBarChart {
    #[prop]
    pub data: Vec<ChartRadialBar>,
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
    pub inner_radius: f64,
    #[prop]
    pub ring_gap: f64,
    #[prop]
    pub start_angle: f64,
    #[prop]
    pub end_angle: f64,
    #[prop]
    pub center_label: String,
    #[prop]
    pub center_value: String,
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
    pub bars: Vec<SvgRadialBar>,
    pub hover_bar: SvgRadialBar,
    pub legend_items: Vec<LegendItem>,
    pub center_x: f64,
    pub center_y: f64,
    pub outer_radius_px: f64,
    pub inner_radius_px: f64,
    pub center_visible: bool,
    pub center_label_text: String,
    pub center_value_text: String,
    pub center_label_y: f64,
    pub center_value_y: f64,
    pub hover_visible: bool,
    pub hover_key: String,
    pub hover_label: String,
    pub hover_value: f64,
    pub hover_value_label: String,
    pub hover_percentage: f64,
    pub hover_percentage_label: String,
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

impl Default for PineRadialBarChart {
    fn default() -> Self {
        let options = RadialBarChartOptions::default();
        Self {
            data: Vec::new(),
            label: "Radial bar chart".into(),
            empty_message: DEFAULT_EMPTY_MESSAGE.into(),
            width: options.width,
            height: options.height,
            margin_top: options.margins.top,
            margin_right: options.margins.right,
            margin_bottom: options.margins.bottom,
            margin_left: options.margins.left,
            inner_radius: options.inner_radius,
            ring_gap: options.ring_gap,
            start_angle: options.start_angle,
            end_angle: options.end_angle,
            center_label: String::new(),
            center_value: String::new(),
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
            bars: Vec::new(),
            hover_bar: SvgRadialBar::default(),
            legend_items: Vec::new(),
            center_x: 0.0,
            center_y: 0.0,
            outer_radius_px: 0.0,
            inner_radius_px: 0.0,
            center_visible: false,
            center_label_text: String::new(),
            center_value_text: String::new(),
            center_label_y: 0.0,
            center_value_y: 0.0,
            hover_visible: false,
            hover_key: String::new(),
            hover_label: String::new(),
            hover_value: 0.0,
            hover_value_label: String::new(),
            hover_percentage: 0.0,
            hover_percentage_label: String::new(),
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
impl PineRadialBarChart {
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

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartRadialBar>, _: Option<Vec<ChartRadialBar>>) {
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

    #[watch(inner_radius)]
    fn on_inner_radius(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(ring_gap)]
    fn on_ring_gap(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(start_angle)]
    fn on_start_angle(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(end_angle)]
    fn on_end_angle(&mut self, _: f64, _: Option<f64>) {
        self.recompute();
    }

    #[watch(center_label)]
    fn on_center_label(&mut self, _: String, _: Option<String>) {
        self.update_center_visibility();
    }

    #[watch(center_value)]
    fn on_center_value(&mut self, _: String, _: Option<String>) {
        self.update_center_visibility();
    }

    pub fn on_pointer_move(&mut self, ev: wasm_bindgen::JsValue) {
        let Some(point) = pointer_event_svg_point(ev, self.width, self.height) else {
            return;
        };
        self.hover_at(point.x, point.y);
    }

    pub fn clear_hover(&mut self) {
        let was_visible = self.hover_visible;
        self.hover_visible = false;
        self.hover_key.clear();
        self.hover_label.clear();
        self.hover_value = 0.0;
        self.hover_value_label.clear();
        self.hover_percentage = 0.0;
        self.hover_percentage_label.clear();
        self.hover_aria_label.clear();
        self.hover_placement_x = "right".into();
        self.hover_placement_y = "above".into();
        self.hover_style.clear();
        self.hover_bar = SvgRadialBar::default();
        self.sync_tooltip_state();
        if was_visible {
            pocopine::emit(CHART_HOVER_END_EVENT, ChartHoverEnd::new("radial"));
        }
    }

    pub fn select_bar(&mut self, key: String) {
        if let Some(selection) = self.selection_for_bar(&key) {
            self.selection_keys().select(key, selection);
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
        let key = self.focused_key.clone();
        if let Some(selection) = self.selection_for_bar(&key) {
            self.selection_keys().select(key, selection);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_keys().clear();
    }
}

impl PineRadialBarChart {
    fn update_animation_style(&mut self) {
        self.animation_style = animation_style(self.animation_duration, &self.animation_easing);
    }

    fn sync_tooltip_state(&mut self) {
        sync_tooltip_fields(
            &self.tooltip,
            self.hover_visible,
            &mut self.tooltip_mode,
            &mut self.tooltip_aria_hidden,
        );
    }

    fn recompute(&mut self) {
        match RadialBarChartGeometry::new(&self.data, &self.options()) {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.bars = geometry.bars;
                self.legend_items = geometry.legend_items;
                self.center_x = geometry.center.x;
                self.center_y = geometry.center.y;
                self.outer_radius_px = geometry.outer_radius;
                self.inner_radius_px = geometry.inner_radius;
                self.update_center_visibility();
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
                self.clear_geometry();
                self.clear_hover();
                self.clear_selection();
                self.error = error.to_string();
                self.state_fields().apply(CartesianChartState::Invalid);
            }
        }
    }

    fn options(&self) -> RadialBarChartOptions {
        RadialBarChartOptions {
            width: self.width,
            height: self.height,
            margins: ChartMargins::new(
                self.margin_top,
                self.margin_right,
                self.margin_bottom,
                self.margin_left,
            ),
            inner_radius: self.inner_radius,
            ring_gap: self.ring_gap,
            start_angle: self.start_angle,
            end_angle: self.end_angle,
        }
    }

    pub fn hover_at(&mut self, svg_x: f64, svg_y: f64) {
        let Ok(point) = Point::new(svg_x, svg_y) else {
            self.clear_hover();
            return;
        };
        if !self.ready {
            self.clear_hover();
            return;
        }

        let center = Point {
            x: self.center_x,
            y: self.center_y,
        };
        let plot = self.plot_rect();
        let Some(update) = self
            .bars
            .iter()
            .find(|bar| bar.contains(point, center))
            .map(|bar| bar.hover_update(plot, self.width, self.height))
        else {
            self.clear_hover();
            return;
        };

        self.apply_hover(update);
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

    fn selection_for_bar(&self, key: &str) -> Option<ChartSelection> {
        self.bars
            .iter()
            .find(|bar| bar.key == key)
            .map(SvgRadialBar::selection)
    }

    fn clear_geometry(&mut self) {
        self.bars.clear();
        self.hover_bar = SvgRadialBar::default();
        self.legend_items.clear();
        self.center_x = 0.0;
        self.center_y = 0.0;
        self.outer_radius_px = 0.0;
        self.inner_radius_px = 0.0;
        self.center_visible = false;
    }

    fn plot_rect(&self) -> ChartRect {
        ChartRect {
            x: self.center_x - self.outer_radius_px,
            y: self.center_y - self.outer_radius_px,
            width: self.outer_radius_px * 2.0,
            height: self.outer_radius_px * 2.0,
        }
    }

    fn update_center_visibility(&mut self) {
        self.center_label_text = self.center_label.clone();
        self.center_value_text = self.center_value.clone();
        self.center_visible = self.inner_radius_px > 0.0
            && (!self.center_label_text.is_empty() || !self.center_value_text.is_empty());
        let has_label = !self.center_label_text.is_empty();
        let has_value = !self.center_value_text.is_empty();
        match (has_value, has_label) {
            (true, true) if self.is_half_radial() => {
                self.center_value_y = self.center_y - 26.0;
                self.center_label_y = self.center_y;
            }
            (true, true) => {
                self.center_value_y = self.center_y - 10.0;
                self.center_label_y = self.center_y + 16.0;
            }
            (true, false) | (false, true) | (false, false) => {
                self.center_value_y = self.center_y;
                self.center_label_y = self.center_y;
            }
        }
    }

    fn is_half_radial(&self) -> bool {
        self.inner_radius_px > 0.0 && (self.end_angle - self.start_angle).abs() <= 180.0 + 1e-9
    }

    fn apply_hover(&mut self, update: RadialHoverUpdate) {
        let hover = ChartHover::share(
            "radial",
            update.key.clone(),
            update.label.clone(),
            update.aria_label.clone(),
            update.value,
            update.value_label.clone(),
            update.percentage,
            update.percentage_label.clone(),
            update.placement.x,
            update.placement.y,
            update.placement.style.clone(),
        );
        self.hover_visible = true;
        self.hover_key = update.key;
        self.hover_label = update.label;
        self.hover_value = update.value;
        self.hover_value_label = update.value_label;
        self.hover_percentage = update.percentage;
        self.hover_percentage_label = update.percentage_label;
        self.hover_aria_label = update.aria_label;
        self.hover_placement_x = update.placement.x.into();
        self.hover_placement_y = update.placement.y.into();
        self.hover_style = update.placement.style;
        self.sync_hover_bar();
        self.sync_tooltip_state();
        pocopine::emit(CHART_HOVER_EVENT, hover);
    }

    fn sync_hover_bar(&mut self) {
        if self.hover_key.is_empty() {
            self.hover_bar = SvgRadialBar::default();
            return;
        }
        self.hover_bar = self
            .bars
            .iter()
            .find(|bar| bar.key == self.hover_key)
            .cloned()
            .unwrap_or_default();
    }

    fn reconcile_selection(&mut self) {
        let has_focused = self.has_bar_key(&self.focused_key);
        let has_selected = self.has_bar_key(&self.selected_key);
        self.selection_keys().reconcile(has_focused, has_selected);
    }

    fn selection_keys(&mut self) -> ChartSelectionKeys<'_> {
        ChartSelectionKeys::new("radial", &mut self.focused_key, &mut self.selected_key)
    }

    fn state_fields(&mut self) -> ChartStateFields<'_> {
        ChartStateFields {
            state: &mut self.state,
            ready: &mut self.ready,
            empty: &mut self.empty,
            invalid: &mut self.invalid,
        }
    }
}

fn normalize_bars(data: &[ChartRadialBar]) -> ChartResult<Vec<NormalizedRadialBar>> {
    if data.is_empty() {
        return Err(ChartError::EmptySeries);
    }

    let mut output = Vec::new();
    for (index, bar) in data.iter().enumerate() {
        let value = finite("bar.value", bar.value)?;
        let max = finite("bar.max", bar.max)?;
        if value < 0.0 {
            return Err(ChartError::InvalidOption {
                field: "bar.value",
                value: value.to_string(),
            });
        }
        if max <= 0.0 {
            return Err(ChartError::InvalidOption {
                field: "bar.max",
                value: max.to_string(),
            });
        }
        if value > max {
            return Err(ChartError::InvalidOption {
                field: "bar.value",
                value: format!("{value} > {max}"),
            });
        }
        if !bar.visible {
            continue;
        }
        let label = radial_bar_label(&bar.label, index);
        output.push(NormalizedRadialBar {
            key: format!("radial-bar-{index}-{label}"),
            label,
            value,
            max,
        });
    }

    if output.is_empty() {
        Err(ChartError::EmptySeries)
    } else {
        Ok(output)
    }
}

fn validate_inner_radius(value: f64) -> ChartResult<f64> {
    let value = finite("inner_radius", value)?;
    if !(0.0..1.0).contains(&value) {
        return Err(ChartError::InvalidOption {
            field: "inner_radius",
            value: value.to_string(),
        });
    }
    Ok(value)
}

fn validate_ring_gap(value: f64) -> ChartResult<f64> {
    let value = finite("ring_gap", value)?;
    if value < 0.0 {
        return Err(ChartError::InvalidOption {
            field: "ring_gap",
            value: value.to_string(),
        });
    }
    Ok(value)
}

pub(crate) fn radial_bar_label(label: &str, index: usize) -> String {
    if label.is_empty() {
        format!("Bar {}", index + 1)
    } else {
        label.to_owned()
    }
}

fn arc_path(center: Point, radius: f64, start_angle: f64, end_angle: f64) -> ChartResult<String> {
    let radius = finite("radius", radius)?;
    if radius <= 0.0 {
        return Err(ChartError::InvalidOption {
            field: "radius",
            value: radius.to_string(),
        });
    }
    let end_angle = visible_arc_end(start_angle, end_angle);
    let start = polar_point(center, radius, start_angle);
    let end = polar_point(center, radius, end_angle);
    let large_arc = if (end_angle - start_angle).abs() > 180.0 {
        1
    } else {
        0
    };

    let mut path = String::new();
    write_point(&mut path, "M", start)?;
    write!(
        path,
        " A{},{} 0 {large_arc} 1 {},{}",
        clean(radius),
        clean(radius),
        clean(end.x),
        clean(end.y)
    )
    .expect("writing to string should not fail");
    Ok(path)
}

fn visible_arc_end(start_angle: f64, end_angle: f64) -> f64 {
    if (end_angle - start_angle).abs() >= FULL_CIRCLE_DEGREES {
        start_angle + FULL_CIRCLE_DEGREES - 0.001
    } else {
        end_angle
    }
}

fn angle_in_span(angle: f64, start_angle: f64, end_angle: f64) -> bool {
    let mut candidate = angle;
    while candidate < start_angle {
        candidate += FULL_CIRCLE_DEGREES;
    }
    candidate >= start_angle && candidate <= end_angle
}

fn polar_point(center: Point, radius: f64, angle_degrees: f64) -> Point {
    let radians = angle_degrees * PI / 180.0;
    Point {
        x: clean(center.x + radius * radians.cos()),
        y: clean(center.y + radius * radians.sin()),
    }
}

fn write_point(output: &mut String, command: &str, point: Point) -> ChartResult<()> {
    let x = finite("point.x", point.x)?;
    let y = finite("point.y", point.y)?;

    if !output.is_empty() {
        output.push(' ');
    }
    write!(output, "{command}{},{}", clean(x), clean(y))
        .expect("writing to string should not fail");
    Ok(())
}

fn clean(value: f64) -> f64 {
    if value.abs() < 1e-9 {
        return 0.0;
    }
    let rounded = value.round();
    if (value - rounded).abs() < 1e-9 {
        rounded
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_geometry_maps_values_to_rings() {
        let options = RadialBarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.2,
            ring_gap: 4.0,
            start_angle: -90.0,
            end_angle: 270.0,
        };

        let geometry = RadialBarChartGeometry::new(
            &[
                ChartRadialBar::new("CPU", 75.0, 100.0),
                ChartRadialBar::new("Memory", 50.0, 100.0),
            ],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.view_box, "0 0 100 100");
        assert_eq!(geometry.bars.len(), 2);
        assert_eq!(geometry.bars[0].percentage, 75.0);
        assert_eq!(geometry.bars[0].inner_radius, 32.0);
        assert_eq!(geometry.bars[0].outer_radius, 50.0);
        assert_eq!(geometry.bars[0].stroke_width, 18.0);
        assert!(geometry.bars[0].value_d.starts_with("M50,9"));
        assert_eq!(geometry.legend_items.len(), 2);
    }

    #[test]
    fn radial_geometry_renders_zero_value_as_track_only() {
        let geometry = RadialBarChartGeometry::new(
            &[ChartRadialBar::new("Done", 0.0, 10.0)],
            &RadialBarChartOptions::default(),
        )
        .unwrap();

        assert_eq!(geometry.bars.len(), 1);
        assert!(!geometry.bars[0].value_visible);
        assert!(geometry.bars[0].value_d.is_empty());
    }

    #[test]
    fn radial_geometry_skips_hidden_bars_and_marks_legend_inactive() {
        let mut data = vec![
            ChartRadialBar::new("CPU", 75.0, 100.0),
            ChartRadialBar::new("Memory", 50.0, 100.0),
        ];
        data[1].visible = false;

        let geometry =
            RadialBarChartGeometry::new(&data, &RadialBarChartOptions::default()).unwrap();

        assert_eq!(geometry.bars.len(), 1);
        assert!(geometry.legend_items[0].active);
        assert!(!geometry.legend_items[1].active);
    }

    #[test]
    fn radial_bar_contains_only_its_ring_span() {
        let options = RadialBarChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.5,
            ring_gap: 0.0,
            start_angle: -90.0,
            end_angle: 270.0,
        };
        let geometry =
            RadialBarChartGeometry::new(&[ChartRadialBar::new("Done", 8.0, 10.0)], &options)
                .unwrap();
        let bar = &geometry.bars[0];

        assert!(bar.contains(Point { x: 50.0, y: 20.0 }, geometry.center));
        assert!(!bar.contains(Point { x: 50.0, y: 50.0 }, geometry.center));
        assert!(!bar.contains(Point { x: 50.0, y: -5.0 }, geometry.center));
    }

    #[test]
    fn radial_geometry_rejects_negative_values() {
        let err = RadialBarChartGeometry::new(
            &[ChartRadialBar::new("CPU", -1.0, 100.0)],
            &RadialBarChartOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ChartError::InvalidOption {
                field: "bar.value",
                ..
            }
        ));
    }

    #[test]
    fn radial_geometry_rejects_values_over_max() {
        let err = RadialBarChartGeometry::new(
            &[ChartRadialBar::new("CPU", 101.0, 100.0)],
            &RadialBarChartOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ChartError::InvalidOption {
                field: "bar.value",
                ..
            }
        ));
    }

    #[test]
    fn component_recomputes_state() {
        let mut chart = PineRadialBarChart {
            data: vec![ChartRadialBar::new("Done", 8.0, 10.0)],
            ..Default::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.bars.len(), 1);
        assert_eq!(chart.bars[0].percentage_label, "80%");
    }

    #[test]
    fn center_text_positions_are_computed_fields() {
        let mut chart = PineRadialBarChart {
            data: vec![ChartRadialBar::new("Done", 8.0, 10.0)],
            center_label: "Health".into(),
            center_value: "80%".into(),
            ..Default::default()
        };

        chart.recompute();

        assert!(chart.center_visible);
        assert_eq!(chart.center_value_y, chart.center_y - 10.0);
        assert_eq!(chart.center_label_y, chart.center_y + 16.0);
    }

    #[test]
    fn radial_legend_items_use_stable_keys() {
        let items = radial_bar_legend_items(&[ChartRadialBar::new("", 1.0, 2.0)]);

        assert_eq!(items[0].key, "radial-bar-0-Bar 1");
        assert_eq!(items[0].label, "Bar 1");
        assert!(items[0].active);
    }
}
