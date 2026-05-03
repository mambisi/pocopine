use pine_charts::{ChartBar, ChartBarSeries, ChartPoint};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub dataset: String,
    pub bar_mode: String,
    pub points: Vec<ChartPoint>,
    pub bar_series: Vec<ChartBarSeries>,
}

impl Default for ChartDemo {
    fn default() -> Self {
        Self {
            dataset: "growth".into(),
            bar_mode: "grouped".into(),
            points: growth_points(),
            bar_series: growth_bar_series(),
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_growth(&mut self) {
        self.dataset = "growth".into();
        self.points = growth_points();
        self.bar_series = growth_bar_series();
    }

    pub fn show_latency(&mut self) {
        self.dataset = "latency".into();
        self.points = latency_points();
        self.bar_series = latency_bar_series();
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
