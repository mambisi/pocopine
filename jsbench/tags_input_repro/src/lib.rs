//! Real-browser reproducer for the tags-input pp-for delete bug.
//!
//! Mirrors the website-demo shape — `pine-tags-input-root` with
//! a `pp-for` of items each carrying a `pine-tags-input-item-delete`
//! `×` button. Driven from `repro.py` via Playwright.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::wasm_bindgen;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TagsReproApp.poco")]
struct TagsReproApp {
    tags: Vec<String>,
}

#[handlers]
impl TagsReproApp {
    pub fn on_setup(&mut self) {
        // Seed three tags so the page renders without manual
        // input — the Playwright test clicks delete buttons
        // directly. No Enter-key dance needed.
        self.tags = vec!["alpha".into(), "beta".into(), "gamma".into()];
    }
    pub fn seed_500(&mut self) {
        self.tags = (0..500).map(|i| format!("t{i}")).collect();
    }
    pub fn add_one(&mut self) {
        let n = self.tags.len();
        self.tags.push(format!("added{n}"));
    }
    pub fn shuffle(&mut self) {
        if self.tags.len() < 2 {
            return;
        }
        let head = self.tags.remove(0);
        self.tags.push(head);
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    pine::register_all();
    pocopine::App::new().register::<TagsReproApp>().run();
}
