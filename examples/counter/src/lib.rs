use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
    pub label: String,
}

#[handlers]
impl Counter {
    pub fn init(&mut self) {
        if self.label.is_empty() {
            self.label = "clicks".into();
        }
    }

    pub fn increment(&mut self) {
        self.count += 1;
    }

    pub fn decrement(&mut self) {
        self.count -= 1;
    }

    pub fn reset(&mut self) {
        self.count = 0;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    Counter::register();
    pocopine::run();
}
