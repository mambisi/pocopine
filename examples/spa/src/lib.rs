//! SPA example — client-side router demo.
//!
//! Four pages: `/`, `/about`, `/blog/:id`, and a `*` fallback.
//! `AppShell` owns the nav + `<pp-outlet>`; the router paints the
//! matched page into the outlet on every URL change.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Home {}

#[handlers]
impl Home {}

impl RouteComponent for Home {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct About {}

#[handlers]
impl About {}

impl RouteComponent for About {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct BlogPost {
    #[prop]
    pub id: u32,
    pub body: String,
}

#[handlers]
impl BlogPost {
    pub fn on_mount(&mut self) {
        // Stand-in for a data fetch — mirrors the `id` path param into
        // a user-visible string. A real app would `dispatch!` a
        // `#[server]` call here.
        self.body = format!("This is post #{}.", self.id);
    }
}

impl RouteComponent for BlogPost {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

impl RouteComponent for NotFound {}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .route::<Home>("/")
        .route::<About>("/about")
        .route::<BlogPost>("/blog/:id")
        .route::<NotFound>("*")
        .run();
}
