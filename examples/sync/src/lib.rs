use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub title: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct SyncBoard {
    pub posts: pocopine_sync::CollectionState<Post>,
}

#[handlers]
impl SyncBoard {
    pub fn on_mount(&mut self) {
        // The sync example is fleshed out after the core protocol compiles.
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(pocopine_sync::sync_plugin().with_live_wakeup(true))
        .register::<SyncBoard>()
        .run();
}
