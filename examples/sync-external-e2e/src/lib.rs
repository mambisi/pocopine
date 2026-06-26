//! Minimal browser harness for `QueryClient::apply_external_changes`.
//!
//! `start()` builds a `QueryClient` over the real `IndexedDbLocalStore` and
//! observes `notes` for board `b1`. `inject(id, body)` feeds a row in as if it
//! arrived over a realtime transport (no server, no mutation). `view_rows()`
//! returns the live view's rows as JSON. The Playwright spec drives these to
//! prove the delta routes into the view AND survives a reload (served from the
//! durable IndexedDB cache).

use std::cell::RefCell;
use std::rc::Rc;

use pocopine_sync::SyncStreamName;
use pocopine_sync_query::{QueryClient, QueryView, RowChange};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[query_resource(name = "notes", schema_version = 1)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    #[query_param(required)]
    pub board: String,
    pub body: String,
}

const BOARD: &str = "b1";

thread_local! {
    static CLIENT: RefCell<Option<Rc<QueryClient>>> = const { RefCell::new(None) };
    static VIEW: RefCell<Option<QueryView<Note>>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
pub fn start() {
    let store: Rc<dyn pocopine_sync::SyncLocalStore> = Rc::new(
        pocopine_sync_indexdb::IndexedDbLocalStore::with_database_name("sync_external_e2e")
            .unwrap_or_else(|_| pocopine_sync_indexdb::IndexedDbLocalStore::new()),
    );
    // `disable_live`: no SSE; there's no server, so the only writes are external
    // injects + the durable-cache hydrate (the /open + /pull fetches just fail and
    // leave the hydrated cache visible — exactly the "offline" scenario we test).
    let config = pocopine_sync_query::QueryClientConfig {
        disable_live: true,
        ..Default::default()
    }
    .with_local_store(store);
    let client = Rc::new(
        pocopine_sync_query::query_client_plugin()
            .config(config)
            .into_client(),
    );
    let view = client.observe(Note::query().eq(notes::field::board, BOARD).build());
    CLIENT.with(|c| *c.borrow_mut() = Some(client));
    VIEW.with(|v| *v.borrow_mut() = Some(view));
}

/// Inject a row as if a realtime transport delivered it — routes into the live
/// view and persists to IndexedDB, with no server round-trip.
#[wasm_bindgen]
pub async fn inject(id: String, body: String) {
    let Some(client) = CLIENT.with(|c| c.borrow().clone()) else {
        return;
    };
    let stream = SyncStreamName::new("notes").unwrap();
    client
        .apply_external_changes(
            &stream,
            vec![RowChange::Upsert(Note {
                id,
                board: BOARD.to_string(),
                body,
            })],
        )
        .await;
}

/// The live view's current rows, as a JSON array (what the test asserts on).
#[wasm_bindgen]
pub fn view_rows() -> String {
    VIEW.with(|v| {
        v.borrow()
            .as_ref()
            .map(|view| serde_json::to_string(&view.rows()).unwrap_or_default())
            .unwrap_or_else(|| "[]".to_string())
    })
}
