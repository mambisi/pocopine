//! Client entry: wire routes, kick off the walker.

use pocopine::prelude::*;

use crate::components::{AppShell, HnComment, NotFound, StoryDetail, StoryList};

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .register::<HnComment>()
        .route::<StoryList>("/")
        .route::<StoryDetail>("/item/:id")
        .route::<NotFound>("*")
        .with_devtools()
        .run();
}
