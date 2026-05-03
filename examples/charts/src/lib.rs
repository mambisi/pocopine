use pine_charts::ChartPoint;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "ChartDemo.poco")]
pub struct ChartDemo {
    pub dataset: String,
    pub points: Vec<ChartPoint>,
}

impl Default for ChartDemo {
    fn default() -> Self {
        Self {
            dataset: "growth".into(),
            points: growth_points(),
        }
    }
}

#[handlers]
impl ChartDemo {
    pub fn show_growth(&mut self) {
        self.dataset = "growth".into();
        self.points = growth_points();
    }

    pub fn show_latency(&mut self) {
        self.dataset = "latency".into();
        self.points = latency_points();
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

#[wasm_bindgen(start)]
pub fn main() {
    pine_charts::register_all();
    App::new().register::<ChartDemo>().run();
}
