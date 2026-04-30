//! Client entry: wire routes, mount the app.

use pocopine::prelude::*;

use crate::components::{AppShell, HnComment, NotFound, StoryDetail, StoryList};

#[wasm_bindgen(start)]
pub fn main() {
    pocopine::app! {
        components: [AppShell, StoryList, StoryDetail, HnComment, NotFound],
        routes: [
            ("/", StoryList),
            ("/item/:id", StoryDetail),
            ("*", NotFound),
        ],
        devtools: true,
    };
}
