use pocopine::prelude::*;
use wasm_bindgen::JsValue;

use crate::{
    KeepAuthGate, KeepBoard, KeepComposer, KeepEditor, KeepGridLayout, KeepListDetail, KeepLogin,
    KeepNoteBody, KeepNoteCard, KeepNoteForm, KeepStore,
    firebase::{KEEP_AUTH_SNAPSHOT_KEY, KEEP_FIREBASE_TOKEN_KEY},
    keep_firebase_auth_plugin,
};

fn sync_client_plugin() -> pocopine_sync_query::QueryClientPlugin {
    let store: std::rc::Rc<dyn pocopine_sync::SyncLocalStore> = if browser_cross_origin_isolated() {
        std::rc::Rc::new(pocopine_sync_sqlite::SqliteLocalStore::new())
    } else {
        tracing::info!(
            target: "pocopine.log",
            "using IndexedDB sync cache because OPFS SQLite requires cross-origin isolation"
        );
        std::rc::Rc::new(pocopine_sync_indexdb::IndexedDbLocalStore::new())
    };

    pocopine_sync_query::query_client_plugin()
        .config(pocopine_sync_query::QueryClientConfig::default().with_local_store(store))
}

fn browser_cross_origin_isolated() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crossOriginIsolated"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
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
        "logout",
        "corner-down-right",
        "layout-grid",
        "layout-list",
    ];
    // Per-instance runtime for Keep note bodies. Adds markdown-
    // style typing shortcuts and smart typography on top of the
    // default extension chain:
    //   * `# ` … `###### ` → heading
    //   * `* ` / `- ` / `+ ` → bullet list
    //   * `1. ` → ordered list
    //   * `> ` → blockquote
    //   * ``` → code block
    //   * `--` → em-dash, `...` → ellipsis, smart quotes
    //
    // Surfaces opt into it via `<pine-rich-text-root runtime="keep">`.
    // Registered before `App::run()` (after that point the
    // runtime registry seals on first resolve).
    let keep_runtime = pine_richtext::runtime::RuntimeBuilder::new()
        .name("keep")
        .with(crate::richtext_schema::KeepNoteSchemaExtension)
        .with(pine_richtext::extensions::SmartTypographyExtension)
        .with(pine_richtext::extensions::MarkdownShortcutsExtension)
        .build();
    pine_richtext::runtime::registry::register("keep", keep_runtime);

    App::new()
        .plugin(
            pocopine_auth_client::auth_plugin()
                .with_token_storage(pocopine_auth_client::storage::LocalStorage::new(
                    KEEP_FIREBASE_TOKEN_KEY,
                ))
                .with_session_snapshot(pocopine_auth_client::storage::LocalStorage::new(
                    KEEP_AUTH_SNAPSHOT_KEY,
                ))
                .wait_for_initial_auth_check(true)
                .with_cross_tab_sync(true),
        )
        .plugin(keep_firebase_auth_plugin())
        .plugin(sync_client_plugin())
        .store::<KeepStore>()
        .register::<pine::PineAvatarRoot>()
        .register::<pine::PineAvatarImage>()
        .register::<pine::PineAvatarFallback>()
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
        .register::<pine::PineTextarea>()
        .register::<pine::PinePopoverRoot>()
        .register::<pine::PinePopoverTrigger>()
        .register::<pine::PinePopoverPortal>()
        .register::<pine::PinePopoverContent>()
        .register::<KeepBoard>()
        .register::<KeepAuthGate>()
        .register::<KeepGridLayout>()
        .register::<KeepListDetail>()
        .register::<KeepLogin>()
        .register::<KeepComposer>()
        .register::<KeepNoteCard>()
        .register::<KeepEditor>()
        .register::<KeepNoteForm>()
        .register::<KeepNoteBody>()
        .register::<pine_richtext::view::PineRichTextRoot>()
        .run();
}
