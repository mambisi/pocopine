use pocopine::prelude::*;

use crate::{
    KeepBoard, KeepComposer, KeepEditor, KeepGridLayout, KeepListDetail, KeepNoteBody,
    KeepNoteCard, KeepNoteForm, KeepStore,
};

fn sync_client_plugin() -> pocopine_sync::SyncClientPlugin {
    // Durable browser-side cache via OPFS-backed SQLite is the whole
    // point of the keep example. No memory-store fallback — if the
    // SQLite/OPFS path can't initialize, sync will surface the error
    // rather than silently degrade to a non-durable store.
    pocopine_sync::sync_plugin()
        .with_live_wakeup(true)
        .local_store(pocopine_sync_sqlite::SqliteLocalStore::new())
}

#[wasm_bindgen(start)]
pub fn main() {
    pine_icons::register_icons![
        "menu-2",
        "search",
        "x",
        "refresh",
        "settings",
        "moon",
        "sun",
        "tag",
        "archive",
        "copy",
        "dots-vertical",
        "pin",
        filled / "pin",
        "bulb",
        filled / "bulb",
        filled / "christmas-tree",
        "palette",
        "plus",
        "check",
        "list-check",
        "notes",
        "pencil",
        "photo",
        "trash",
        "bell",
        "user",
        "corner-down-right",
        "layout-grid",
        "layout-list",
    ];
    App::new()
        .plugin(sync_client_plugin())
        .store::<KeepStore>()
        .register::<pine_icons::PineIcon>()
        .register::<pine::PineCommandRoot>()
        .register::<pine::PineCommandPortal>()
        .register::<pine::PineCommandOverlay>()
        .register::<pine::PineCommandContent>()
        .register::<pine::PineCommandInput>()
        .register::<pine::PineCommandList>()
        .register::<pine::PineCommandItem>()
        .register::<pine::PineCommandEmpty>()
        .register::<pine::PineDropdownMenuRoot>()
        .register::<pine::PineDropdownMenuTrigger>()
        .register::<pine::PineDropdownMenuPortal>()
        .register::<pine::PineDropdownMenuContent>()
        .register::<pine::PineDropdownMenuItem>()
        .register::<pine::PineDropdownMenuSeparator>()
        .register::<pine::PineDropdownMenuLabel>()
        .register::<pine::PineInput>()
        .register::<pine::PinePopoverRoot>()
        .register::<pine::PinePopoverTrigger>()
        .register::<pine::PinePopoverPortal>()
        .register::<pine::PinePopoverContent>()
        .register::<KeepBoard>()
        .register::<KeepGridLayout>()
        .register::<KeepListDetail>()
        .register::<KeepComposer>()
        .register::<KeepNoteCard>()
        .register::<KeepEditor>()
        .register::<KeepNoteForm>()
        .register::<KeepNoteBody>()
        .run();
}
