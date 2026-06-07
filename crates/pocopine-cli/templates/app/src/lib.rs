use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Root component — the welcome page. Composes a hero, a live counter demo,
/// and a grid of `<welcome-item>` cards.
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct WelcomeApp {}

#[handlers]
impl WelcomeApp {}

/// A card with a `title` prop and a default `<slot>` for its body — shows
/// component composition + slots.
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct WelcomeItem {
    #[prop]
    pub title: String,
}

#[handlers]
impl WelcomeItem {}

/// A small interactive counter — reactive state, `@event` handlers, and
/// two-way `pp-model` binding. Props can be seeded from attributes.
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
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<WelcomeApp>()
        .register::<WelcomeItem>()
        .register::<Counter>()
        .run();
}
