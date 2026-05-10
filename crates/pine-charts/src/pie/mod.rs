use core::f64::consts::PI;
use core::fmt::Write;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::animation::{
    animation_duration_ms, animation_style, DEFAULT_ANIMATION_DURATION_MS, DEFAULT_ANIMATION_EASING,
};
use crate::cartesian::{
    pointer_event_svg_point, step_key, tooltip_aria_hidden, tooltip_mode, CartesianHoverPlacement,
    ChartStateFields, DEFAULT_EMPTY_MESSAGE,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::events::{
    ChartHover, ChartHoverEnd, ChartSelection, ChartSelectionEnd, CHART_HOVER_END_EVENT,
    CHART_HOVER_EVENT, CHART_SELECT_END_EVENT, CHART_SELECT_EVENT,
};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::LegendItem;
use crate::svg::format_tick;

const FULL_CIRCLE_DEGREES: f64 = 360.0;
const PIE_SLICE_DELAY_MS: u32 = 28;
const PIE_LEAVING_PRUNE_PROGRESS: f64 = 0.995;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartPieSlice {
    pub label: String,
    pub value: f64,
    #[serde(default = "crate::legend::default_visible")]
    pub visible: bool,
}

impl Default for ChartPieSlice {
    fn default() -> Self {
        Self {
            label: String::new(),
            value: 0.0,
            visible: true,
        }
    }
}

impl ChartPieSlice {
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
            visible: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PieChartOptions {
    pub width: f64,
    pub height: f64,
    pub margins: ChartMargins,
    pub inner_radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
}

impl Default for PieChartOptions {
    fn default() -> Self {
        Self {
            width: 320.0,
            height: 320.0,
            margins: ChartMargins::new(16.0, 16.0, 16.0, 16.0),
            inner_radius: 0.0,
            start_angle: -90.0,
            end_angle: 270.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PieChartGeometry {
    pub view_box: String,
    pub plot: ChartRect,
    pub center: Point,
    pub outer_radius: f64,
    pub inner_radius: f64,
    pub slices: Vec<SvgPieSlice>,
    pub legend_items: Vec<LegendItem>,
    pub total: f64,
}

impl PieChartGeometry {
    pub fn new(data: &[ChartPieSlice], options: &PieChartOptions) -> ChartResult<Self> {
        let width = finite("width", options.width)?;
        let height = finite("height", options.height)?;
        let plot = ChartRect::from_outer(width, height, options.margins)?;
        let inner_ratio = validate_inner_radius(options.inner_radius)?;
        let start_angle = finite("start_angle", options.start_angle)?;
        let end_angle = finite("end_angle", options.end_angle)?;
        if end_angle <= start_angle {
            return Err(ChartError::InvalidRange {
                start: start_angle,
                end: end_angle,
            });
        }

        let normalized = normalize_slices(data)?;
        let total = normalized.iter().map(|slice| slice.value).sum::<f64>();
        if total <= 0.0 {
            return Err(ChartError::EmptySeries);
        }

        let center = Point {
            x: plot.x + plot.width * 0.5,
            y: plot.y + plot.height * 0.5,
        };
        let outer_radius = plot.width.min(plot.height) * 0.5;
        let inner_radius = outer_radius * inner_ratio;
        let span = end_angle - start_angle;
        let mut current_angle = start_angle;
        let last_index = normalized.len().saturating_sub(1);
        let slices = normalized
            .iter()
            .enumerate()
            .map(|(index, slice)| {
                let slice_start = current_angle;
                let slice_end = if index == last_index {
                    end_angle
                } else {
                    slice_start + span * (slice.value / total)
                };
                current_angle = slice_end;
                let percentage = slice.value / total * 100.0;
                let percentage_label = format!("{}%", format_tick(percentage));
                let value_label = format_tick(slice.value);
                let label = slice.label.clone();
                let aria_label = format!("{label}: {value_label} ({percentage_label})");
                let d = pie_slice_path(center, outer_radius, inner_radius, slice_start, slice_end)?;
                let label_point = polar_point(
                    center,
                    label_radius(outer_radius, inner_radius),
                    (slice_start + slice_end) * 0.5,
                );
                Ok(SvgPieSlice {
                    key: slice.key.clone(),
                    label,
                    value: slice.value,
                    value_label,
                    percentage,
                    percentage_label,
                    aria_label,
                    d: d.clone(),
                    start_angle: slice_start,
                    end_angle: slice_end,
                    label_x: label_point.x,
                    label_y: label_point.y,
                    animation_style: slice_animation_style(index),
                    center_x: center.x,
                    center_y: center.y,
                    outer_radius,
                    inner_radius,
                    entering: false,
                    leaving: false,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;
        let legend_items = pie_legend_items(data);

        Ok(Self {
            view_box: format!("0 0 {width} {height}"),
            plot,
            center,
            outer_radius,
            inner_radius,
            slices,
            legend_items,
            total,
        })
    }
}

pub fn pie_legend_items(data: &[ChartPieSlice]) -> Vec<LegendItem> {
    data.iter()
        .enumerate()
        .filter(|(_, slice)| slice.value > 0.0)
        .map(|(index, slice)| {
            let label = slice_label(&slice.label, index);
            LegendItem::with_series_active(
                format!("pie-slice-{index}-{label}"),
                label.clone(),
                label,
                slice.visible,
            )
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SvgPieSlice {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub value_label: String,
    pub percentage: f64,
    pub percentage_label: String,
    pub aria_label: String,
    pub d: String,
    pub start_angle: f64,
    pub end_angle: f64,
    pub label_x: f64,
    pub label_y: f64,
    pub animation_style: String,
    pub center_x: f64,
    pub center_y: f64,
    pub outer_radius: f64,
    pub inner_radius: f64,
    pub entering: bool,
    pub leaving: bool,
}

impl SvgPieSlice {
    fn selection(&self) -> ChartSelection {
        ChartSelection::share(
            "pie",
            self.key.clone(),
            self.aria_label.clone(),
            self.aria_label.clone(),
            self.value,
            self.value_label.clone(),
            self.percentage,
            self.percentage_label.clone(),
        )
    }

    fn contains(&self, point: Point, center: Point, inner_radius: f64, outer_radius: f64) -> bool {
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = (dx * dx + dy * dy).sqrt();
        if radius < inner_radius || radius > outer_radius {
            return false;
        }

        angle_in_span(dy.atan2(dx) * 180.0 / PI, self.start_angle, self.end_angle)
    }

    fn hover_update(&self, plot: ChartRect, width: f64, height: f64) -> PieHoverUpdate {
        PieHoverUpdate {
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct PieSliceFrame {
    center_x: f64,
    center_y: f64,
    outer_radius: f64,
    inner_radius: f64,
    start_angle: f64,
    end_angle: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct PieSliceAnimation {
    key: String,
    from: PieSliceFrame,
    to: PieSliceFrame,
    delay_ms: u32,
    duration_ms: u32,
    entering: bool,
    leaving: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PieSliceBoundary {
    key: String,
    frame: PieSliceFrame,
}

#[derive(Clone, Debug, PartialEq)]
struct PieHoverUpdate {
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
#[component(template = "PinePieChart.poco", role = "panel")]
pub struct PinePieChart {
    #[prop]
    pub data: Vec<ChartPieSlice>,
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
    pub animation_generation: u32,
    pub animating_slices: bool,
    slice_animations: Vec<PieSliceAnimation>,
    animation_started_at_ms: f64,
    pub state: String,
    pub view_box: String,
    pub slices: Vec<SvgPieSlice>,
    pub hover_slice: SvgPieSlice,
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
    pub hover_overlay_visible: bool,
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

impl Default for PinePieChart {
    fn default() -> Self {
        let options = PieChartOptions::default();
        Self {
            data: Vec::new(),
            label: "Pie chart".into(),
            empty_message: DEFAULT_EMPTY_MESSAGE.into(),
            width: options.width,
            height: options.height,
            margin_top: options.margins.top,
            margin_right: options.margins.right,
            margin_bottom: options.margins.bottom,
            margin_left: options.margins.left,
            inner_radius: options.inner_radius,
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
            animation_generation: 0,
            animating_slices: false,
            slice_animations: Vec::new(),
            animation_started_at_ms: 0.0,
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            slices: Vec::new(),
            hover_slice: SvgPieSlice::default(),
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
            hover_overlay_visible: false,
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
impl PinePieChart {
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
    fn on_data(&mut self, _: Vec<ChartPieSlice>, _: Option<Vec<ChartPieSlice>>) {
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

    #[watch(inner_radius)]
    fn on_inner_radius(&mut self, _: f64, _: Option<f64>) {
        self.recompute_shape();
    }

    #[watch(start_angle)]
    fn on_start_angle(&mut self, _: f64, _: Option<f64>) {
        self.recompute_shape();
    }

    #[watch(end_angle)]
    fn on_end_angle(&mut self, _: f64, _: Option<f64>) {
        self.recompute_shape();
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
        self.hover_overlay_visible = false;
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
        self.hover_slice = SvgPieSlice::default();
        self.sync_tooltip_state();
        if was_visible {
            pocopine::emit(CHART_HOVER_END_EVENT, ChartHoverEnd::new("pie"));
        }
    }

    pub fn select_slice(&mut self, key: String) {
        if let Some(selection) = self.selection_for_slice(&key) {
            self.focused_key = key.clone();
            self.selected_key = key;
            pocopine::emit(CHART_SELECT_EVENT, selection);
        }
    }

    pub fn focus_next_slice(&mut self) {
        self.step_slice_focus(1);
    }

    pub fn focus_prev_slice(&mut self) {
        self.step_slice_focus(-1);
    }

    pub fn select_focused_slice(&mut self) {
        if self.focused_key.is_empty() {
            self.step_slice_focus(1);
        }
        if let Some(selection) = self.selection_for_slice(&self.focused_key) {
            self.selected_key = self.focused_key.clone();
            pocopine::emit(CHART_SELECT_EVENT, selection);
        }
    }

    pub fn clear_selection(&mut self) {
        self.focused_key.clear();
        self.clear_selected_key();
    }
}

impl PinePieChart {
    fn update_animation_style(&mut self) {
        self.animation_style = animation_style(self.animation_duration, &self.animation_easing);
    }

    fn sync_tooltip_state(&mut self) {
        self.tooltip_mode = tooltip_mode(&self.tooltip).into();
        self.tooltip_aria_hidden =
            tooltip_aria_hidden(&self.tooltip_mode, self.hover_visible).into();
    }

    fn recompute(&mut self) {
        self.recompute_inner(false, false);
    }

    fn recompute_with_leaving(&mut self) {
        self.recompute_inner(self.animate, self.animate);
    }

    fn recompute_shape(&mut self) {
        self.recompute_inner(false, self.animate);
    }

    fn recompute_inner(&mut self, retain_leaving: bool, animate_geometry: bool) {
        match PieChartGeometry::new(&self.data, &self.options()) {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                let mut slices = geometry.slices;
                if animate_geometry || retain_leaving {
                    self.prepare_slice_animations(&mut slices, retain_leaving);
                } else {
                    self.clear_slice_animations();
                }
                self.slices = slices;
                self.legend_items = geometry.legend_items;
                self.center_x = geometry.center.x;
                self.center_y = geometry.center.y;
                self.outer_radius_px = geometry.outer_radius;
                self.inner_radius_px = geometry.inner_radius;
                self.update_center_visibility();
                self.error.clear();
                self.state_fields()
                    .apply(crate::cartesian::CartesianChartState::Ready);
                self.reconcile_selection();
                self.clear_hover();
            }
            Err(ChartError::EmptySeries) => {
                if retain_leaving
                    && !self.slices.is_empty()
                    && animation_duration_ms(self.animation_duration) > 0
                {
                    self.prepare_empty_slice_exit();
                    self.legend_items = pie_legend_items(&self.data);
                    self.clear_hover();
                    self.clear_selection();
                    self.error.clear();
                    self.state_fields()
                        .apply(crate::cartesian::CartesianChartState::Ready);
                    return;
                }
                self.clear_geometry();
                self.clear_hover();
                self.clear_selection();
                self.error.clear();
                self.state_fields()
                    .apply(crate::cartesian::CartesianChartState::Empty);
            }
            Err(error) => {
                self.clear_geometry();
                self.clear_hover();
                self.clear_selection();
                self.error = error.to_string();
                self.state_fields()
                    .apply(crate::cartesian::CartesianChartState::Invalid);
            }
        }
    }

    fn prepare_slice_animations(&mut self, next: &mut Vec<SvgPieSlice>, retain_leaving: bool) {
        let previous_boundaries = slice_boundaries(&self.slices);
        if previous_boundaries.is_empty() {
            self.clear_slice_animations();
            return;
        }
        let next_boundaries = slice_boundaries(next);

        let duration_ms = animation_duration_ms(self.animation_duration);
        if duration_ms == 0 {
            self.clear_slice_animations();
            return;
        }

        let mut animations = Vec::new();
        for (index, slice) in next.iter_mut().enumerate() {
            if let Some(previous) = self
                .slices
                .iter()
                .find(|previous| !previous.leaving && previous.key == slice.key)
            {
                let from = slice_frame(previous);
                let to = slice_frame(slice);
                if frames_match(&from, &to) && !previous.entering {
                    configure_static_slice(slice);
                    continue;
                }
                let entering = previous.entering;
                slice.entering = entering;
                slice.leaving = false;
                let delay_ms = if entering { slice_delay_ms(index) } else { 0 };
                animations.push(PieSliceAnimation {
                    key: slice.key.clone(),
                    from: from.clone(),
                    to,
                    delay_ms,
                    duration_ms,
                    entering,
                    leaving: false,
                });
                apply_slice_frame(slice, &from);
                continue;
            }

            let to = slice_frame(slice);
            let collapse_angle = entering_collapse_angle(
                index,
                &next_boundaries,
                &previous_boundaries,
                to.start_angle,
            );
            let from = collapsed_frame(&to, collapse_angle);
            slice.entering = true;
            slice.leaving = false;
            animations.push(PieSliceAnimation {
                key: slice.key.clone(),
                from: from.clone(),
                to,
                delay_ms: slice_delay_ms(index),
                duration_ms,
                entering: true,
                leaving: false,
            });
            apply_slice_frame(slice, &from);
        }

        if retain_leaving {
            for (previous_index, previous_boundary) in previous_boundaries.iter().enumerate() {
                if next_boundaries
                    .iter()
                    .any(|slice| slice.key == previous_boundary.key)
                {
                    continue;
                }
                let Some(previous) = self
                    .slices
                    .iter()
                    .find(|slice| slice.key == previous_boundary.key)
                else {
                    continue;
                };
                let mut leaving = previous.clone();
                let from = previous_boundary.frame.clone();
                let collapse_angle = leaving_collapse_angle(
                    previous_index,
                    &previous_boundaries,
                    &next_boundaries,
                    from.end_angle,
                );
                let to = collapsed_frame(&from, collapse_angle);
                leaving.entering = false;
                leaving.leaving = true;
                apply_slice_frame(&mut leaving, &from);
                animations.push(PieSliceAnimation {
                    key: leaving.key.clone(),
                    from,
                    to,
                    delay_ms: 0,
                    duration_ms,
                    entering: false,
                    leaving: true,
                });
                next.push(leaving);
            }
        }

        if animations.is_empty() {
            self.clear_slice_animations();
            return;
        }
        self.slice_animations = animations;
        self.animating_slices = true;
        self.update_hover_overlay_visibility();
        self.animation_started_at_ms = now_ms();
        self.start_slice_animation_loop();
    }

    fn prepare_empty_slice_exit(&mut self) {
        let duration_ms = animation_duration_ms(self.animation_duration);
        if duration_ms == 0 || self.slices.is_empty() {
            self.clear_slice_animations();
            return;
        }
        let mut animations = Vec::new();
        for slice in &mut self.slices {
            let from = slice_frame(slice);
            let to = collapsed_frame(&from, from.end_angle);
            slice.entering = false;
            slice.leaving = true;
            animations.push(PieSliceAnimation {
                key: slice.key.clone(),
                from,
                to,
                delay_ms: 0,
                duration_ms,
                entering: false,
                leaving: true,
            });
        }
        self.slice_animations = animations;
        self.animating_slices = true;
        self.update_hover_overlay_visibility();
        self.animation_started_at_ms = now_ms();
        self.start_slice_animation_loop();
    }

    fn clear_slice_animations(&mut self) {
        self.slice_animations.clear();
        self.animating_slices = false;
        self.update_hover_overlay_visibility();
    }

    fn start_slice_animation_loop(&mut self) {
        self.animation_generation = self.animation_generation.wrapping_add(1);
        let generation = self.animation_generation;
        let me = pocopine::this::<Self>();
        pocopine::spawn_scoped(async move {
            loop {
                let now = pocopine::timers::next_frame().await;
                if me.update(|chart| chart.tick_slice_animations(generation, now)) {
                    break;
                }
            }
        });
    }

    fn tick_slice_animations(&mut self, generation: u32, now_ms: f64) -> bool {
        if generation != self.animation_generation || self.slice_animations.is_empty() {
            return true;
        }

        let animations = self.slice_animations.clone();
        let elapsed_ms = (now_ms - self.animation_started_at_ms).max(0.0);
        let easing = self.animation_easing.clone();
        let complete = animations
            .iter()
            .all(|animation| slice_animation_progress(animation, elapsed_ms) >= 1.0);
        let mut prune_leaving_keys = Vec::new();

        for slice in &mut self.slices {
            let Some(animation) = animations
                .iter()
                .find(|animation| animation.key == slice.key)
            else {
                configure_static_slice(slice);
                continue;
            };

            let progress = slice_animation_progress(animation, elapsed_ms);
            if animation.leaving && progress >= PIE_LEAVING_PRUNE_PROGRESS {
                prune_leaving_keys.push(slice.key.clone());
                continue;
            }
            let frame = interpolate_slice_frame(
                &animation.from,
                &animation.to,
                easing_progress(progress, &easing),
            );
            apply_slice_frame(slice, &frame);
            slice.entering = animation.entering && progress < 1.0;
            slice.leaving = animation.leaving;
        }

        if !prune_leaving_keys.is_empty() {
            self.slices.retain(|slice| {
                !slice.leaving || !prune_leaving_keys.iter().any(|key| key == &slice.key)
            });
        }
        self.sync_hover_slice();

        if complete {
            self.finish_slice_animations(generation);
            return true;
        }
        false
    }

    fn finish_slice_animations(&mut self, generation: u32) {
        if generation != self.animation_generation {
            return;
        }
        self.clear_slice_animations();
        self.slices.retain(|slice| !slice.leaving);
        self.slices.iter_mut().for_each(|slice| {
            configure_static_slice(slice);
        });
        self.sync_hover_slice();
        if self.slices.is_empty() {
            self.clear_geometry();
            self.state_fields()
                .apply(crate::cartesian::CartesianChartState::Empty);
        }
    }

    fn options(&self) -> PieChartOptions {
        PieChartOptions {
            width: self.width,
            height: self.height,
            margins: ChartMargins::new(
                self.margin_top,
                self.margin_right,
                self.margin_bottom,
                self.margin_left,
            ),
            inner_radius: self.inner_radius,
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
            .slices
            .iter()
            .filter(|slice| !slice.leaving)
            .find(|slice| slice.contains(point, center, self.inner_radius_px, self.outer_radius_px))
            .map(|slice| slice.hover_update(plot, self.width, self.height))
        else {
            self.clear_hover();
            return;
        };

        self.apply_hover(update);
    }

    fn step_slice_focus(&mut self, step: isize) {
        if let Some(key) = step_key(
            self.slices
                .iter()
                .filter(|slice| !slice.leaving)
                .map(|slice| slice.key.as_str()),
            &self.focused_key,
            step,
        ) {
            self.focused_key = key;
        }
    }

    fn has_slice_key(&self, key: &str) -> bool {
        self.slices
            .iter()
            .any(|slice| !slice.leaving && slice.key == key)
    }

    fn selection_for_slice(&self, key: &str) -> Option<ChartSelection> {
        self.slices
            .iter()
            .find(|slice| !slice.leaving && slice.key == key)
            .map(SvgPieSlice::selection)
    }

    fn clear_geometry(&mut self) {
        self.slices.clear();
        self.clear_slice_animations();
        self.hover_slice = SvgPieSlice::default();
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
            (true, true) if self.is_half_donut() => {
                self.center_value_y = self.center_y - 26.0;
                self.center_label_y = self.center_y;
            }
            (true, true) => {
                self.center_value_y = self.center_y - 10.0;
                self.center_label_y = self.center_y + 16.0;
            }
            (true, false) => {
                self.center_value_y = self.center_y;
                self.center_label_y = self.center_y;
            }
            (false, true) => {
                self.center_value_y = self.center_y;
                self.center_label_y = self.center_y;
            }
            (false, false) => {
                self.center_value_y = self.center_y;
                self.center_label_y = self.center_y;
            }
        }
    }

    fn is_half_donut(&self) -> bool {
        self.inner_radius_px > 0.0 && (self.end_angle - self.start_angle).abs() <= 180.0 + 1e-9
    }

    fn apply_hover(&mut self, update: PieHoverUpdate) {
        let hover = ChartHover::share(
            "pie",
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
        self.sync_hover_slice();
        self.update_hover_overlay_visibility();
        self.sync_tooltip_state();
        pocopine::emit(CHART_HOVER_EVENT, hover);
    }

    fn sync_hover_slice(&mut self) {
        if self.hover_key.is_empty() {
            self.hover_slice = SvgPieSlice::default();
            return;
        }
        self.hover_slice = self
            .slices
            .iter()
            .find(|slice| !slice.leaving && slice.key == self.hover_key)
            .cloned()
            .unwrap_or_default();
    }

    fn update_hover_overlay_visibility(&mut self) {
        self.hover_overlay_visible = self.hover_visible && !self.animating_slices;
    }

    fn reconcile_selection(&mut self) {
        if !self.has_slice_key(&self.focused_key) {
            self.focused_key.clear();
        }
        if !self.has_slice_key(&self.selected_key) {
            self.clear_selected_key();
        }
    }

    fn clear_selected_key(&mut self) {
        let key = std::mem::take(&mut self.selected_key);
        if !key.is_empty() {
            pocopine::emit(CHART_SELECT_END_EVENT, ChartSelectionEnd::new("pie", key));
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
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedPieSlice {
    key: String,
    label: String,
    value: f64,
}

fn normalize_slices(data: &[ChartPieSlice]) -> ChartResult<Vec<NormalizedPieSlice>> {
    if data.is_empty() {
        return Err(ChartError::EmptySeries);
    }

    let mut output = Vec::new();
    for (index, slice) in data.iter().enumerate() {
        let value = finite("slice.value", slice.value)?;
        if value < 0.0 {
            return Err(ChartError::InvalidOption {
                field: "slice.value",
                value: value.to_string(),
            });
        }
        if value == 0.0 || !slice.visible {
            continue;
        }
        let label = slice_label(&slice.label, index);
        output.push(NormalizedPieSlice {
            key: format!("pie-slice-{index}-{label}"),
            label,
            value,
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

pub(crate) fn slice_label(label: &str, index: usize) -> String {
    if label.is_empty() {
        format!("Slice {}", index + 1)
    } else {
        label.to_owned()
    }
}

fn pie_slice_path(
    center: Point,
    outer_radius: f64,
    inner_radius: f64,
    start_angle: f64,
    end_angle: f64,
) -> ChartResult<String> {
    let end_angle = visible_arc_end(start_angle, end_angle);
    let outer_start = polar_point(center, outer_radius, start_angle);
    let outer_end = polar_point(center, outer_radius, end_angle);
    let large_arc = if (end_angle - start_angle).abs() > 180.0 {
        1
    } else {
        0
    };

    let mut path = String::new();
    if inner_radius <= 0.0 {
        write_point(&mut path, "M", center)?;
        write_point(&mut path, "L", outer_start)?;
        write!(
            path,
            " A{},{} 0 {large_arc} 1 {},{} Z",
            clean(outer_radius),
            clean(outer_radius),
            clean(outer_end.x),
            clean(outer_end.y)
        )
        .expect("writing to string should not fail");
        return Ok(path);
    }

    let inner_start = polar_point(center, inner_radius, start_angle);
    let inner_end = polar_point(center, inner_radius, end_angle);
    write_point(&mut path, "M", outer_start)?;
    write!(
        path,
        " A{},{} 0 {large_arc} 1 {},{}",
        clean(outer_radius),
        clean(outer_radius),
        clean(outer_end.x),
        clean(outer_end.y)
    )
    .expect("writing to string should not fail");
    write_point(&mut path, "L", inner_end)?;
    write!(
        path,
        " A{},{} 0 {large_arc} 0 {},{} Z",
        clean(inner_radius),
        clean(inner_radius),
        clean(inner_start.x),
        clean(inner_start.y)
    )
    .expect("writing to string should not fail");
    Ok(path)
}

fn configure_static_slice(slice: &mut SvgPieSlice) {
    slice.entering = false;
    slice.leaving = false;
}

fn slice_frame(slice: &SvgPieSlice) -> PieSliceFrame {
    PieSliceFrame {
        center_x: slice.center_x,
        center_y: slice.center_y,
        outer_radius: slice.outer_radius,
        inner_radius: slice.inner_radius,
        start_angle: slice.start_angle,
        end_angle: slice.end_angle,
    }
}

fn slice_boundaries(slices: &[SvgPieSlice]) -> Vec<PieSliceBoundary> {
    slices
        .iter()
        .filter(|slice| !slice.leaving)
        .map(|slice| PieSliceBoundary {
            key: slice.key.clone(),
            frame: slice_frame(slice),
        })
        .collect()
}

fn entering_collapse_angle(
    next_index: usize,
    next: &[PieSliceBoundary],
    previous: &[PieSliceBoundary],
    fallback: f64,
) -> f64 {
    let before = next
        .get(..next_index)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|slice| boundary_frame(previous, &slice.key).map(|frame| frame.end_angle));
    let after = next
        .get(next_index + 1..)
        .unwrap_or_default()
        .iter()
        .find_map(|slice| boundary_frame(previous, &slice.key).map(|frame| frame.start_angle));
    collapse_angle(before, after, fallback)
}

fn leaving_collapse_angle(
    previous_index: usize,
    previous: &[PieSliceBoundary],
    next: &[PieSliceBoundary],
    fallback: f64,
) -> f64 {
    let before = previous
        .get(..previous_index)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|slice| boundary_frame(next, &slice.key).map(|frame| frame.end_angle));
    let after = previous
        .get(previous_index + 1..)
        .unwrap_or_default()
        .iter()
        .find_map(|slice| boundary_frame(next, &slice.key).map(|frame| frame.start_angle));
    collapse_angle(before, after, fallback)
}

fn boundary_frame<'a>(boundaries: &'a [PieSliceBoundary], key: &str) -> Option<&'a PieSliceFrame> {
    boundaries
        .iter()
        .find(|boundary| boundary.key == key)
        .map(|boundary| &boundary.frame)
}

fn collapse_angle(before: Option<f64>, after: Option<f64>, fallback: f64) -> f64 {
    match (before, after) {
        (Some(before), Some(after)) => clean((before + after) * 0.5),
        (Some(before), None) => before,
        (None, Some(after)) => after,
        (None, None) => fallback,
    }
}

fn collapsed_frame(frame: &PieSliceFrame, angle: f64) -> PieSliceFrame {
    PieSliceFrame {
        center_x: frame.center_x,
        center_y: frame.center_y,
        outer_radius: frame.outer_radius,
        inner_radius: frame.inner_radius,
        start_angle: angle,
        end_angle: angle,
    }
}

fn apply_slice_frame(slice: &mut SvgPieSlice, frame: &PieSliceFrame) {
    let center = Point {
        x: frame.center_x,
        y: frame.center_y,
    };
    if let Ok(path) = pie_slice_path(
        center,
        frame.outer_radius,
        frame.inner_radius,
        frame.start_angle,
        frame.end_angle,
    ) {
        slice.d = path;
    }
    slice.center_x = frame.center_x;
    slice.center_y = frame.center_y;
    slice.outer_radius = frame.outer_radius;
    slice.inner_radius = frame.inner_radius;
    slice.start_angle = frame.start_angle;
    slice.end_angle = frame.end_angle;
    let label_point = polar_point(
        center,
        label_radius(frame.outer_radius, frame.inner_radius),
        (frame.start_angle + frame.end_angle) * 0.5,
    );
    slice.label_x = label_point.x;
    slice.label_y = label_point.y;
}

fn interpolate_slice_frame(
    from: &PieSliceFrame,
    to: &PieSliceFrame,
    progress: f64,
) -> PieSliceFrame {
    PieSliceFrame {
        center_x: lerp(from.center_x, to.center_x, progress),
        center_y: lerp(from.center_y, to.center_y, progress),
        outer_radius: lerp(from.outer_radius, to.outer_radius, progress),
        inner_radius: lerp(from.inner_radius, to.inner_radius, progress),
        start_angle: lerp(from.start_angle, to.start_angle, progress),
        end_angle: lerp(from.end_angle, to.end_angle, progress),
    }
}

fn frames_match(left: &PieSliceFrame, right: &PieSliceFrame) -> bool {
    const EPSILON: f64 = 0.001;
    (left.center_x - right.center_x).abs() <= EPSILON
        && (left.center_y - right.center_y).abs() <= EPSILON
        && (left.outer_radius - right.outer_radius).abs() <= EPSILON
        && (left.inner_radius - right.inner_radius).abs() <= EPSILON
        && (left.start_angle - right.start_angle).abs() <= EPSILON
        && (left.end_angle - right.end_angle).abs() <= EPSILON
}

fn slice_animation_progress(animation: &PieSliceAnimation, elapsed_ms: f64) -> f64 {
    let elapsed_ms = elapsed_ms - animation.delay_ms as f64;
    if elapsed_ms <= 0.0 {
        return 0.0;
    }
    if animation.duration_ms == 0 {
        return 1.0;
    }
    (elapsed_ms / animation.duration_ms as f64).clamp(0.0, 1.0)
}

fn easing_progress(progress: f64, easing: &str) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    match easing.trim() {
        "linear" => progress,
        "ease-in" => progress * progress,
        "ease-out" => 1.0 - (1.0 - progress).powi(2),
        "ease-in-out" => {
            if progress < 0.5 {
                2.0 * progress * progress
            } else {
                1.0 - (-2.0 * progress + 2.0).powi(2) * 0.5
            }
        }
        _ => progress * progress * (3.0 - 2.0 * progress),
    }
}

fn lerp(from: f64, to: f64, progress: f64) -> f64 {
    clean(from + (to - from) * progress)
}

fn slice_animation_style(index: usize) -> String {
    format!("--pine-chart-slice-delay: {}ms;", slice_delay_ms(index))
}

fn slice_delay_ms(index: usize) -> u32 {
    index as u32 * PIE_SLICE_DELAY_MS
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or_else(js_sys::Date::now)
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

fn label_radius(outer_radius: f64, inner_radius: f64) -> f64 {
    if inner_radius > 0.0 {
        inner_radius + (outer_radius - inner_radius) * 0.5
    } else {
        outer_radius * 0.66
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
    fn pie_geometry_maps_values_to_slices() {
        let options = PieChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.0,
            start_angle: -90.0,
            end_angle: 270.0,
        };

        let geometry = PieChartGeometry::new(
            &[ChartPieSlice::new("A", 1.0), ChartPieSlice::new("B", 3.0)],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.view_box, "0 0 100 100");
        assert_eq!(geometry.slices.len(), 2);
        assert_eq!(geometry.slices[0].percentage, 25.0);
        assert_eq!(geometry.slices[0].d, "M50,50 L50,0 A50,50 0 0 1 100,50 Z");
        assert_eq!(geometry.legend_items.len(), 2);
    }

    #[test]
    fn pie_geometry_skips_hidden_slices_and_marks_legend_inactive() {
        let options = PieChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.0,
            start_angle: -90.0,
            end_angle: 270.0,
        };
        let mut data = vec![ChartPieSlice::new("A", 1.0), ChartPieSlice::new("B", 3.0)];
        data[1].visible = false;

        let geometry = PieChartGeometry::new(&data, &options).unwrap();

        assert_eq!(geometry.slices.len(), 1);
        assert_eq!(geometry.slices[0].label, "A");
        assert_eq!(geometry.total, 1.0);
        assert!(geometry.legend_items[0].active);
        assert!(!geometry.legend_items[1].active);
    }

    #[test]
    fn donut_geometry_uses_inner_arc() {
        let options = PieChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.5,
            start_angle: -90.0,
            end_angle: 270.0,
        };

        let geometry = PieChartGeometry::new(&[ChartPieSlice::new("A", 1.0)], &options).unwrap();

        assert_eq!(geometry.inner_radius, 25.0);
        assert!(geometry.slices[0].d.contains("A50,50"));
        assert!(geometry.slices[0].d.contains("A25,25"));
    }

    #[test]
    fn half_pie_uses_custom_angle_span() {
        let options = PieChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.0,
            start_angle: 180.0,
            end_angle: 360.0,
        };

        let geometry = PieChartGeometry::new(
            &[ChartPieSlice::new("A", 1.0), ChartPieSlice::new("B", 1.0)],
            &options,
        )
        .unwrap();

        assert_eq!(geometry.slices[0].start_angle, 180.0);
        assert_eq!(geometry.slices[0].end_angle, 270.0);
        assert_eq!(geometry.slices[1].end_angle, 360.0);
    }

    #[test]
    fn donut_slice_contains_only_its_ring_span() {
        let options = PieChartOptions {
            width: 100.0,
            height: 100.0,
            margins: ChartMargins::ZERO,
            inner_radius: 0.5,
            start_angle: -90.0,
            end_angle: 270.0,
        };
        let geometry = PieChartGeometry::new(
            &[ChartPieSlice::new("A", 1.0), ChartPieSlice::new("B", 1.0)],
            &options,
        )
        .unwrap();
        let slice = &geometry.slices[0];

        assert!(slice.contains(
            Point { x: 50.0, y: 10.0 },
            geometry.center,
            geometry.inner_radius,
            geometry.outer_radius
        ));
        assert!(!slice.contains(
            Point { x: 50.0, y: 50.0 },
            geometry.center,
            geometry.inner_radius,
            geometry.outer_radius
        ));
        assert!(!slice.contains(
            Point { x: 10.0, y: 50.0 },
            geometry.center,
            geometry.inner_radius,
            geometry.outer_radius
        ));
    }

    #[test]
    fn pie_rejects_negative_values() {
        let err = PieChartGeometry::new(
            &[ChartPieSlice::new("A", -1.0)],
            &PieChartOptions::default(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ChartError::InvalidOption {
                field: "slice.value",
                ..
            }
        ));
    }

    #[test]
    fn component_recomputes_state() {
        let mut chart = PinePieChart {
            data: vec![ChartPieSlice::new("A", 2.0)],
            ..Default::default()
        };

        chart.recompute();

        assert!(chart.ready);
        assert_eq!(chart.slices.len(), 1);
    }

    #[test]
    fn center_text_positions_are_computed_fields() {
        let mut chart = PinePieChart {
            data: vec![ChartPieSlice::new("A", 2.0)],
            inner_radius: 0.5,
            center_label: "Total".into(),
            center_value: "2".into(),
            ..Default::default()
        };

        chart.recompute();

        assert!(chart.center_visible);
        assert_eq!(chart.center_value_y, chart.center_y - 10.0);
        assert_eq!(chart.center_label_y, chart.center_y + 16.0);
    }

    #[test]
    fn half_donut_center_label_sits_on_center_line() {
        let mut chart = PinePieChart {
            data: vec![ChartPieSlice::new("A", 2.0)],
            inner_radius: 0.5,
            start_angle: 180.0,
            end_angle: 360.0,
            center_label: "Progress".into(),
            center_value: "74%".into(),
            ..Default::default()
        };

        chart.recompute();

        assert!(chart.center_visible);
        assert_eq!(chart.center_value_y, chart.center_y - 26.0);
        assert_eq!(chart.center_label_y, chart.center_y);
    }

    #[test]
    fn entering_slice_collapses_at_old_neighbor_boundary() {
        let previous = vec![boundary("a", -90.0, 180.0), boundary("b", 180.0, 270.0)];
        let next = vec![
            boundary("a", -90.0, 90.0),
            boundary("b", 90.0, 150.0),
            boundary("c", 150.0, 270.0),
        ];

        assert_eq!(
            entering_collapse_angle(2, &next, &previous, next[2].frame.start_angle),
            270.0
        );
    }

    #[test]
    fn entering_middle_slice_collapses_between_old_neighbors() {
        let previous = vec![boundary("a", -90.0, 90.0), boundary("b", 90.0, 270.0)];
        let next = vec![
            boundary("a", -90.0, 54.0),
            boundary("new", 54.0, 126.0),
            boundary("b", 126.0, 270.0),
        ];

        assert_eq!(
            entering_collapse_angle(1, &next, &previous, next[1].frame.start_angle),
            90.0
        );
    }

    #[test]
    fn leaving_middle_slice_collapses_to_new_neighbor_boundary() {
        let previous = vec![
            boundary("one", -90.0, -54.0),
            boundary("two", -54.0, -18.0),
            boundary("three", -18.0, 270.0),
        ];
        let next = vec![
            boundary("one", -90.0, -45.0),
            boundary("three", -45.0, 270.0),
        ];

        assert_eq!(
            leaving_collapse_angle(1, &previous, &next, previous[1].frame.end_angle),
            -45.0
        );
    }

    fn boundary(key: &str, start_angle: f64, end_angle: f64) -> PieSliceBoundary {
        PieSliceBoundary {
            key: key.into(),
            frame: PieSliceFrame {
                start_angle,
                end_angle,
                ..PieSliceFrame::default()
            },
        }
    }
}
