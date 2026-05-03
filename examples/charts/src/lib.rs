use pine_charts::{ChartBar, ChartPoint};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub dataset: String,
    pub points: Vec<ChartPoint>,
    pub bars: Vec<ChartBar>,
}

impl Default for ChartDemo {
    fn default() -> Self {
        Self {
            dataset: "growth".into(),
            points: growth_points(),
            bars: growth_bars(),
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_growth(&mut self) {
        self.dataset = "growth".into();
        self.points = growth_points();
        self.bars = growth_bars();
    }

    pub fn show_latency(&mut self) {
        self.dataset = "latency".into();
        self.points = latency_points();
        self.bars = latency_bars();
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

fn growth_bars() -> Vec<ChartBar> {
    vec![
        ChartBar::new("W1", 14.0),
        ChartBar::new("W2", 18.0),
        ChartBar::new("W3", 17.0),
        ChartBar::new("W4", 24.0),
        ChartBar::new("W5", 31.0),
        ChartBar::new("W6", 38.0),
        ChartBar::new("W7", 44.0),
    ]
}

fn latency_bars() -> Vec<ChartBar> {
    vec![
        ChartBar::new("P50", 58.0),
        ChartBar::new("P75", 64.0),
        ChartBar::new("P90", 78.0),
        ChartBar::new("P95", 86.0),
        ChartBar::new("P99", 92.0),
    ]
}

#[wasm_bindgen(start)]
pub fn main() {
    pine_charts::register_all();
    App::new().register::<ChartDemo>().run();
}
