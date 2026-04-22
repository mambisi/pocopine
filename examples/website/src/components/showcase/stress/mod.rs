use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StressDemo.poco", role = "panel")]
pub struct StressDemo {
    pub open: bool,
    pub items: Vec<String>,
    pub last_shuffle_ms: f64,
}

#[handlers]
impl StressDemo {
    pub fn mount_fixture(&mut self) {
        if !self.open {
            self.items = (0..500).map(|i| format!("item-{i:03}")).collect();
            self.open = true;
        } else {
            self.items.clear();
            self.open = false;
        }
    }
    pub fn shuffle(&mut self) {
        let start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or(0.0);
        if self.items.len() >= 2 {
            let head = self.items.remove(0);
            self.items.push(head);
        }
        let end = web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or(0.0);
        self.last_shuffle_ms = end - start;
    }
}
