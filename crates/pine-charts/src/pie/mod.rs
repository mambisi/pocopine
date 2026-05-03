use core::f64::consts::PI;
use core::fmt::Write;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cartesian::{
    pointer_event_svg_point, step_key, CartesianHoverPlacement, ChartStateFields,
};
use crate::error::{finite, ChartError, ChartResult};
use crate::events::{ChartSelection, CHART_SELECT_EVENT};
use crate::geometry::{ChartMargins, ChartRect, Point};
use crate::legend::LegendItem;
use crate::svg::format_tick;

const FULL_CIRCLE_DEGREES: f64 = 360.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartPieSlice {
    pub label: String,
    pub value: f64,
}

impl ChartPieSlice {
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
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
                let label = slice_label(&slice.label, index);
                let aria_label = format!("{label}: {value_label} ({percentage_label})");
                let d = pie_slice_path(center, outer_radius, inner_radius, slice_start, slice_end)?;
                let label_point = polar_point(
                    center,
                    label_radius(outer_radius, inner_radius),
                    (slice_start + slice_end) * 0.5,
                );
                Ok(SvgPieSlice {
                    key: format!("pie-slice-{index}-{label}"),
                    label,
                    value: slice.value,
                    value_label,
                    percentage,
                    percentage_label,
                    aria_label,
                    d,
                    start_angle: slice_start,
                    end_angle: slice_end,
                    label_x: label_point.x,
                    label_y: label_point.y,
                })
            })
            .collect::<ChartResult<Vec<_>>>()?;
        let legend_items = slices
            .iter()
            .map(|slice| {
                LegendItem::with_series(slice.key.clone(), slice.label.clone(), slice.label.clone())
            })
            .collect();

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
            LegendItem::with_series(format!("pie-slice-{index}-{label}"), label.clone(), label)
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
}

impl SvgPieSlice {
    fn selection(&self) -> ChartSelection {
        ChartSelection::share(
            "pie",
            self.key.clone(),
            self.aria_label.clone(),
            self.value,
            self.percentage,
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
    pub state: String,
    pub view_box: String,
    pub slices: Vec<SvgPieSlice>,
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

impl Default for PinePieChart {
    fn default() -> Self {
        let options = PieChartOptions::default();
        Self {
            data: Vec::new(),
            label: "Pie chart".into(),
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
            state: "empty".into(),
            view_box: format!("0 0 {} {}", options.width, options.height),
            slices: Vec::new(),
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
impl PinePieChart {
    fn on_setup(&mut self) {
        self.recompute();
    }

    #[watch(data)]
    fn on_data(&mut self, _: Vec<ChartPieSlice>, _: Option<Vec<ChartPieSlice>>) {
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
}

impl PinePieChart {
    fn recompute(&mut self) {
        match PieChartGeometry::new(&self.data, &self.options()) {
            Ok(geometry) => {
                self.view_box = geometry.view_box;
                self.slices = geometry.slices;
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
            self.slices.iter().map(|slice| slice.key.as_str()),
            &self.focused_key,
            step,
        ) {
            self.focused_key = key;
        }
    }

    fn has_slice_key(&self, key: &str) -> bool {
        self.slices.iter().any(|slice| slice.key == key)
    }

    fn selection_for_slice(&self, key: &str) -> Option<ChartSelection> {
        self.slices
            .iter()
            .find(|slice| slice.key == key)
            .map(SvgPieSlice::selection)
    }

    fn clear_geometry(&mut self) {
        self.slices.clear();
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
    }

    fn reconcile_selection(&mut self) {
        if !self.has_slice_key(&self.focused_key) {
            self.focused_key.clear();
        }
        if !self.has_slice_key(&self.selected_key) {
            self.selected_key.clear();
        }
    }

    fn clear_selection(&mut self) {
        self.focused_key.clear();
        self.selected_key.clear();
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
        if value == 0.0 {
            continue;
        }
        output.push(NormalizedPieSlice {
            label: slice_label(&slice.label, index),
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

fn slice_label(label: &str, index: usize) -> String {
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
}
