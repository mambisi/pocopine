//! Out-of-line storage for large session payloads (roadmap W7, anti-bloat rule
//! #2: *large tool outputs go out-of-line, never inline in the event stream*).
//!
//! A [`BlobStore`] holds content-addressed bytes. [`ExternalizingSessionStore`]
//! wraps **any** [`SessionStore`] so a record whose payload exceeds a threshold
//! is written to the blob store and replaced in the log by a small reference,
//! rehydrated transparently on read. Identical payloads share one blob (the key
//! is their SHA-256), so a tool that returns the same large output twice costs
//! one copy — and the event log stays small and cat-able regardless of how big
//! individual tool results get.
//!
//! ```no_run
//! # async fn demo() -> pocopine_agenkit::server::session::SessionResult<()> {
//! use std::sync::Arc;
//! use pocopine_agenkit::server::session::{
//!     ExternalizingSessionStore, FsBlobStore, Session, SessionStore, SqliteSessionStore,
//! };
//!
//! // The indexed log keeps refs; the blob dir holds the big tool-output bytes.
//! let store: Arc<dyn SessionStore> = Arc::new(ExternalizingSessionStore::new(
//!     Arc::new(SqliteSessionStore::open("/var/lib/app/threads.db")?),
//!     Arc::new(FsBlobStore::new("/var/lib/app/blobs")),
//! ));
//! let chat = Session::create(store, None).await?;
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::{
    ParentLink, Record, RecordKind, SessionError, SessionFuture, SessionStore, ThreadId,
    ThreadMeta, backend,
};

/// Content-addressed byte storage for out-of-line session payloads. The key is
/// the payload's SHA-256 (hex) — so a `put` of identical bytes is idempotent and
/// naturally de-duplicates.
pub trait BlobStore: Send + Sync {
    /// Store `bytes` under `digest` (idempotent: same digest ⇒ same bytes).
    fn put<'a>(&'a self, digest: &'a str, bytes: Vec<u8>) -> SessionFuture<'a, ()>;

    /// Fetch the bytes stored under `digest`, or `None` if absent.
    fn get<'a>(&'a self, digest: &'a str) -> SessionFuture<'a, Option<Vec<u8>>>;
}

/// An in-memory blob store (tests / dev — bytes are lost on drop).
#[derive(Default, Clone)]
pub struct MemoryBlobStore {
    blobs: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl MemoryBlobStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of distinct blobs held (post-dedup).
    pub fn len(&self) -> usize {
        self.blobs.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the store holds no blobs.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl BlobStore for MemoryBlobStore {
    fn put<'a>(&'a self, digest: &'a str, bytes: Vec<u8>) -> SessionFuture<'a, ()> {
        let result = self
            .blobs
            .lock()
            .map_err(|_| SessionError::Backend("blob store mutex poisoned".into()))
            .map(|mut m| {
                m.insert(digest.to_string(), bytes);
            });
        Box::pin(async move { result })
    }

    fn get<'a>(&'a self, digest: &'a str) -> SessionFuture<'a, Option<Vec<u8>>> {
        let result = self
            .blobs
            .lock()
            .map_err(|_| SessionError::Backend("blob store mutex poisoned".into()))
            .map(|m| m.get(digest).cloned());
        Box::pin(async move { result })
    }
}

/// A filesystem blob store: one content-addressed file `<dir>/<digest>` per blob.
/// Pairs naturally with [`super::JsonlSessionStore`] / [`super::SqliteSessionStore`]
/// — the log/db stays small while the bytes live beside it.
pub struct FsBlobStore {
    dir: PathBuf,
}

impl FsBlobStore {
    /// A store rooted at `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, digest: &str) -> PathBuf {
        self.dir.join(digest)
    }
}

impl BlobStore for FsBlobStore {
    fn put<'a>(&'a self, digest: &'a str, bytes: Vec<u8>) -> SessionFuture<'a, ()> {
        let path = self.path(digest);
        let dir = self.dir.clone();
        Box::pin(async move {
            // Content-addressed: identical bytes ⇒ identical path, so an existing
            // blob is already correct — don't rewrite it.
            if tokio::fs::try_exists(&path).await.map_err(backend)? {
                return Ok(());
            }
            tokio::fs::create_dir_all(&dir).await.map_err(backend)?;
            tokio::fs::write(&path, &bytes).await.map_err(backend)?;
            Ok(())
        })
    }

    fn get<'a>(&'a self, digest: &'a str) -> SessionFuture<'a, Option<Vec<u8>>> {
        let path = self.path(digest);
        Box::pin(async move {
            match tokio::fs::read(&path).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(backend(e)),
            }
        })
    }
}

/// The **reserved** single-key object a record payload is replaced with when its
/// bytes are stored out-of-line: `{ "$session_blob": { "sha256": …, "len": N } }`.
///
/// A record payload must never legitimately be exactly this shape — tools and
/// messages must not emit a top-level `$session_blob` key. On read, a value of
/// this shape is treated as a reference and rehydrated; the recorded `len` is
/// re-checked against the stored bytes, so a forged ref whose length doesn't
/// match is rejected as `Corrupt` rather than silently swapped.
const BLOB_KEY: &str = "$session_blob";

#[derive(Serialize, Deserialize)]
struct BlobRef {
    sha256: String,
    len: u64,
}

/// The wire form of a blob reference (the only thing written to the log).
fn blob_ref_value(r: &BlobRef) -> serde_json::Value {
    serde_json::json!({ BLOB_KEY: { "sha256": r.sha256, "len": r.len } })
}

/// Read `data` back as a blob reference iff it is exactly `{ "$session_blob": … }`.
fn as_blob_ref(data: &serde_json::Value) -> Option<BlobRef> {
    let obj = data.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    serde_json::from_value::<BlobRef>(obj.get(BLOB_KEY)?.clone()).ok()
}

/// Default threshold (bytes): a record payload larger than this is stored
/// out-of-line. 8 KiB comfortably inlines ordinary messages and state changes
/// while externalizing the large tool outputs that otherwise bloat a log.
pub const DEFAULT_BLOB_THRESHOLD: usize = 8 * 1024;

/// A [`SessionStore`] decorator that stores oversized record payloads in a
/// [`BlobStore`] and keeps only a small reference in the log. Reads rehydrate
/// transparently — callers see the original payload, never the reference.
pub struct ExternalizingSessionStore {
    inner: Arc<dyn SessionStore>,
    blobs: Arc<dyn BlobStore>,
    threshold: usize,
}

impl ExternalizingSessionStore {
    /// Wrap `inner`, externalizing payloads over [`DEFAULT_BLOB_THRESHOLD`].
    pub fn new(inner: Arc<dyn SessionStore>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            inner,
            blobs,
            threshold: DEFAULT_BLOB_THRESHOLD,
        }
    }

    /// Override the externalization threshold (bytes).
    pub fn with_threshold(mut self, bytes: usize) -> Self {
        self.threshold = bytes;
        self
    }
}

impl SessionStore for ExternalizingSessionStore {
    fn create_thread<'a>(
        &'a self,
        parent: Option<ParentLink>,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ThreadMeta> {
        // Metadata is small and frequently queried — never externalized.
        self.inner.create_thread(parent, attributes)
    }

    fn meta<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Option<ThreadMeta>> {
        self.inner.meta(id)
    }

    fn threads<'a>(&'a self) -> SessionFuture<'a, Vec<ThreadMeta>> {
        self.inner.threads()
    }

    fn set_attributes<'a>(
        &'a self,
        id: &'a ThreadId,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ()> {
        // Like create_thread: metadata is small and frequently queried — never
        // externalized.
        self.inner.set_attributes(id, attributes)
    }

    fn append<'a>(
        &'a self,
        id: &'a ThreadId,
        kind: RecordKind,
        data: serde_json::Value,
    ) -> SessionFuture<'a, Record> {
        let inner = self.inner.clone();
        let blobs = self.blobs.clone();
        let threshold = self.threshold;
        Box::pin(async move {
            let bytes =
                serde_json::to_vec(&data).map_err(|e| backend(format!("blob serialize: {e}")))?;
            if bytes.len() <= threshold {
                return inner.append(id, kind, data).await;
            }
            // Externalize: store the bytes under their digest, log a small ref.
            let digest = pocopine_crypto::sha256_hex(&bytes);
            let len = bytes.len() as u64;
            blobs.put(&digest, bytes).await?;
            let stored = blob_ref_value(&BlobRef {
                sha256: digest,
                len,
            });
            let record = inner.append(id, kind, stored).await?;
            // The ref is an implementation detail — hand back the original payload.
            Ok(Record { data, ..record })
        })
    }

    fn records<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<Record>> {
        let inner = self.inner.clone();
        let blobs = self.blobs.clone();
        Box::pin(async move {
            let records = inner.records(id).await?;
            let mut out = Vec::with_capacity(records.len());
            for mut record in records {
                if let Some(blob_ref) = as_blob_ref(&record.data) {
                    // A missing blob errors loudly (naming the digest) rather than
                    // passing the raw ref through, which would fail to decode a
                    // layer up and brick the whole thread.
                    let bytes = blobs.get(&blob_ref.sha256).await?.ok_or_else(|| {
                        SessionError::Corrupt(format!(
                            "session blob {} missing ({} bytes)",
                            blob_ref.sha256, blob_ref.len
                        ))
                    })?;
                    // Length re-check (BLOB_KEY is reserved): rejects a forged
                    // look-alike whose length the externalizer never wrote.
                    if bytes.len() as u64 != blob_ref.len {
                        return Err(SessionError::Corrupt(format!(
                            "session blob {} length mismatch: ref says {}, store has {}",
                            blob_ref.sha256,
                            blob_ref.len,
                            bytes.len()
                        )));
                    }
                    record.data = serde_json::from_slice(&bytes)
                        .map_err(|e| SessionError::Corrupt(format!("blob payload: {e}")))?;
                }
                out.push(record);
            }
            Ok(out)
        })
    }

    fn children<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<ThreadId>> {
        self.inner.children(id)
    }

    fn delete_thread<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, ()> {
        // Blobs are content-addressed and may be shared across threads/forks, so
        // deleting a thread does not delete its blobs (GC is a separate sweep).
        self.inner.delete_thread(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::session::{MemorySessionStore, Session};

    fn store() -> (
        Arc<ExternalizingSessionStore>,
        Arc<MemorySessionStore>,
        MemoryBlobStore,
    ) {
        let inner = Arc::new(MemorySessionStore::new());
        let blobs = MemoryBlobStore::new();
        let store = Arc::new(
            ExternalizingSessionStore::new(inner.clone(), Arc::new(blobs.clone()))
                .with_threshold(64),
        );
        (store, inner, blobs)
    }

    #[tokio::test]
    async fn small_payloads_stay_inline() {
        let (store, inner, blobs) = store();
        let store: Arc<dyn SessionStore> = store;
        let meta = store
            .create_thread(None, serde_json::Value::Null)
            .await
            .unwrap();
        store
            .append(
                &meta.id,
                RecordKind::Message,
                serde_json::json!({ "t": "hi" }),
            )
            .await
            .unwrap();

        assert!(blobs.is_empty(), "small payload must not be externalized");
        // The inner store holds the literal payload, not a ref.
        let raw = inner.records(&meta.id).await.unwrap();
        assert_eq!(raw[0].data, serde_json::json!({ "t": "hi" }));
    }

    #[tokio::test]
    async fn large_payloads_go_out_of_line_and_rehydrate() {
        let (store, inner, blobs) = store();
        let store: Arc<dyn SessionStore> = store;
        let meta = store
            .create_thread(None, serde_json::Value::Null)
            .await
            .unwrap();

        let huge = "x".repeat(10_000);
        let payload = serde_json::json!({ "tool_output": huge });
        store
            .append(&meta.id, RecordKind::Message, payload.clone())
            .await
            .unwrap();

        // One blob written; the inner log holds only a small ref.
        assert_eq!(blobs.len(), 1);
        let raw = inner.records(&meta.id).await.unwrap();
        assert!(as_blob_ref(&raw[0].data).is_some(), "log must hold a ref");
        assert!(serde_json::to_vec(&raw[0].data).unwrap().len() < 200);

        // Reading through the decorator rehydrates the original payload.
        let rehydrated = store.records(&meta.id).await.unwrap();
        assert_eq!(rehydrated[0].data, payload);
    }

    #[tokio::test]
    async fn a_missing_blob_is_a_hard_error_not_a_silent_passthrough() {
        // Write a large record (externalized) through a populated blob store.
        let inner = Arc::new(MemorySessionStore::new());
        let writer =
            ExternalizingSessionStore::new(inner.clone(), Arc::new(MemoryBlobStore::new()))
                .with_threshold(64);
        let meta = writer
            .create_thread(None, serde_json::Value::Null)
            .await
            .unwrap();
        writer
            .append(
                &meta.id,
                RecordKind::Message,
                serde_json::json!({ "x": "z".repeat(5_000) }),
            )
            .await
            .unwrap();

        // Reopen over the SAME inner log but a FRESH, empty blob store: the ref's
        // blob is "lost". `records()` surfaces a `Corrupt` naming the digest —
        // not a raw ref that would brick the thread with a decode error upstream.
        let reopened = ExternalizingSessionStore::new(inner, Arc::new(MemoryBlobStore::new()))
            .with_threshold(64);
        let err = reopened.records(&meta.id).await.unwrap_err();
        assert!(
            matches!(err, SessionError::Corrupt(_)),
            "a missing blob must be Corrupt, got {err:?}"
        );
    }

    #[tokio::test]
    async fn identical_large_payloads_share_one_blob() {
        let (store, _inner, blobs) = store();
        let store: Arc<dyn SessionStore> = store;
        let meta = store
            .create_thread(None, serde_json::Value::Null)
            .await
            .unwrap();
        let payload = serde_json::json!({ "out": "y".repeat(5_000) });

        store
            .append(&meta.id, RecordKind::Message, payload.clone())
            .await
            .unwrap();
        store
            .append(&meta.id, RecordKind::Message, payload)
            .await
            .unwrap();

        // Two records, but content-addressing collapses them to one blob.
        assert_eq!(blobs.len(), 1);
        assert_eq!(store.records(&meta.id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn round_trips_a_big_message_through_the_session_handle() {
        let (store, _inner, blobs) = store();
        let store: Arc<dyn SessionStore> = store;
        let chat = Session::create(store, None).await.unwrap();
        let big = serde_json::json!({ "role": "tool", "text": "z".repeat(20_000) });
        chat.append_message(big.clone()).await.unwrap();

        assert_eq!(blobs.len(), 1);
        let history = chat.history().await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].data, big);
    }

    #[tokio::test]
    async fn fs_blob_store_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = FsBlobStore::new(dir.path());
        let digest = pocopine_crypto::sha256_hex(b"payload bytes");

        assert!(blobs.get(&digest).await.unwrap().is_none());
        blobs.put(&digest, b"payload bytes".to_vec()).await.unwrap();
        assert_eq!(
            blobs.get(&digest).await.unwrap().as_deref(),
            Some(&b"payload bytes"[..])
        );
        // Idempotent re-put of identical bytes is a no-op (still readable).
        blobs.put(&digest, b"payload bytes".to_vec()).await.unwrap();
        assert!(blobs.get(&digest).await.unwrap().is_some());
    }
}
