//! Client entry: wire routes, mount the app.

use pocopine::prelude::*;

#[wasm_bindgen(start)]
pub fn main() {
    pocopine::app! {
        components: [
            crate::shell::AppShell,
            crate::routes::home::StoryList,
            crate::routes::story::StoryDetail,
            crate::routes::story::HnComment,
            crate::routes::not_found::NotFound,
        ],
        routes: [
            ("/", crate::routes::home::StoryList),
            ("/item/:id", crate::routes::story::StoryDetail),
            ("*", crate::routes::not_found::NotFound),
        ],
        devtools: true,
    };
}
