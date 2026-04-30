//! Client entry: wire routes, mount the app.

use pocopine::prelude::*;

#[wasm_bindgen(start)]
pub fn main() {
    pocopine::app! {
        components: [
            crate::components::AppShell,
            crate::components::StoryList,
            crate::components::StoryDetail,
            crate::components::HnComment,
            crate::components::NotFound,
        ],
        routes: [
            ("/", crate::components::StoryList),
            ("/item/:id", crate::components::StoryDetail),
            ("*", crate::components::NotFound),
        ],
        devtools: true,
    };
}
