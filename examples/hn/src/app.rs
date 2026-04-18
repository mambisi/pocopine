//! Client entry: wire routes, kick off the walker.

use pocopine::prelude::*;

use crate::components::{AppShell, NotFound, StoryDetail, StoryList};

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .route::<StoryList>("/")
        .route::<StoryDetail>("/item/:id")
        .route::<NotFound>("*")
        .run();
}
