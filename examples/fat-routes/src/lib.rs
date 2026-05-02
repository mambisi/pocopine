//! Split-size fixture with intentionally route-heavy code.
//!
//! This example is not a UI showcase. It exists to answer one question:
//! when route-private code grows, does `pocopine build --split --strict`
//! keep shell size flat and move those bytes into route chunks?

use pocopine::prelude::*;

pub mod routes;
pub mod shared;
pub mod shell;

#[wasm_bindgen(start)]
pub fn main() {
    pocopine::app! {
        components: [
            crate::shell::AppShell,
            crate::routes::charts::Charts,
            crate::routes::editor::Editor,
            crate::routes::reports::Reports,
        ],
        routes: [
            ("/", crate::routes::charts::Charts),
            ("/editor", crate::routes::editor::Editor),
            ("/reports", crate::routes::reports::Reports),
        ],
    };
}
