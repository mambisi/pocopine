use pine_charts::{
    area_legend_items, bar_legend_items, line_legend_items, ChartAreaSeries, ChartBar,
    ChartBarSeries, ChartLineSeries, ChartPoint, LegendItem,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub dataset: String,
    pub bar_mode: String,
    pub line_series: Vec<ChartLineSeries>,
    pub line_legend: Vec<LegendItem>,
    pub area_series: Vec<ChartAreaSeries>,
    pub area_legend: Vec<LegendItem>,
    pub bar_series: Vec<ChartBarSeries>,
    pub bar_legend: Vec<LegendItem>,
}

impl Default for ChartDemo {
    fn default() -> Self {
        let line_series = growth_line_series();
        let area_series = growth_area_series();
        let bar_series = growth_bar_series();
        Self {
            dataset: "growth".into(),
            bar_mode: "grouped".into(),
            line_legend: line_legend_items(&line_series),
            line_series,
            area_legend: area_legend_items(&area_series),
            area_series,
            bar_legend: bar_legend_items(&bar_series),
            bar_series,
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_growth(&mut self) {
        let line_series = growth_line_series();
        let area_series = growth_area_series();
        let bar_series = growth_bar_series();
        self.dataset = "growth".into();
        self.line_legend = line_legend_items(&line_series);
        self.line_series = line_series;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
    }

    pub fn show_latency(&mut self) {
        let line_series = latency_line_series();
        let area_series = latency_area_series();
        let bar_series = latency_bar_series();
        self.dataset = "latency".into();
        self.line_legend = line_legend_items(&line_series);
        self.line_series = line_series;
        self.area_legend = area_legend_items(&area_series);
        self.area_series = area_series;
        self.bar_legend = bar_legend_items(&bar_series);
        self.bar_series = bar_series;
    }

    pub fn show_grouped(&mut self) {
        self.bar_mode = "grouped".into();
    }

    pub fn show_stacked(&mut self) {
        self.bar_mode = "stacked".into();
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

#[wasm_bindgen(start)]
pub fn main() {
    pine_charts::register_all();
    App::new().register::<ChartDemo>().run();
}
