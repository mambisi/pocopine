use pine_charts::{
    area_legend_items, bar_legend_items, line_legend_items, scatter_legend_items,
    set_area_series_visible, set_bar_series_visible, set_pie_slice_visible,
    set_scatter_series_visible, ChartAreaSeries, ChartBar, ChartBarSeries, ChartHover,
    ChartLayerPoint, ChartLineSeries, ChartPieSlice, ChartPoint, ChartScatterSeries,
    ChartSelection, LegendItem, LegendToggle,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub combo_dataset: String,
    pub area_dataset: String,
    pub bar_dataset: String,
    pub scatter_dataset: String,
    pub pie_dataset: String,
    pub bar_mode: String,
    pub pie_shape: String,
    pub line_series: Vec<ChartLineSeries>,
    pub line_legend: Vec<LegendItem>,
    pub combo_bar_label: String,
    pub combo_bar_color: String,
    pub combo_bar_data: Vec<ChartBar>,
    pub combo_line_label: String,
    pub combo_line_color: String,
    pub combo_line_data: Vec<ChartBar>,
    pub combo_area_points: Vec<ChartPoint>,
    pub combo_scatter_points: Vec<ChartPoint>,
    pub combo_goal: Option<f64>,
    pub combo_latest_x: f64,
    pub combo_latest_y: f64,
    pub metro_line_a: Vec<ChartLayerPoint>,
    pub metro_line_b: Vec<ChartLayerPoint>,
    pub metro_line_c: Vec<ChartLayerPoint>,
    pub area_forecast_visible: bool,
    pub area_series: Vec<ChartAreaSeries>,
    pub area_legend: Vec<LegendItem>,
    pub bar_series: Vec<ChartBarSeries>,
    pub bar_legend: Vec<LegendItem>,
    pub scatter_series: Vec<ChartScatterSeries>,
    pub scatter_legend: Vec<LegendItem>,
    pub pie_data: Vec<ChartPieSlice>,
    pub pie_legend: Vec<LegendItem>,
    pub pie_inner_radius: f64,
    pub pie_start_angle: f64,
    pub pie_end_angle: f64,
    pub pie_center_label: String,
    pub pie_center_value: String,
    pub custom_tooltip_visible: bool,
    pub custom_tooltip_title: String,
    pub custom_tooltip_value: String,
    pub custom_tooltip_meta: String,
    pub custom_tooltip_x: String,
    pub custom_tooltip_y: String,
    pub custom_tooltip_style: String,
    pub selection_visible: bool,
    pub selection_title: String,
    pub selection_value: String,
    pub selection_meta: String,
}

impl Default for ChartDemo {
    fn default() -> Self {
        let line_series = growth_line_series();
        let area_series = growth_area_series();
        let bar_series = growth_bar_series();
        let scatter_series = growth_scatter_series();
        let pie_data = growth_pie_data();
        let pie_center_value = pie_total_label(&pie_data);
        Self {
            combo_dataset: "growth".into(),
            area_dataset: "growth".into(),
            bar_dataset: "growth".into(),
            scatter_dataset: "growth".into(),
            pie_dataset: "growth".into(),
            bar_mode: "grouped".into(),
            pie_shape: "pie".into(),
            line_legend: line_legend_items(&line_series),
            combo_bar_label: line_series[0].label.clone(),
            combo_bar_color: "#16a085".into(),
            combo_bar_data: bars_from_points(&line_series[0].data),
            combo_line_label: line_series[1].label.clone(),
            combo_line_color: "#d96c2c".into(),
            combo_line_data: bars_from_points(&line_series[1].data),
            combo_area_points: combo_area_points(&line_series[0].data),
            combo_scatter_points: combo_scatter_points(&line_series[0].data),
            combo_goal: combo_goal(&line_series[1].data),
            combo_latest_x: latest_x(&line_series[0].data),
            combo_latest_y: latest_y(&line_series[0].data),
            line_series,
            metro_line_a: metro_line_a(),
            metro_line_b: metro_line_b(),
            metro_line_c: metro_line_c(),
            area_forecast_visible: false,
            area_legend: area_legend_items(&area_series),
            area_series,
            bar_legend: bar_legend_items(&bar_series),
            bar_series,
            scatter_legend: scatter_legend_items(&scatter_series),
            scatter_series,
            pie_legend: pine_charts::pie_legend_items(&pie_data),
            pie_data,
            pie_inner_radius: 0.0,
            pie_start_angle: -90.0,
            pie_end_angle: 270.0,
            pie_center_label: "Total".into(),
            pie_center_value,
            custom_tooltip_visible: false,
            custom_tooltip_title: "Custom tooltip".into(),
            custom_tooltip_value: "Hover the trend area".into(),
            custom_tooltip_meta: "Built with pp:chart:hover".into(),
            custom_tooltip_x: "right".into(),
            custom_tooltip_y: "above".into(),
            custom_tooltip_style: String::new(),
            selection_visible: false,
            selection_title: "Select a point".into(),
            selection_value: "Click a marker or use keyboard selection.".into(),
            selection_meta: "Selection emits pp:chart:select".into(),
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_combo_growth(&mut self) {
        let line_series = growth_line_series();
        self.combo_dataset = "growth".into();
        self.line_legend = line_legend_items(&line_series);
        self.update_combo_chart(&line_series, "#16a085", "#d96c2c");
        self.line_series = line_series;
    }

    pub fn show_combo_latency(&mut self) {
        let line_series = latency_line_series();
        self.combo_dataset = "latency".into();
        self.line_legend = line_legend_items(&line_series);
        self.update_combo_chart(&line_series, "#16a085", "#d96c2c");
        self.line_series = line_series;
    }

    pub fn show_scatter_growth(&mut self) {
        let scatter_series = growth_scatter_series();
        self.scatter_dataset = "growth".into();
        self.scatter_legend = scatter_legend_items(&scatter_series);
        self.scatter_series = scatter_series;
    }

    pub fn show_scatter_latency(&mut self) {
        let scatter_series = latency_scatter_series();
        self.scatter_dataset = "latency".into();
        self.scatter_legend = scatter_legend_items(&scatter_series);
        self.scatter_series = scatter_series;
    }

    pub fn show_area_growth(&mut self) {
        let area_series = growth_area_series();
        self.area_dataset = "growth".into();
        self.area_forecast_visible = false;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
    }

    pub fn show_area_latency(&mut self) {
        let area_series = latency_area_series();
        self.area_dataset = "latency".into();
        self.area_forecast_visible = false;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
    }

    pub fn add_area_forecast(&mut self) {
        if self.area_forecast_visible {
            return;
        }
        self.area_series
            .push(area_forecast_series(&self.area_dataset));
        self.area_forecast_visible = true;
        self.area_legend = area_legend_items(&self.area_series);
    }

    pub fn remove_area_forecast(&mut self) {
        if !self.area_forecast_visible {
            return;
        }
        self.area_series.retain(|series| series.label != "Forecast");
        self.area_forecast_visible = false;
        self.area_legend = area_legend_items(&self.area_series);
    }

    pub fn show_bar_growth(&mut self) {
        let bar_series = growth_bar_series();
        self.bar_dataset = "growth".into();
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
    }

    pub fn show_bar_latency(&mut self) {
        let bar_series = latency_bar_series();
        self.bar_dataset = "latency".into();
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
    }

    pub fn show_pie_growth(&mut self) {
        let pie_data = growth_pie_data();
        self.pie_dataset = "growth".into();
        self.pie_legend = pine_charts::pie_legend_items(&pie_data);
        self.pie_data = pie_data;
        self.update_pie_center();
    }

    pub fn show_pie_latency(&mut self) {
        let pie_data = latency_pie_data();
        self.pie_dataset = "latency".into();
        self.pie_legend = pine_charts::pie_legend_items(&pie_data);
        self.pie_data = pie_data;
        self.update_pie_center();
    }

    pub fn show_grouped(&mut self) {
        self.bar_mode = "grouped".into();
    }

    pub fn show_stacked(&mut self) {
        self.bar_mode = "stacked".into();
    }

    pub fn show_pie(&mut self) {
        self.pie_shape = "pie".into();
        self.pie_inner_radius = 0.0;
        self.pie_start_angle = -90.0;
        self.pie_end_angle = 270.0;
        self.update_pie_center();
    }

    pub fn show_donut(&mut self) {
        self.pie_shape = "donut".into();
        self.pie_inner_radius = 0.58;
        self.pie_start_angle = -90.0;
        self.pie_end_angle = 270.0;
        self.update_pie_center();
    }

    pub fn show_half_donut(&mut self) {
        self.pie_shape = "half-donut".into();
        self.pie_inner_radius = 0.58;
        self.pie_start_angle = 180.0;
        self.pie_end_angle = 360.0;
        self.update_pie_center();
    }

    pub fn toggle_scatter_series(&mut self, event: JsValue) {
        let Some(toggle) = LegendToggle::from_event_value(event) else {
            return;
        };
        if set_scatter_series_visible(&mut self.scatter_series, &toggle.key, toggle.active) {
            self.scatter_legend = scatter_legend_items(&self.scatter_series);
        }
    }

    pub fn toggle_area_series(&mut self, event: JsValue) {
        let Some(toggle) = LegendToggle::from_event_value(event) else {
            return;
        };
        if set_area_series_visible(&mut self.area_series, &toggle.key, toggle.active) {
            self.area_legend = area_legend_items(&self.area_series);
        }
    }

    pub fn toggle_bar_series(&mut self, event: JsValue) {
        let Some(toggle) = LegendToggle::from_event_value(event) else {
            return;
        };
        if set_bar_series_visible(&mut self.bar_series, &toggle.key, toggle.active) {
            self.bar_legend = bar_legend_items(&self.bar_series);
        }
    }

    pub fn toggle_pie_slice(&mut self, event: JsValue) {
        let Some(toggle) = LegendToggle::from_event_value(event) else {
            return;
        };
        if set_pie_slice_visible(&mut self.pie_data, &toggle.key, toggle.active) {
            self.pie_legend = pine_charts::pie_legend_items(&self.pie_data);
            self.update_pie_center();
        }
    }

    pub fn show_custom_tooltip(&mut self, event: JsValue) {
        let Some(hover) = ChartHover::from_event_value(event) else {
            return;
        };
        self.custom_tooltip_visible = true;
        self.custom_tooltip_title = if hover.series.is_empty() {
            hover.chart
        } else {
            hover.series
        };
        self.custom_tooltip_value = format!("Week {}: {}", hover.x_label, hover.y_label);
        self.custom_tooltip_meta = hover.aria_label;
        self.custom_tooltip_x = hover.tooltip_x;
        self.custom_tooltip_y = hover.tooltip_y;
        self.custom_tooltip_style = hover.tooltip_style;
    }

    pub fn hide_custom_tooltip(&mut self) {
        self.custom_tooltip_visible = false;
    }

    pub fn show_selection_detail(&mut self, event: JsValue) {
        let Some(selection) = ChartSelection::from_event_value(event) else {
            return;
        };
        self.selection_visible = true;
        self.selection_title = if selection.series.is_empty() {
            selection.chart
        } else {
            selection.series
        };
        self.selection_value = match selection.kind.as_str() {
            "xy" => format!("{}: {}", selection.x_label, selection.y_label),
            "category" => format!("{}: {}", selection.category, selection.value_label),
            "share" => format!("{} · {}", selection.value_label, selection.percentage_label),
            _ => selection.label,
        };
        self.selection_meta = format!("Selected {}", selection.key);
    }

    pub fn hide_selection_detail(&mut self) {
        self.selection_visible = false;
    }
}

impl ChartDemo {
    fn update_combo_chart(
        &mut self,
        series: &[ChartLineSeries],
        bar_color: &str,
        line_color: &str,
    ) {
        if let Some(primary) = series.first() {
            self.combo_bar_label = primary.label.clone();
            self.combo_bar_data = bars_from_points(&primary.data);
            self.combo_bar_color = bar_color.into();
        }
        if let Some(secondary) = series.get(1) {
            self.combo_line_label = secondary.label.clone();
            self.combo_line_data = bars_from_points(&secondary.data);
            self.combo_line_color = line_color.into();
        }
        if let Some(primary) = series.first() {
            self.combo_area_points = combo_area_points(&primary.data);
            self.combo_scatter_points = combo_scatter_points(&primary.data);
            self.combo_latest_x = latest_x(&primary.data);
            self.combo_latest_y = latest_y(&primary.data);
        }
        if let Some(secondary) = series.get(1) {
            self.combo_goal = combo_goal(&secondary.data);
        }
    }

    fn update_pie_center(&mut self) {
        if self.pie_shape == "half-donut" {
            self.pie_center_label = "Progress".into();
            self.pie_center_value = if self.pie_dataset == "growth" {
                "74%".into()
            } else {
                "62%".into()
            };
        } else {
            self.pie_center_label = "Total".into();
            self.pie_center_value = pie_total_label(&self.pie_data);
        }
    }
}

fn bars_from_points(points: &[ChartPoint]) -> Vec<ChartBar> {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| ChartBar::new(format!("W{}", index + 1), point.y))
        .collect()
}

fn combo_area_points(points: &[ChartPoint]) -> Vec<ChartPoint> {
    points
        .iter()
        .map(|point| ChartPoint::new(point.x, point.y * 0.82))
        .collect()
}

fn combo_scatter_points(points: &[ChartPoint]) -> Vec<ChartPoint> {
    points
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 3 == 1)
        .map(|(_, point)| ChartPoint::new(point.x, point.y * 1.05))
        .collect()
}

fn combo_goal(points: &[ChartPoint]) -> Option<f64> {
    points.last().map(|point| point.y)
}

fn latest_x(points: &[ChartPoint]) -> f64 {
    points.last().map(|point| point.x).unwrap_or_default()
}

fn latest_y(points: &[ChartPoint]) -> f64 {
    points.last().map(|point| point.y).unwrap_or_default()
}

fn growth_points() -> Vec<ChartPoint> {
    vec![
        ChartPoint::new(0.0, 14.0),
        ChartPoint::new(1.0, 18.0),
        ChartPoint::new(2.0, 17.0),
        ChartPoint::new(3.0, 24.0),
        ChartPoint::new(4.0, 31.0),
        ChartPoint::new(5.0, 38.0),
        ChartPoint::new(6.0, 44.0),
    ]
}

fn latency_points() -> Vec<ChartPoint> {
    vec![
        ChartPoint::new(0.0, 92.0),
        ChartPoint::new(1.0, 86.0),
        ChartPoint::new(2.0, 78.0),
        ChartPoint::new(3.0, 82.0),
        ChartPoint::new(4.0, 69.0),
        ChartPoint::new(5.0, 64.0),
        ChartPoint::new(6.0, 58.0),
    ]
}

fn growth_line_series() -> Vec<ChartLineSeries> {
    vec![
        ChartLineSeries::new("Actual", growth_points()),
        ChartLineSeries::new(
            "Target",
            vec![
                ChartPoint::new(0.0, 12.0),
                ChartPoint::new(1.0, 15.0),
                ChartPoint::new(2.0, 19.0),
                ChartPoint::new(3.0, 24.0),
                ChartPoint::new(4.0, 30.0),
                ChartPoint::new(5.0, 37.0),
                ChartPoint::new(6.0, 45.0),
            ],
        ),
    ]
}

fn latency_line_series() -> Vec<ChartLineSeries> {
    vec![
        ChartLineSeries::new("API", latency_points()),
        ChartLineSeries::new(
            "Render",
            vec![
                ChartPoint::new(0.0, 64.0),
                ChartPoint::new(1.0, 59.0),
                ChartPoint::new(2.0, 57.0),
                ChartPoint::new(3.0, 52.0),
                ChartPoint::new(4.0, 48.0),
                ChartPoint::new(5.0, 46.0),
                ChartPoint::new(6.0, 42.0),
            ],
        ),
    ]
}

fn metro_line_a() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(100.0, 120.0),
        ChartLayerPoint::new(220.0, 120.0),
        ChartLayerPoint::new(340.0, 120.0),
        ChartLayerPoint::new(420.0, 180.0),
        ChartLayerPoint::new(480.0, 300.0),
        ChartLayerPoint::new(620.0, 360.0),
        ChartLayerPoint::new(780.0, 360.0),
    ]
}

fn metro_line_b() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(80.0, 240.0),
        ChartLayerPoint::new(220.0, 240.0),
        ChartLayerPoint::new(420.0, 180.0),
        ChartLayerPoint::new(520.0, 180.0),
        ChartLayerPoint::new(700.0, 240.0),
        ChartLayerPoint::new(840.0, 240.0),
    ]
}

fn metro_line_c() -> Vec<ChartLayerPoint> {
    vec![
        ChartLayerPoint::new(140.0, 360.0),
        ChartLayerPoint::new(300.0, 360.0),
        ChartLayerPoint::new(480.0, 300.0),
        ChartLayerPoint::new(520.0, 180.0),
        ChartLayerPoint::new(620.0, 120.0),
        ChartLayerPoint::new(760.0, 120.0),
    ]
}

fn growth_scatter_series() -> Vec<ChartScatterSeries> {
    vec![
        ChartScatterSeries::new(
            "Segment A",
            vec![
                ChartPoint::new(12.0, 42.0),
                ChartPoint::new(18.0, 49.0),
                ChartPoint::new(26.0, 57.0),
                ChartPoint::new(32.0, 61.0),
                ChartPoint::new(41.0, 68.0),
            ],
        ),
        ChartScatterSeries::new(
            "Segment B",
            vec![
                ChartPoint::new(10.0, 35.0),
                ChartPoint::new(16.0, 39.0),
                ChartPoint::new(22.0, 44.0),
                ChartPoint::new(31.0, 47.0),
                ChartPoint::new(38.0, 52.0),
            ],
        ),
    ]
}

fn latency_scatter_series() -> Vec<ChartScatterSeries> {
    vec![
        ChartScatterSeries::new(
            "API",
            vec![
                ChartPoint::new(20.0, 31.0),
                ChartPoint::new(45.0, 43.0),
                ChartPoint::new(60.0, 55.0),
                ChartPoint::new(82.0, 68.0),
                ChartPoint::new(100.0, 81.0),
            ],
        ),
        ChartScatterSeries::new(
            "Render",
            vec![
                ChartPoint::new(18.0, 24.0),
                ChartPoint::new(38.0, 30.0),
                ChartPoint::new(56.0, 34.0),
                ChartPoint::new(79.0, 43.0),
                ChartPoint::new(94.0, 49.0),
            ],
        ),
    ]
}

fn growth_area_series() -> Vec<ChartAreaSeries> {
    vec![
        ChartAreaSeries::new(
            "Organic",
            vec![
                ChartPoint::new(0.0, 4.0),
                ChartPoint::new(1.0, 7.0),
                ChartPoint::new(2.0, 9.0),
                ChartPoint::new(3.0, 13.0),
                ChartPoint::new(4.0, 18.0),
                ChartPoint::new(5.0, 25.0),
                ChartPoint::new(6.0, 31.0),
            ],
        ),
        ChartAreaSeries::new(
            "Referral",
            vec![
                ChartPoint::new(0.0, 3.0),
                ChartPoint::new(1.0, 4.0),
                ChartPoint::new(2.0, 6.0),
                ChartPoint::new(3.0, 9.0),
                ChartPoint::new(4.0, 11.0),
                ChartPoint::new(5.0, 14.0),
                ChartPoint::new(6.0, 16.0),
            ],
        ),
    ]
}

fn latency_area_series() -> Vec<ChartAreaSeries> {
    vec![
        ChartAreaSeries::new(
            "API",
            vec![
                ChartPoint::new(0.0, 88.0),
                ChartPoint::new(1.0, 83.0),
                ChartPoint::new(2.0, 76.0),
                ChartPoint::new(3.0, 72.0),
                ChartPoint::new(4.0, 68.0),
                ChartPoint::new(5.0, 61.0),
                ChartPoint::new(6.0, 54.0),
            ],
        ),
        ChartAreaSeries::new(
            "Render",
            vec![
                ChartPoint::new(0.0, 62.0),
                ChartPoint::new(1.0, 56.0),
                ChartPoint::new(2.0, 49.0),
                ChartPoint::new(3.0, 44.0),
                ChartPoint::new(4.0, 41.0),
                ChartPoint::new(5.0, 38.0),
                ChartPoint::new(6.0, 35.0),
            ],
        ),
    ]
}

fn area_forecast_series(dataset: &str) -> ChartAreaSeries {
    if dataset == "latency" {
        ChartAreaSeries::new(
            "Forecast",
            vec![
                ChartPoint::new(0.0, 90.0),
                ChartPoint::new(1.0, 82.0),
                ChartPoint::new(2.0, 74.0),
                ChartPoint::new(3.0, 67.0),
                ChartPoint::new(4.0, 61.0),
                ChartPoint::new(5.0, 55.0),
                ChartPoint::new(6.0, 49.0),
            ],
        )
    } else {
        ChartAreaSeries::new(
            "Forecast",
            vec![
                ChartPoint::new(0.0, 5.0),
                ChartPoint::new(1.0, 8.0),
                ChartPoint::new(2.0, 12.0),
                ChartPoint::new(3.0, 17.0),
                ChartPoint::new(4.0, 23.0),
                ChartPoint::new(5.0, 30.0),
                ChartPoint::new(6.0, 38.0),
            ],
        )
    }
}

fn growth_bar_series() -> Vec<ChartBarSeries> {
    vec![
        ChartBarSeries::new(
            "Organic",
            vec![
                ChartBar::new("W1", 9.0),
                ChartBar::new("W2", 11.0),
                ChartBar::new("W3", 12.0),
                ChartBar::new("W4", 16.0),
                ChartBar::new("W5", 19.0),
                ChartBar::new("W6", 23.0),
                ChartBar::new("W7", 27.0),
            ],
        ),
        ChartBarSeries::new(
            "Referral",
            vec![
                ChartBar::new("W1", 5.0),
                ChartBar::new("W2", 7.0),
                ChartBar::new("W3", 5.0),
                ChartBar::new("W4", 8.0),
                ChartBar::new("W5", 12.0),
                ChartBar::new("W6", 15.0),
                ChartBar::new("W7", 17.0),
            ],
        ),
    ]
}

fn latency_bar_series() -> Vec<ChartBarSeries> {
    vec![
        ChartBarSeries::new(
            "API",
            vec![
                ChartBar::new("P50", 31.0),
                ChartBar::new("P75", 36.0),
                ChartBar::new("P90", 47.0),
                ChartBar::new("P95", 51.0),
                ChartBar::new("P99", 62.0),
            ],
        ),
        ChartBarSeries::new(
            "Render",
            vec![
                ChartBar::new("P50", 27.0),
                ChartBar::new("P75", 28.0),
                ChartBar::new("P90", 31.0),
                ChartBar::new("P95", 35.0),
                ChartBar::new("P99", 30.0),
            ],
        ),
    ]
}

fn growth_pie_data() -> Vec<ChartPieSlice> {
    vec![
        ChartPieSlice::new("Organic", 42.0),
        ChartPieSlice::new("Referral", 24.0),
        ChartPieSlice::new("Paid", 18.0),
        ChartPieSlice::new("Partner", 10.0),
    ]
}

fn latency_pie_data() -> Vec<ChartPieSlice> {
    vec![
        ChartPieSlice::new("API", 38.0),
        ChartPieSlice::new("Render", 29.0),
        ChartPieSlice::new("Network", 21.0),
        ChartPieSlice::new("Idle", 12.0),
    ]
}

fn pie_total_label(data: &[ChartPieSlice]) -> String {
    data.iter()
        .filter(|slice| slice.visible)
        .map(|slice| slice.value)
        .sum::<f64>()
        .round()
        .to_string()
}

#[wasm_bindgen(start)]
pub fn main() {
    pine_charts::register_all();
    App::new().register::<ChartDemo>().run();
}
