use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(display = "contents")]
pub struct AppDemo {
    pub count: i32,
    pub show_details: bool,
}

#[handlers]
impl AppDemo {
    pub fn increment(&mut self) {
        self.count += 1;
    }
    pub fn decrement(&mut self) {
        self.count -= 1;
    }
    pub fn reset(&mut self) {
        self.count = 0;
    }
    pub fn toggle_details(&mut self) {
        self.show_details = !self.show_details;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<AppDemo>().run();
}
