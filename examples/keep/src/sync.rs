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
