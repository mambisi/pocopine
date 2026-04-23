use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    #[prop]
    pub count: i32,
    #[prop]
    pub label: String,
}

#[handlers]
impl Counter {
    pub fn on_mount(&mut self) {
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
    App::new().register::<Counter>().run();
}
