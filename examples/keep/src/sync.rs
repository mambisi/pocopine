use pocopine::prelude::*;

#[cfg(pocopine_host)]
use {
    crate::{
        KeepNote, KeepTag,
        model::{keep_notes_for_user, keep_tags_for_user},
        sqlite_stream::{
            KeepNoteSource, KeepTagSource, default_keep_notes_path, default_keep_tags_path,
            keep_row_version,
        },
    },
    pocopine_events::MemoryEventBackend,
    pocopine_sync::SyncServer,
    std::sync::{Arc, OnceLock},
};

#[cfg(pocopine_host)]
static KEEP_SYNC: OnceLock<KeepNoteSource> = OnceLock::new();
#[cfg(pocopine_host)]
static KEEP_TAGS_SYNC: OnceLock<KeepTagSource> = OnceLock::new();
#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();
#[cfg(pocopine_host)]
static SYNC_SERVER: OnceLock<SyncServer> = OnceLock::new();

#[cfg(pocopine_host)]
pub fn keep_notes_source() -> KeepNoteSource {
    KEEP_SYNC
        .get_or_init(|| {
            KeepNoteSource::open(default_keep_notes_path())
                .expect("keep example notes store must open")
        })
        .clone()
}

#[cfg(pocopine_host)]
pub fn keep_tags_source() -> KeepTagSource {
    KEEP_TAGS_SYNC
        .get_or_init(|| {
            KeepTagSource::open(default_keep_tags_path())
                .expect("keep example tags store must open")
        })
        .clone()
}

#[cfg(pocopine_host)]
pub fn live_backend() -> MemoryEventBackend {
    LIVE_BACKEND.get_or_init(MemoryEventBackend::new).clone()
}

#[cfg(pocopine_host)]
pub fn sync_server() -> SyncServer {
    SYNC_SERVER
        .get_or_init(|| {
            // `<resource>::resource(source)` (macro-emitted) pre-wires
            // the row id + RFC 088 §C partition projector and picks up
            // the SQLite-backed `MutationLog` via `Source::mutation_log()`.
            // We add the optimistic-concurrency version extractor
            // (`updated_at_ms`).
            let notes = keep_notes_for_user::resource(keep_notes_source())
                .version_field(keep_row_version::<KeepNote>);
            let tags = keep_tags_for_user::resource(keep_tags_source())
                .version_field(keep_row_version::<KeepTag>);
            SyncServer::builder()
                .public_stream(notes)
                .public_stream(tags)
                .events(Arc::new(live_backend()))
                .build()
        })
        .clone()
}

#[cfg(pocopine_host)]
async fn invalidate_keep_notes() {
    invalidate_keep_stream(crate::KEEP_STREAM).await;
}

#[cfg(pocopine_host)]
async fn invalidate_keep_tags() {
    invalidate_keep_stream(crate::KEEP_TAGS_STREAM).await;
}

#[cfg(pocopine_host)]
async fn invalidate_keep_stream(stream: &str) {
    if let Err(err) = sync_server().invalidate_stream(stream).await {
        tracing::warn!(
            target: "pocopine.log",
            stream,
            error = %err,
            "failed to publish keep sync invalidation"
        );
    }
}

#[pocopine::server(public)]
pub async fn reset_keep_notes() -> ServerResult<()> {
    keep_notes_source()
        .reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    keep_tags_source()
        .reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    invalidate_keep_notes().await;
    invalidate_keep_tags().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client-side sync-query wiring.
//
// `KeepStore` no longer owns a `CollectionState`; the durable cache and
// optimistic overlay live inside `pocopine_sync_query::QueryView`s. These
// helpers are the only seam between the target-agnostic store/handler code
// and the (wasm-only) `QueryClient`:
//
//   * `write_note` / `delete_note_remote` / `write_tag` — fire a single
//     mutation against the client. Handlers build the typed mutation via
//     the macro-emitted `KeepNote::create/update/delete` (and the tag
//     equivalents) and hand it here, so handler bodies stay the same on
//     host and wasm.
//   * `open_keep_sync` — observes the two resource queries, mirrors their
//     rendered rows into `KeepStore.notes` / `.tags`, and keeps the views
//     + update tokens alive for the app lifetime.
//
// Every helper has a real wasm impl and a host no-op stub so the host
// store unit tests build without a `QueryClient` (and never touch the
// network).
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod client {
    use std::cell::Cell;
    use std::rc::Rc;

    use pocopine::prelude::*;
    use pocopine_sync_query::{QueryClient, QueryView, UpdateToken, write};

    use crate::{KeepNote, KeepNoteDraft, KeepStore, KeepTag, KeepTagDraft};

    thread_local! {
        // Monotonic suffix so two mutations fired within the same
        // millisecond still get distinct `MutationId`s.
        static MUTATION_SEQ: Cell<u64> = const { Cell::new(0) };
        // The two live `QueryView`s + their `on_update` tokens. Dropping
        // a token unregisters the listener and dropping the last view
        // clone releases the subscription, so we park both here for the
        // app's lifetime (KeepBoard never unmounts).
        static KEEP_SYNC_HANDLES: std::cell::RefCell<Option<KeepSyncHandles>> =
            const { std::cell::RefCell::new(None) };
    }

    struct KeepSyncHandles {
        _notes_view: QueryView<KeepNote>,
        _tags_view: QueryView<KeepTag>,
        _notes_token: UpdateToken<KeepNote>,
        _tags_token: UpdateToken<KeepTag>,
    }

    fn next_mutation_id(action: &str) -> Option<pocopine_sync::MutationId> {
        let seq = MUTATION_SEQ.with(|cell| {
            let next = cell.get().wrapping_add(1);
            cell.set(next);
            next
        });
        pocopine_sync::MutationId::new(format!("keep:{action}:{}:{seq}", crate::now_ms())).ok()
    }

    fn query_client() -> Option<Rc<QueryClient>> {
        Plugins
            .get::<Rc<QueryClient>>()
            .map(|plugin| (*plugin).clone())
    }

    fn report_save_failure() {
        // These helpers are invoked from inside `KeepStore::update`
        // closures (the store's mutation methods). Writing `status`
        // synchronously here would re-enter the store's `RefCell` and
        // panic, so defer the failure flag to the next microtask —
        // by which point the outer update has released its borrow.
        pocopine::tick::next(|| {
            pocopine::store::<KeepStore>().update(|s| s.status = "save failed".into());
        });
    }

    /// Push a note create/update mutation. The macro-built `TypedMutation`
    /// already carries the optimistic overlay (via `From<(id, draft)>`),
    /// so matching `QueryView`s render the change instantly; the bridge
    /// copies the canonical rows back once the server confirms.
    pub fn write_note(
        mutation: write::TypedMutation<KeepNote, String, KeepNoteDraft>,
        action: &str,
    ) {
        let Some(client) = query_client() else {
            report_save_failure();
            return;
        };
        let Ok(stream) = pocopine_sync::SyncStreamName::new(crate::KEEP_STREAM) else {
            report_save_failure();
            return;
        };
        let Some(mutation_id) = next_mutation_id(action) else {
            report_save_failure();
            return;
        };
        let action = action.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(err) = client
                .push_typed(stream, mutation_id, mutation, pocopine_sync::SYNC_PUSH_PATH)
                .await
            {
                tracing::warn!(
                    target: "pocopine.log",
                    action = %action,
                    error = %err,
                    "keep note write failed"
                );
                report_save_failure();
            }
        });
    }

    /// Optimistically delete a note. Goes through `push` + a
    /// `RowChange::Delete` overlay (rather than `KeepNote::delete().push_typed`)
    /// so the row vanishes from matching views immediately instead of
    /// waiting for the next `/pull`.
    pub fn delete_note_remote(
        id: String,
        expected_version: Option<pocopine_sync::RowVersion>,
        action: &str,
    ) {
        let Some(client) = query_client() else {
            report_save_failure();
            return;
        };
        let Ok(stream) = pocopine_sync::SyncStreamName::new(crate::KEEP_STREAM) else {
            report_save_failure();
            return;
        };
        let Ok(row_key) = pocopine_sync::RowKey::new(id.clone()) else {
            report_save_failure();
            return;
        };
        let Some(mutation_id) = next_mutation_id(action) else {
            report_save_failure();
            return;
        };
        let mut delete = write::DeletePayload::new(id);
        if let Some(version) = expected_version {
            delete = delete.with_expected_version(version);
        }
        let payload = write::MutationPayload::<String, KeepNoteDraft>::Delete(delete);
        let action = action.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(err) = client
                .push(
                    stream,
                    mutation_id,
                    payload,
                    pocopine_sync_query::RowChange::<KeepNote>::Delete(row_key),
                    pocopine_sync::SYNC_PUSH_PATH,
                )
                .await
            {
                tracing::warn!(
                    target: "pocopine.log",
                    action = %action,
                    error = %err,
                    "keep note delete failed"
                );
                report_save_failure();
            }
        });
    }

    /// Push a tag create/update mutation (label registry rows).
    pub fn write_tag(mutation: write::TypedMutation<KeepTag, String, KeepTagDraft>, action: &str) {
        let Some(client) = query_client() else {
            report_save_failure();
            return;
        };
        let Ok(stream) = pocopine_sync::SyncStreamName::new(crate::KEEP_TAGS_STREAM) else {
            report_save_failure();
            return;
        };
        let Some(mutation_id) = next_mutation_id(action) else {
            report_save_failure();
            return;
        };
        let action = action.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(err) = client
                .push_typed(stream, mutation_id, mutation, pocopine_sync::SYNC_PUSH_PATH)
                .await
            {
                tracing::warn!(
                    target: "pocopine.log",
                    action = %action,
                    error = %err,
                    "keep tag write failed"
                );
                report_save_failure();
            }
        });
    }

    /// Observe the two resource queries and bridge their rendered rows
    /// into `KeepStore`. Idempotent for practical purposes — the caller
    /// (`KeepBoard::open_sync_if_needed`) guards against re-entry — but a
    /// second call would simply replace the parked handles.
    pub fn open_keep_sync() {
        let Some(client) = query_client() else {
            pocopine::store::<KeepStore>()
                .update(|s| s.status = "sync plugin not installed".into());
            return;
        };

        let notes_view: QueryView<KeepNote> = client.observe(KeepNote::query().build());
        let tags_view: QueryView<KeepTag> = client.observe(KeepTag::query().build());

        // Seed the store with whatever the views already hold (hydrated
        // cache rows, if any) before wiring the live listeners.
        {
            let notes = notes_view.rows();
            let tags = tags_view.rows();
            pocopine::store::<KeepStore>().update(move |s| {
                s.notes = notes;
                s.tags = tags;
            });
        }

        // The store's own `note_view_signature` / `tag_label_registry`
        // observers (registered in `KeepBoard::on_mount`) rebuild the
        // derived view state when `notes` / `tags` change, so the bridge
        // only mirrors rows — it never calls `rebuild_visible_notes`.
        let notes_for_cb = notes_view.clone();
        let notes_token = notes_view.on_update(move || {
            let notes = notes_for_cb.rows();
            pocopine::store::<KeepStore>().update(move |s| s.notes = notes);
        });
        let tags_for_cb = tags_view.clone();
        let tags_token = tags_view.on_update(move || {
            let tags = tags_for_cb.rows();
            pocopine::store::<KeepStore>().update(move |s| s.tags = tags);
        });

        KEEP_SYNC_HANDLES.with(|cell| {
            *cell.borrow_mut() = Some(KeepSyncHandles {
                _notes_view: notes_view,
                _tags_view: tags_view,
                _notes_token: notes_token,
                _tags_token: tags_token,
            });
        });
    }
}

#[cfg(target_arch = "wasm32")]
pub use client::{delete_note_remote, open_keep_sync, write_note, write_tag};

// Host stubs — keep the store's host unit tests off the network. The
// signatures mirror the wasm impls exactly so handler/store code is
// target-agnostic.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_note(
    _mutation: pocopine_sync_query::write::TypedMutation<
        crate::KeepNote,
        String,
        crate::KeepNoteDraft,
    >,
    _action: &str,
) {
}

#[cfg(not(target_arch = "wasm32"))]
pub fn delete_note_remote(
    _id: String,
    _expected_version: Option<pocopine_sync::RowVersion>,
    _action: &str,
) {
}

#[cfg(not(target_arch = "wasm32"))]
pub fn write_tag(
    _mutation: pocopine_sync_query::write::TypedMutation<
        crate::KeepTag,
        String,
        crate::KeepTagDraft,
    >,
    _action: &str,
) {
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open_keep_sync() {}
