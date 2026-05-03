use pine_charts::{
    area_legend_items, bar_legend_items, line_legend_items, scatter_legend_items, ChartAreaSeries,
    ChartBar, ChartBarSeries, ChartLayerPoint, ChartLineSeries, ChartPieSlice, ChartPoint,
    ChartScatterSeries, LegendItem,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub dataset: String,
    pub bar_mode: String,
    pub pie_shape: String,
    pub line_series: Vec<ChartLineSeries>,
    pub line_legend: Vec<LegendItem>,
    pub metro_line_a: Vec<ChartLayerPoint>,
    pub metro_line_b: Vec<ChartLayerPoint>,
    pub metro_line_c: Vec<ChartLayerPoint>,
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
            dataset: "growth".into(),
            bar_mode: "grouped".into(),
            pie_shape: "pie".into(),
            line_legend: line_legend_items(&line_series),
            line_series,
            metro_line_a: metro_line_a(),
            metro_line_b: metro_line_b(),
            metro_line_c: metro_line_c(),
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
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_growth(&mut self) {
        let line_series = growth_line_series();
        let area_series = growth_area_series();
        let bar_series = growth_bar_series();
        let scatter_series = growth_scatter_series();
        let pie_data = growth_pie_data();
        self.dataset = "growth".into();
        self.line_legend = line_legend_items(&line_series);
        self.line_series = line_series;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
        self.scatter_legend = scatter_legend_items(&scatter_series);
        self.scatter_series = scatter_series;
        self.pie_legend = pine_charts::pie_legend_items(&pie_data);
        self.pie_data = pie_data;
        self.update_pie_center();
    }

    pub fn show_latency(&mut self) {
        let line_series = latency_line_series();
        let area_series = latency_area_series();
        let bar_series = latency_bar_series();
        let scatter_series = latency_scatter_series();
        let pie_data = latency_pie_data();
        self.dataset = "latency".into();
        self.line_legend = line_legend_items(&line_series);
        self.line_series = line_series;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
        self.scatter_legend = scatter_legend_items(&scatter_series);
        self.scatter_series = scatter_series;
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
}

impl ChartDemo {
    fn update_pie_center(&mut self) {
        if self.pie_shape == "half-donut" {
            self.pie_center_label = "Progress".into();
            self.pie_center_value = if self.dataset == "growth" {
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
