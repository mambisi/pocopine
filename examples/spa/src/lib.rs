//! SPA example — client-side router demo.
//!
//! Flat pages live at `/`, `/about`, and `/blog/:id`. `/admin` is a nested
//! route whose layout owns a second `<pp-outlet>` and remains mounted while its
//! overview/settings children change. `AppShell` owns the root outlet.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct Home {}

#[handlers]
impl Home {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct About {}

#[handlers]
impl About {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
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

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct AdminLayout {
    pub visits: u32,
}

#[handlers]
impl AdminLayout {
    pub fn bump(&mut self) {
        self.visits += 1;
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct AdminOverview {}

#[handlers]
impl AdminOverview {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct AdminSettings {}

#[handlers]
impl AdminSettings {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .route::<Home>("/")
        .route::<About>("/about")
        .route::<BlogPost>("/blog/:id")
        .layout::<AdminLayout>("/admin", |admin| {
            admin.index::<AdminOverview>();
            admin.route::<AdminSettings>("settings");
        })
        .route::<NotFound>("*")
        .run();
}
