//! Todo example — demonstrates tag-based composition, static props,
//! reactive `pp-bind:` props into a child, default `<slot>` content,
//! and a singleton `#[store]` accessed via the `$store` magic path.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoList {
    pub title: String,
    pub current_label: String,
}

#[handlers]
impl TodoList {
    pub fn on_mount(&mut self) {
        if self.title.is_empty() {
            self.title = "Things to do".into();
        }
        if self.current_label.is_empty() {
            self.current_label = "Edit me".into();
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoItem {
    #[prop]
    pub id: i32,
    #[prop]
    pub label: String,
    #[prop]
    pub done: bool,
}

#[handlers]
impl TodoItem {
    pub fn toggle(&mut self) {
        self.done = !self.done;
    }
}

/// App-wide preferences — shared across all components via `$store.preferences`.
#[derive(Serialize, Deserialize)]
#[store]
pub struct Preferences {
    pub theme: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: "light".into(),
        }
    }
}

// An empty `#[handlers]` is required for any struct used with
// `#[component]` or `#[store]`. Populated once we need actions on the store.
#[handlers]
impl Preferences {}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<TodoList>()
        .register::<TodoItem>()
        .store::<Preferences>()
        .run();
}
