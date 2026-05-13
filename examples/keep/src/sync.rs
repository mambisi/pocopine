use pocopine::prelude::*;

#[cfg(pocopine_host)]
use {
    crate::{
        sqlite_stream::{
            default_keep_notes_path, default_keep_tags_path, SqliteKeepStream, SqliteKeepTagStream,
        },
        KEEP_COLLECTION, KEEP_STREAM, KEEP_TAGS_COLLECTION, KEEP_TAGS_STREAM,
    },
    pocopine_events::MemoryEventBackend,
    pocopine_sync::SyncServer,
    std::sync::{Arc, OnceLock},
};

#[cfg(pocopine_host)]
static KEEP_SYNC: OnceLock<SqliteKeepStream> = OnceLock::new();
#[cfg(pocopine_host)]
static KEEP_TAGS_SYNC: OnceLock<SqliteKeepTagStream> = OnceLock::new();
#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();
#[cfg(pocopine_host)]
static SYNC_SERVER: OnceLock<SyncServer> = OnceLock::new();

#[cfg(pocopine_host)]
pub fn keep_stream() -> SqliteKeepStream {
    KEEP_SYNC
        .get_or_init(|| {
            let stream =
                SqliteKeepStream::open(KEEP_STREAM, KEEP_COLLECTION, default_keep_notes_path())
                    .expect("keep example stream names must be valid");
            seed_keep_notes(&stream).expect("seed keep notes should sync");
            stream
        })
        .clone()
}

#[cfg(pocopine_host)]
pub fn keep_tags_stream() -> SqliteKeepTagStream {
    KEEP_TAGS_SYNC
        .get_or_init(|| {
            SqliteKeepTagStream::open(
                KEEP_TAGS_STREAM,
                KEEP_TAGS_COLLECTION,
                default_keep_tags_path(),
            )
            .expect("keep example tag stream names must be valid")
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
            SyncServer::builder()
                .public_stream(keep_stream())
                .public_stream(keep_tags_stream())
                .events(Arc::new(live_backend()))
                .build()
        })
        .clone()
}

#[cfg(pocopine_host)]
fn seed_keep_notes(_stream: &SqliteKeepStream) -> pocopine_sync::SyncResult<()> {
    // Intentionally empty: the keep example starts with a blank
    // workspace so the user immediately sees the optimistic-insert +
    // live-wakeup loop instead of a pre-baked demo.
    Ok(())
}

#[cfg(pocopine_host)]
async fn invalidate_keep_notes() {
    invalidate_keep_stream(KEEP_STREAM).await;
}

#[cfg(pocopine_host)]
async fn invalidate_keep_tags() {
    invalidate_keep_stream(KEEP_TAGS_STREAM).await;
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
    let stream = keep_stream();
    stream
        .reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    let tags = keep_tags_stream();
    tags.reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    seed_keep_notes(&stream).map_err(|err| ServerError::App(err.to_string()))?;
    invalidate_keep_notes().await;
    invalidate_keep_tags().await;
    Ok(())
}
